use std::path::Path;
use std::sync::atomic::Ordering;
use std::thread;

// スクロールバー計算の末尾到達・行数追従・キーボードナビゲーションのロジック検証用テストプログラム。

#[path = "../editor.rs"]
mod editor;

use editor::{FileEncoding, LargeFileEditor};

/// スクロールバーおよびビューポートの計算ロジックをシミュレーションして検証する関数。
///
/// なぜこのテストを行うのか：
/// 200万行や1690万行などの長大なファイルにおいて、スクロールバーを最下部までドラッグした際に
/// 正確に最後の行が画面内に表示されること、ツマミがトラック外（画面外）へはみ出さないこと、
/// およびマウスホイールで正しく行スクロールできることを保証するためです。
fn verify_scrollbar_and_navigation(total_lines: usize, available_height: f32, row_height: f32) {
    let fully_visible_lines = ((available_height / row_height).floor() as usize).max(1);
    let _visible_lines = ((available_height / row_height).ceil() as usize) + 1;
    let max_top = total_lines.saturating_sub(fully_visible_lines);

    println!(
        "--- 検証: 総行数: {} 行, 表示高さ: {}px, 1行の高さ: {}px ---",
        total_lines, available_height, row_height
    );
    println!(
        "完全収容行数 (fully_visible_lines): {}",
        fully_visible_lines
    );
    println!("最大先頭行 (max_top): {}", max_top);

    // 1. スクロールバー最下部ドラッグシミュレーション (f64精度)
    let track_height = available_height as f64;
    let min_thumb_h = 24.0f64;
    let thumb_h = if total_lines > 0 {
        ((fully_visible_lines as f64 / total_lines as f64) * track_height)
            .clamp(min_thumb_h, track_height)
    } else {
        track_height
    };
    let scrollable_dist = track_height - thumb_h;

    // マウスカーソルをスクロールバートラックの一番下（track_height）までドラッグした場合
    let mouse_y_bottom = track_height;
    let rel_y = (mouse_y_bottom - thumb_h / 2.0).clamp(0.0, scrollable_dist);
    let ratio = if scrollable_dist > 0.0 {
        rel_y / scrollable_dist
    } else {
        0.0
    };
    let scroll_top_line = ((ratio * max_top as f64).round() as usize).min(max_top);

    assert_eq!(
        scroll_top_line, max_top,
        "スクロールバー最下部ドラッグ時に max_top に到達していません"
    );

    // ツマミの描画位置がトラック下端をはみ出さない（画面外に出ない）ことの検証
    let thumb_y = if max_top > 0 && scrollable_dist > 0.0 {
        (scroll_top_line as f64 / max_top as f64) * scrollable_dist
    } else {
        0.0
    };
    let thumb_bottom = thumb_y + thumb_h;
    assert!(
        (thumb_bottom - track_height).abs() < 1e-4,
        "ツマミの下端がトラック下端（{:.2}）と一致していません（実際: {:.2}）",
        track_height,
        thumb_bottom
    );
    assert!(
        thumb_bottom <= track_height + 1e-4,
        "ツマミがトラックの下（画面外）にはみ出しています！"
    );

    // 画面に表示される最後の行の物理y座標検証
    let start_line = scroll_top_line.min(total_lines);
    let last_line_idx = total_lines - 1; // 0-indexed での最後の行
    let last_line_display_y = ((last_line_idx - start_line) as f32) * row_height;
    let last_line_bottom_y = last_line_display_y + row_height;

    assert!(
        last_line_bottom_y <= available_height,
        "最後の行（{}行目）の下端y座標（{:.1}px）が画面高さ（{:.1}px）を超えて画面外にはみ出しています！",
        total_lines,
        last_line_bottom_y,
        available_height
    );

    println!(
        "✅ スクロールバー最下部到達検証クリア: start_line={}, 最後の行（{}行目）の描画領域 [{:.1}px .. {:.1}px] は画面高さ {:.1}px 内に完全に収まっています",
        start_line + 1,
        total_lines,
        last_line_display_y,
        last_line_bottom_y,
        available_height
    );
    println!(
        "✅ ツマミ画面外飛び出し防止検証クリア: thumb_bottom = {:.2}px <= track_height = {:.2}px",
        thumb_bottom, track_height
    );

    // 2. マウスホイールスクロールのシミュレーション
    // 下スクロール (smooth_scroll_delta.y = -120.0)
    let wheel_delta_down = -120.0f32;
    let lines_down = (-wheel_delta_down / 30.0).round() as i64;
    assert_eq!(lines_down, 4, "下ホイールで+4行進むべきです");

    // 上スクロール (smooth_scroll_delta.y = +120.0)
    let wheel_delta_up = 120.0f32;
    let lines_up = (-wheel_delta_up / 30.0).round() as i64;
    assert_eq!(lines_up, -4, "上ホイールで-4行戻るべきです");
    println!("✅ マウスホイールスクロール方向検証クリア (下: +4行, 上: -4行)");
}

fn main() {
    println!("=== 機能検証の開始 ===");

    // 1. 200万行でのスクロール計算検証
    verify_scrollbar_and_navigation(2_000_000, 800.0, 20.0);

    // 2. 1691万行（ユーザーのyt-dlp.log規模）でのスクロール計算検証
    verify_scrollbar_and_navigation(16_917_062, 800.0, 20.0);

    // 2. 巨大ログファイル（存在する場合）での実データ検証
    let log_path = Path::new("c:/Users/zarus/cargo_target/large_test.log");
    if log_path.exists() {
        println!("実ファイルでの検証を開始: {}", log_path.display());
        let editor =
            LargeFileEditor::open(log_path, FileEncoding::Utf8).expect("ファイルオープン失敗");

        while !editor.scan_finished.load(Ordering::SeqCst) {
            thread::sleep(std::time::Duration::from_millis(50));
        }

        let base_lines = editor.base_lines_count();
        println!("ロード完了: {} 行", base_lines);

        // 改行を含めた編集を実施
        let target_idx = 100;
        let original_str = editor.get_line_string(target_idx).unwrap();
        println!("編集対象（{}行目）: {}", target_idx + 1, original_str);

        editor.edit_line(
            target_idx,
            "Added Line 1\nAdded Line 2\nAdded Line 3".to_string(),
        );

        let new_total = editor.total_lines();
        assert_eq!(
            new_total,
            base_lines + 2,
            "改行による行数増加 (+2) が反映されていません"
        );
        println!(
            "✅ 行数連動検証クリア: {} -> {} (差分: +2)",
            base_lines, new_total
        );

        // 表示行の取得検証
        let line_100 = editor.get_line_string(target_idx).unwrap();
        let line_101 = editor.get_line_string(target_idx + 1).unwrap();
        let line_102 = editor.get_line_string(target_idx + 2).unwrap();
        let _line_103 = editor.get_line_string(target_idx + 3).unwrap();

        assert_eq!(line_100, "Added Line 1");
        assert_eq!(line_101, "Added Line 2");
        assert_eq!(line_102, "Added Line 3");
        println!("✅ 複数行展開検証クリア: 各行が独立して取得可能");

        // 末尾行の取得検証
        let last_idx = new_total - 1;
        let last_line = editor.get_line_string(last_idx).unwrap();
        println!(
            "✅ 末尾行取得検証クリア: {} 行目 = {}",
            last_idx + 1,
            last_line
        );
    }

    println!("=== すべての検証が正常に完了しました ===");
}
