#![allow(dead_code)]
use encoding_rs::Encoding;
use memmap2::Mmap;
use regex::Regex;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;

// メモリマッピングと非同期インデックス作成により巨大ファイルを高速かつ安全に閲覧・編集するためのバックエンドモジュール。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEncoding {
    Utf8,
    ShiftJis,
    Utf16Le,
    Utf16Be,
}

impl FileEncoding {
    pub fn to_encoding(self) -> &'static Encoding {
        match self {
            FileEncoding::Utf8 => encoding_rs::UTF_8,
            FileEncoding::ShiftJis => encoding_rs::SHIFT_JIS,
            FileEncoding::Utf16Le => encoding_rs::UTF_16LE,
            FileEncoding::Utf16Be => encoding_rs::UTF_16BE,
        }
    }

    pub fn all_cases() -> &'static [FileEncoding] {
        &[
            FileEncoding::Utf8,
            FileEncoding::ShiftJis,
            FileEncoding::Utf16Le,
            FileEncoding::Utf16Be,
        ]
    }

    pub fn to_label(self) -> &'static str {
        match self {
            FileEncoding::Utf8 => "UTF-8",
            FileEncoding::ShiftJis => "Shift-JIS",
            FileEncoding::Utf16Le => "UTF-16 LE",
            FileEncoding::Utf16Be => "UTF-16 BE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineState {
    /// 1行が複数行（1行以上）に編集・分割された状態
    Modified(Vec<String>),
    /// 行が削除された状態（0行）
    Deleted,
}

pub struct LargeFileEditor {
    pub path: PathBuf,
    pub encoding: FileEncoding,
    pub mmap: Arc<Mmap>,
    _file: File, // ライフタイム維持のために保持
    pub file_size: usize,

    // 行インデックスの非同期管理
    pub line_offsets: Arc<RwLock<Vec<usize>>>,
    pub scan_progress: Arc<AtomicUsize>,
    pub scan_finished: Arc<AtomicBool>,

    // 編集差分管理 (行インデックス -> 編集状態)
    pub edited_lines: Arc<RwLock<BTreeMap<usize, LineState>>>,

    // 非同期検索・フィルター管理
    filter_thread: Option<thread::JoinHandle<()>>,
    filter_cancel: Arc<AtomicBool>,
    pub filter_results: Arc<RwLock<Vec<usize>>>,
    pub filter_progress: Arc<AtomicUsize>, // スキャン済みの行数
    pub filter_finished: Arc<AtomicBool>,
    pub filter_error: Arc<RwLock<Option<String>>>,
}

impl LargeFileEditor {
    pub fn open<P: AsRef<Path>>(
        path: P,
        encoding: FileEncoding,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let file_size = file.metadata()?.len() as usize;

        // 空ファイル対応
        let mmap = if file_size == 0 {
            // 空のMmapを作ることはできないため、ダミーの空Mmapやエラーハンドリングが必要
            // ここでは簡易的に0バイト書き込み済みのテンポラリMmapなどを作るか、
            // 空ファイル用の特別扱いを行う。
            // memmap2は0バイトのマップを許容しないため、ダミーデータで代用するか、
            // 内部で空スライスとして扱う。
            return Err("Empty files are not supported via mmap. Please use a regular text editor for empty files.".into());
        } else {
            unsafe { Mmap::map(&file)? }
        };

        let mmap = Arc::new(mmap);
        let line_offsets = Arc::new(RwLock::new(Vec::new()));
        let scan_progress = Arc::new(AtomicUsize::new(0));
        let scan_finished = Arc::new(AtomicBool::new(false));

        // バックグラウンドスレッドで改行インデックスの作成を開始
        let mmap_clone = Arc::clone(&mmap);
        let offsets_clone = Arc::clone(&line_offsets);
        let progress_clone = Arc::clone(&scan_progress);
        let finished_clone = Arc::clone(&scan_finished);

        thread::spawn(move || {
            scan_offsets(
                &mmap_clone,
                encoding,
                offsets_clone,
                progress_clone,
                finished_clone,
            );
        });

        Ok(Self {
            path,
            encoding,
            mmap,
            _file: file,
            file_size,
            line_offsets,
            scan_progress,
            scan_finished,
            edited_lines: Arc::new(RwLock::new(BTreeMap::new())),
            filter_thread: None,
            filter_cancel: Arc::new(AtomicBool::new(false)),
            filter_results: Arc::new(RwLock::new(Vec::new())),
            filter_progress: Arc::new(AtomicUsize::new(0)),
            filter_finished: Arc::new(AtomicBool::new(false)),
            filter_error: Arc::new(RwLock::new(None)),
        })
    }

