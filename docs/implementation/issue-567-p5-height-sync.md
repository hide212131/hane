# Issue #567 P5: 高さ索引の同期処理の機械的分離

## 目的と基準

Refs #567（RF2-A）、親 #297。P4のPR #620はmerge済み（`c843a2d1a357a766ef2a3bd3750d2d4476e90c3f`）。本P5は高さ索引の同期・disclosure追従をmoduleで見分けられる配置にする。設計時のmainは `3b3bdd6735042445710bbced43852c66e75a5bd4`。

本書を置く準備commitは文書だけであり、製品実装・検証・受入の完了を意味しない。実装開始時にcurrent main、PR head、#567、関連PR、同じ対象の実行中runを取り直す。判断はdefault branchのCommander Policy、実装依頼はClaude実行手順に従う。

Jev `jev-1.14.0` は、残る初期化・I/O整理・契約文書照合と比較して `height_sync` を選択した（probability 0.99、confidence 0.99）。初期化は進行中のproxy設定UI #633と重なる。P5はそのfield・初期化・settings画面を動かさず、高さ同期の連続範囲だけを扱う。RF0全体を待つ必要はないが、#307/#368の意味・寿命変更を混ぜない。

## 旧→新と可視性

`crates/ui/src/view.rs` の次の10メソッドを、既存の兄弟moduleと同じ形式の新しいprivate module `crates/ui/src/view/height_sync.rs` へ、元の順序・doc comment・属性・本体を保って移動する。新ファイルは `use super::*;` と `impl EditorView { ... }` だけで包む。親にはprivateな `mod height_sync;` を一つ追加する。再exportや新しいpublic moduleは追加しない。

基準sourceの範囲は4845〜5456行。開始は `/// Initial height of every entry, from the line height alone.`、終了は `/// Source offset of the item currently at the top of the viewport.` の直前。UTF-8で26,791 bytes、SHA-256は `bf19513848d4094aa49ee9ba5cb67ec97c5df58fd7a82ae02a05febd2c0b62d6`。行番号だけを移動命令とせず、current codeのシンボルと範囲が一致することを確認する。

| メソッド | 移動後の可視性 | 範囲外の呼出元・理由 |
| --- | --- | --- |
| `item_heights` | `pub(super)` | 親のrebuild、reading_mode、render、settings_screen |
| `ensure_active_disclosure_height` | `pub(super)` | 親after_input、background_parse |
| `fence_height_delta` | private維持 | 移動範囲内だけから呼ぶ |
| `table_delimiter_height_delta` | private維持 | 移動範囲内だけから呼ぶ |
| `install_disclosure_heights_preserving_measurements` | `pub(super)` | background_parse |
| `active_height_disclosure` | `pub(super)` | 親visible-line処理・tests、background_parse、reading_mode |
| `resync_heights` | `pub(super)` | 親after_input |
| `resync_heights_for_current_disclosure` | `pub(super)` | background_parse |
| `install_heights` | `pub(super)` | 親rebuild・tests、background_parse、reading_mode、render |
| `apply_height_index_update` | `pub(super)` | 親after_input・tests |

可視性調整は上記8件に限定する。元のprivateメソッドを兄弟・親から引き続き呼ぶための局所調整であり、crate外APIは増やさない。callerは `self.method(...)` / `view.method(...)` のまま変更しない。新しいtrait、wrapper、manager、状態所有者を作らない。

## 所有・共有境界

全fieldとその初期化はEditorViewに維持する。移動範囲が直接参照するfieldは `granularity`、`heights`、`height_blocks`、`scroll_y`、`last_applied_height_disclosure`、`last_endpoint_height_disclosure`、`block_cache`、`layout_cache`、`content_width`、`layout_font_revision`。型、DocumentKey、revision、jobとcacheの所有、session/workspace切替処理は親・既存moduleのまま。

`current_index`、`desired_layout`、`line_height`、`top_source_offset`、`scroll_for_offset`、`rebuild_height_estimates`、`invalidate_layout_font_revision`、HeightBlocks/HeightBlock、`rebase_ordinal_after_splice`、`preserve_measured_block_heights`、`preserve_measured_height_after_fence_delta` とその他free helperは親に残す。子moduleは祖先のprivate itemへアクセスできるため、移動のためにこれらをpub化しない。親importは兄弟のglob importを含む既存利用を尊重して維持する。

height以外のcached_block/block_layout、render範囲、row geometry/hit test、viewport操作、new/from_sessions、外部open/save/draft、settingsとproxy設定は移さない。既存testsは名前・属性・入力・期待値・本体と配置を維持する。

## 維持する不変条件

- `current_index` のrevision一致と空index拒否、Lines/Blocksの切替条件、同じshapeならresyncを省くfast pathを維持する。
- IME current range優先、selection rangeへのfallback、reading modeでdisclosureなしとする条件を維持する。同期endpoint bridgeは端点/IME境界を対象とし、選択範囲全体への走査へ広げない。
- `last_endpoint_height_disclosure` と `last_applied_height_disclosure` を別々に維持し、background snapshotの基準を同期bridgeで上書きしない。初回parse公開時のLines→Blocks橋渡しを維持する。
- fence/tableの算術fallback、cacheのwidth/font revision/document revision照合、実測wrapped rowによる補正、未測定/zero-height時の扱いを維持する。
- disclosure-only snapshotはblock geometryと測定済み高さを保持し、snapshot/index/height長の不一致を拒否する。anchorとintra-item offsetの取得順、clamp、scroll復元、最後のdisclosure記録の順を維持する。
- full rebuild時のblock id/revision/line_count/source_range/estimated bits/content widthによる再利用と、revision/widthが変わる場合の測定値不継承を維持する。
- incremental spliceの範囲・長さ検査、失敗時false、`range_eq` fast path、touchする範囲、source block idに基づくanchor rebaseとclampを維持する。全体走査や全文cloneを追加しない。

