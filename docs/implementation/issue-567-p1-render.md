# RF2-A / #567 P1: render組立の実装引き継ぎ

## 状態と契約の正本

Refs #567、親 #297。台帳RF0-05、実行計画§9.8〜§9.13の受入済み分割を継承する次の一責務である。

**本書は実装前に作成した仕様である（準備時点の履歴）。** 作成時点では製品・testの変更、build/test/GUI/性能測定は未実施で、Claudeの起動依頼も未投稿であり、利用者の依頼は実装担当への引き継ぎPR作成までだった。

**その後の状態（実装コードhead `7412384071cace5889952ce5fe67eaea673d8e69` 時点の履歴）:** 初回依頼はPR Conversationに投稿され、render移動は適用済みである。同headでtrusted run 40051416483のprepare・Claude Sonnet 5.5 worker・finalizer・exact-head verifyが成功し、verifyのworkspace testsとClippyも成功した。PR CI run 40051653682も同head／base `c1f387ded69180aa5cc5f3075d67a99ec067a7b2` でWindows/macOS required jobsが成功し、macOS focused GUIも完了している。**未完了:** Windows focused GUIと、受入に必要な最終review。Issue #567全体も完了していない。本書の修正でPR headは更新されるため、上記のrun・CI・GUI結果は `7412384` 時点のものであり、文書修正後の新headに対する検証結果ではない。

仕事の契約はIssue #567、全域の契約は `docs/refactor-plan.md` と `docs/refactor-execution-plan.md`、運用判断はdefault branchの `docs/aadw-command-policy.md`、実行入口は `docs/aadw-claude-execution.md` を使う。本書は今回の移動仕様を補う。既存ADRを変更する設計判断はない。

## 基準と着手可能な範囲

調査mainは `c1f387ded69180aa5cc5f3075d67a99ec067a7b2`。既存の作業branchを流用せず、このSHAから準備branchを作った。Issue本文の古いmainやrender候補のhashを再利用しない。

以前の直接競合PR #548は `69ea1c64c1710b3db3dff936d7d194f00ab09757` でmerge済み。閲覧モードを含むコードを今回の基準に含める。#555もmerge済み。#548後の初回release待ちという過去の条件について、v0.29.0/v0.30.0の公開を確認した。調査時はopen PRなし、実行中はmainのrelease系runだけで、同じ対象の実装workerはない。これは将来のresource占有やcurrent-headの実装許可を保証しない。実装開始時に取り直す。

#298/#300のRF0-05にある「API削除でなく分割」の引き渡しと既存配置を使う。RF0全体を待たせない。#305の未完native入力確認、#306の全経路照合、#307のgeometry、#308/#309の保存・命名、#368のcache/job寿命の意味変更は、この配置変更の先行条件にも完了成果にも読み替えない。該当責務の実装PRが新しく開いた場合は同じ状態を並行変更せず順序を調整する。

## 旧→新と可視性

| 旧配置・シンボル | 新配置 | 変更 |
| --- | --- | --- |
| `crates/ui/src/view.rs::impl Render for EditorView`（`render`一件） | `crates/ui/src/view/render.rs` | impl全体を原文どおり移動 |
| `view.rs`のmodule宣言 | `view.rs` | privateな `mod render;` を追加 |
| 型・field・helper・tests・隣接定数 | 元の場所 | 維持 |

新moduleは既存の兄弟moduleと同様に `use super::*;` を使う。`EditorView`の型・公開経路、全fieldの所有者、trait実装の署名は不変。親のprivate itemは子から参照でき、兄弟の `pub(super)` メソッドは共通親 `view` の範囲で参照できるため、新しいpub化は予定しない。名前解決はコンパイラで検証する。追加の可視性変更が必要なら具体的なcallerと理由を記録し、広範なpub化で通さない。

親importは他の親コードや子moduleのglob importにも使われる。移動だけを理由に一括削除しない。warnings deniedで不要と確認されたimportだけを必要最小限調整し、本文の移動と区別する。属性・コメント・closureのcapture・登録順も維持する。

基準sourceの対象範囲は7160〜7726行（impl開始から直後のmacOS `REVEAL_IN_FILE_MANAGER_LABEL` のcfg直前、末尾空行を含む）、28,795 bytes、SHA-256 `64fadfb1da98701b5dc43f0f686bc415a123897b8dbdfc16ec34c0712d90723d`。位置依存の `include*` / `file!` / `line!` / `column!` / `module_path!` はこの範囲にない。これは移動前snapshotの識別であり、移動後同等性の証拠ではない。

### stateとcaller

GPUIが既存の `Render` traitを通じて呼ぶ。型を使うapp、capture、actions、既存testsに新しい入口やwrapperを作らない。

直接参照するfieldは次のとおり。これらは親 `EditorView` に残し、初期化・破棄・更新の所有者を増やさない。

