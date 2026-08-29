use crate::editor::{FileEncoding, LargeFileEditor, LineState};
use regex::Regex;
use std::sync::atomic::Ordering;

use eframe::egui;
use eframe::egui::Panel;

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
    fn default() -> Self {
        Self {
            editor: None,
            selected_encoding: FileEncoding::Utf8,
            search_pattern: String::new(),
            replace_pattern: String::new(),
            filter_pattern: String::new(),
            create_backup: true,
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
                            LineState::Modified(s) => Some(s.clone()),
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
        Panel::bottom("status_bar").show(ui, |ui| {
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
                        } else {
                            if let Some(err) = ed.filter_error.read().unwrap().as_ref() {
                                ui.colored_label(
                                    egui::Color32::LIGHT_RED,
                                    format!("❌ 正規表現エラー: {}", err),
                                );
                            } else {
                                ui.label(format!("✅ 完了: {} 行ヒット", matched_count));
                            }
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

        // 5. 中央・メインテキストエディタ
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
            let total_lines = ed.total_lines();
            let row_height = 20.0;

            egui::ScrollArea::vertical()
                .id_salt("main_editor_scroll")
                .max_width(ui.available_width())
                .show_rows(ui, row_height, total_lines, |ui, range| {
                    for line_idx in range {
                        let (line_text, is_edited, is_deleted) = {
                            let edited = ed.edited_lines.read().unwrap();
                            if let Some(state) = edited.get(&line_idx) {
                                match state {
                                    LineState::Modified(s) => (Some(s.clone()), true, false),
                                    LineState::Deleted => (None, false, true),
                                }
                            } else {
                                (ed.get_line_string(line_idx), false, false)
                            }
                        };

                        if is_deleted {
                            ui.horizontal(|ui| {
                                draw_line_number(ui, line_idx + 1, row_height);
                                ui.colored_label(
                                    egui::Color32::DARK_GRAY,
                                    egui::RichText::new("(削除された行)").strikethrough(),
                                );
                            });
                            continue;
                        }

                        if let Some(line) = line_text {
                            let mut layout_job = egui::text::LayoutJob::default();

                            // 行番号
                            let num_text = format!("{:>6} │ ", line_idx + 1);
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

                            let line_str = line.trim_end();

                            // 検索パターンのハイライト表示
                            if !self.search_pattern.is_empty() {
                                if let Ok(re) = Regex::new(&self.search_pattern) {
                                    let mut last_idx = 0;
                                    for mat in re.find_iter(line_str) {
                                        if mat.start() > last_idx {
                                            layout_job.append(
                                                &line_str[last_idx..mat.start()],
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
                                    if last_idx < line_str.len() {
                                        layout_job.append(
                                            &line_str[last_idx..],
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
                                        line_str,
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
                                    line_str,
                                    0.0,
                                    egui::TextFormat {
                                        font_id: egui::FontId::monospace(13.0),
                                        color: text_color,
                                        ..Default::default()
                                    },
                                );
                            }

                            let is_selected = self.editing_line_idx == Some(line_idx);
                            let bg_color = if is_selected {
                                Some(egui::Color32::from_rgba_unmultiplied(80, 80, 120, 50))
                            } else {
                                None
                            };

                            let mut button = egui::Button::new(layout_job)
                                .frame(bg_color.is_some())
                                .min_size(egui::vec2(ui.available_width(), row_height));

                            if let Some(bg) = bg_color {
                                button = button.fill(bg);
                            } else {
                                button = button.frame(false);
                            }

                            let response = ui.add(button);

                            if response.clicked() {
                                self.editing_line_idx = Some(line_idx);
                                self.editing_text = line_str.to_string();
                            }

                            if self.scroll_to_line == Some(line_idx) {
                                response.scroll_to_me(Some(egui::Align::Center));
                                self.scroll_to_line = None;
                            }
                        }
                    }
                });
        } else {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new("📁 上部のボタンからファイルを開いてください。\n（GBクラスのログファイルも高速に開くことができます）")
                    .size(18.0)
                    .weak());
            });
        }

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

fn draw_line_number(ui: &mut egui::Ui, line_num: usize, row_height: f32) {
    ui.add_sized(
        egui::vec2(60.0, row_height),
        egui::Label::new(
            egui::RichText::new(format!("{:>6} │ ", line_num))
                .monospace()
                .color(egui::Color32::from_gray(120))
                .size(13.0),
        ),
    );
}