    /// インデックス作成済みの基本行数を取得する。
    ///
    /// なぜスキャン完了前は未確定の末尾行を除外するのか：
    /// バックグラウンドでのインデックス作成中（`!scan_finished`）は、最後のオフセット行は
    /// まだ次の改行が見つかっておらず終端が未確定です。もしこの行を描画対象に含めてしまうと、
    /// 行の終端がファイル末尾（`self.file_size`）とみなされ、ファイル全体の数億文字が1行として
    /// メモリ展開されてUIがクラッシュするため、スキャン完了までは改行が確定している行のみを返します。
    pub fn base_lines_count(&self) -> usize {
        let offsets = self.line_offsets.read().unwrap();
        let len = offsets.len();
        if self.scan_finished.load(Ordering::SeqCst) {
            len
        } else {
            len.saturating_sub(1)
        }
    }

    /// 現在安全に表示・アクセス可能な総行数を取得する（編集による増減を反映）。
    ///
    /// なぜ編集差分を反映して計算するのか：
    /// ユーザーが行編集で改行を挿入して複数行に増やした場合や、行を削除した場合に、
    /// 全体の行カウントが追従して正しく更新され、後続の行番号がずれるのを防ぐためです。
    pub fn total_lines(&self) -> usize {
        let base = self.base_lines_count();
        let edited = self.edited_lines.read().unwrap();
        let mut total = base;
        for (&orig_idx, state) in edited.iter() {
            if orig_idx < base {
                match state {
                    LineState::Modified(lines) => {
                        total = (total as isize + (lines.len() as isize - 1)).max(0) as usize;
                    }
                    LineState::Deleted => {
                        total = total.saturating_sub(1);
                    }
                }
            }
        }
        total
    }

    pub fn has_unsaved_changes(&self) -> bool {
        !self.edited_lines.read().unwrap().is_empty()
    }

    /// 指定された元の行の生のバイト列スライスをmmapから取得する。
    ///
    /// なぜスキャン未完了時の末尾行アクセスをガードするのか：
    /// スキャン中に未確定の行オフセットへアクセスされた場合、次の行が存在しないからといって
    /// `file_size` までを行とみなすと、ファイル全体（数百MB〜数GB）が1行として扱われ
    /// メモリ枯渇やUIのTDRフリーズを引き起こすためです。スキャン未完了時は確定範囲外のアクセスに対して安全にNoneを返します。
    pub fn get_line_raw_from_mmap(&self, line_idx: usize) -> Option<&[u8]> {
        let offsets = self.line_offsets.read().unwrap();
        if line_idx >= offsets.len() {
            return None;
        }
        let start = offsets[line_idx];
        let end = if line_idx + 1 < offsets.len() {
            offsets[line_idx + 1]
        } else if self.scan_finished.load(Ordering::SeqCst) {
            self.file_size
        } else {
            // スキャン途中で次の改行が未確定の場合は、巨大なファイル全体を1行として返さないようガード
            return None;
        };

        if start >= self.file_size || start >= end || end > self.file_size {
            return None;
        }

        Some(&self.mmap[start..end])
    }

