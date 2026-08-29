#![allow(dead_code)]
use encoding_rs::Encoding;
use memmap2::Mmap;
use regex::Regex;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;

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

pub enum LineState {
    Modified(String),
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
    pub edited_lines: Arc<RwLock<HashMap<usize, LineState>>>,

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
            edited_lines: Arc::new(RwLock::new(HashMap::new())),
            filter_thread: None,
            filter_cancel: Arc::new(AtomicBool::new(false)),
            filter_results: Arc::new(RwLock::new(Vec::new())),
            filter_progress: Arc::new(AtomicUsize::new(0)),
            filter_finished: Arc::new(AtomicBool::new(false)),
            filter_error: Arc::new(RwLock::new(None)),
        })
    }

    pub fn total_lines(&self) -> usize {
        self.line_offsets.read().unwrap().len()
    }

    pub fn has_unsaved_changes(&self) -> bool {
        !self.edited_lines.read().unwrap().is_empty()
    }

    pub fn get_line_raw(&self, line_idx: usize) -> Option<&[u8]> {
        let offsets = self.line_offsets.read().unwrap();
        if line_idx >= offsets.len() {
            return None;
        }
        let start = offsets[line_idx];
        let end = if line_idx + 1 < offsets.len() {
            offsets[line_idx + 1]
        } else {
            self.file_size
        };

        if start >= self.file_size {
            return None;
        }

        Some(&self.mmap[start..end])
    }

    pub fn get_line_string(&self, line_idx: usize) -> Option<String> {
        // メモリ上の編集差分を確認
        if let Some(state) = self.edited_lines.read().unwrap().get(&line_idx) {
            return match state {
                LineState::Modified(s) => Some(s.clone()),
                LineState::Deleted => None,
            };
        }

        let raw_bytes = self.get_line_raw(line_idx)?;
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

    pub fn edit_line(&self, line_idx: usize, new_content: String) {
        let mut edited = self.edited_lines.write().unwrap();
        edited.insert(line_idx, LineState::Modified(new_content));
    }

    pub fn delete_line(&self, line_idx: usize) {
        let mut edited = self.edited_lines.write().unwrap();
        edited.insert(line_idx, LineState::Deleted);
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

                // インデックス作成がどこまで進んだかを取得
                let max_line = offsets.read().unwrap().len();

                if current_line >= max_line {
                    // 全インデックス作成が終わっているなら終了
                    if scan_finished.load(Ordering::SeqCst) {
                        break;
                    }
                    // まだインデックス作成中なら、少し待って再試行
                    thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }

                // 1行取得して正規表現マッチを確認
                let has_match = {
                    // 編集差分があるか確認
                    if let Some(state) = edited_lines.read().unwrap().get(&current_line) {
                        match state {
                            LineState::Modified(s) => re.is_match(s),
                            LineState::Deleted => false,
                        }
                    } else {
                        // ファイルから直接バイトを取得して一時的に文字列にデコード
                        let start = offsets.read().unwrap()[current_line];
                        let end = if current_line + 1 < max_line {
                            offsets.read().unwrap()[current_line + 1]
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
                                         if (clean_bytes[len - 2] == 0x0A || clean_bytes[len - 2] == 0x0D)
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
                                         if (clean_bytes[len - 1] == 0x0A || clean_bytes[len - 1] == 0x0D)
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

        let total_lines = self.total_lines();
        let encoder = self.encoding.to_encoding();

        for i in 0..total_lines {
            if let Some(line) = self.get_line_string(i) {
                // 行の内容をエンコードして書き出し
                let (encoded, _, _) = encoder.encode(&line);
                writer.write_all(&encoded)?;

                // 改行コードを維持して書き出し
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
}
