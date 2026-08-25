# Phase 4 実装・測定レポート

## 結論

Phase 4を完了し、RFPのMinimum Viable Productを **完成** とする。

Phase 3の入力latencyとSource ↔ Visual契約を維持したまま、local画像、pipe table、Open、atomic Save / Save As、revision付き自動保存、Recent Files、永続設定、system/light/dark themeを実装した。機能・入力latency・scroll・startup・memoryのgateを満たした。

## 実装内容

- standalone Markdown imageのalt/destination抽出と、可視行だけのGPUI image decode。
- active画像行では元sourceを表示し、非active行では画像とcaptionを表示。
- pipe tableのbackground context indexと、hidden pipe / alignment marker、synthesized cell separator。
- synthesized segmentを含むcanonical SourceMap。
- `Command+O`、`Command+S`、`Command+Shift+S`とnative path prompt。
- Rope chunkを全文`String`化せず同一directoryへ書くatomic save。
- 750 ms debounce、generation/revision検証付き自動保存。
- 最大10件のRecent Files、OS recent documents連携。
- autosaveとsystem/light/dark themeの永続設定、header toolbar。
- Phase 4用capture、performance、memory entrypoint。

## 自動検証

- `cargo fmt --all -- --check`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo test --workspace`: 61 tests pass
- 画像のinactive/active presentationとsource非変更: pass
- table synthesized separator、Unicode、canonical mapping: pass
- atomic saveのMarkdown byte一致: pass
- stale autosave generation/revision rejection: pass
- Recent Filesの重複排除、上限、path escape: pass

## 測定条件

測定完了日: 2026-08-26。Apple M3 Pro、macOS 26.5.1、release profile、Rust 1.93.1、GPUI 0.2.2。測定対象はbase commit `a91818a`に本report記載のPhase 4 working tree変更を加えた状態である。修正後の生CSVと集計表は`target/phase4/2026-08-26-final-optimized/`に保存した。`/usr/sbin/purge`は権限不足のためcold startupはOS cache未purge条件である。

入力、startupは30 samples、100 MB start/middle/endは合計90 samples、scrollは184 framesを採取した。日本語IMEは30回のautomationすべてでcomposition→commitを観測し、240 input eventsと30 commitsを取得した。10 MB / 100 MBのload、first visible layout、30秒idle RSSは各1 processの回帰確認値である。

## 主要結果

| 指標 | 結果 | Phase 3 | 判定 |
|---|---:|---:|---|
| warm startup median | 136.004 ms | 138.629 ms | Pass（<= 150 ms） |
| cold startup median（cache未purge） | 130.749 ms | 137.197 ms | Pass（<= 400 ms、条件付き） |
| normal keystroke-to-frame p95 / p99 | 4.284 / 5.021 ms | 2.707 / 3.014 ms | Pass |
| 100 MB keystroke-to-frame p95 / p99 | 3.982 / 4.655 ms | 4.245 / 4.747 ms | Pass |
| 100 MB scroll frame interval p95 / p99 | 8.632 / 9.256 ms | 8.805 / 9.309 ms | Pass |
| scroll中input p95 / p99 | 2.621 / 3.449 ms | 3.467 / 3.797 ms | Pass |
| background presentation中input p95 / p99 | 4.289 / 4.379 ms | 2.955 / 3.202 ms | Pass |
| 日本語IME全event p95 / p99 | 3.412 / 4.940 ms | 9.701 / 15.082 ms | Pass |
| 日本語IME commit p95 / p99 | 4.212 / 5.150 ms | 11 commitsのみ | Pass（30 commits） |
| 空editor ready median / max RSS | 64,503,808 / 64,733,184 bytes | 63,438,848 / 65,716,224 | Pass（< 65 MiB） |
| 10 MB idle RSS | 103,759,872 bytes | 99,024,896 | Pass（< 120 MB） |
| 100 MB idle RSS | 262,864,896 bytes | 248,610,816 | Pass（< 350 MB） |

100 MB presentationのvisible layoutはp95 1.602 ms、p99 1.625 msだった。scroll-only p95は約115.8 fps相当で、60 fps条件を満たす。画像はvisible blockでのみdecodeされ、通常の100 MB fixtureに画像は含まれない。

最初の測定`target/phase4/2026-08-26-final/`ではtable context構築中に全行の`String`を一時保持し、10 MB / 100 MB idle RSSが130,039,808 / 551,469,056 bytesとなりFailした。原因を行ごとの2本のboolean indexへ置換した後、103,759,872 / 262,864,896 bytesへ改善しgateを満たした。失敗測定は性能回帰の根拠として保存する。

実UI captureは`target/captures/phase4-polish.png`へ保存した。dark system theme、local SVG、caption、pipe table、autosave/theme/Recent toolbarを目視確認した。

## ADR-0017完了条件

- [x] local imageとpipe tableを通常表示し、active時に元Markdownを編集できる。
- [x] synthesized segmentを含むSource ↔ Visual mappingを自動テストする。
- [x] Open、Save、Save Asで元Markdownをbyte単位で保存する。
- [x] 自動保存をdebounceし、stale revisionを保存済みとして扱わない。
- [x] Recent Files、autosave、theme設定を永続化する。
- [x] format、clippy、workspace testがpassする。
- [x] 実UI captureと性能・memory測定を完了する。

## MVP判断

最終判断は **完成** とする。

RFPの最初の完成条件である起動、100 MB Open、任意位置への移動、日本語入力、WYSIWYG表示、Markdown記号の段階表示、Undo、元MarkdownとしてのSaveが一連の操作として成立した。画像、表、保存、自動保存、Recent Files、設定、themeを加えた後も入力、scroll、startup、memory gateを満たしている。

Phase 4の非目標としていたnetwork画像管理、高度なtable、複数window/tab、外部変更merge、packaging/署名/配布は今後の製品化範囲とする。