    /// 元のmmap上の指定行の文字列を取得する（改行トリミング・デコード済み）。
    pub fn get_line_string_raw_from_mmap(&self, line_idx: usize) -> Option<String> {
        let raw_bytes = self.get_line_raw_from_mmap(line_idx)?;
        let mut clean_bytes = raw_bytes;

        // 末尾の改行バイトを除去
        match self.encoding {
            FileEncoding::Utf8 | FileEncoding::ShiftJis => {
                while !clean_bytes.is_empty()
                    && (clean_bytes[clean_bytes.len() - 1] == b'\n'
                        || clean_bytes[clean_bytes.len() - 1] == b'\r')
                {
                    clean_bytes = &clean_bytes[..clean_bytes.len() - 1];
                }
            }
            FileEncoding::Utf16Le => {
                while clean_bytes.len() >= 2 {
                    let len = clean_bytes.len();
                    let b1 = clean_bytes[len - 2];
                    let b2 = clean_bytes[len - 1];
                    if (b1 == 0x0A || b1 == 0x0D) && b2 == 0x00 {
                        clean_bytes = &clean_bytes[..len - 2];
                    } else {
                        break;
                    }
                }
            }
            FileEncoding::Utf16Be => {
                while clean_bytes.len() >= 2 {
                    let len = clean_bytes.len();
                    let b1 = clean_bytes[len - 2];
                    let b2 = clean_bytes[len - 1];
                    if (b2 == 0x0A || b2 == 0x0D) && b1 == 0x00 {
                        clean_bytes = &clean_bytes[..len - 2];
                    } else {
                        break;
                    }
                }
            }
        }

        let encoder = self.encoding.to_encoding();
        let (res, _, _) = encoder.decode(clean_bytes);
        Some(res.into_owned())
    }

    /// 後方互換用：生のバイト列取得。
    pub fn get_line_raw(&self, line_idx: usize) -> Option<&[u8]> {
        self.get_line_raw_from_mmap(line_idx)
    }

    /// 表示行インデックスから、(元の行インデックス, 内容文字列, 編集済みフラグ) を取得する。
    ///
    /// なぜ差分テーブルを順次適用してインデックスを解決するのか：
    /// 編集箇所（BTreeMap）は通常数行〜数十行と非常に少ないため、元の巨大な配列全体を作り直すことなく
    /// O(K)（Kは編集箇所数）で瞬時に表示行の内容と元の行番号を特定できるためです。
    pub fn get_display_line(&self, display_idx: usize) -> Option<(usize, String, bool)> {
        let edited = self.edited_lines.read().unwrap();
        if edited.is_empty() {
            let s = self.get_line_string_raw_from_mmap(display_idx)?;
            return Some((display_idx, s, false));
        }

        let base_lines = self.base_lines_count();
        let mut curr_disp = 0;
        let mut curr_orig = 0;

        for (&orig_idx, state) in edited.iter() {
            if orig_idx >= base_lines {
                break;
            }
            let unedited_count = orig_idx.saturating_sub(curr_orig);
            if display_idx < curr_disp + unedited_count {
                let target_orig = curr_orig + (display_idx - curr_disp);
                let s = self.get_line_string_raw_from_mmap(target_orig)?;
                return Some((target_orig, s, false));
            }
            curr_disp += unedited_count;
            curr_orig = orig_idx;

            match state {
                LineState::Modified(lines) => {
                    if display_idx < curr_disp + lines.len() {
                        let sub_idx = display_idx - curr_disp;
                        return Some((orig_idx, lines[sub_idx].clone(), true));
                    }
                    curr_disp += lines.len();
                }
                LineState::Deleted => {}
            }
            curr_orig += 1;
        }

        if curr_orig < base_lines {
            let target_orig = curr_orig + display_idx.saturating_sub(curr_disp);
            if target_orig < base_lines {
                let s = self.get_line_string_raw_from_mmap(target_orig)?;
                return Some((target_orig, s, false));
            }
        }

        None
    }

    /// 表示用の行文字列を取得する。
    pub fn get_line_string(&self, display_idx: usize) -> Option<String> {
        self.get_display_line(display_idx).map(|(_, s, _)| s)
    }

