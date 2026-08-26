# R1 background workload baseline

R1でPhase 0の太字専用実験 `parse_bold` / `present_bold` を削除し、背景負荷を現在の製品経路を
代表する `parse_document` + `present_markdown` に置き換えた。このシナリオは処理内容が変わるため、
R0以前のbackground presentation値とは直接比較しない。

## 条件

- Date: 2026-08-26
- Hardware: Apple M3 Pro、36 GB RAM
- Power: AC接続、Low Power Mode off
- Profile: release
- Rust: 1.93.1
- GPUI: 0.2.2
- Source snapshot: 819,200 bytes、Markdown強調と日本語・emojiを含む
- Work per generation: `parse_document`を1回、`present_markdown`を1回
- Input: `com.apple.keylayout.ABC`、30 samples
- Raw results: `target/refactor-r1/2026-08-26-ac/`

## 新しい比較原本

| Metric | Samples | Median | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| keystroke-to-model | 30 | 0.006 ms | 0.011 ms | 0.013 ms | 0.013 ms |
| keystroke-to-frame | 30 | 5.442 ms | 5.720 ms | 5.753 ms | 5.753 ms |
| visible layout | 33 | 0.134 ms | 0.153 ms | 0.153 ms | 0.153 ms |

入力p95は通常文書の8 ms gate内であり、正式parser負荷はbackground executor上で動作して入力経路を
停止させていない。以降はこの値をbackground workloadの相対比較原本とする。

## 全体回帰確認

- 100 MB input p95 / p99: 6.955 / 8.133 ms（gate 16 / 33 ms以内）
- 100 MB scroll frame interval p95 / p99: 9.117 / 9.349 ms（60 fps相当以上）
- 100k paragraphs input p95: start 5.735 ms、middle 6.106 ms、end 5.680 ms
- 10 MB / 100 MB idle RSS: 103,579,648 / 263,028,736 bytes（gate内）
- warm startup median: 168.950 ms（150 ms gate超過）

startupは同一製品コードのR0 AC測定でも165.934 msだったため、R1固有の回帰とは判定しない。
startupに関係する変更では静穏状態で独立再測定する。
