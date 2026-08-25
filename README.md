# Hane Phase 1

Rust + GPUIによる巨大Markdown文書向けの高速plain text editorです。Phase 1ではUTF-8 byte offsetのRope、cursor / selection、日本語IME、Undo/Redo、clipboard、可視行cache、仮想scroll、性能計測を実装しています。Markdown記号はsourceのまま表示します。

## 実行

```sh
cargo run -p hane
cargo run -p hane -- target/fixtures/markdown_100mb.md
```

macOS で Metal Toolchain を別途導入しなくても開発ビルドできるよう、GPUI の `runtime_shaders` feature を使っています。

## Fixture と benchmark

```sh
cargo run -p hane-benchmark --bin hane-bench -- fixtures
cargo run --release -p hane-benchmark --bin hane-bench -- buffer
```

fixture は `target/fixtures/` に生成され、Git には含まれません。

## 検証

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

主なkey bindingは矢印、Shift+矢印、Home/End、Command+A/C/X/V/Z、Command+Shift+Zです。

## Phase 1測定

```sh
scripts/measure_phase1.sh
scripts/measure_phase1_memory.sh
```

## UI の画面キャプチャ

行境界にカーソルを置いた再現画面は、次のコマンドで取得できます。

```sh
scripts/capture_cursor_boundary.sh
```

既定の出力先は `target/captures/cursor-line-boundary.png` です。引数で別の出力先も指定できます。初回実行時にmacOSから画面収録の許可を求められた場合は、使用中のターミナルまたはCodexに許可してください。

末尾改行後の空行など、別のbyte offsetを確認する場合は `HANE_CAPTURE_CURSOR_OFFSET` を指定できます。

```sh
HANE_CAPTURE_CURSOR_OFFSET=23 scripts/capture_cursor_boundary.sh
```

40行の文書で↓キー相当のカーソル移動を32回実行し、追従スクロールを確認する場合は
次のコマンドを使います。

```sh
scripts/capture_cursor_scroll.sh
```

既定の出力先は `target/captures/cursor-scroll.png` です。移動回数は
`HANE_CAPTURE_CURSOR_DOWN` で変更できます。

Phase 0の技術検証結果は[Phase 0 report](docs/phase0/report.md)、現在の実装・測定結果とPhase 2判断は[Phase 1 report](docs/phase1/report.md)を参照してください。