    /// 画面表示に必要な行データ（数十行分）を一括で取得する。
    ///
    /// 返り値: Vec<(表示行インデックス, 元の行インデックス, 行文字列, 編集フラグ)>
    /// なぜ一括取得するのか：1フレームの描画に必要な行データを1度のロック取得でまとめてマッピングすることで、
    /// バックグラウンドスキャンとのロック競合を最小化し、描画を高速化するためです。
    pub fn get_display_lines_batch(
        &self,
        start_line: usize,
        end_line: usize,
    ) -> Vec<(usize, usize, String, bool)> {
        let mut result = Vec::with_capacity(end_line.saturating_sub(start_line));
        for idx in start_line..end_line {
            if let Some((orig_idx, line_str, is_edited)) = self.get_display_line(idx) {
                result.push((idx, orig_idx, line_str, is_edited));
            }
        }
        result
    }

    /// 指定された元の行を新しい内容で置換する。改行が含まれている場合は複数行として分割登録される。
    ///
    /// なぜ改行で分割して保持するのか：
    /// 右ペインの編集で改行を入れて行が増えた際に、エディタ全体の行数が増加し、
    /// それぞれの行が独立した行番号を持って表示されるようにするためです。
    pub fn edit_line(&self, orig_idx: usize, new_content: String) {
        let lines: Vec<String> = new_content
            .split('\n')
            .map(|s| s.trim_end_matches('\r').to_string())
            .collect();
        let mut edited = self.edited_lines.write().unwrap();
        edited.insert(orig_idx, LineState::Modified(lines));
    }

    /// 指定された元の行を削除する。
    pub fn delete_line(&self, orig_idx: usize) {
        let mut edited = self.edited_lines.write().unwrap();
        edited.insert(orig_idx, LineState::Deleted);
    }

