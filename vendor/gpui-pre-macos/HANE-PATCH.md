# Hane の GPUI 0.3.6 macOS パッチ

crates.io の `gpui-pre-macos 0.3.6`（Zed `bcf6582ce3500df93a8a39366640173e6786cea6`）を同梱する。
配布 crate の SHA-256 は `951d17e41a72067ad2d35c60e08a62e8ad7fa9400b41c802e26cfb459f6c05da`。
Apache-2.0 ライセンスは `LICENSE-APACHE` に保持する。Hane 固有の製品コード変更は
`src/text_system.rs`、`src/platform.rs`、`src/pasteboard.rs`、および公開 re-export の `src/gpui_macos.rs` に限る。GPUI の他プラットフォーム実装は変更しない。

## クリップボードの本文と画像を同一世代で読む（Issue #591）

`Pasteboard::read` は、ファイルパスがない場合に本文と画像表現（PNG 優先、SVG は最後）を
一つの `ClipboardItem` にまとめる。従来は本文があると画像を返さなかったため、
本文と画像が混在するクリップボードで画像を優先できなかった。読み取りの前後で
`changeCount` を比較し、世代が変わった場合は最大 3 回までやり直す。それでも変化し続ける
場合は `None` を返す（GPUI の `Option` では「空」と区別できないため、Hane の本文 Paste は
下記の公開 API を使う）。ファイルパス・本文のみ・画像のみの既存の戻り値は変えない。

### 狭い公開 API `read_rich_clipboard`（Issue #591 / #596）

crate root から `read_rich_clipboard() -> Result<Option<RichClipboardSnapshot>, RichClipboardError>`、
`RichClipboardSnapshot { item, html, images }`、`RichClipboardError`（`Busy` / `ChangedDuringRead` /
`InvalidFormat` / `TooLarge`）を公開する。Windows crate と同じ形で、`Platform` trait や
`ClipboardItem` は変えない。`Pasteboard::read_rich` は `changeCount` が前後で同じ世代だけを返し、
3 回変化し続けたら `ChangedDuringRead`（空として扱わない）。`Ok(None)` は本当に空のときだけ。

- `item` は本文とファイルパス、`images` は `pasteboardItems` の項目ごとに優先形式（PNG → TIFF → … →
  SVG）を 1 つ選んだ結果。同じ項目の別形式は 1 枚にまとめ、独立した項目は別の要素にする。
  ファイルパス・本文・画像が併存しても画像を返す。項目が報告されない場合は pasteboard 自体を 1 項目として扱う。
- 画像データが空・nil は `InvalidFormat`、64 MiB 超は `TooLarge`（コピー前に判定）。
- `html` は #596 用の予約欄で、今回は読まず `None`。HTML 変換や書き込み API は追加していない。
- GPUI の `read` / `write`、find pasteboard、ファイルパスのコピーは従来のまま。

```sh
cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml \
  --features runtime_shaders,font-kit,test-support --lib hane_clipboard
```

`ChangedDuringRead` は決定的に再現できず、自動テストしていない。

```sh
cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml \
  --features runtime_shaders,font-kit,test-support --lib pasteboard
```

実際のスクリーンショットや画像アプリからの貼り付けは、Hane GUI で macOS 実機確認が必要。

## 日本語の合成斜体

システム UI フォントで日本語が斜体を持たない Hiragino へフォールバックする場合、
日本語の glyph にだけ 12 度の合成斜体を適用する。ネイティブ斜体と絵文字は二重加工しない。
通常字形とは FontId を分離し、字送り・glyph 位置と空白のゼロ描画範囲を保つ。
描画と raster bounds に同じせん断変換を適用する。

macOS の実フォントを使う回帰テスト:

```sh
cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml \
  --features runtime_shaders,font-kit,test-support --lib hane_oblique
```

通常・太字の日本語字形、倍率とサブピクセル位置、描画範囲、通常字形とのキャッシュ分離、
ネイティブ斜体・絵文字・空白を検証する。

## 入力ソース切替後の IME 再同期

アプリ起動後に入力ソースが変わっても AppKit は現在の first responder の
`NSTextInputContext` を自動的に再同期しないため、実際に source ID または ASCII 可否が
変わった通知でだけ `deactivate` → `activate` する。key window、first responder、
context が無い場合は何もしない。再入を抑止し、キーボードマッパー更新は維持する。

macOS の Objective-C mock による回帰テスト:

```sh
cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml \
  --features runtime_shaders,font-kit,test-support --lib hane_input_source
```

これは context の再入防止と no-op 条件の検証であり、実際の日本語 IME の変換結果を
保証するものではない。採用前に Hane GUI で、起動後に日本語入力へ切り替え、
通常段落とリスト項目で変換前テキストが誤確定せず、一度だけ正しい位置へ確定することを確認する。

上流で同等の修正が入ったと判断してパッチを外す場合も、実字形テストと GUI の
日本語 IME 検証を通してから削除する。移植前の経緯は `vendor/gpui/HANE-PATCH.md` を参照。

## リッチテキスト貼り付け用のクリップボード読取り（Issue #596）

`src/pasteboard.rs` に `read_rich_clipboard()` / `RichClipboardSnapshot` /
`RichClipboardError` を追加し、`src/gpui_macos.rs` から公開する。`Platform` trait と
`ClipboardItem` の形は変えず、既存の `Pasteboard::read` と plain text・find pasteboard・
ファイルパスの経路も変えない。

- 同じ pasteboard 世代から plain text、`public.html`、既存の画像、ファイルパスを読む。
  `changeCount` を読取り前後で比較し、異なれば `ChangedDuringRead` を返して異世代の
  plain と HTML を結合しない。HTML だけの pasteboard も `item` が空のスナップショットとして返す。
- HTML は UTF-8 のみ。Rust へ複製する前に native の長さを 8 MiB 上限と比較する。UTF-8 として
  不正な場合は `InvalidFormat`、上限超過は `TooLarge` を `html_problem` に入れ、`html` は
  `None` にして同じ世代の plain 等は残す。
- HTML の解析や Markdown 判定は行わない。読取り専用で、書式付き書き込み（export）は含まない。

```sh
cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml \
  --features runtime_shaders,font-kit,test-support --lib hane_clipboard
```

上流が同等の公開 API を提供した場合はそちらへ置き換えてこの節を外す。
