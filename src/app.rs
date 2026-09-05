use crate::editor::{FileEncoding, LargeFileEditor, LineState};
use regex::Regex;
use std::sync::atomic::Ordering;

use eframe::egui;
use eframe::egui::Panel;

// ギガバイト級の超巨大ファイルをスムーズに閲覧・検索・編集するためのGUIアプリケーションコンポーネント。

/// 巨大ファイルエディタのGUI状態およびイベントハンドリングを管理するメインアプリケーション構造体。
///
/// なぜこの構造に設計したのか：
/// GUIフレームワーク（eframe/egui）の即時モードUIライフサイクルにおいて、
/// バックグラウンドのインデックススキャン・差分編集・検索フィルタリング・仮想スクロール表示を
/// フレームドロップなく滑らかに連携・描画できるように状態を一元管理するためです。
pub struct LargeFileEditorApp {
    editor: Option<LargeFileEditor>,
    selected_encoding: FileEncoding,

    // 検索・置換・フィルターのUI状態
    search_pattern: String,
    replace_pattern: String,
    filter_pattern: String,

    // 設定
    create_backup: bool,

    // スクロールおよび編集状態
    scroll_top_line: usize,
    scroll_accum: f32,
    scroll_to_line: Option<usize>,
    editing_line_idx: Option<usize>,
    editing_text: String,

    // ステータスメッセージ
    status_message: String,
    status_is_error: bool,

    // 終了確認用状態
    show_close_confirmation: bool,
    allowed_to_close: bool,
}

impl Default for LargeFileEditorApp {
    /// デフォルト設定でアプリケーション状態を初期化する。
    ///
    /// なぜデフォルト値をこのように設定したのか：
    /// 起動直後はファイル未選択の安全なアイドル状態とし、UTF-8エンコーディングとバックアップ保存を
    /// 標準で有効にすることでデータ破損を未然に防止するためです。
    fn default() -> Self {
        Self {
            editor: None,
            selected_encoding: FileEncoding::Utf8,
            search_pattern: String::new(),
            replace_pattern: String::new(),
            filter_pattern: String::new(),
            create_backup: true,
            scroll_top_line: 0,
            scroll_accum: 0.0,
            scroll_to_line: None,
            editing_line_idx: None,
            editing_text: String::new(),
            status_message: "ファイルをオープンしてください。".to_string(),
            status_is_error: false,
            show_close_confirmation: false,
            allowed_to_close: false,
        }
    }
}

impl LargeFileEditorApp {
    /// eframeのコンテキストからアプリケーションインスタンスを構築する。
    ///
    /// なぜフォントのセットアップを最初に行うのか：
    /// 日本語フォントを初期化フレームでコンテキストに登録し、ファイル名やログ本文の文字化け（豆腐化）を防止するためです。
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let app = Self::default();
        app.setup_fonts(&ui_ctx_from_creation_context(_cc));
        app
    }

    fn setup_fonts(&self, ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();

        // 埋め込み日本語フォントをロード
        fonts.font_data.insert(
            "noto_sans_jp".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/NotoSansJP-Regular.ttf")).into(),
        );

        // プロポーショナル（標準）とモノスペース（コード）の両方の最優先にセットして豆腐化（文字化け）を解消
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "noto_sans_jp".to_owned());

        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "noto_sans_jp".to_owned());

        ctx.set_fonts(fonts);
    }

    fn show_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status_message = msg.into();
        self.status_is_error = is_error;
    }
}

fn ui_ctx_from_creation_context(cc: &eframe::CreationContext<'_>) -> egui::Context {
    cc.egui_ctx.clone()
}

