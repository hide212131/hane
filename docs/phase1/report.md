# Phase 1 実装・測定レポート

## 実装結果

ADR-0012の順序に沿って、Markdown presentationを行わないplain text editorを実装した。

- Markdown記号を含むsourceをそのまま表示する`present_plain`とidentity `SourceMap`。
- grapheme単位の左右移動、上下移動、行頭・行末、文書先頭・末尾、Shiftによる範囲拡張。
- Return / Shift+Returnによる改行。IME composition中はOS側の確定処理を優先する。
- mouse click、Shift-click、mouse dragによるsource selection。
- 選択範囲だけを分割して描画するselection backgroundと、IME marked rangeのunderline。
- GPUI clipboardを使うCopy、Cut、Paste。
- source edit、編集前後selection、編集種別を保持するUndo/Redo transaction。
- 750 ms以内の連続Insert、Backspace、Deleteのgrouping。改行とselection置換は独立transaction。
- composition updateを履歴へ入れず、IME commit / unmarkを1 transactionにする履歴統合。cancelは履歴を作らない。
- `Rope::from_reader`を使うstreaming file open。文書全体の一時`String`を除去。
- 表示周辺に上限を持つ可視行cache。revision deltaで非交差行をrebaseし、cursor・selection更新では文字列とpresentationを再利用。
- native scroll eventごとのcoalesced paintと、`Window::request_animation_frame`をrender内で使うdisplay-linked scroll測定。
- Phase 1用UI/RSS測定entrypointと、不完全な末尾CSV行を無視できる集約処理。
- 最小GPUI windowのRSSを分離測定する`gpui_baseline` example。

Phase 1の非目標であるMarkdown装飾、Markdown block parser拡張、Save系UI、Recent Files、自動保存、設定、packagingは実装していない。Phase 0の太字presentation実験は測定資産として残したが、本文描画経路から外した。

## 自動測定

測定日: 2026-08-25。Apple M3 Pro、macOS 26.5.1、release profile、Rust 1.93.1、GPUI 0.2.2。測定対象はbase commit `1196d17`に本report記載のPhase 1 working tree変更を加えた状態である。生CSVと集計表は`target/phase1/2026-08-25/`に保存した。

### 非UI benchmark

各buffer editは先頭・中央・末尾を循環し90 samples、streaming file openは30 samplesとした。

| Scenario | Samples | Median (ms) | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| 10 MB buffer edit | 90 | < 0.001 | 0.002 | 0.022 | 0.022 |
| 100 MB buffer edit | 90 | < 0.001 | 0.002 | 0.003 | 0.003 |
| 10 MB streaming file open | 30 | 11.495 | 12.935 | 15.089 | 15.089 |
| 100 MB streaming file open | 30 | 67.236 | 74.521 | 75.779 | 75.779 |
| visible layout index | 1,000 | < 0.001 | < 0.001 | < 0.001 | < 0.001 |

### 入力latency

通常入力と100 MB三位置は各30入力、100 MB combinedは90入力、実日本語IMEは30 composition→commit（240 input events、commit 30件）を測定した。

| Scenario / metric | Samples | Median (ms) | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| normal ASCII — keystroke-to-model | 30 | 0.006 | 0.009 | 0.011 | 0.011 |
| normal ASCII — keystroke-to-frame | 30 | 1.978 | 4.091 | 4.253 | 4.253 |
| real Japanese IME — all events to model | 240 | 0.008 | 0.010 | 0.012 | 0.027 |
| real Japanese IME — all events to frame | 240 | 1.568 | 3.302 | 4.683 | 7.219 |
| real Japanese IME — commit to frame | 30 | 3.106 | 3.644 | 7.219 | 7.219 |
| 100 MB combined — keystroke-to-model | 90 | 0.009 | 0.011 | 0.018 | 0.018 |
| 100 MB combined — keystroke-to-frame | 90 | 1.961 | 3.641 | 4.684 | 4.684 |
| 100 MB input while scrolling — to frame | 30 | 1.025 | 3.278 | 4.352 | 4.352 |
| input during background presentation — to frame | 30 | 0.342 | 2.034 | 2.142 | 2.142 |

### Display-linked scrollとvisible layout

Phase 0の16.667 ms固定timerを廃止し、GPUIがdisplayへ同期して要求するanimation frameごとにscrollを更新した。可変refresh displayでは約120 Hzでcallbackされた。

| Scenario / metric | Samples | Median (ms) | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| 100 MB scroll only — frame interval | 185 | 8.324 | 9.224 | 10.051 | 10.092 |
| 100 MB scroll only — layout | 185 | 0.121 | 0.267 | 0.299 | 0.312 |
| 100 MB input while scrolling — frame interval | 221 | 8.311 | 9.490 | 10.106 | 10.428 |
| 100 MB input while scrolling — layout | 221 | 0.105 | 0.266 | 0.291 | 0.301 |

scroll-only p95は約108.4 fps相当、p99でも約99.5 fps相当であり、最低60 fpsの安定目標を満たす。Phase 0のp95 25.078 msから9.224 msへ改善した。

### Startup

| Scenario | Samples | Median (ms) | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| empty warm startup | 30 | 139.600 | 178.667 | 249.194 | 249.194 |
| empty cold startup（OS cache未purge） | 30 | 136.077 | 161.652 | 167.114 | 167.114 |