    // 非同期フィルター処理の開始
    pub fn start_filter(&mut self, pattern: &str) {
        // 既存のフィルター処理をキャンセル
        self.filter_cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.filter_thread.take() {
            let _ = handle.join();
        }

        // 状態のリセット
        self.filter_cancel.store(false, Ordering::SeqCst);
        *self.filter_results.write().unwrap() = Vec::new();
        self.filter_progress.store(0, Ordering::Relaxed);
        self.filter_finished.store(false, Ordering::SeqCst);
        *self.filter_error.write().unwrap() = None;

        if pattern.is_empty() {
            self.filter_finished.store(true, Ordering::SeqCst);
            return;
        }

        let re_res = Regex::new(pattern);
        let re = match re_res {
            Ok(r) => r,
            Err(e) => {
                *self.filter_error.write().unwrap() = Some(e.to_string());
                self.filter_finished.store(true, Ordering::SeqCst);
                return;
            }
        };

        let cancel = Arc::clone(&self.filter_cancel);
        let results = Arc::clone(&self.filter_results);
        let progress = Arc::clone(&self.filter_progress);
        let finished = Arc::clone(&self.filter_finished);

        // スキャン状況を同期しながら検索するために、自分自身の一部データを共有
        let offsets = Arc::clone(&self.line_offsets);
        let scan_finished = Arc::clone(&self.scan_finished);
        let mmap_clone = Arc::clone(&self.mmap);
        let encoding = self.encoding;
        let edited_lines = Arc::clone(&self.edited_lines);
        let file_size = self.file_size;

        self.filter_thread = Some(thread::spawn(move || {
            let mut current_line = 0;
            loop {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }

                // インデックス作成の進捗状況を取得
                // なぜsafe_max_lineを用いるのか：
                // スキャン中の末尾行はまだ次の改行が見つかっておらず終端が未確定です。
                // スキャン完了前はその行を検索対象に含めず、次の改行が発見されるまで待機することで
                // 検索スレッドで巨大なファイル末尾までをデコードする負荷・クラッシュを回避します。
                let is_scan_finished = scan_finished.load(Ordering::SeqCst);
                let current_offsets_len = offsets.read().unwrap().len();
                let safe_max_line = if is_scan_finished {
                    current_offsets_len
                } else {
                    current_offsets_len.saturating_sub(1)
                };

                if current_line >= safe_max_line {
                    // 全インデックス作成が終わっているなら終了
                    if is_scan_finished {
                        break;
                    }
                    // まだインデックス作成中なら、少し待って再試行
                    thread::sleep(std::time::Duration::from_millis(30));
                    continue;
                }

                // 1行取得して正規表現マッチを確認
                let has_match = {
                    // 編集差分があるか確認
                    if let Some(state) = edited_lines.read().unwrap().get(&current_line) {
                        match state {
                            LineState::Modified(lines) => lines.iter().any(|l| re.is_match(l)),
                            LineState::Deleted => false,
                        }
                    } else {
                        // ファイルから直接バイトを取得して一時的に文字列にデコード
                        let offsets_guard = offsets.read().unwrap();
                        let start = offsets_guard[current_line];
                        let end = if current_line + 1 < offsets_guard.len() {
                            offsets_guard[current_line + 1]
                        } else {
                            file_size
                        };

                        if start < file_size {
                            let raw_bytes = &mmap_clone[start..end];
                            // 改行コードトリミング
                            let mut clean_bytes = raw_bytes;
                            match encoding {
                                FileEncoding::Utf8 | FileEncoding::ShiftJis => {
                                    while !clean_bytes.is_empty()
                                        && (clean_bytes[clean_bytes.len() - 1] == b'\n'
                                            || clean_bytes[clean_bytes.len() - 1] == b'\r')
                                    {
                                        clean_bytes = &clean_bytes[..clean_bytes.len() - 1];
                                    }
                                }
                                FileEncoding::Utf16Le => {
                                    while clean_bytes.len() >= 2 {
                                        let len = clean_bytes.len();
                                        if (clean_bytes[len - 2] == 0x0A
                                            || clean_bytes[len - 2] == 0x0D)
                                            && clean_bytes[len - 1] == 0x00
                                        {
                                            clean_bytes = &clean_bytes[..len - 2];
                                        } else {
                                            break;
                                        }
                                    }
                                }
                                FileEncoding::Utf16Be => {
                                    while clean_bytes.len() >= 2 {
                                        let len = clean_bytes.len();
                                        if (clean_bytes[len - 1] == 0x0A
                                            || clean_bytes[len - 1] == 0x0D)
                                            && clean_bytes[len - 2] == 0x00
                                        {
                                            clean_bytes = &clean_bytes[..len - 2];
                                        } else {
                                            break;
                                        }
                                    }
                                }
                            }

                            let encoder = encoding.to_encoding();
                            let (decoded, _, _) = encoder.decode(clean_bytes);
                            re.is_match(&decoded)
                        } else {
                            false
                        }
                    }
                };

                if has_match {
                    results.write().unwrap().push(current_line);
                }

                current_line += 1;
                // 1000行ごとに進捗を更新
                if current_line % 1000 == 0 {
                    progress.store(current_line, Ordering::Relaxed);
                }
            }
            progress.store(current_line, Ordering::Relaxed);
            finished.store(true, Ordering::SeqCst);
        }));
    }

    // 保存処理 (アトミック書き込み + バックアップ)
    pub fn save(&self, dest_path: &Path, backup: bool) -> Result<(), Box<dyn std::error::Error>> {
        let temp_file_path = dest_path.with_extension("tmp_save");
        let out_file = File::create(&temp_file_path)?;
        let mut writer = BufWriter::new(out_file);

        let base_lines = self.base_lines_count();
        let encoder = self.encoding.to_encoding();
        let edited = self.edited_lines.read().unwrap();

        // 既定の改行バイト列（ファイル内に改行があればそれを採用、なければCRLF/LF）
        let default_newline: &[u8] = match self.encoding {
            FileEncoding::Utf8 | FileEncoding::ShiftJis => b"\r\n",
            FileEncoding::Utf16Le => &[0x0D, 0x00, 0x0A, 0x00],
            FileEncoding::Utf16Be => &[0x00, 0x0D, 0x00, 0x0A],
        };

        for i in 0..base_lines {
            if let Some(state) = edited.get(&i) {
                match state {
                    LineState::Modified(lines) => {
                        let raw = self.get_line_raw_from_mmap(i);
                        let newline = raw
                            .map(|r| get_newline_bytes(r, self.encoding))
                            .unwrap_or(default_newline);
                        let nl = if newline.is_empty() {
                            default_newline
                        } else {
                            newline
                        };

                        for (sub_idx, sub_line) in lines.iter().enumerate() {
                            let (encoded, _, _) = encoder.encode(sub_line);
                            writer.write_all(&encoded)?;
                            if sub_idx + 1 < lines.len()
                                || !newline.is_empty()
                                || i + 1 < base_lines
                            {
                                writer.write_all(nl)?;
                            }
                        }
                    }
                    LineState::Deleted => {
                        // 削除された行はスキップ
                    }
                }
            } else if let Some(raw) = self.get_line_raw_from_mmap(i) {
                writer.write_all(raw)?;
            }
        }

        writer.flush()?;
        drop(writer);

        if backup && dest_path.exists() {
            let backup_path = dest_path.with_extension("bak");
            std::fs::rename(dest_path, backup_path)?;
        }

        std::fs::rename(temp_file_path, dest_path)?;
        Ok(())
    }

    // 高速バッチ置換機能 (ファイル全体を対象に置換し別名または同名保存)
    pub fn batch_replace(
        &self,
        pattern: &str,
        replacement: &str,
        dest_path: &Path,
        backup: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let re = Regex::new(pattern)?;
        let temp_file_path = dest_path.with_extension("tmp_replace");
        let out_file = File::create(&temp_file_path)?;
        let mut writer = BufWriter::new(out_file);

        let total_lines = self.total_lines();
        let encoder = self.encoding.to_encoding();

        for i in 0..total_lines {
            if let Some(line) = self.get_line_string(i) {
                // 置換処理
                let replaced = re.replace_all(&line, replacement).into_owned();
                let (encoded, _, _) = encoder.encode(&replaced);
                writer.write_all(&encoded)?;

                // 改行コード書き出し
                if let Some(raw) = self.get_line_raw(i) {
                    let newline_bytes = get_newline_bytes(raw, self.encoding);
                    writer.write_all(newline_bytes)?;
                }
            }
        }

        writer.flush()?;
        drop(writer);

        if backup && dest_path.exists() {
            let backup_path = dest_path.with_extension("bak");
            std::fs::rename(dest_path, backup_path)?;
        }

        std::fs::rename(temp_file_path, dest_path)?;
        Ok(())
    }
}

