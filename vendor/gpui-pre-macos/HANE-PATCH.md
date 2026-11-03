# Hane の GPUI 0.3.6 macOS パッチ

crates.io の `gpui-pre-macos 0.3.6`（Zed `bcf6582ce3500df93a8a39366640173e6786cea6`）を同梱する。
配布 crate の SHA-256 は `951d17e41a72067ad2d35c60e08a62e8ad7fa9400b41c802e26cfb459f6c05da`。
Apache-2.0 ライセンスは `LICENSE-APACHE` に保持する。Hane 固有の製品コード変更は
`src/text_system.rs`、`src/platform.rs`、`src/pasteboard.rs` に限る。GPUI の他プラットフォーム実装は変更しない。

## クリップボードの本文と画像を同一世代で読む（Issue #591）

`Pasteboard::read` は、ファイルパスがない場合に本文と画像表現（PNG 優先、SVG は最後）を
一つの `ClipboardItem` にまとめる。従来は本文があると画像を返さなかったため、
本文と画像が混在するクリップボードで画像を優先できなかった。読み取りの前後で
`changeCount` を比較し、世代が変わった場合は最大 3 回までやり直す。それでも変化し続ける
場合は `None` を返す（GPUI の `Option` では「空」と区別できない。区別は #596 の rich snapshot
公開 API で扱う）。ファイルパス・本文のみ・画像のみの既存の戻り値は変えない。

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