`_input_mode_focus_subscription`, `block_cache`, `caret_geometry`, `caret_input_mode`, `content_width`, `file_tab_context_menu`, `focus_handle`, `granularity`, `heights`, `joined_parse_cache`, `layout_cache`, `layout_font_revision`, `line_owners`, `main_column_left`, `metrics`, `pending_caret_visibility_after_layout`, `pending_center_caret_frames`, `pending_find_visibility_after_layout`, `pending_zoom_anchor`, `scroll_inertia`, `scroll_y`, `sessions`, `settings`, `settings_open`, `sidebar_context_menu`, `sidebar_width`, `tab_close_confirm`, `theme`, `viewport_height`, `zoom`。

直接 `self` から呼ぶ既存メソッドは次のとおり。各定義を移動せず既存の接続を維持する。

`ai_selection_overlay`, `apply_height_anchor`, `block_layout`, `body_caret_visible`, `body_editable`, `cached_block`, `cancel_center_caret`, `current_index`, `desired_layout`, `document_find_bar`, `document_find_current_match`, `document_find_matches_in_view`, `document_find_reserved_height`, `document_instance`, `editor`, `editor_scrollbar`, `file_tab_context_menu_element`, `focus_handle`, `follow_retained_center_window`, `footer_element`, `fresh_point_for_offset`, `header_element`, `install_heights`, `invalidate_layout_font_revision`, `item_heights`, `line_height`, `reading_visible_row`, `render_window_for`, `request_layout_followup_frame`, `resolve_mode_anchor`, `schedule_document_parse`, `schedule_joined_parse`, `scroll_y_for_zoom_anchor`, `scrollable_content_height`, `settings_screen_element`, `sidebar_context_menu_element`, `sidebar_resizer`, `sidebar_shown`, `source_offset_row_bounds`, `step_measurement_scroll`, `step_scroll_inertia`, `step_wheel_zoom_animation`, `tab_close_confirm_element`, `visible_blocks`, `visible_line_window`, `work_folder_sidebar`。

`Self::on_external_paths_drop`、`Self::on_panel_mouse_move`、`Self::finish_panel_drag`、`Self::on_editor_mouse_down`、`Self::on_scroll`、`Self::on_pinch`、`EditorView::open_ai_selection_action`、row closure内の `on_row_mouse_down` / `on_row_mouse_move` とaction listenerも変更しない。`block_element` / `row_element` / `WindowShaper` / `InputCapture`は既存の実装を使う。

## 順序と不変条件

| 区間 | 維持する条件 |
| --- | --- |
| 入口 | layout開始時刻、focus購読の一度だけの登録、theme変更時cache/height更新、wheel zoom、inertiaの順 |
| settings | 既存early returnとdrop listener。末尾layout計測を通らない現状を変更しない |
| 文書/frame | tab menu/close確認のDocumentInstance guard、document parse、find bar分のviewport、sidebar幅、font revisionとclamp |
| 可視範囲 | visible blocks、joined parse開始、cache retain、presentation/layout、height更新とline ownersの順・上限 |
| 位置補正 | mode anchor→zoom anchor→height anchor、inertia追従、再clamp、fresh caret、caret/center/find補正。閲覧時の非表示rowとfollow-up frame要求を維持 |
| 描画・入力 | caret geometryのvisibility条件、spacer、sidebar/resizer、action/drop/mouse/scroll/pinch listener順、capture、row引数と捕捉 |
| 終端 | table row debug selectorのphysical source line、footer、各menu、close確認、AI overlay、layout記録の位置 |

helper化、式の書換え、RAII化、cache範囲変更、新しいManager/trait/Entity/状態所有者、最適化、大量整形、機能追加、既存不具合修正を混ぜない。source/snapshotのstrict-match、BlockIndexの条件付きrebase、表示/同期編集projectionの区別は維持する。

## 他の残作業との対応

| 責務 | 今回の判定と後続 |
| --- | --- |
| sidebar/filter、rename、scroll/zoom、save/draft/H1、background parse | §9.8〜§9.13の既存moduleで処理済みの配置を維持。意味変更は#305/#306/#308/#309等 |
| render組立 | 今回移動。helperの意味と状態所有は維持 |
| height/cache共有helper | 親に維持。renderと入力の共有があり、一括移動せず#567の残作業と#368の寿命整理で追跡 |
| pointer/panel、settings、計測、初期化・非同期要求接続 | 今回は親に維持。別の責務移動として#567で棚卸しを続ける |
| AI/search/update、input_routing、短い入力欄 | current mainの既存子moduleを維持。旧設計からの重複分離はしない |
| 各tests/fixture | 現位置を維持。module path・test名・属性・入力・期待値・本体を変えない |

この表はP1の境界であり、#567全対象の棚卸し・受入完了を主張しない。

## 実装後の受入と証拠

