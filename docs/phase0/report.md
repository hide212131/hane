# Phase 0 実装・測定レポート

## 実装結果

ADR-0010 の順序に沿って、以下を実装した。

- GPUI `0.2.2` と Rust `1.93.1` を固定。Metal Toolchain がない開発環境では `runtime_shaders` を使用。
- `app`、`document`、`markdown`、`metrics`、`presentation`、`editor`、`ui`、`benchmark` の Cargo workspace。
- `ropey` による UTF-8 byte offset の Text Buffer、revision、edit summary、inverse edit、anchor、revision delta。
- Text Buffer に集約した改行込み／改行除外の行範囲計算と、文書全体を `String` 化しない UTF-16 ↔ UTF-8 offset 変換。
- grapheme 単位の cursor、単一 selection、通常 text input。cursor は表示文字列へ文字を挿入せず、文字位置を変えないオーバーレイとして描画。
- UTF-16 ↔ UTF-8 変換を含む IME composition transaction、commit、cancel、競合検出。
- 太字だけの局所 parse、hidden marker を持つ SourceMap、bold style run。
- style と cursor の境界を純粋計算で分割する line span と、GPUI element への変換を分離した行描画。
- Fenwick tree の `HeightIndex`、overscan 付き visible range、scroll anchoring primitive。
- GPUI `EntityInputHandler`、未確定範囲、選択 UTF-16 range、paint callback への入力 latency 観測点。
- `metrics` crate の rolling window、percentile集計、macOS Mach RSS取得。UI と benchmark は同じ実装を使用する。
- ウィンドウ実寸から算出する viewport height と、色・行高・overscan を集約した UI theme。
- 10 MB / 100 MB / 100,000段落 / 日本語 / Unicode混在 fixture generator。
- median / p95 / p99 / max、環境情報、RSS、buffer edit、file open、presentation、layout の harness。

Phase 0 の非目標である保存UI、完全なMarkdown parser、Undo/Redo command、設定、packaging は実装していない。

## 自動測定

測定日: 2026-08-24。Apple M3 Pro、macOS 26.5.1、release profile、Rust 1.93.1、GPUI 0.2.2。各buffer editは先頭・中央・末尾を循環し90 samples、file openは30 samples。fixtureは測定直前に自動生成した。

| Scenario | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---|---:|---:|---:|---:|---:|
| 10 MB buffer edit | 90 | 0.001 | 0.003 | 0.020 | 0.020 |
| 100 MB buffer edit | 90 | 0.000 | 0.002 | 0.009 | 0.009 |
| 10 MB file open | 30 | 12.047 | 12.676 | 14.388 | 14.388 |
| 100 MB file open | 30 | 69.889 | 71.643 | 93.728 | 93.728 |
| bold presentation update | 1,000 | < 0.001 | < 0.001 | < 0.001 | 0.005 |
| visible layout index | 1,000 | < 0.001 | < 0.001 | < 0.001 | < 0.001 |

測定プロセスの100 MB scenario後RSSは238,108,672 bytesだった。この値にはallocatorが保持した以前のscenarioの領域も含まれるため、ADR-0001の「100 MB文書 <= 350 MB」に対する保守的な参考値として扱う。

## 追加自動UI検証

検証日: 2026-08-25。release profileのネイティブGPUIウィンドウを起動し、PIDからウィンドウを特定してmacOSの`System Events`から入力し、ヘッダのrevision・byte数と本文をスクリーンショットで確認した。キャプチャは`target/captures/`に保存した。

- 10 MB fixtureは10,485,760 bytesで起動し、OS入力後にrevision 7、10,485,767 bytesとなり、先頭行の表示も更新された。
- 100 MB fixtureはrevision 0、104,857,600 bytesで起動し、OS入力後にrevision 3、104,857,609 bytesとなり、画面表示の`frame p95`は1.97 msだった。
- macOS日本語IMEで`nihongo`を入力すると、composition中に本文先頭へ「日本語」が反映されrevision 7、1,048,585 bytesとなった。Return確定後はrevision 8、同じbyte数となり、composition updateとcommitが実IME経由で別イベントとして通ることを確認した。
- 40行fixtureで下方カーソル移動を32回実行し、表示が6〜33行目へスクロールし、カーソルが33行目のviewport下端に追従することを確認した。