// ヘルパー：行の生バイト列から末尾の改行バイトスライスを抽出する
fn get_newline_bytes(raw: &[u8], encoding: FileEncoding) -> &[u8] {
    match encoding {
        FileEncoding::Utf8 | FileEncoding::ShiftJis => {
            if raw.ends_with(b"\r\n") {
                &raw[raw.len() - 2..]
            } else if raw.ends_with(b"\n") {
                &raw[raw.len() - 1..]
            } else {
                &[]
            }
        }
        FileEncoding::Utf16Le => {
            if raw.ends_with(&[0x0D, 0x00, 0x0A, 0x00]) {
                &raw[raw.len() - 4..]
            } else if raw.ends_with(&[0x0A, 0x00]) {
                &raw[raw.len() - 2..]
            } else {
                &[]
            }
        }
        FileEncoding::Utf16Be => {
            if raw.ends_with(&[0x00, 0x0D, 0x00, 0x0A]) {
                &raw[raw.len() - 4..]
            } else if raw.ends_with(&[0x00, 0x0A]) {
                &raw[raw.len() - 2..]
            } else {
                &[]
            }
        }
    }
}

// 非同期行インデックス作成
fn scan_offsets(
    mmap: &[u8],
    encoding: FileEncoding,
    line_offsets: Arc<RwLock<Vec<usize>>>,
    scan_progress: Arc<AtomicUsize>,
    scan_finished: Arc<AtomicBool>,
) {
    // 最初の行はオフセット0から開始
    {
        let mut offsets = line_offsets.write().unwrap();
        offsets.push(0);
    }

    let len = mmap.len();
    let mut pos = 0;

    match encoding {
        FileEncoding::Utf8 | FileEncoding::ShiftJis => {
            while pos < len {
                let chunk_end = (pos + 4 * 1024 * 1024).min(len); // 4MB chunks
                let chunk = &mmap[pos..chunk_end];
                let mut local_offsets = Vec::new();

                for (i, &b) in chunk.iter().enumerate() {
                    if b == b'\n' {
                        let next_pos = pos + i + 1;
                        if next_pos < len {
                            local_offsets.push(next_pos);
                        }
                    }
                }

                if !local_offsets.is_empty() {
                    let mut offsets = line_offsets.write().unwrap();
                    offsets.extend(local_offsets);
                }

                pos = chunk_end;
                scan_progress.store(pos, Ordering::Relaxed);
            }
        }
        FileEncoding::Utf16Le => {
            while pos < len {
                let chunk_end = (pos + 4 * 1024 * 1024).min(len);
                let mut local_offsets = Vec::new();
                let mut i = 0;

                while pos + i + 1 < chunk_end {
                    let b1 = mmap[pos + i];
                    let b2 = mmap[pos + i + 1];
                    if b1 == 0x0A && b2 == 0x00 {
                        let next_pos = pos + i + 2;
                        if next_pos < len {
                            local_offsets.push(next_pos);
                        }
                    }
                    i += 2;
                }

                if !local_offsets.is_empty() {
                    let mut offsets = line_offsets.write().unwrap();
                    offsets.extend(local_offsets);
                }

                pos = chunk_end;
                scan_progress.store(pos, Ordering::Relaxed);
            }
        }
        FileEncoding::Utf16Be => {
            while pos < len {
                let chunk_end = (pos + 4 * 1024 * 1024).min(len);
                let mut local_offsets = Vec::new();
                let mut i = 0;

                while pos + i + 1 < chunk_end {
                    let b1 = mmap[pos + i];
                    let b2 = mmap[pos + i + 1];
                    if b1 == 0x00 && b2 == 0x0A {
                        let next_pos = pos + i + 2;
                        if next_pos < len {
                            local_offsets.push(next_pos);
                        }
                    }
                    i += 2;
                }

                if !local_offsets.is_empty() {
                    let mut offsets = line_offsets.write().unwrap();
                    offsets.extend(local_offsets);
                }

                pos = chunk_end;
                scan_progress.store(pos, Ordering::Relaxed);
            }
        }
    }

    scan_progress.store(len, Ordering::Relaxed);
    scan_finished.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_utf8_load_and_search() {
        let mut file = NamedTempFile::new().unwrap();
        let content = "Hello World\nこんにちは 世界\nRust Programming\nLine with ERROR in it\n";
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let mut editor = LargeFileEditor::open(file.path(), FileEncoding::Utf8).unwrap();

        while !editor.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(editor.total_lines(), 4);
        assert_eq!(editor.get_line_string(0).unwrap(), "Hello World");
        assert_eq!(editor.get_line_string(1).unwrap(), "こんにちは 世界");

        editor.start_filter("ERROR");
        while !editor.filter_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        let results = editor.filter_results.read().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], 3);
    }

    #[test]
    fn test_shift_jis_load() {
        let mut file = NamedTempFile::new().unwrap();
        let sjis_encoder = encoding_rs::SHIFT_JIS;
        let (bytes, _, _) = sjis_encoder.encode("こんにちは 世界\n改行テスト\n");
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();

        let editor = LargeFileEditor::open(file.path(), FileEncoding::ShiftJis).unwrap();
        while !editor.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(editor.total_lines(), 2);
        assert_eq!(editor.get_line_string(0).unwrap(), "こんにちは 世界");
        assert_eq!(editor.get_line_string(1).unwrap(), "改行テスト");
    }

    #[test]
    fn test_batch_replace() {
        let mut file = NamedTempFile::new().unwrap();
        let content = "log level: INFO\nlog level: ERROR\nlog level: INFO\n";
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let editor = LargeFileEditor::open(file.path(), FileEncoding::Utf8).unwrap();
        while !editor.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        let dest = NamedTempFile::new().unwrap();
        editor
            .batch_replace("INFO", "DEBUG", dest.path(), false)
            .unwrap();

        let new_editor = LargeFileEditor::open(dest.path(), FileEncoding::Utf8).unwrap();
        while !new_editor.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(new_editor.total_lines(), 3);
        assert_eq!(new_editor.get_line_string(0).unwrap(), "log level: DEBUG");
        assert_eq!(new_editor.get_line_string(2).unwrap(), "log level: DEBUG");
    }

    /// 行編集で改行を含めた際に、行数（total_lines）が増加し、行表示および行番号が正しく追従することを検証するテスト。
    ///
    /// なぜこのテストが必要か：
    /// ユーザーが1行を編集して複数行（改行）に増やした際、行数が正しく増加して各行が個別の行として
    /// 表示・取得できることを保証するためです。
    #[test]
    fn test_multiline_edit_and_line_count() {
        let mut file = NamedTempFile::new().unwrap();
        let content = "Line 1\nLine 2\nLine 3\n";
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let editor = LargeFileEditor::open(file.path(), FileEncoding::Utf8).unwrap();
        while !editor.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(editor.total_lines(), 3);

        // 1行目（Line 2）を改行を含む3行に編集
        editor.edit_line(1, "Line 2-A\nLine 2-B\nLine 2-C".to_string());

        // 行数が 3 から 5 (+2) に増えていることを検証
        assert_eq!(editor.total_lines(), 5);

        // 表示行が正しくマッピングされているか検証
        assert_eq!(editor.get_line_string(0).unwrap(), "Line 1");
        assert_eq!(editor.get_line_string(1).unwrap(), "Line 2-A");
        assert_eq!(editor.get_line_string(2).unwrap(), "Line 2-B");
        assert_eq!(editor.get_line_string(3).unwrap(), "Line 2-C");
        assert_eq!(editor.get_line_string(4).unwrap(), "Line 3");

        // 一括取得バッチでも正しく取得できることを検証
        let batch = editor.get_display_lines_batch(0, 5);
        assert_eq!(batch.len(), 5);
        assert_eq!(batch[1].2, "Line 2-A");
        assert_eq!(batch[1].3, true); // is_edited が true
        assert_eq!(batch[4].2, "Line 3");
        assert_eq!(batch[4].3, false); // is_edited が false

        // 行削除の検証
        editor.delete_line(2); // 元の Line 3 を削除
        assert_eq!(editor.total_lines(), 4);
        assert_eq!(editor.get_line_string(3).unwrap(), "Line 2-C");
    }

    /// 複数行に編集した内容がファイル保存（save）時に正しく書き出されるかを検証するテスト。
    ///
    /// なぜこのテストが必要か：
    /// メモリ上で改行分割された差分が、ファイルストリーミング保存時にも正確に改行コード付きで書き込まれることを担保するためです。
    #[test]
    fn test_multiline_save() {
        let mut file = NamedTempFile::new().unwrap();
        let content = "First\nSecond\nThird\n";
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let editor = LargeFileEditor::open(file.path(), FileEncoding::Utf8).unwrap();
        while !editor.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        // 0行目と1行目を編集
        editor.edit_line(0, "Header 1\nHeader 2".to_string());
        editor.edit_line(1, "Modified Second".to_string());

        let save_file = NamedTempFile::new().unwrap();
        editor.save(save_file.path(), false).unwrap();

        let reloaded = LargeFileEditor::open(save_file.path(), FileEncoding::Utf8).unwrap();
        while !reloaded.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(reloaded.total_lines(), 4);
        assert_eq!(reloaded.get_line_string(0).unwrap(), "Header 1");
        assert_eq!(reloaded.get_line_string(1).unwrap(), "Header 2");
        assert_eq!(reloaded.get_line_string(2).unwrap(), "Modified Second");
        assert_eq!(reloaded.get_line_string(3).unwrap(), "Third");
    }
}