impl eframe::App for LargeFileEditorApp {
    #[allow(clippy::collapsible_if, clippy::unnecessary_unwrap)]
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());

        // 未保存の変更があり、かつクローズ要求があった場合の処理
        let mut has_unsaved = false;
        if let Some(ref ed) = self.editor {
            if ed.has_unsaved_changes() {
                has_unsaved = true;
            } else if let Some(line_idx) = self.editing_line_idx {
                // 現在編集中のテキストが、元の行テキストと異なっているかチェック
                let original_text = {
                    let edited = ed.edited_lines.read().unwrap();
                    if let Some(state) = edited.get(&line_idx) {
                        match state {
                            LineState::Modified(s) => Some(s.join("\n")),
                            LineState::Deleted => None,
                        }
                    } else {
                        ed.get_line_string(line_idx)
                    }
                };
                if let Some(orig) = original_text {
                    if orig.trim_end() != self.editing_text.trim_end() {
                        has_unsaved = true;
                    }
                }
            }
        }

        if has_unsaved && ctx.input(|i| i.viewport().close_requested()) && !self.allowed_to_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.show_close_confirmation = true;
        }

        // イベントやアクションを蓄積する一時変数 (借用チェッカー対策)
        let mut open_file_clicked = false;
        let mut save_clicked = false;
        let mut save_as_clicked = false;
        let mut batch_replace_clicked = false;
        let mut filter_applied = false;
        let mut filter_cleared = false;

        let mut next_encoding = None;
        let mut show_status_msg = None;

        let mut apply_edit = None;
        let mut delete_edit = None;
        let mut local_replace_text = None;
        let mut deselect_clicked = false;

        // 1. 上部コントロールパネル
        Panel::top("top_bar").show(ui, |ui| {
            ui.style_mut().spacing.item_spacing = egui::vec2(8.0, 8.0);
            ui.add_space(4.0);

            // ファイル関連コントロール
            ui.horizontal(|ui| {
                ui.heading("🚀 Giant Text Editor");
                ui.separator();

                if ui.button("📁 ファイルを開く").clicked() {
                    open_file_clicked = true;
                }

                ui.separator();
                ui.label("エンコーディング:");

                let mut temp_encoding = self.selected_encoding;
                egui::ComboBox::from_id_salt("encoding_select")
                    .selected_text(temp_encoding.to_label())
                    .show_ui(ui, |ui| {
                        for enc in FileEncoding::all_cases() {
                            if ui
                                .selectable_value(&mut temp_encoding, *enc, enc.to_label())
                                .clicked()
                            {
                                next_encoding = Some(*enc);
                            }
                        }
                    });

                ui.separator();
                ui.checkbox(&mut self.create_backup, "バックアップ保存 (.bak)");

                if self.editor.is_some() {
                    if ui.button("💾 保存").clicked() {
                        save_clicked = true;
                    }

                    if ui.button("💾 別名で保存").clicked() {
                        save_as_clicked = true;
                    }
                }
            });

            // 検索・置換・フィルター コントロール
            if self.editor.is_some() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("🔍 検索/置換 (正規表現):");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search_pattern)
                            .hint_text("パターン...")
                            .desired_width(180.0),
                    );

                    ui.label("置換先:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.replace_pattern)
                            .hint_text("置換先...")
                            .desired_width(150.0),
                    );

                    if ui.button("🔄 全体一括バッチ置換").clicked() {
                        batch_replace_clicked = true;
                    }

                    ui.separator();

                    ui.label("⚡ フィルター行抽出:");
                    let filter_edit = ui.add(
                        egui::TextEdit::singleline(&mut self.filter_pattern)
                            .hint_text("パターンで絞り込み...")
                            .desired_width(180.0),
                    );

                    if filter_edit.changed() || ui.button("適用").clicked() {
                        filter_applied = true;
                    }

                    if ui.button("クリア").clicked() {
                        filter_cleared = true;
                    }
                });
            }
            ui.add_space(4.0);
        });

        // 2. 下部ステータスバー
        // なぜPanel::bottomを設定しFrameとselectable_labels=falseを指定するのか：
        // CentralPanelの外側かつウィンドウ最下部にステータスバーを独立配置してエディタ領域を
        // ステータスバーの上端で厳密にクリッピングし、かつ不透明なフレーム背景を設定することで、
        // テキストの潜り込み透過を物理的に防止します。また、selectable_labelsを無効化することで、
        // ドラッグ等の誤操作でステータスバーが青くテキスト選択反転するのを完全に防ぎます。
        Panel::bottom("status_bar")
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(22, 22, 26))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(45, 45, 52)))
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                ui.horizontal(|ui| {
                    if let Some(ref ed) = self.editor {
                        let scan_progress = ed.scan_progress.load(Ordering::Relaxed);
                        let scan_finished = ed.scan_finished.load(Ordering::SeqCst);
                        let line_count = ed.total_lines();

                        ui.label(format!("行数: {}", line_count));
                        ui.separator();

                        if !scan_finished {
                            let pct = if scan_progress > 0 {
                                (scan_progress as f64 / ed.file_size as f64 * 100.0).min(100.0)
                            } else {
                                0.0
                            };
                            ui.label(format!("⏳ インデックス作成中: {:.1}%...", pct));
                            ui.ctx().request_repaint();
                        } else {
                            ui.label("✅ インデックス作成完了");
                        }

                        if !self.filter_pattern.is_empty() {
                            ui.separator();
                            let filter_finished = ed.filter_finished.load(Ordering::SeqCst);
                            let filter_progress = ed.filter_progress.load(Ordering::Relaxed);
                            let matched_count = ed.filter_results.read().unwrap().len();

                            if !filter_finished {
                                ui.label(format!(
                                    "⏳ 検索中: {} 行スキャン ({} 行ヒット)...",
                                    filter_progress, matched_count
                                ));
                                ui.ctx().request_repaint();
                            } else if let Some(err) = ed.filter_error.read().unwrap().as_ref() {
                                ui.colored_label(
                                    egui::Color32::LIGHT_RED,
                                    format!("❌ 正規表現エラー: {}", err),
                                );
                            } else {
                                ui.label(format!("✅ 完了: {} 行ヒット", matched_count));
                            }
                        }
                    }

                    ui.separator();
                    if self.status_is_error {
                        ui.colored_label(egui::Color32::LIGHT_RED, &self.status_message);
                    } else {
                        ui.label(&self.status_message);
                    }
                });
            });

        // 3. 左側：フィルター結果サイドペイン
        if self.editor.is_some() && !self.filter_pattern.is_empty() {
            let ed = self.editor.as_ref().unwrap();
            Panel::left("filter_side_panel")
                .resizable(true)
                .default_size(280.0)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.heading("⚡ フィルター結果");
                        ui.separator();

                        let matched_lines = ed.filter_results.read().unwrap().clone();
                        if matched_lines.is_empty() {
                            ui.colored_label(egui::Color32::GRAY, "一致する行はありません。");
                        } else {
                            let row_height = 22.0;
                            egui::ScrollArea::vertical()
                                .id_salt("filter_scroll")
                                .show_rows(ui, row_height, matched_lines.len(), |ui, range| {
                                    for idx in range {
                                        let original_line_idx = matched_lines[idx];
                                        if let Some(line) = ed.get_line_string(original_line_idx) {
                                            let label_text = format!(
                                                "{}: {}",
                                                original_line_idx + 1,
                                                line.trim_end()
                                            );

                                            let response = ui.add(
                                                egui::Button::new(
                                                    egui::RichText::new(label_text)
                                                        .monospace()
                                                        .size(12.0),
                                                )
                                                .frame(false)
                                                .wrap_mode(egui::TextWrapMode::Truncate)
                                                .min_size(egui::vec2(
                                                    ui.available_width(),
                                                    row_height,
                                                )),
                                            );

                                            if response.clicked() {
                                                self.scroll_to_line = Some(original_line_idx);
                                                self.editing_line_idx = Some(original_line_idx);
                                                self.editing_text = line;
                                            }
                                        }
                                    }
                                });
                        }
                    });
                });
        }

        // 4. 右側：行編集サイドペイン (Vim風の縦分割レイアウト - 選択されている行がある場合のみ右側に表示)
        if self.editor.is_some() && self.editing_line_idx.is_some() {
            let line_idx = self.editing_line_idx.unwrap();
            Panel::right("edit_side_panel")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.heading("📝 行の編集");
                        ui.separator();

                        ui.label(format!("行番号: {} 行目", line_idx + 1));
                        ui.add_space(4.0);

                        ui.label("編集テキスト:");

                        // 動的な高さ計算：利用可能な高さからボタン群の領域 (約90px) を差し引いた分をテキストエリアに割り当てる
                        let button_height = 90.0;
                        let text_edit_height = (ui.available_height() - button_height).max(100.0);

                        egui::ScrollArea::vertical()
                            .id_salt("edit_text_scroll")
                            .max_height(text_edit_height)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.editing_text)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(ui.available_width())
                                        .desired_rows(12),
                                );
                            });
                        ui.add_space(8.0);

                        // 下部に固定されるボタンエリア (ScrollArea の外)
                        ui.horizontal(|ui| {
                            if !self.search_pattern.is_empty() {
                                if ui
                                    .button("🔍 置換適用")
                                    .on_hover_text("この行に正規表現置換を適用")
                                    .clicked()
                                {
                                    if let Ok(re) = Regex::new(&self.search_pattern) {
                                        local_replace_text = Some(
                                            re.replace_all(
                                                &self.editing_text,
                                                &self.replace_pattern,
                                            )
                                            .into_owned(),
                                        );
                                    }
                                }
                            }
                        });
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            if ui.button("確定").clicked() {
                                apply_edit = Some((line_idx, self.editing_text.clone()));
                            }
                            if ui.button("🗑️ 削除").clicked() {
                                delete_edit = Some(line_idx);
                            }
                            if ui.button("選択解除").clicked() {
                                deselect_clicked = true;
                            }
                        });
                    });
                });
        }

        // ローカルな編集テキストの適用 (右パネルの置換適用イベントの反映)
        if let Some(replaced) = local_replace_text {
            self.editing_text = replaced;
        }

        // 5. 中央：メインテキストエディタ
        // なぜCentralPanelを使用するのか：
        // 上下左右のパネル（Panel::top/bottom/left/right）が確定した後の残りの全領域を専有し、
        // 下部ステータスバーの上端でエディタ領域を厳密にクリッピングするためです。
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(ref ed) = self.editor {
                ui.horizontal(|ui| {
                    ui.heading("📄 テキストビュー");
                    if let Some(sel) = self.editing_line_idx {
                        ui.separator();
                        ui.label(format!("選択中: {} 行目", sel + 1));
                        if ui.button("選択解除").clicked() {
                            deselect_clicked = true;
                        }
                    }
                });
                ui.separator();

                if let Some((idx, text)) = apply_edit {
                    ed.edit_line(idx, text);
                    show_status_msg =
                        Some((format!("{}行目を更新しました (メモリ上)。", idx + 1), false));
                }

                if let Some(idx) = delete_edit {
                    ed.delete_line(idx);
                    self.editing_line_idx = None;
                    show_status_msg =
                        Some((format!("{}行目を削除しました (メモリ上)。", idx + 1), false));
                }

                if deselect_clicked {
                    self.editing_line_idx = None;
                }

                // メインエディタビューポート
                // なぜ available_rect_before_wrap で矩形領域を一括取得するのか：
                // CentralPanel 内の残余矩形（ステータスバーの手前で厳密に区切られた排他領域）を直接取得し、
                // 左側のテキストビューと右側のスクロールバーへ幾何学的に排他分割することで、
                // テキスト行の描画によってスクロールバーの高さがブレたり、ツマミがステータスバーの下（画面外）へ
                // 突き抜けてしまう現象を物理的に 100% 防止するためです。
                let full_rect = ui.available_rect_before_wrap();
                let sb_width = 14.0f32;
                let sb_margin = 4.0f32;

                let sb_rect = egui::Rect::from_min_max(
                    egui::pos2(full_rect.max.x - sb_width, full_rect.min.y),
                    egui::pos2(full_rect.max.x, full_rect.max.y),
                );

                let text_rect = egui::Rect::from_min_max(
                    full_rect.min,
                    egui::pos2(
                        (full_rect.max.x - sb_width - sb_margin).max(full_rect.min.x + 50.0),
                        full_rect.max.y,
                    ),
                );

                let total_lines = ed.total_lines();
                let row_height = 20.0f32;
                let available_height = text_rect.height().max(100.0);
                // なぜ fully_visible_lines を floor で計算し max_top を決定するのか：
                // 画面内に完全に収まる行数を基準に max_top = total_lines.saturating_sub(fully_visible_lines) とすることで、
                // スクロールバーを最下部までドラッグした時に、ファイルの一番最後の行（例: 2,000,000行目）が
                // 画面最下部（ステータスバーのすぐ上）にピッタリ完全に表示されるようにするためです。
                let fully_visible_lines = ((available_height / row_height).floor() as usize).max(1);
                let visible_lines = ((available_height / row_height).ceil() as usize) + 1;
                let max_top = total_lines.saturating_sub(fully_visible_lines);

                // スクロール先へのジャンプ要求の処理
                if let Some(target_line) = self.scroll_to_line.take() {
                    let half_visible = fully_visible_lines / 2;
                    self.scroll_top_line = target_line.saturating_sub(half_visible).min(max_top);
                }

                // キーボードナビゲーション処理
                // なぜフォーカスチェックを行うのか：
                // 検索ボックスやテキスト編集エリアに入力中（フォーカス中）は、カーソル移動やテキスト入力を優先し、
                // エディタ画面全体のスクロール操作と競合させないためです。
                let is_any_input_focused = ui.ctx().memory(|m| m.focused().is_some());
                if !is_any_input_focused {
                    let mut key_scrolled = false;
                    if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        self.scroll_top_line = self.scroll_top_line.saturating_sub(1);
                        key_scrolled = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        self.scroll_top_line = (self.scroll_top_line + 1).min(max_top);
                        key_scrolled = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::PageUp)) {
                        self.scroll_top_line = self.scroll_top_line.saturating_sub(fully_visible_lines);
                        key_scrolled = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::PageDown)) {
                        self.scroll_top_line = (self.scroll_top_line + fully_visible_lines).min(max_top);
                        key_scrolled = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Home)) {
                        self.scroll_top_line = 0;
                        key_scrolled = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::End)) {
                        self.scroll_top_line = max_top;
                        key_scrolled = true;
                    }
                    if key_scrolled {
                        ui.ctx().request_repaint();
                    }
                }

                // マウスホイールによるスクロールイベント処理
                // なぜ smooth_scroll_delta と raw.events の双方を取得するのか：
                // 通常のマウスホイール回転イベントはOSから raw.events や smooth_scroll_delta として届きます。
                // 子ウィジェット（横ScrollArea）に消費される前に確実に取得し、中央エディタ領域全体（full_rect）上で
                // スクロールを100%確実に機能させるためです。
                let mut wheel_delta_y = 0.0f32;
                ui.input(|i| {
                    if i.smooth_scroll_delta.y != 0.0 {
                        wheel_delta_y = i.smooth_scroll_delta.y;
                    } else {
                        for event in &i.raw.events {
                            if let egui::Event::MouseWheel { delta, .. } = event {
                                wheel_delta_y += delta.y;
                            }
                        }
                    }
                });

                let pointer_pos = ui.ctx().input(|i| i.pointer.hover_pos());
                let pointer_in_editor = pointer_pos.is_some_and(|pos| full_rect.contains(pos));

                if wheel_delta_y != 0.0 && pointer_in_editor {
                    let lines_to_scroll = if wheel_delta_y.abs() >= 30.0 {
                        (-wheel_delta_y / 30.0).round() as i64
                    } else {
                        self.scroll_accum -= wheel_delta_y;
                        let lines = (self.scroll_accum / row_height) as i64;
                        if lines != 0 {
                            self.scroll_accum -= lines as f32 * row_height;
                        }
                        lines
                    };

                    if lines_to_scroll != 0 {
                        let new_top = (self.scroll_top_line as i64 + lines_to_scroll).clamp(0, max_top as i64);
                        self.scroll_top_line = new_top as usize;
                        ui.ctx().request_repaint();
                    }
                }

                // スクロール位置の境界クリッピング
                if self.scroll_top_line > max_top {
                    self.scroll_top_line = max_top;
                }

                // 画面内に表示する行範囲の決定
                let start_line = self.scroll_top_line.min(total_lines);
                let end_line = (start_line + visible_lines).min(total_lines);

                // ロックの取得回数を減らし描画を高速化するため、表示行データを一括取得
                let line_data = ed.get_display_lines_batch(start_line, end_line);

                // 左側：テキスト表示エリア（text_rect 領域内に子UIを作成して左揃えで描画）
                // なぜLayout::top_down(Align::LEFT)を明示するのか：
                // デフォルトの中央揃えや親レイアウトの継承によるセンタリングを防止し、
                // 通常のテキストエディタと同様にすべての行を左端から整然と描画するためです。
                let mut text_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(text_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                text_ui.set_clip_rect(text_rect);
                egui::ScrollArea::horizontal()
                    .id_salt("main_editor_scroll_h")
                    .show(&mut text_ui, |ui| {
                        ui.style_mut().spacing.item_spacing.y = 0.0;
                        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                            for (display_idx, orig_idx, line_str, is_edited) in line_data {
                                let mut layout_job = egui::text::LayoutJob {
                                    halign: egui::Align::Min,
                                    ..Default::default()
                                };

                                // 表示上の連番行番号（編集で行が増加した際も正しく1行ずつカウント）
                                let num_text = format!("{:>6} │ ", display_idx + 1);
                                layout_job.append(
                                    &num_text,
                                    0.0,
                                    egui::TextFormat {
                                        font_id: egui::FontId::monospace(13.0),
                                        color: egui::Color32::from_gray(120),
                                        ..Default::default()
                                    },
                                );

                                let text_color = if is_edited {
                                    egui::Color32::LIGHT_BLUE
                                } else {
                                    egui::Color32::from_gray(220)
                                };

                                let clean_line = line_str.trim_end();

                                let max_display_len = 4000;
                                let (display_str, _is_truncated) =
                                    if clean_line.len() > max_display_len {
                                        let mut boundary = max_display_len;
                                        while !clean_line.is_char_boundary(boundary)
                                            && boundary > 0
                                        {
                                            boundary -= 1;
                                        }
                                        (&clean_line[..boundary], true)
                                    } else {
                                        (clean_line, false)
                                    };

                                // 検索パターンのハイライト表示
                                if !self.search_pattern.is_empty() {
                                    if let Ok(re) = Regex::new(&self.search_pattern) {
                                        let mut last_idx = 0;
                                        for mat in re.find_iter(display_str) {
                                            if mat.start() > last_idx {
                                                layout_job.append(
                                                    &display_str[last_idx..mat.start()],
                                                    0.0,
                                                    egui::TextFormat {
                                                        font_id: egui::FontId::monospace(13.0),
                                                        color: text_color,
                                                        ..Default::default()
                                                    },
                                                );
                                            }

                                            layout_job.append(
                                                mat.as_str(),
                                                0.0,
                                                egui::TextFormat {
                                                    font_id: egui::FontId::monospace(13.0),
                                                    color: egui::Color32::BLACK,
                                                    background: egui::Color32::YELLOW,
                                                    ..Default::default()
                                                },
                                            );
                                            last_idx = mat.end();
                                        }
                                        if last_idx < display_str.len() {
                                            layout_job.append(
                                                &display_str[last_idx..],
                                                0.0,
                                                egui::TextFormat {
                                                    font_id: egui::FontId::monospace(13.0),
                                                    color: text_color,
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                    } else {
                                        layout_job.append(
                                            display_str,
                                            0.0,
                                            egui::TextFormat {
                                                font_id: egui::FontId::monospace(13.0),
                                                color: text_color,
                                                ..Default::default()
                                            },
                                        );
                                    }
                                } else {
                                    layout_job.append(
                                        display_str,
                                        0.0,
                                        egui::TextFormat {
                                            font_id: egui::FontId::monospace(13.0),
                                            color: text_color,
                                            ..Default::default()
                                        },
                                    );
                                }

                                let is_selected = self.editing_line_idx == Some(orig_idx);

                                // なぜpainter().galleyによる直接描画を行うのか：
                                // Button等の既定ウィジェットが持つセンタリングや内部マージンの影響を100%排除し、
                                // エディタのテキスト行を左端（row_rect.min.x + 4px）から厳密に左揃えで配置するためです。
                                // また、行全体のクリック選択やホバー時の背景ハイライト、横スクロール幅の追従も高パフォーマンスに実現します。
                                let galley = ui.fonts_mut(|f| f.layout_job(layout_job));
                                let row_width = ui.available_width().max(galley.size().x + 20.0);
                                let (row_rect, response) = ui.allocate_exact_size(
                                    egui::vec2(row_width, row_height),
                                    egui::Sense::click(),
                                );

                                if is_selected {
                                    ui.painter().rect_filled(
                                        row_rect,
                                        0.0,
                                        egui::Color32::from_rgb(45, 50, 65),
                                    );
                                } else if response.hovered() {
                                    ui.painter().rect_filled(
                                        row_rect,
                                        0.0,
                                        egui::Color32::from_rgb(30, 32, 40),
                                    );
                                }

                                let text_pos = egui::pos2(
                                    row_rect.min.x + 4.0,
                                    row_rect.min.y + (row_height - galley.size().y).max(0.0) / 2.0,
                                );
                                ui.painter().galley(text_pos, galley, egui::Color32::WHITE);

                                if response.clicked() {
                                    self.editing_line_idx = Some(orig_idx);
                                    let current_text = if let Some(LineState::Modified(lines)) =
                                        ed.edited_lines.read().unwrap().get(&orig_idx)
                                    {
                                        lines.join("\n")
                                    } else {
                                        line_str.to_string()
                                    };
                                    self.editing_text = current_text;
                                }
                            }
                        });
                    });

                // 右側：専用の縦スクロールバー
                // なぜ sb_rect に直接描画し対話するのか：
                // テキスト行の描画量や ScrollArea のコンテンツサイズに一切左右されず、
                // ウィンドウの CentralPanel の高さ（ステータスバーの上端まで）に 100% 厳密に一致した
                // スクロールバーとツマミの描画・操作を実現するためです。
                let sb_response = ui.interact(
                    sb_rect,
                    ui.id().with("editor_vertical_scrollbar"),
                    egui::Sense::click_and_drag(),
                );

                ui.painter().rect_filled(sb_rect, 2.0, egui::Color32::from_rgb(25, 25, 30));

                let track_height = sb_rect.height() as f64;
                let min_thumb_h = 24.0f64;
                let thumb_h = if total_lines > 0 {
                    ((fully_visible_lines as f64 / total_lines as f64) * track_height)
                        .clamp(min_thumb_h, track_height)
                } else {
                    track_height
                };
                let scrollable_dist = track_height - thumb_h;

                // クリックまたはドラッグ時のスクロール位置計算
                // なぜ f64 精度で計算するのか：
                // 数千万行（例: 16,917,062行）の巨大ログファイルでは、f32（仮数部24bit ≈ 1677万）だと
                // 浮動小数点精度が不足して最下端や位置計算にズレが生じるため、f64で厳密に計算します。
                if max_top > 0 && scrollable_dist > 0.0 {
                    if sb_response.dragged() || sb_response.clicked() {
                        if let Some(mouse_pos) = ui.ctx().pointer_interact_pos() {
                            let rel_y = (mouse_pos.y as f64 - sb_rect.min.y as f64 - thumb_h / 2.0)
                                .clamp(0.0, scrollable_dist);
                            let ratio = rel_y / scrollable_dist;
                            self.scroll_top_line =
                                ((ratio * max_top as f64).round() as usize).min(max_top);
                            ui.ctx().request_repaint();
                        }
                    }
                }

                // スクロールバーのつまみ（Thumb）の描画
                let thumb_y = if max_top > 0 && scrollable_dist > 0.0 {
                    ((self.scroll_top_line as f64 / max_top as f64) * scrollable_dist) as f32
                } else {
                    0.0
                };
                let thumb_rect = egui::Rect::from_min_size(
                    egui::pos2(sb_rect.min.x + 2.0, sb_rect.min.y + thumb_y),
                    egui::vec2(sb_rect.width() - 4.0, thumb_h as f32),
                );

                let thumb_color = if sb_response.dragged() {
                    egui::Color32::from_rgb(150, 150, 165)
                } else if sb_response.hovered() {
                    egui::Color32::from_rgb(115, 115, 125)
                } else {
                    egui::Color32::from_rgb(75, 75, 85)
                };
                ui.painter().rect_filled(thumb_rect, 3.0, thumb_color);

                // 中央パネルの領域を消費
                ui.advance_cursor_after_rect(full_rect);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new("📁 上部のボタンからファイルを開いてください。\n（GBクラスのログファイルも高速に開くことができます）")
                            .size(18.0)
                            .weak(),
                    );
                });
            }
        });

        // 後処理: 一時変数に溜めたアクションを self に書き戻す (借用競合の回避完了後)
        if open_file_clicked {
            let opt_path = rfd::FileDialog::new()
                .set_title("テキストファイルを選択")
                .pick_file();
            if let Some(path) = opt_path {
                match LargeFileEditor::open(&path, self.selected_encoding) {
                    Ok(ed) => {
                        self.editor = Some(ed);
                        self.editing_line_idx = None;
                        self.scroll_top_line = 0;
                        self.scroll_accum = 0.0;
                        self.show_status(
                            format!("ファイルをロードしました: {}", path.display()),
                            false,
                        );
                    }
                    Err(e) => {
                        self.show_status(format!("エラー: {}", e), true);
                    }
                }
            }
        }

        if let Some(enc) = next_encoding {
            self.selected_encoding = enc;
            if let Some(ref ed) = self.editor {
                let path = ed.path.clone();
                match LargeFileEditor::open(&path, self.selected_encoding) {
                    Ok(new_ed) => {
                        self.editor = Some(new_ed);
                        self.editing_line_idx = None;
                        self.scroll_top_line = 0;
                        self.scroll_accum = 0.0;
                        self.show_status(
                            "指定エンコーディングでファイルを再ロードしました。",
                            false,
                        );
                    }
                    Err(e) => {
                        self.show_status(format!("再ロードエラー: {}", e), true);
                    }
                }
            }
        }

        if save_clicked && self.editor.is_some() {
            let ed = self.editor.as_ref().unwrap();
            let path = ed.path.clone();
            match ed.save(&path, self.create_backup) {
                Ok(_) => self.show_status("保存が完了しました。", false),
                Err(e) => self.show_status(format!("保存失敗: {}", e), true),
            }
        }

        if save_as_clicked && self.editor.is_some() {
            let ed = self.editor.as_ref().unwrap();
            let opt_save_path = rfd::FileDialog::new()
                .set_title("名前を付けて保存")
                .set_file_name(
                    ed.path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref(),
                )
                .save_file();
            if let Some(save_path) = opt_save_path {
                match ed.save(&save_path, false) {
                    Ok(_) => self.show_status(format!("保存完了: {}", save_path.display()), false),
                    Err(e) => self.show_status(format!("保存失敗: {}", e), true),
                }
            }
        }

        if batch_replace_clicked && self.editor.is_some() {
            let ed = self.editor.as_ref().unwrap();
            if self.search_pattern.is_empty() {
                self.show_status("検索正規表現パターンを入力してください。", true);
            } else {
                let opt_dest_path = rfd::FileDialog::new()
                    .set_title("一括置換後のファイル保存先")
                    .set_file_name(
                        ed.path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                    )
                    .save_file();
                if let Some(dest_path) = opt_dest_path {
                    match ed.batch_replace(
                        &self.search_pattern,
                        &self.replace_pattern,
                        &dest_path,
                        false,
                    ) {
                        Ok(_) => {
                            show_status_msg = Some((
                                format!("バッチ置換・保存完了: {}", dest_path.display()),
                                false,
                            ))
                        }
                        Err(e) => show_status_msg = Some((format!("置換失敗: {}", e), true)),
                    }
                }
            }
        }

        if filter_applied && self.editor.is_some() {
            let ed = self.editor.as_mut().unwrap();
            ed.start_filter(&self.filter_pattern);
        }

        if filter_cleared {
            self.filter_pattern.clear();
            if self.editor.is_some() {
                let ed = self.editor.as_mut().unwrap();
                ed.start_filter("");
            }
        }

        if let Some((msg, is_err)) = show_status_msg {
            self.show_status(msg, is_err);
        }

        // 終了確認ダイアログの表示 (未保存の変更がある状態でクローズされようとした時)
        if self.show_close_confirmation {
            let mut close_confirmed = false;
            let mut save_and_close_confirmed = false;
            let mut cancel_confirmed = false;

            egui::Window::new("⚠️ 未保存の変更があります")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(&ctx, |ui| {
                    ui.label("保存されていない編集中のデータがあります。ファイルを保存せずに終了しますか？");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("💾 保存して終了").clicked() {
                            save_and_close_confirmed = true;
                        }
                        if ui.button("⚠️ 保存せずに終了").clicked() {
                            close_confirmed = true;
                        }
                        if ui.button("キャンセル").clicked() {
                            cancel_confirmed = true;
                        }
                    });
                });

            if save_and_close_confirmed && self.editor.is_some() {
                let ed = self.editor.as_ref().unwrap();
                let path = ed.path.clone();
                let backup = self.create_backup;
                if ed.save(&path, backup).is_ok() {
                    self.allowed_to_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }

            if close_confirmed {
                self.allowed_to_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }

            if cancel_confirmed {
                self.show_close_confirmation = false;
            }
        }
    }
}
