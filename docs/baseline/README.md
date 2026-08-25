# Refactor baseline

R0で固定したリファクタリング前の回帰基準。製品コードの基準commitは `a811425`、
計画書を含む採取時のHEADは `7646ad0` である。両commit間の製品コード、Cargo設定、
計測スクリプトに差分はない。

## 検証結果

採取日: 2026-08-26

- `cargo test --workspace`: pass（61 tests）
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo fmt --all -- --check`: pass
- 公開API: [`public-api.md`](public-api.md)

## 環境

- Hardware: MacBook Pro（Mac15,7）、Apple M3 Pro、12 CPU cores、36 GB RAM
- Display: 内蔵Liquid Retina XDR、3456 × 2234、可変refresh rate
- OS: macOS 26.5.1
- Rust: 1.93.1（Homebrew）
- GPUI: 0.2.2
- Build profile: `release`（thin LTO、codegen-units = 1）
- 非GUIベンチ採取時: battery 70%、AC未接続、Low Power Mode off
- UI測定時の電源状態: 既存raw dataに記録がないため不明

## 非GUIベンチ

実行コマンド:

```sh
cargo run --release -p hane-benchmark --bin hane-bench -- fixtures
cargo run --release -p hane-benchmark --bin hane-bench -- buffer
```

| Scenario | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---|---:|---:|---:|---:|---:|
| 1 MB buffer edit | 90 | 0.000 | 0.001 | 0.011 | 0.011 |
| 10 MB buffer edit | 90 | 0.000 | 0.001 | 0.003 | 0.003 |
| 100 MB buffer edit | 90 | 0.000 | 0.001 | 0.003 | 0.003 |
| 1 MB file open | 30 | 1.073 | 1.165 | 1.416 | 1.416 |
| 10 MB file open | 30 | 11.165 | 11.376 | 12.483 | 12.483 |
| 100 MB file open | 30 | 64.684 | 67.705 | 72.331 | 72.331 |
| Markdown presentation update | 1,000 | 0.001 | 0.001 | 0.001 | 0.121 |
| visible layout index | 1,000 | 0.000 | 0.000 | 0.000 | 0.001 |

RSSの `285,097,984` bytesは同一process内で100 MBシナリオまで順番に実行した後の参考値であり、
単独のmemory gateには使わない。

## UIベンチ

現行製品コードと同一内容に対して採取済みの
`target/phase4/2026-08-26-final-optimized/`を基準にする。raw CSVはローカルの同directory、
集計結果は `results.md` にある。主要値は次のとおり。

| Metric | Baseline |
|---|---:|
| warm startup median | 136.004 ms |
| cold startup median（OS cache未purge） | 130.749 ms |
| normal input keystroke-to-frame p95 / p99 | 4.284 / 5.021 ms |
| 100 MB input keystroke-to-frame p95 / p99 | 3.982 / 4.655 ms |
| 100 MB input layout p95 / p99 | 1.602 / 1.625 ms |
| 100 MB scroll frame interval p95 / p99 | 8.632 / 9.256 ms |
| input while scrolling p95 / p99 | 2.621 / 3.449 ms |
| background presentation input p95 / p99 | 4.289 / 4.379 ms |
| Japanese IME all-event p95 / p99 | 3.412 / 4.940 ms |
| Japanese IME commit p95 / p99 | 4.212 / 5.150 ms |
| empty editor ready median / max RSS | 64,503,808 / 64,733,184 bytes |
| 10 MB idle RSS | 103,759,872 bytes |
| 100 MB idle RSS | 262,864,896 bytes |

100,000段落fixtureは先頭・中央・末尾入力、scroll、scroll中入力のUIシナリオへ接続済み。
ただし、R0ではデスクトップ入力を奪う自動操作と条件の悪いbattery駆動での採取を避けたため、
この5シナリオの初回数値は未採取である。

## 回帰判定

比較測定は同じhardware、release profile、Rust/GPUI version、入力source、refresh-rate設定で行う。
新規の正式UI基準線はAC接続、Low Power Mode off、30 samples以上で採り、電源状態を結果に残す。

- test、clippy、source↔visual契約: 1件でも失敗したら回帰。
- latency p95 / p99: 基準値から15%超の悪化を暫定failとし、同条件で再測定する。
- startup medianとfile-open p95: 基準値から10%超の悪化を暫定failとする。
- RSS: 基準値から10%超の増加、または既存の絶対gate超過を暫定failとする。
- 1回だけ閾値を超えた場合は同条件で再測定し、2回連続で超えたときに回帰と確定する。
- 改善値への基準更新は、同条件の独立した2回の測定で再現した場合だけ行う。

## R0時点の計測ギャップ

現在の製品経路は `file_open`、presentation/parse、visible layout、memoryを記録するが、
`local_parse_time`、`full_parse_time`、cache hit/miss、block-index update timeを独立した指標としては
まだ出力しない。BlockIndex未導入の指標を先取りせず、該当構造を導入するフェーズで追加する。

