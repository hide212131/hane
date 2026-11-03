# Issue #591 画像貼り付けの実装記録

- Issue: https://github.com/hide212131/hane/issues/591
- 基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`
- ブランチ: `codex/issue-591-image-paste`
- 詳細設計: [画像貼り付け](../design/issue-591-image-paste.md)
- 関連採用済み設計: [#596 clipboard](../design/issue-596-rich-clipboard.md)

macOS / Windows のクリップボード実画像1枚を、文書隣接の自動作成 `images/` へ透過保持PNGとして排他保存し、通常Markdownの単独行画像参照で表示する先行フェーズの実装記録。PNG/JPEGファイルのドロップは #591 全体に残る後続範囲で、設定・不要画像削除・加工・遠隔取得・#596 のHTML変換は含めない。`Refs #591` で追跡し、このPRだけでIssueをcloseしない。

## 実装の構成

| 責務 | 場所 |
| --- | --- |
| decode→PNG、上限、排他保存、挿入形状、生成参照の識別、Save As用コピー（GPUI非依存） | `crates/ui/src/clipboard_image.rs` |
| 単一snapshotとdispatcher、非同期job、鮮度照合、記録付き置換、Save As連携 | `crates/ui/src/view/image_paste.rs` |
| 本文Pasteの入口（snapshotを一度だけ読む） | `crates/ui/src/actions.rs` の `Paste` |
| 無名draftの有効base（`target_directory`）を一箇所で接続 | `EditorView::document_base_directory` / `document_resource_resolver`（描画も同じ resolver） |
| platformのクリップボード読取 | `vendor/gpui-pre-macos/src/pasteboard.rs`、`vendor/gpui-pre-windows/src/clipboard.rs`（各 `HANE-PATCH.md`） |

## 有限の上限（`clipboard_image.rs`、境界テストあり）

| 定数 | 値 | 根拠 |
| --- | --- | --- |
| `MAX_INPUT_BYTES` | 64 MiB | 4K スクリーンショットのPNGは10 MiB未満。非圧縮8K bitmap（128 MiB）は拒否する。 |
| `MAX_IMAGE_SIDE` | 16,384 px | `image::Limits` の幅・高さ。ヘッダ段階でallocation前に拒否される。 |
| `MAX_IMAGE_PIXELS` | 40,000,000 | 8K×5K。16bit RGBAでも320 MiB。デコード前に `dimensions()` で確認する。 |
| `MAX_DECODE_ALLOC_BYTES` | 512 MiB | `image::Limits::max_alloc`。 |
| `MAX_OUTPUT_BYTES` | 64 MiB | 出力側をカウントする `Write` で打ち切る。 |

decode・PNG変換・ファイルI/Oはすべて background executor で行い、UIスレッドで実行しない。`image 0.25.10`（Cargo.lock に既存。`png`/`jpeg`/`tiff`/`bmp`/`gif`/`webp`）を hane-ui の直接依存にした。EXIF向きは画素へ反映し、PNGにはorientationを持たせない。SVG・ICO・PNMは実ラスタ画像として扱わず、本文があれば本文を貼る。

## 保存先・名前・命名規則

- 保存済み文書は `<文書のディレクトリ>/images/`、workspace無名draftは既存の `target_directory`。cwdや作業フォルダrootは使わない。`images` が無ければ `create_dir` で作成する（文書ディレクトリ自体は作らない）。`images` が通常ファイルなら保存開始前に拒否する。
- 生成名は `hane-paste-<unix millis>-<8桁hex>.png`（ASCII）。一意性は名前ではなく `create_new` の排他作成に依存し、衝突時は別名を最大32回試す。書き込みが途中で失敗したときは当該jobが作ったファイルだけを削除する。
- 再読込後も生成画像と分かる狭い規則: 本文中の `](images/hane-paste-<数字>-<8桁小文字hex>.png)` だけ。manifestやMarkdown独自属性は持たない。手書きの `images/photo.png` などは管理しない。

## 貼り付けと履歴

- 本文Pasteは同一snapshotの実画像を優先し、画像なしならtext経路。検索・設定・renameなど本文外の入力は従来どおりtext。画像のdecode/保存に失敗してもtextへ黙ってfallbackしない。
- 通常段落または空文書だけを許可する（現在のblock indexで選択範囲が全て `Paragraph`）。リスト・引用・表・コード・見出し等は保存開始前に拒否して案内する。indexが現revisionに追いついていないときは再貼り付けを案内する。
- 単独行にするための前後改行だけを足し、1回の `Editor::replace_range_recorded` で挿入する。1 Undoで選択元本文へ、Redoで同じ画像参照へ戻り、画像ファイルは削除・再生成しない。
- 要求時に SessionId・DocumentInstance（workspace世代込み）・revision・selection・base・draft id・job番号を捕捉し、完了時に全て照合する。不一致なら参照を挿入せず、当該jobが作った未公開ファイルだけを削除する。連続Pasteは先に完了したjobが適用され、後のjobはrevision不一致で安全に中止される。状態表示は最新のjob番号のものだけが更新する。

## 無名文書・Save As

- workspaceの無名draftは先行保存や命名を要求しない。同directoryのH1命名・renameでも `images/` 参照は同じ相対位置のまま有効。
- 保存先のない単独無題だけ既存の初回保存ダイアログを開く。キャンセル・失敗では画像を作らず本文・選択を保つ。保存が完了し、本文・選択が変わっていなければ貼り付けを再開する。
- 別directoryへのSave Asは、本文が参照する生成画像を新しい `images/` へ先にコピーする。同名で内容が同一なら再利用、内容が違えば別名にして参照を更新する。コピーに失敗したときは保存せず、作成済みのコピーを削除し、元の本文・画像・未保存状態を保つ。コピー中に本文が変わった場合も保存を中止する。元画像は削除しない。

## 既知の制約（要確認）

- GPUIの `read_from_clipboard` は `Option` を返すため、Busy / ChangedDuringRead をUI側で「空」と区別できない。macOSは `changeCount` の前後比較で最大3回再読込し、Windowsは `OpenClipboard` を最大5回再試行するが、最終的に失敗した場合は空として扱われる。区別は #596 の `read_rich_clipboard` / `RichClipboardError` 公開APIの導入で解消する想定で、本PRでは導入していない。
- macOSで複数のpasteboard itemを区別する公開APIは使っていない。同一世代の本文と画像表現を1つのitemにまとめる。
- Save As で参照を更新する必要があるのは、保存対象がアクティブなタブのときだけ。非アクティブなタブの名前付き保存で参照の改名が必要な場合は保存せずに案内する。

## 未実施の検証（passとして扱わない）

この作業環境ではビルド・テスト・clippyを実行していない。Commanderが別途、current headで確認する。

- `cargo test --workspace --all-features --locked` と `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、通常feature/Releaseのbuild。
- vendor変更のtest: `cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml --features runtime_shaders,font-kit,test-support --lib pasteboard`、`cargo test --manifest-path vendor/gpui-pre-windows/Cargo.toml --features test-support --lib`。
- macOS / Windows実機のスクリーンショット・画像アプリからの貼り付け、日本語IME、検索/設定/renameの通常paste、連続貼付、閉じる/切替、書込不可先、文書+imagesを一緒に移動して開く。
- 変更前後の大文書応答性（同じRelease/fixtureの入力・scroll・RSS/処理時間）。
- current-head CI と CodeRabbit の manual full review。

通常の意味判断はJev、製品変更はClaude、Commanderは客観guardと一actionの実行を担当する。開始時・push直前のexact-head guardを維持する。