既存の不具合修正、式の単純化、キャッシュ統合、閾値変更、API縮小、型/fieldの移動、所有権やtask寿命の変更、機能追加、大量formatは対象外。必要な意味変更は別のroot cause/PRとして判断する。

## 検証と受入

実装後はcurrent headと必要なbase contextを記録して、次を区別して提出する。

1. 機械的比較: 新moduleから外側wrapper/importと8つの `pub(super)` だけを取り除いて旧範囲とbyte比較。親の差分はmodule宣言追加と旧範囲撤去だけ。既存tests・caller・型/field/初期化が同一で、変更ファイルがRust 2つと本書/実行記録に限定されることを確認する。
2. 通常構成の `cargo test --workspace --locked`、all-featuresの `cargo test --workspace --locked --all-features`、`cargo clippy --workspace --locked --all-targets --all-features -- -D warnings`。macOS/Windowsのcurrent-head required CIを確認する。workerの実行権限制限を緩めず、trusted verification/CIとCommanderの検証を分けて記録する。
3. Rust toolchainはrepository指定に合わせ、通常構成の `cargo build --release --locked -p hane` を確認する。instrument版を通常Releaseの代替にしない。fmt/diff確認で既存差分を見つけても無関係なformatを混ぜない。
4. 安定候補でcurrent-head CI成功後にPolicy所定の最終review、次表のfocused native GUIを実施する。旧P4のGUI Unknown受入をP5へ継承しない。失敗/blocked/unknownを成功へ変えない。

既存回帰testの代表例（すべて親view.rsのまま）:

| 境界 | 維持・実行するtest |
| --- | --- |
| 端点移動・初回公開 | `moving_into_a_hidden_closing_fence_rechecks_scroll_after_height_expands`、`moving_away_from_a_hidden_fence_shrinks_the_previous_height`、`initial_line_heights_become_block_heights_when_parse_publishes_after_caret_move` |
| 遅い解析・選択の縮小 | `background_formal_parse_rebuilds_heights_when_disclosure_moves_during_parse`、`background_selection_height_snapshot_covers_the_selected_fence_range`、`background_selection_snapshot_preserves_measured_height_and_cached_presentation`、`background_selection_height_snapshot_collapses_middle_blocks_when_selection_ends` |
| 測定値の保持・無効化 | `full_height_rebuild_keeps_measurements_for_unchanged_source_blocks`、`full_height_rebuild_does_not_inherit_measurement_across_a_revision_change`、`full_height_rebuild_does_not_inherit_measurement_across_a_width_change` |
| 局所splice・anchor | `height_anchor_rebases_without_scanning_untouched_blocks`、`local_block_height_seed_matches_the_full_disclosure_projection`、`incremental_height_splice_preserves_measured_height_outside_the_update`、`apply_height_index_update_resets_height_when_only_revision_changes` |

| GUI | macOS / Windowsの同一fixture・候補Releaseで観測すること |
| --- | --- |
| G1 通常編集とdisclosure | 日本語本文・複数行fence・table delimiterを含む隔離文書で、caretを構造行へ入れて戻す。日本語IMEを開始/確定/取消し、行の出現/縮小、caret・選択とhit位置の一致、文書内容・undo/redoを確認する |
| G2 選択と遅い公開 | 複数fence/tableをまたぐ選択を広げて縮める。解析完了後も中間行の不要な高さが残らず、入力対象・scroll位置が整合する。初回openのparse公開前後も観測する |
| G3 rebuildと測定 | 長いdelimiterを折り返す幅でスクロールしてから幅/zoom/theme変更、編集/閲覧モード往復。見えていたsource付近のanchor、行欠落・重なり・末尾を越すscrollがないことを比較する |
| G4 incrementalと文書切替 | スクロール途中で局所編集によるblock split/join・undo/redo後、別文書へ切替え再表示。未編集範囲の測定とanchorの整合、旧文書の行やheightの混入がないことを確認する |

GUI fixture内容・操作・期待値と実結果、対象head/base、OS/toolchain、成果物hash、隔離state、実PID/実行パスを残す。Windowsは接続済みWindows環境で直接操作し、GUI占有と所有者を確認する。失敗は `docs/gui-test-recovery.md` に従って復旧し、未知の所有者のappを停止しない。

これはbyte-identicalな機械移動であり性能改善を主張しない。新しい最適化・処理量変更は計画せず、既存のbounded処理・無関係block保持のassertを維持する。意味変更が必要になったら本P5から分け、#23/#307/#368の該当する変更前測定を取得する。

## 後続と復旧

#567の残りは初期化・非同期I/O接続、残るheight/cache/geometry共有helper、関連testsを含む棚卸しであり、この10メソッドだけで完了しない。親維持にした共有helperは意味・所有整理の#307/#368と対応させる。I/O/H1は#308/#309へ引き渡す。未判断を対象外へ変えて#567/#297をcloseしない。

実装受入時に `docs/refactor-execution-plan.md` の§9.13配下へ、移動/親維持/後続の対応と検証URLを追記する（設計全文は複製しない）。rollbackはこの機械移動PRを一単位でrevertし、永続データ移行は不要とする。次のRF Issue/フェーズを選ぶのは本P5 PRのmerge後だけとする。
