// テスト用の巨大なログファイルを書き込み権限のあるカスタムターゲットディレクトリに生成するためのプログラム。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// テスト用のログファイルを生成するメイン関数。
///
/// なぜ 'c:\Users\zarus\cargo_target\' を使うのか：ドキュメントフォルダ以下への
/// 新規ファイル書き込みがOSやセキュリティソフトによってブロックされるのを避けるため、
/// 確実に書き込み可能なビルドディレクトリを使用します。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path_str = "c:\\Users\\zarus\\cargo_target\\large_test.log";
    println!("Creating file at: {}", path_str);

    let path = Path::new(path_str);
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    // 200万行のダミーログを書き出す
    // なぜ200万行か：数百万行規模のファイルでUIがクラッシュするかを検証するため
    for i in 1..=2000000 {
        if i % 10000 == 0 {
            writeln!(
                writer,
                "Line {}: [ERROR] Something went wrong! Code: {}",
                i,
                i * 3
            )?;
        } else if i % 2500 == 0 {
            writeln!(writer, "Line {}: [WARN] High memory usage detected.", i)?;
        } else {
            writeln!(
                writer,
                "Line {}: [INFO] Application is running smoothly. User session active.",
                i
            )?;
        }
    }

    writer.flush()?;
    println!("Generated large_test.log with 2,000,000 lines.");
    Ok(())
}
