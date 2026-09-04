# Hane

![Hane logo](./assets/phase4-feather.svg)

Hane は、巨大な Markdown 文書も軽快に編集できるデスクトップ向け Markdown エディタです。Rust と GPUI で作られており、Typora のように Markdown 記号を普段は隠して見やすく表示し、カーソルや選択範囲が構文に入ったときだけ記号を表示します。編集中のデータは常に Markdown のまま保持されるので、いつでもそのままのテキストとして扱えます。

## 主な機能

- **軽快な編集** — 100MB 級の Markdown でもスムーズにスクロール・編集できます。
- **すっきりした表示** — 見出しや強調などの Markdown 記号を普段は隠し、編集する箇所だけ記号を表示します。
- **画像・表の表示** — 画像やパイプテーブルをインラインで表示します。
- **安全な保存** — atomic save により保存中の破損を防ぎます。自動保存にも対応しています。
- **Recent Files** — 最近開いたファイルにすぐアクセスできます。
- **設定の保存** — テーマや自動保存などの設定は次回起動時も引き継がれます。
- **テーマ** — system / light / dark のテーマを切り替えられます。
- **Work folder mode** — フォルダを指定すると、配下の Markdown 一覧をサイドバーから選んですぐ編集できます。`+` を押すだけでファイル名を聞かれずに新しいメモを書き始められ、最初の H1 がファイル名になります。

## 動作環境

