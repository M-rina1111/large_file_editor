Edited by Gemini

# Giant Text Editor (🚀 Giant Text Editor)

A blazing-fast, cross-platform desktop text editor written in Rust, designed specifically for viewing, searching, and editing massive files (multi-gigabyte log files, data dumps, etc.) without freezing or consuming excessive memory. Powered by `egui` and memory-mapped file access.

---

# 🚀 Giant Text Editor (日本語概要)

数GBクラスの巨大なログファイルやデータダンプを、メモリ消費を抑えながら高速に閲覧・検索・編集するために設計された、Rustと`egui`、メモリマッピング（`memmap2`）製の高速クロスプラットフォームデスクトップテキストエディタです。

---

## Features / 主な機能

- **Blazing-Fast Huge File Loading** / **超高速のファイルロード**:

  - Leverages memory mapping (`memmap2`) and asynchronous line indexing to open GB-class files almost instantly.
  - メモリマッピング（`memmap2`）と非同期の行インデックス生成により、巨大ファイルも瞬時にロード。
- **Encoding Support** / **多言語エンコーディング対応**:
  - Easily switch and view files in UTF-8, Shift_JIS, UTF-16LE, and UTF-16BE.
  - UTF-8、Shift_JIS、UTF-16LE、UTF-16BEの文字コードを相互切り替え可能。
- **Regex Search & Highlight** / **正規表現検索・ハイライト**:
  - Instantly find matching occurrences with real-time highlighting in the viewport.
  - リアルタイムで正規表現に一致する箇所をハイライト表示。
- **Dynamic Filter & Extract** / **動的フィルター（行抽出）**:
  - Extract and display matching lines dynamically in a dedicated side-panel without blocking the UI.
  - 検索パターンに一致する行をサイドパネルに非同期で抽出し、クリックで該当行へ高速ジャンプ。
- **Line-by-Line Editing** / **行単位のエディタ**:
  - Modify or delete specific lines individually using a side-panel editor interface.
  - 選択した行をサイドペインで安全に編集・削除できます。
- **Batch Find & Replace** / **バッチ一括置換**:
  - Execute massive regex find & replace operations directly on huge files and stream the output to a new file.
  - 巨大なファイル全体に対して正規表現置換をバッチ適用し、別ファイルへ高速書き出し。
- **Safe Save & Backup** / **安全な保存とバックアップ**:
  - Prevents data loss with optional automatic backup generation (`.bak`).
  - 保存時に自動でバックアップファイル（`.bak`）を作成する安全オプションを搭載。
  
---

## Installation & Run / 導入と実行

### Prerequisites / 必須要件

Make sure you have the Rust toolchain (cargo) installed.
Rustのツールチェーン（cargo）がインストールされていることを確認してください。

For **Linux (Ubuntu)**, install the required graphics and GTK3 system development dependencies:
**Linux (Ubuntu)** の場合は、以下のシステム依存関係（グラフィックスおよびGTK3デベロップメントヘッダ）をあらかじめインストールしてください：

```bash
sudo apt-get update
sudo apt-get install -y libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libclang-dev libgtk-3-dev
```

### Running Locally / ローカルでの起動

To run the application:
以下のコマンドを実行してアプリケーションを起動します：

```bash
cargo run --release
```

---

## GitHub Actions CI/CD / 自動化ワークフロー

This repository includes pre-configured GitHub Actions under `.github/workflows/`:
本リポジトリには、`.github/workflows/` 以下に以下のGitHub Actions自動化が設定されています：

1. **CI (`ci.yml`)**:

   - Runs formatting, clippy lint checks, cargo checks, and tests across macOS, Windows, and Linux on every push or pull request.
   - すべてのプッシュおよびプルリクエスト時に、macOS、Windows、Linux上でフォーマットチェック、clippyによるコード解析、テストを自動実行します。
2. **Release (`release.yml`)**:
   - Automatically cross-compiles release binaries for macOS (Intel & Apple Silicon), Windows, and Linux, and uploads them to GitHub Releases upon pushing a version tag (e.g., `v1.0.0`).
   - `v*` 形式のタグがプッシュされた際に、macOS（Intel / Apple Silicon）、Windows、Linux向けにプロダクションビルド（`cargo build --release`）を行い、GitHub Releaseの成果物（Assets）として自動アップロードします。
  
---

## Git Hooks / コミット前自動チェック

This repository includes a pre-commit hook that automatically formats, checks, lints, and tests your Rust code before each commit.
本リポジトリには、コミット時に自動でコードのフォーマット整形、ビルド確認、Clippy警告の検証、およびテストを実行するコミット前フックが用意されています。

To enable the pre-commit hook, run the following command in your terminal:
このコミット前フックを有効化するには、以下のコマンドを実行してください：

```bash
chmod +x scripts/pre-commit
ln -sf ../../scripts/pre-commit .git/hooks/pre-commit
```

---

## License / ライセンス

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
本プロジェクトは **MITライセンス** のもとで公開されています。詳細は [LICENSE](LICENSE) ファイルをご覧ください。
