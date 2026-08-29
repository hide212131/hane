# Phase 3 実装・測定レポート

## 結論

Phase 3を完了し、Phase 4への判断を **Go** とする。

Markdown sourceを唯一の正に保ったまま、通常時の構文記号非表示とactive構文の段階表示、非一対一Source ↔ Visual mapping、revision付きbackground block contextを実装した。機能・入力latency・scroll・startup・memoryのgateを満たした。

## 実装内容

- 見出し、quote、list、太字、斜体、取り消し線、inline code、link markerの`HiddenMarkup`化。
- caret、selection、IME marked rangeに応じた`ExpandedMarkup`。
- zero-length visual segmentを保持するSourceMapと、affinity別candidate選択。
- `normalize_source` / `normalize_visual`によるcanonical position。
- disclosure rangeを含むvisible-line cache invalidation。
- 共有Rope snapshotを使うrevision付き`BlockContextIndex`。
- 40 ms debounce、最新revisionへのcoalesce、最大1 background job。
- background index完成前だけ利用する2,048行局所fence fallback。

## 自動検証

- `cargo fmt --all -- --check`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo test --workspace`: 54 tests pass
- ASCII、日本語、emoji、hidden boundary affinity、canonical round trip: pass
- active inlineだけの段階表示: pass
- 2,048行を超えるfenced block context: pass

## 測定条件

測定完了日: 2026-08-26。Apple M3 Pro、macOS 26.5.1、release profile、Rust 1.93.1、GPUI 0.2.2。測定対象はbase commit `552d47e`に本report記載のPhase 3 working tree変更を加えた状態である。生CSVと集計表は`target/phase3/2026-08-25-final/`に保存した。`/usr/sbin/purge`は権限不足のためcold startupはOS cache未purge条件である。

入力、startupは30 samples、100 MB start/middle/endは合計90 samples、scrollは187 framesを採取した。日本語IMEは30回のautomationに対して95 input eventsを観測したが、ことえりのcomposition→commitとして観測できたcommitは11件だった。この制約のためIME commit分布は参考値とし、全event分布と自動IME transaction testを併記する。

## 主要結果

| 指標 | 結果 | Phase 2 | 判定 |
|---|---:|---:|---|
| warm startup median | 138.629 ms | 144.408 ms | Pass（<= 150 ms） |
| cold startup median（cache未purge） | 137.197 ms | 150.271 ms | Pass（<= 400 ms、条件付き） |
| normal keystroke-to-frame p95 / p99 | 2.707 / 3.014 ms | 3.387 / 3.990 ms | Pass |
| 100 MB keystroke-to-frame p95 / p99 | 4.245 / 4.747 ms | 4.246 / 4.938 ms | Pass |
| 100 MB scroll frame interval p95 / p99 | 8.805 / 9.309 ms | 8.821 / 9.128 ms | Pass |
| scroll中input p95 / p99 | 3.467 / 3.797 ms | 3.203 / 3.355 ms | Pass |
| background presentation中input p95 / p99 | 2.955 / 3.202 ms | 2.981 / 4.590 ms | Pass |
| 日本語IME全event p95 / p99 | 9.701 / 15.082 ms | 3.302 / 4.683 ms | Pass（commit 11件の制約あり） |
| 空editor ready median / max RSS | 63,438,848 / 65,716,224 bytes | 63,062,016 / 63,406,080 | Pass（< 65 MiB） |
| 10 MB idle RSS | 99,024,896 bytes | 99,188,736 | Pass（< 120 MB） |
| 100 MB idle RSS | 248,610,816 bytes | 242,188,288 | Pass（< 350 MB） |

100 MB presentationのvisible layoutはp95 1.667 ms、p99 1.718 msだった。scroll-only p95は約113.6 fps相当で、60 fps条件を満たす。100 MB idle RSSはPhase 2から約6.4 MB増えたが、350 MB上限に約101.4 MBの余裕がある。

実UI captureは`target/captures/phase3-typora-editing.png`へ保存した。active見出しのmarker展開、非active inline markerとcode fenceの非表示、native code block表示を目視確認した。

## ADR-0016完了条件

- [x] 通常表示で主要Markdown記号を隠す。
- [x] caret、selection、IMEでactive構文だけを段階表示する。
- [x] hidden/expanded境界とUnicodeのSource ↔ Visual変換を自動テストする。
- [x] 2,048行を超えるfenced blockをbackground indexで判定する。
- [x] background jobをrevision付き・最大1本でcoalesceする。
- [x] format、clippy、workspace testがpassする。
- [x] 実UI captureと性能・memory測定を完了する。

## Phase 4判断

最終判断は **Go** とする。

Phase 3の目的であるMarkdown記号の段階表示と非一対一Source ↔ Visual mappingを実装し、性能・memory gateを満たした。RFP上、この時点で製品の主要価値が成立したと判断する。

Phase 4では画像、表、保存、自動保存、Recent Files、設定、themeへ進める。IME commit測定だけは30件を再取得し、画像・表等のsynthesized visual segment追加時にもcanonical mapping testを継続する。
