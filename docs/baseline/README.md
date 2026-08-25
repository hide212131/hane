# Refactor baseline

R0で固定したリファクタリング前の回帰基準。製品コードの基準commitは `a811425`、
UI採取時のHEADは `3efbaac` である。両commit間で製品コードに差分はなく、後者には
計画書とR0計測シナリオだけが追加されている。

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
- UIベンチ採取時: battery 80%、AC接続、Low Power Mode off

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

現行製品コードに対してAC接続状態で採取した
`target/refactor-baseline/r0-2026-08-26-ac-v3/`を基準にする。raw CSVはローカルの同directory、
追跡対象の集計結果は [`ui-results.md`](ui-results.md) にある。主要値は次のとおり。

| Metric | Baseline |
|---|---:|
| warm startup median | 165.934 ms |
| cold startup median（OS cache未purge） | 174.552 ms |
| normal input keystroke-to-frame p95 / p99 | 4.104 / 4.149 ms |
| 100 MB input keystroke-to-frame p95 / p99 | 4.975 / 7.648 ms |
| 100 MB input layout p95 / p99 | 1.667 / 1.709 ms |
| 100 MB scroll frame interval p95 / p99 | 9.038 / 9.273 ms |
| 100 MB input while scrolling p95 / p99 | 4.676 / 4.679 ms |
| 100k paragraphs input at start p95 / p99 | 5.302 / 5.671 ms |
| 100k paragraphs input at middle p95 / p99 | 7.219 / 9.622 ms |
| 100k paragraphs input at end p95 / p99 | 5.362 / 6.093 ms |
| 100k paragraphs scroll frame interval p95 / p99 | 9.191 / 9.551 ms |
| 100k paragraphs input while scrolling p95 / p99 | 3.846 / 4.686 ms |
| background presentation input p95 / p99 | 2.615 / 3.468 ms |
| Japanese IME all-event p95 / p99 | 3.414 / 4.419 ms |
| Japanese IME commit p95 / p99 | 5.300 / 6.125 ms |
| empty editor ready median / max RSS | 64,323,584 / 64,520,192 bytes |
| 10 MB idle RSS | 103,579,648 bytes |
| 100 MB idle RSS | 263,061,504 bytes |

通常ASCII、大容量、100,000段落の各入力ケースは30 samples以上、日本語IMEは
210 composition eventsと30 commitsを採取した。`/usr/sbin/purge`は権限不足のため、
cold startupはOS cache未purge条件である。

このAC測定のwarm startup medianは165.934 msで、RFPの絶対gate 150 msを超えた。同一製品コードの
Phase 4確定測定は136.004 msだったため、製品回帰とは判定せず測定環境差として両方を保存する。
startupに触れる変更では、静穏状態で再測定して絶対gateと相対差の両方を確認する。

## 回帰判定

比較測定は同じhardware、release profile、Rust/GPUI version、入力source、refresh-rate設定で行う。
正式UI基準線はAC接続、Low Power Mode off、30 samples以上で採り、電源状態を結果に残す。

- test、clippy、source↔visual契約: 1件でも失敗したら回帰。
- latency p95 / p99: 基準値から15%超の悪化を暫定failとし、同条件で再測定する。
- startup medianとfile-open p95: 基準値から10%超の悪化を暫定failとする。
- RSS: 基準値から10%超の増加、または既存の絶対gate超過を暫定failとする。
- 1回だけ閾値を超えた場合は同条件で再測定し、2回連続で超えたときに回帰と確定する。
- 改善値への基準更新は、同条件の独立した2回の測定で再現した場合だけ行う。

## R0時点の計測ギャップ

現在の製品経路は `file_open`、presentation/parse、visible layout、memoryを記録するが、
`local_parse_time`、`full_parse_time`、cache hit/miss、block-index update timeを独立した指標としては
まだ出力しない。これらはR0時点では「未実装」を基準状態として記録し、BlockIndexやcacheを
導入する該当フェーズで追加する。