1. **移動の同等性:** 実装に用いたbaseからimpl全体を抽出し、新moduleの `use super::*;` を除く移動範囲とbyte単位で比較する。本文・コメント・listener順が一致し、旧定義がなく、`impl Render for EditorView`が一つだけであることを確認する。baseが変わったら本書のhashに合わせて旧コードへ戻さず、current sourceから再抽出する。
2. **境界:** 全field/初期化・API・caller・test一覧が不変、差分が親と新moduleおよび説明文書だけであることを確認。import調整を別記する。不要なtest追加や既存testの削減をしない。
3. **既存回帰:** 下記の既存testが実装headで実行されることを確認する。sourceを読んだだけではpassにしない。
4. **workspace/CI:** Rust 1.98.1で `cargo test --workspace --locked`、`cargo test --workspace --all-features --locked`、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、`git diff --check`。macOS/Windowsの関連required CIをcurrent headで確認する。並列実行固有の既存failureはbaseとの対照と観測を残し、単一スレッド成功だけで解消扱いにしない。
5. **focused GUI:** renderの実画面配線に関わるため、実装headでmacOS/Windowsの設定→本文復帰、find barの開閉とcaret/scrollbar、zoom・soft wrap・table click、sidebar/filter/renameのfocus/入力、編集↔閲覧のsource位置維持を確認する。本文入力・選択・undo、reading時の変更拒否も確認。既存scenarioを使い、対象SHA・base・OS・fixture・操作・実観測をPRに残す。GPUI testをnative入力/実字形の代替にしない。Windowsは接続先Windowsから直接操作し、復旧は `docs/gui-test-recovery.md` に従う。
6. **性能と構成:** 機械的配置だけなら新しい処理量・性能改善は主張しない。計測位置と上限の不変を確認し、処理量/遅延を変える必要が出たら別PRに分け変更前測定を行う。起動/feature配線を変える場合の通常/release buildも別途必要とし、移動だけを理由に配線を変えない。
7. **受入記録:** 実行計画へ今回の旧→新、維持理由と後続を記録し、CI/review/GUIのURL・対象head/baseをPRにまとめる。current-head CI成功後の最終full review等はCommander Policyに従う。このP1だけで#567/#297/#315をcloseしない。

既存testの対応（所在は準備baseで確認。実装コードhead `7412384` のverifyでworkspace testsが成功した。新headでは未確認）:

- `view.rs`: `zoom_scales_the_painted_row_height`、`table_mouse_hit_testing_uses_the_clicked_cell_fragment_y`、`opening_the_find_bar_pushes_rows_down_and_shrinks_the_viewport_by_its_reserved_height`、`caret_and_scrollbar_geometry_match_the_shrunken_viewport_while_the_find_bar_is_open`、`file_drop_on_settings_screen_returns_to_editor_without_discarding_dirty_text`、`height_anchor_recompute_resyncs_a_still_tracking_inertia_coast`。
- `view/reading_mode.rs`: `a_wrapped_long_line_keeps_its_reading_position_across_mode_switches`、`reading_real_down_keys_cross_tables_and_code_in_a_narrow_viewport`、`zero_movement_wheel_events_keep_the_editing_to_reading_anchor`、`zero_movement_wheel_events_keep_the_reading_to_editing_anchor`、`reading_frames_are_still_painted_and_measured`。

## 実装担当への渡し方と復旧

製品・testは通常担当Claude Code（`claude-sonnet-5-5`）へ引き継ぐ。初回準備時点の本PRでは起動しなかった（過去の経緯。初回依頼はその後投稿され、実装・verifyは上記のとおり完了している）。開始を指示されたCommanderはdefault branchの実行手順を再読し、open/same-repository/current exact head、write権限、重複worker、current main/競合/resourceを取り直す。本書をbranchから読み、上記移動・不変条件・検証条件を含む依頼をPR Conversationへ一度投稿する。

初回依頼の機械項目（履歴。後続の修正依頼の値とは別）は `AADW_ACTION: implement`、`AADW_FAILURE_CLASS: initial`、`AADW_ROOT_CAUSE: issue-567-p1-render-initial`、`AADW_EVIDENCE: https://github.com/hide212131/hane/issues/567`。`AADW_TARGET_HEAD`には投稿直前の40桁headを使う。マーカーとメンションは実行手順の正規templateに従い、準備commitや古いSHAを無条件に流用しない。

workerにはこの移動と直接関連する文書だけを許可し、shell/git/gh/test/build/lint/commit/push、次工程、モデル切替、merge、Issue closeを行わせない。検証はtrusted finalizer/Commander側で行い、`prepare`成功と実Sonnet使用、patch/finalizer、exact-head verifyを区別する。準備時点ではそれらの受理・成功を主張しなかった。実装コードhead `7412384` では、上記のrun 40051416483でprepare・実Sonnet使用・finalizer・exact-head verifyの成功を確認済みである（新headの結果ではない）。

revert単位はP1の一責務移動PR。新moduleに依存する後続を先に逆順で戻す。製品データ・永続形式・外部設定の移行はない。

準備対象の選択はJev `jev-1.14.0` / provider `typesafe` の `prepare_rf2a_render`（probability 0.87、confidence 0.74）。Commanderはcurrent main、既存契約、重複不在と文書準備の範囲を確認した。
