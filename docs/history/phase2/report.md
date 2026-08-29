# Phase 2 実装・測定レポート

## 結論

Phase 2を完了し、Phase 3への判断を **Go** とする。

正式なCommonMark parser、source range付きpresentation model、GPUI native style、見出しの可変高さ、可視行cache、fenced code contextを実装した。Markdown記号は表示したままSource ↔ Visualを恒等対応に保ち、cursor、selection、mouse hit test、IME、Undo/Redoを回帰させていない。

## 実装内容

- `pulldown-cmark 0.13.4`の`into_offset_iter()`を使い、block/spanを絶対UTF-8 byte rangeへ変換。
- 見出し、段落、太字、斜体、取り消し線、inline code、fenced code、quote、list item、link、horizontal ruleのpresentation model。
- 見出しlevel別のfont size/weight、codeのmonospace/background、link、quote等のGPUI native style。
- 表示font runとmouse hit testのfont runを共通のstyle情報から構築。
- 見出しとcode blockの推定高さを`HeightIndex`へ反映する可変高さ仮想scroll。
- 可視範囲の最大2,048行前からfence状態を復元する局所code-block context。
- 空blockのparser fast-pathと、metrics rolling windowの遅延allocation。
- Phase 2用UI capture、performance、memory entrypoint。

Phase 2ではMarkdown記号を隠さない。`VisualBlock.visual_text`はsource sliceと同一で、`SourceMap`は1本の`Visible` identity segmentである。記号の段階表示とhidden boundary affinityはPhase 3で実装する。

## 検証

- `cargo fmt --all -- --check`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo test --workspace`: 50 tests pass
- Unicode、日本語、emoji、nested inline style、fenced code、source identity: pass
- 実macOS日本語IME composition→commit: 30 sequence pass
- 100 MB start / middle / end入力: 各30 sequence pass
- display-linked scroll only / scroll中入力: pass
- 実UI capture: `target/captures/phase2-markdown-presentation.png`

## 測定条件

測定日: 2026-08-25。Apple M3 Pro、macOS 26.5.1、release profile、Rust 1.93.1、GPUI 0.2.2。測定対象はbase commit `8266c32`に本report記載のPhase 2 working tree変更を加えた状態である。

最終CSVと集計表は`target/phase2/2026-08-25-final-fast-empty/`に保存した。ASCII測定前に入力ソースを`com.apple.keylayout.ABC`へ固定し、IMEシナリオだけことえりへ切り替えた。empty startupは`HANE_MEASUREMENT_EMPTY=1`で真の空文書を開く。`/usr/sbin/purge`は権限不足のためcold startupはOS cache未purge条件である。

入力、IME、startupは30 samples、100 MB start/middle/endは合計90 samples、scrollは188 framesを採取した。10 MB / 100 MBのload、first visible layout、30秒idle RSSは各1 processの回帰確認値であり、Phase 1の30-process分布と比較した。

## 主要結果

| 指標 | 結果 | Phase 1 | 判定 |
|---|---:|---:|---|
| warm startup median | 144.408 ms | 139.600 ms | Pass（<= 150 ms） |
| cold startup median（cache未purge） | 150.271 ms | 136.077 ms | Pass（<= 400 ms、条件付き） |
| normal keystroke-to-frame p95 / p99 | 3.387 / 3.990 ms | 4.091 / 4.253 ms | Pass |
| 100 MB keystroke-to-frame p95 / p99 | 4.246 / 4.938 ms | 3.641 / 4.684 ms | Pass |
| 100 MB scroll frame interval p95 / p99 | 8.821 / 9.128 ms | 9.224 / 10.051 ms | Pass |
| scroll中input p95 / p99 | 3.203 / 3.355 ms | 3.278 / 4.352 ms | Pass |
| background presentation中input p95 / p99 | 2.981 / 4.590 ms | 2.034 / 2.142 ms | Pass |
| 空editor ready median / max RSS | 63,062,016 / 63,406,080 bytes | 64,077,824 / 64,339,968 | Pass |
| 10 MB idle RSS | 99,188,736 bytes | max 87,851,008 | Pass（< 120 MB） |
| 100 MB idle RSS | 242,188,288 bytes | max 242,221,056 | Pass（< 350 MB） |

100 MB presentationではvisible layout p95が1.754 ms、p99が2.629 msだった。100 MB入力p95はPhase 1から0.605 ms増えたが16 ms上限に対して10.754 msの余裕がある。scroll-only p95は約113.4 fps相当で、60 fps条件を満たす。

10 MB idleはPhase 1より約11.3 MB増えた。今回は1 process値なので分布比較ではないが、120 MB上限には約20.8 MBの余裕がある。100 MB idleはPhase 1 maxとほぼ同じで、350 MB上限に約107.8 MBの余裕がある。

## ADR-0015完了条件

- [x] 見出し、太字、斜体、取り消し線、inline code、fenced code、linkを装飾する。
- [x] Phase 2のSource ↔ Visualをidentity mappingに保つ。
- [x] cursor、selection、mouse hit test、IME、Undo/Redoを装飾中もsourceへ接続する。
- [x] 可視範囲とoverscanだけをpaint用に構築する。
- [x] 非交差可視行をrevision deltaでrebaseしてcache再利用する。
- [x] 可変高さを`HeightIndex`へ反映する。
- [x] format、clippy、workspace testがpassする。
- [x] 実UIとperformanceを測定しreportを作成する。

## Phase 3判断

最終判断は **Go** とする。

Phase 2の目的であるMarkdown presentationを実装し、入力latency、scroll、startup、memoryのgateをすべて満たした。SourceMapは意図的にidentityのままなので、Phase 3ではparser rangeを使ってMarkdown記号を隠し、cursor近傍だけ展開する。

Phase 3の開始条件は、hidden markup境界のaffinityとcanonical positionをADR-0004どおり自動テストし、2,048行を超えるfenced block contextを正式なbackground block parseで置き換え、入力pathで全文parseを待たないこととする。
