use std::sync::atomic::Ordering;
use std::thread;
use std::time::Instant;

// バックエンドのファイル読み込みとインデックス作成、検索処理のパフォーマンスを測定するためのベンチマークプログラム。

#[path = "../editor.rs"]
mod editor;

use editor::{FileEncoding, LargeFileEditor};

/// バックエンド処理のパフォーマンス計測と検証を行うメイン関数。
///
/// 巨大なログファイルを開き、インデックス作成と正規表現による
/// フィルタリングが正しく、かつ効率的に動作するかを検証します。
fn main() {
    let path = "c:\\Users\\zarus\\cargo_target\\large_test.log";
    println!("--- Benchmarking Large File Backend ---");
    println!("Opening file: {}", path);

    let start = Instant::now();
    let mut editor = LargeFileEditor::open(path, FileEncoding::Utf8).unwrap();
    println!("File mapped in {:?}", start.elapsed());

    // インデックス作成を待機
    // なぜループで待機するのか：非同期で実行されているインデックス作成処理が
    // 完了するまでの正確な時間を測定するため。
    let index_start = Instant::now();
    while !editor.scan_finished.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(10));
    }
    let index_duration = index_start.elapsed();
    println!("Indexing completed in {:?}", index_duration);
    println!("Total lines indexed: {}", editor.total_lines());

    // フィルターテスト
    let filter_start = Instant::now();
    editor.start_filter("ERROR");
    while !editor.filter_finished.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(10));
    }
    let filter_duration = filter_start.elapsed();
    let matched_lines = editor.filter_results.read().unwrap().len();
    println!("Regex filter ('ERROR') completed in {:?}", filter_duration);
    println!("Matched lines count: {}", matched_lines);

    // 特定の行の取得
    // なぜ1,500,000行目を取得するのか：インデックス境界値付近で
    // 正しくデータが取得できるかテストするため。
    let line_start = Instant::now();
    let line = editor.get_line_string(1500000).unwrap();
    println!("Retrieved line 1,500,001 in {:?}", line_start.elapsed());
    println!("Content: {}", line);
}