## ADR-0009 正式UI測定

測定日: 2026-08-25。Apple M3 Pro、macOS 26.5.1、release profile、Rust 1.93.1、GPUI 0.2.2。ASCII入力は`com.apple.keylayout.ABC`、日本語入力はmacOS標準の`com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese`を使用した。内蔵ProMotion displayは可変refresh rateで、CoreGraphicsのcurrent modeは`0 Hz`を返すため「variable」と記録した。scroll模擬更新の要求周期は60 Hzである。各入力シナリオは5回warmup後に30 sequence、IMEは30 composition→commit（240 input events、うちcommit 30件）、100 MB三位置は合計90入力を測定した。RSSは各サイズについて別プロセス30回で採取した。生CSVと全metricの集計表は`target/phase0/2026-08-25/`に保存した。

### 入力latency

| Scenario / metric | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---|---:|---:|---:|---:|---:|
| normal ASCII — keystroke-to-model | 30 | 0.005 | 0.009 | 0.010 | 0.010 |
| normal ASCII — keystroke-to-frame | 30 | 1.448 | 3.040 | 3.127 | 3.127 |
| real Japanese IME — all events to model | 240 | 0.007 | 0.009 | 0.012 | 0.020 |
| real Japanese IME — all events to frame | 240 | 1.522 | 3.097 | 4.099 | 6.070 |
| real Japanese IME — commit to model | 30 | 0.007 | 0.009 | 0.018 | 0.018 |
| real Japanese IME — commit to frame | 30 | 2.377 | 3.555 | 4.099 | 4.099 |
| 100 MB start — keystroke-to-model | 30 | 0.007 | 0.010 | 0.016 | 0.016 |
| 100 MB start — keystroke-to-frame | 30 | 1.178 | 2.479 | 2.573 | 2.573 |
| 100 MB middle — keystroke-to-model | 30 | 0.008 | 0.012 | 0.013 | 0.013 |
| 100 MB middle — keystroke-to-frame | 30 | 1.309 | 2.689 | 2.760 | 2.760 |
| 100 MB end — keystroke-to-model | 30 | 0.007 | 0.009 | 0.009 | 0.009 |
| 100 MB end — keystroke-to-frame | 30 | 1.329 | 2.601 | 2.752 | 2.752 |
| 100 MB combined — keystroke-to-model | 90 | 0.007 | 0.010 | 0.016 | 0.016 |
| 100 MB combined — keystroke-to-frame | 90 | 1.278 | 2.573 | 2.760 | 2.760 |
| 100 MB input while scrolling — to model | 30 | 0.007 | 0.008 | 0.010 | 0.010 |
| 100 MB input while scrolling — to frame | 30 | 1.277 | 2.818 | 2.904 | 2.904 |
| input during background presentation — to model | 30 | 0.003 | 0.005 | 0.006 | 0.006 |
| input during background presentation — to frame | 30 | 0.356 | 3.037 | 3.205 | 3.205 |

### Frame intervalとvisible layout

layoutはvisible range計算だけでなく、各可視行のpresentation、line span、GPUI element構築までを含む。background条件では約0.8 MBのbold presentationをbackground executorで連続構築し、完了generationをUIへ適用してpaintを要求した。

| Scenario / metric | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---|---:|---:|---:|---:|---:|
| 100 MB scroll only — frame interval | 89 | 16.660 | 25.078 | 26.399 | 26.399 |
| 100 MB scroll only — layout | 89 | 0.163 | 0.353 | 0.375 | 0.375 |
| 100 MB input while scrolling — frame interval | 122 | 16.593 | 24.865 | 25.847 | 26.002 |
| 100 MB input while scrolling — layout | 122 | 0.120 | 0.350 | 0.384 | 0.385 |
| background presentation + input — frame interval | 262 | 8.308 | 8.450 | 9.016 | 9.117 |
| background presentation + input — layout | 262 | 0.052 | 0.075 | 0.081 | 0.087 |
| 100 MB start/middle/end input — layout | 90 | 0.082 | 0.165 | 0.173 | 0.173 |

