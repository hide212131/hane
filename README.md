# Hane Phase 0

Rust + GPUI による巨大 Markdown 文書向け編集ループの技術検証です。Phase 0 の範囲は、UTF-8 byte offset の rope buffer、単一 cursor / selection、日本語 IME composition、太字 presentation、可視範囲描画、性能計測です。

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

測定結果と残りの手動検証は [Phase 0 report](docs/phase0/report.md) を参照してください。

