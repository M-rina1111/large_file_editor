mod app;
mod editor;

use app::LargeFileEditorApp;

// 巨大ログファイルエディタを起動するためのメインエントリーポイント。

/// アプリケーションのエントリーポイント関数。
///
/// なぜネイティブウィンドウの初期サイズを1200x800に設定するのか：
/// 巨大なログファイルのテキストビュー、サイドの検索フィルター結果ペイン、
/// および行編集パネルを同時に快適に一覧・操作できる十分な領域を確保するためです。
fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("🚀 Giant Text Editor")
            .with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "giant_text_editor",
        options,
        Box::new(|cc| Ok(Box::new(LargeFileEditorApp::new(cc)))),
    )
}