scroll-onlyのmedianは60.02 fps相当だが、p95は39.88 fps相当であり、「安定60 fps」は満たさない。現在の模擬scrollはGPUI `Timer`を16.667 ms周期で起こすため、timer wakeupとpaint schedulingだけでbudgetを超える。Phase 1ではdisplay-linked schedulingまたはnative scroll eventのcoalescing、およびvisible presentation/element cacheを先に導入する。

### Startup

`hane_ready`は、view生成時にstartup timingをarmし、最初の`InputCapture::paint`が`window.handle_input`を設置した直後に出力する。旧測定の初回paint前観測点は廃止した。

| Scenario | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---|---:|---:|---:|---:|---:|
| empty warm startup | 30 | 126.563 | 142.408 | 158.016 | 158.016 |
| empty cold startup（OS cache未purge） | 30 | 133.853 | 151.821 | 158.041 | 158.041 |

cold再現手順は、release binaryを事前buildし、各sampleで新規プロセスを起動して`hane_ready` CSV行を待ち、終了する、を30回繰り返す。今回の権限では`/usr/sbin/purge`が`Operation not permitted`となったためOS cacheはpurgeしていない。より厳しい再測定では各起動前に管理者が`sudo /usr/sbin/purge`を実行し、同じ手順を使う。ADR-0009が述べる通りcache影響を完全には排除できないため、今回値には条件を明記して比較する。

### RSS

RSSはMach `task_info(MACH_TASK_BASIC_INFO)`の`resident_size`を使用した。外部process起動をstartup経路へ混入させていない。

| Scenario / point | Samples | Median (bytes) | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| empty ready（warm startup process） | 30 | 64,716,800 | 64,815,104 | 64,897,024 | 64,897,024 |
| 10 MB load直後 | 30 | 85,557,248 | 85,770,240 | 85,852,160 | 85,852,160 |
| 10 MB first visible layout後 | 30 | 92,160,000 | 92,487,680 | 92,569,600 | 92,569,600 |
| 10 MB 30秒idle後 | 30 | 108,183,552 | 108,806,144 | 109,314,048 | 109,314,048 |
| 100 MB load直後 | 30 | 325,287,936 | 325,566,464 | 325,779,456 | 325,779,456 |
| 100 MB first visible layout後 | 30 | 331,284,480 | 331,612,160 | 331,743,232 | 331,743,232 |
| 100 MB 30秒idle後 | 30 | 337,838,080 | 348,356,608 | 349,437,952 | 349,437,952 |

100 MB idleはmaxでも350 MB以内だが、headroomは約0.56 MBしかない。allocatorのpage返却とfixture読込時の一時`String`→Rope二重保持をPhase 1開始時に分離測定し、回帰余地を作る。

## UI測定点

アプリはarm後の初回InputCapture paintで`startup_time_ms`と`file_open_time_ms`を標準エラーおよびCSVへ出力する。入力ごとにevent種別、`input received → model updated`、その編集を含む`InputCapture::paint`までを保存する。paint間隔とvisible layout時間も上限付きrolling windowへ記録し、median / p95 / p99 / maxを標準エラーへ出力できる。`HANE_METRICS_CSV`、`HANE_METRICS_SCENARIO`、`HANE_METRICS_GATE`で保存先、scenario、warmup除外を制御する。visible range、scroll上限、cursor追従にはrender時のウィンドウ実寸からheader高を除いたviewport heightを使用する。

## 検証結果

