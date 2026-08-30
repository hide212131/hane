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

- macOS
- [Rust](https://www.rust-lang.org/tools/install)（ビルドに必要）

> macOS で Metal Toolchain を別途導入しなくても動かせるよう、GPUI の `runtime_shaders` を使っています。

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

起動したら Markdown を入力・編集するだけです。主なキーボードショートカット:

| 操作 | ショートカット |
| --- | --- |
| カーソル移動 | 矢印キー / Home / End |
| 選択 | Shift + 矢印キー |
| 全選択 | Command + A |
| コピー / 切り取り / 貼り付け | Command + C / X / V |
| 元に戻す / やり直す | Command + Z / Command + Shift + Z |
| 開く | Command + O |
| フォルダを開く（work folder mode） | Command + Shift + O |
| 保存 / 名前を付けて保存 | Command + S / Command + Shift + S |
| 自動保存の切り替え | Command + Option + A |

テーマや自動保存は、ヘッダーの設定からも変更できます。

### Work folder mode

起動時にディレクトリを引数として渡すか、起動後に Command + Shift + O でフォルダを選ぶと、その配下の Markdown 一覧が左サイドバーに表示されます。一覧からメモを選ぶと保存ダイアログを挟まずすぐ編集でき、`+` を押すとファイル名を聞かれない空のメモがその場で開きます。

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

計測は `scripts/measure.sh all`（`startup` / `input` / `memory` も指定可）、画面キャプチャは
`scripts/capture.sh editor`（`cursor-boundary` / `cursor-scroll` も指定可）で実行します。
性能基準線は [docs/baseline/](docs/baseline/)、設計判断は [ADR index](docs/adr/README.md)、
リファクタリングの進捗は [実施計画](docs/refactor-execution-plan.md) を正とします。