`/usr/sbin/purge`は権限不足のため、cold値はPhase 0と同じくcache未purge条件である。判定対象はmedianとする。

### RSS

10 MBと100 MBは別processを同時起動する組を30回繰り返し、Mach `resident_size`を採取した。

| Scenario / point | Samples | Median (bytes) | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| empty ready（warm startup process） | 30 | 64,077,824 | 64,307,200 | 64,339,968 | 64,339,968 |
| 10 MB load直後 | 30 | 75,087,872 | 75,317,248 | 75,415,552 | 75,415,552 |
| 10 MB first visible layout後 | 30 | 80,855,040 | 81,149,952 | 81,199,104 | 81,199,104 |
| 10 MB 30秒idle後 | 30 | 87,539,712 | 87,834,624 | 87,851,008 | 87,851,008 |
| 100 MB load直後 | 30 | 220,495,872 | 220,725,248 | 220,790,784 | 220,790,784 |
| 100 MB first visible layout後 | 30 | 226,263,040 | 226,492,416 | 226,557,952 | 226,557,952 |
| 100 MB 30秒idle後 | 30 | 232,767,488 | 233,160,704 | 242,221,056 | 242,221,056 |

100 MB idle maxはPhase 0の349,437,952 bytesから107,216,896 bytes減少した。350 MB上限に対するheadroomは約107.8 MBとなり、Phase 1開始時に要求した20 MB以上を満たす。

空editorは元の60 MB目標を約4.34 MB超えるためFailのままである。最小GPUI windowを30 process測定した結果はmedian 59,326,464、max 59,637,760 bytesだった。Hane固有増分がmedian約4.75 MBであることを分離できたため、ADR-0014でGPUI baseline max 60 MB、Hane空editor max 65 MB、固有増分median 6 MBを継続gateとして再承認した。

## 検証結果

- `cargo fmt --all -- --check`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo test --workspace`: 46 tests pass
- 10 MB / 100 MB fixture size: 10,485,760 / 104,857,600 bytes
- 実macOS日本語IME: 30 composition→commit sequence pass
- 実macOS Return key: 2 command eventとpaintを確認
- 100 MB start / middle / end入力: 各30 sequence pass
- display-linked scroll only / scroll中入力: pass
- 30-process RSS測定: pass
- 最小GPUI baseline 30-process測定: pass

単体テストはPhase 0対象に加え、streaming Rope構築、plain identity mapping、Markdown記号の可視性、選択とmarked rangeのspan分割、cache invalidation、連続Insert/Backspace/Delete grouping、selection置換、Undo後のredo破棄、IME transaction、行頭・行末移動を対象とする。

## ADR-0012完了条件

- [x] Markdown sourceを記号込みのplain textとして表示する。
- [x] grapheme cursor、上下、行頭・行末、文書先頭・末尾が動作する。
- [x] Shiftとmouse dragによる範囲選択を実装する。
- [x] Copy/Cut/Pasteをsource selectionへ接続する。
- [x] 日本語IME composition / commit / cancelと1 transaction Undo/Redoを実装する。
- [x] 連続入力、Backspace、Deleteを自然な単位でUndo/Redoする。
- [x] native scrollとdisplay-linked測定で可視範囲だけを描画する。
- [x] 非交差可視行をrevision deltaでrebaseしてcache再利用する。
- [x] 100 MB fileを一時的な文書全体`String`なしでRopeへ読み込む。
- [x] format、clippy、workspace testがpassする。
- [x] Phase 1測定reportを作成する。

## 目標比較

| 目標 | 結果 | 判定 |
|---|---:|---|
| warm startup median <= 150 ms | 139.600 ms | Pass |
| cold startup median <= 400 ms | 136.077 ms（cache未purge） | Pass（条件付き） |
| normal keystroke-to-frame p95 <= 8 ms | 4.091 ms | Pass |
| normal keystroke-to-frame p99 <= 16 ms | 4.253 ms | Pass |
| 100 MB keystroke-to-frame p95 <= 16 ms | 3.641 ms | Pass |
| 100 MB keystroke-to-frame p99 <= 33 ms | 4.684 ms | Pass |
| scroll中の安定frame rate >= 60 fps | p95 108.4 / p99 99.5 fps相当 | Pass |
| 空文書・起動直後RSS <= 60 MB | max 64.34 MB | **Fail、ADR-0014でbaseline分離して再承認** |
| 10 MB RSS <= 120 MB | max 87.85 MB | Pass |
| 100 MB RSS <= 350 MB | max 242.22 MB | Pass |

## Phase 2判断

最終判断は **Go** とする。

Phase 1の目的であるplain text editorの入力、selection、IME、Undo/Redo、clipboard、巨大文書、scrollを実装し、入力latencyとmemoryの目標を満たした。Phase 0で未達だったscrollは安定60 fpsを超え、100 MB RSSにも十分なheadroomを確保した。

空editorの元目標だけは未達だが、最小GPUI baselineとHane固有増分を分離し、ADR-0014で数値gateを再承認した。Phase 2ではMarkdown presentationを追加するが、可視行cache、source identity、入力path非同期性、65 MB空editor上限を回帰させないことを開始条件とする。