- `cargo check --workspace`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo test --workspace`: 34 tests pass
- GPUI window smoke launch: pass（2026-08-24時点。行境界の cursor overlay をスクリーンショットで確認）
- 10 MB / 100 MB GPUI windowでOS入力と画面更新: pass（2026-08-25）
- macOS日本語IME composition / commit: pass（2026-08-25、warmup後30 sequences）
- cursor追従scroll capture: pass（2026-08-25）
- fixture byte size: 10,485,760 bytes / 104,857,600 bytes
- `paragraphs_100k.md`: 100,000 lines

単体テストはUTF-8境界、CRLFを含む行範囲、anchor bias、inverse edit、revision rebase、日本語・絵文字・結合文字・サロゲートペア、Ropeを平坦化しないUTF-16 offset変換、cursorのgrapheme・行境界、IME update / commit / cancel、bold SourceMap、cursorとboldが重なるline span、HeightIndex、rolling metrics windowを対象とする。

## 現時点の制約

`VisualBlock::rebase`、`measured_height`、`ScrollAnchor`、`anchored_scroll_y` は後続実装のprimitiveとして存在するが、UIの描画経路にはまだ統合していない。現在のUIは可視範囲とoverscanに含まれる行だけを毎frame再構築する。したがって、ADR-0006で定めたblock cache、局所invalidation、実測高さ更新に伴うscroll anchoringは未完了である。

## ADR-0001目標比較

| 目標 | 結果 | 判定 |
|---|---:|---|
| warm startup median <= 150 ms | 126.563 ms | Pass |
| cold startup median <= 400 ms | 133.853 ms（cache未purge） | Pass（条件付き） |
| normal keystroke-to-frame p95 <= 8 ms | 3.040 ms | Pass |
| normal keystroke-to-frame p99 <= 16 ms | 3.127 ms | Pass |
| 100 MB keystroke-to-frame p95 <= 16 ms | 2.573 ms | Pass |
| 100 MB keystroke-to-frame p99 <= 33 ms | 2.760 ms | Pass |
| scroll中の安定frame rate >= 60 fps | median 60.02 / p95 39.88 fps相当 | **Fail** |
| empty startup RSS <= 60 MB | median 64.72 MB | **Fail** |
| 10 MB RSS <= 120 MB | max 109.31 MB | Pass |
| 100 MB RSS <= 350 MB | max 349.44 MB | Pass |

## ADR-0010完了条件

- [x] 10 MB / 100 MB fixtureを開いた状態で入力できる。
- [x] 日本語IME compositionと確定入力ができ、実IMEで30 sequence測定した。
- [x] scroll中に可視範囲のみ描画でき、frame intervalを保存した。
- [x] 太字だけのpresentation更新ができる。
- [x] ADR-0009のstartup、file open、keystroke-to-model/frame、frame interval、presentation、layout、RSSをmedian / p95 / p99 / maxで出力した。
- [x] 測定条件、結果、Pass/Fail、次phase判断を本reportへ記録した。

## Phase 1 判断

最終判断は **Go** とする。入力KPIは通常文書・100 MB・scroll中・background presentation中・実IME commitのすべてで目標内であり、Phase 0の主目的である巨大文書編集loopの成立を確認できた。

未達は隠さず、Phase 1の先頭で次の構造変更を必須とする。

1. timer駆動scrollをdisplay-linked schedulingまたはnative scroll event coalescingへ置き換える。
2. visible `VisualBlock`、line span、element構築をrevision/range単位でcacheし、毎frame再構築を止める。
3. fixture open時の一時`String`→Rope二重保持とallocator page返却を分離し、100 MB RSSに20 MB以上の回帰headroomを作る。
4. empty shellの固定memoryを60 MB以内へ下げるか、GPUI baselineとの差を明示して目標を再承認する。

ADR-0001は、目標未達でもbottleneckとPhase 1前の構造変更が明確ならPhase 0を有効な完了成果とみなす。本測定ではscroll scheduling、visible element再構築、file-open時のmemory peakという変更箇所まで限定できたため、Phase 0を正式完了とする。
