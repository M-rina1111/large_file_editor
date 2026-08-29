mod app;
mod editor;

use app::LargeFileEditorApp;

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