- macOS Apple Silicon — [最新の GitHub Release](https://github.com/hide212131/hane/releases/latest) から `hane-macos-arm64.zip` をダウンロードし、展開して `hane` を実行します。[ZIP を直接ダウンロード](https://github.com/hide212131/hane/releases/latest/download/hane-macos-arm64.zip)することもできます。
- macOS Intel — [最新の GitHub Release](https://github.com/hide212131/hane/releases/latest) から `hane-macos-x64.zip` をダウンロードし、展開して `hane` を実行します。[ZIP を直接ダウンロード](https://github.com/hide212131/hane/releases/latest/download/hane-macos-x64.zip)することもできます。
- Windows 11 x64 — [最新の GitHub Release](https://github.com/hide212131/hane/releases/latest) から `hane-windows-x64.zip` をダウンロードし、展開して `hane.exe` を実行するポータブル運用のみに対応しています（Rust のインストールは不要）。[ZIP を直接ダウンロード](https://github.com/hide212131/hane/releases/latest/download/hane-windows-x64.zip)することもできます。個人利用の動作確認段階（dogfooding）であり、正式な Windows サポートは今後の課題です。

macOS 版は現在 Developer ID による署名と Apple の notarization には未対応です。そのため、ダウンロード後の初回起動時に macOS のセキュリティ警告が表示される場合があります。

Windows版は GitHub Actions の `hane-windows-x64` artifact から ZIP を取得し、ユーザーが書き込めるフォルダへ展開して `hane.exe` を実行します。設定と Recent Files は `%LOCALAPPDATA%\Hane` に保存されます（`HANE_STATE_DIR` で変更可能）。CLI の使い方とエクスプローラーへの統合については [Windows CLI / Explorer 統合](#windows-cli--explorer-統合) を参照してください。

> macOS で Metal Toolchain を別途導入しなくても動かせるよう、GPUI の `runtime_shaders` を使っています。

### Windows CLI / Explorer 統合

```text
hane.exe
hane.exe path\to\document.md
hane.exe path\to\folder
hane.exe --register-context-menu
hane.exe --unregister-context-menu
```

- `hane.exe`（引数なし）— 保存済みの既定フォルダを開きます。既定フォルダが未設定の場合（初回起動時など）は、起動後にフォルダ選択ダイアログが表示され、選んだフォルダが以後の既定フォルダとして保存されます。
- `hane.exe path\to\document.md` — 指定した Markdown ファイルを開きます。
- `hane.exe path\to\folder` — 指定したフォルダを、その起動時に限り Work folder mode で開きます。既定フォルダは読み書きされず、変更もされません。
- `hane.exe --register-context-menu` — Windows エクスプローラーのフォルダ右クリックメニューに「Haneで開く」を追加します。現在のユーザーのみに登録され、管理者権限は不要です。追加後は、フォルダを右クリックして「Haneで開く」を選ぶと `hane.exe path\to\folder` と同様にそのフォルダを開けます。
- `hane.exe --unregister-context-menu` — 追加した「Haneで開く」をエクスプローラーのメニューから削除します。

現在のエクスプローラー統合はフォルダのみが対象です。ファイルの右クリックメニューへの統合は本リリースの対象外です。

## インストールと起動

ソースからビルドして起動します。

```sh
# そのまま起動
cargo run -p hane

# ファイルを指定して起動
cargo run -p hane -- path/to/document.md

# フォルダを指定して work folder mode で起動
cargo run -p hane -- path/to/work-folder
```

快適に使うにはリリースビルドをおすすめします。

```sh
cargo run --release -p hane
```

## 使い方

起動したら Markdown を入力・編集するだけです。主なキーボードショートカット（macOS / Windows・Linux）:

| 操作 | macOS | Windows / Linux |
| --- | --- | --- |
| カーソル移動 | 矢印キー / Home / End | 矢印キー / Home / End |
| 選択 | Shift + 矢印キー | Shift + 矢印キー |
| 文頭・文末へ移動 | Command + ↑ / ↓ | Ctrl + Home / End（Ctrl + ↑ / ↓ も可） |
| 全選択 | Command + A | Ctrl + A |
| コピー / 切り取り / 貼り付け | Command + C / X / V | Ctrl + C / X / V |
| 元に戻す / やり直す | Command + Z / Command + Shift + Z | Ctrl + Z / Ctrl + Y（Ctrl + Shift + Z も可） |
| 開く | Command + O | Ctrl + O |
| フォルダを開く（work folder mode） | Command + Shift + O | Ctrl + Shift + O |
| 保存 / 名前を付けて保存 | Command + S / Command + Shift + S | Ctrl + S / Ctrl + Shift + S |
| 自動保存の切り替え | Command + Option + A | Ctrl + Alt + A |

テーマや自動保存は、ヘッダーの設定からも変更できます。

### Work folder mode

起動時にディレクトリを引数として渡すか、起動後に Command + Shift + O（Windows / Linux は Ctrl + Shift + O）でフォルダを選ぶと、その配下の Markdown 一覧が左サイドバーに表示されます。一覧からメモを選ぶと保存ダイアログを挟まずすぐ編集でき、`+` を押すとファイル名を聞かれない空のメモがその場で開きます。

新規メモは最初の H1（`# ...`）がファイル名になり、自動命名対象のメモは以後 H1 を変更するとファイル名も追従します。ファイル名と H1 が最初から異なる既存の Markdown は、自動命名対象と判断されないため勝手にリネームされません。

H1 をまだ付けていない新規メモは `.hane/drafts` にバックグラウンドで保存されるため、アプリを終了・クラッシュさせても内容を失いません。

## ライセンス

Apache-2.0

## 開発者向け

現在の crate 構造とデータフローは [architecture](docs/architecture.md) を参照してください。

標準の検証は次のとおりです。

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Windows の開発環境を再現する場合は、PowerShell で次を実行します。`-Install` は
winget 経由で LLVM と Visual Studio C++ Build Tools を導入し、`-Verify` はテスト、
フォーマット、clippy、Windows バイナリのビルドまで実行します。ターゲットはホストの
x64 / ARM64 を自動判定し、必要なら `-Architecture x64` または `-Architecture arm64` で
明示できます。

```powershell
.\scripts\setup-windows-dev.ps1 -Install -Verify
```

計測は `scripts/measure.sh all`（`startup` / `input` / `memory` も指定可）、画面キャプチャは
`scripts/capture.sh editor`（`cursor-boundary` / `cursor-scroll` も指定可）で実行します。
性能基準線は [docs/baseline/](docs/baseline/)、設計判断は [ADR index](docs/adr/README.md)、
リファクタリングの進捗は [実施計画](docs/refactor-execution-plan.md) を正とします。
