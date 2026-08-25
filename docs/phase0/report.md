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
- 依存を持たない `metrics` crate の rolling window と percentile 集計。UI と benchmark は同じ percentile 実装を使用する。
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

100 MB UIプロセスのRSSは次の通りだった。

| 観測点 | RSS (bytes) |
|---|---:|
| `hane_ready`直後 | 347,635,712 |
| visible layout・入力後 | 350,404,608 |
| 30秒idle後 | 245,694,464 |

連続起動によるwarm条件を30回測定した。現在の`hane_ready`は初回paintより前に出力されるため、これらは現在の観測点における起動時間であり、ADR-0009の「入力可能になるまで」の厳密な代替とはしない。

| Scenario | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---|---:|---:|---:|---:|---:|
| empty warm startup | 30 | 134.840 | 144.412 | 148.211 | 148.211 |
| empty file open section | 30 | 65.216 | 81.523 | 85.114 | 85.114 |
| 100 MB warm startup | 30 | 236.017 | 294.510 | 344.317 | 344.317 |
| 100 MB file open section | 30 | 175.334 | 238.555 | 246.082 | 246.082 |

UI画面の`frame p95`は少数イベントのrolling window値である。上記の1.97 ms、5.19 ms、11.04 ms、15.12 msは機能動作の証跡にのみ使い、ADR-0001の性能合否判定には使わない。

## UI測定点

アプリは起動時に `startup_time_ms` と `file_open_time_ms` を標準エラーへ出力する。入力ごとに `input received → model updated` を記録し、その編集を含む `InputCapture::paint` で `keystroke_to_frame` を確定する。直近のp95はウィンドウ上部に表示する。paint間隔とvisible layout時間は上限付きrolling windowへ記録し、RSS取得APIも実装済みである。visible range、scroll上限、cursor追従には固定値ではなく、render時に取得したウィンドウ実寸からヘッダ高を除いたviewport heightを使用する。

## 検証結果

- `cargo check --workspace`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo test --workspace`: 31 tests pass
- GPUI window smoke launch: pass（2026-08-24時点。行境界の cursor overlay をスクリーンショットで確認）
- 10 MB / 100 MB GPUI windowでOS入力と画面更新: pass（2026-08-25）
- macOS日本語IME composition / commit: pass（2026-08-25、1 sequence）
- cursor追従scroll capture: pass（2026-08-25）
- fixture byte size: 10,485,760 bytes / 104,857,600 bytes
- `paragraphs_100k.md`: 100,000 lines

単体テストはUTF-8境界、CRLFを含む行範囲、anchor bias、inverse edit、revision rebase、日本語・絵文字・結合文字・サロゲートペア、Ropeを平坦化しないUTF-16 offset変換、cursorのgrapheme・行境界、IME update / commit / cancel、bold SourceMap、cursorとboldが重なるline span、HeightIndex、rolling metrics windowを対象とする。

## 現時点の制約

`VisualBlock::rebase`、`measured_height`、`ScrollAnchor`、`anchored_scroll_y` は後続実装のprimitiveとして存在するが、UIの描画経路にはまだ統合していない。現在のUIは可視範囲とoverscanに含まれる行だけを毎frame再構築する。したがって、ADR-0006で定めたblock cache、局所invalidation、実測高さ更新に伴うscroll anchoringは未完了である。

## Phase 1 判断

現時点の判断は **保留** とする。10 MB / 100 MBの実UI入力、実IMEのcomposition / commit、カーソル追従スクロール、warm条件30回、100 MB UIプロセスの3点RSSは追加確認済みである。ADR-0001の合格判断には、実IMEを使った30回以上の`keystroke_to_frame`・`keystroke_to_model`、初回paint以降を観測点とするcold / warm startup、100 MB文書でのscroll frame time、scroll中・background presentation更新中の入力値を分布として採取する必要がある。

手動検証では次を固定する。

1. `cargo run --release -p hane -- target/fixtures/markdown_100mb.md` を起動する。
2. 文書の先頭・中央・末尾でASCII入力と日本語IME composition / commit / Escape cancelを各30回行う。
3. scrollのみ、scroll中の入力、background presentation更新中の入力を各30秒計測する。
4. 上部のframe p95、標準エラーのstartup / file open、RSSを記録する。
5. ADR-0001の閾値と比較し、Phase 1の Go / Hold を更新する。
