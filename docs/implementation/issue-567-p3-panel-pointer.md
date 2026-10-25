# RF2-A P3: パネルのpointer操作と選択autoscrollをviewportへ配置する

対象: #567、親 #297。基準main: `48ff77240f89c8c53b13f273eaed49847b9f9ab5`。2026-10-25 JST。

## 目的と開始時の事実

巨大なEditorViewの責務別配置を継続する。P1 #583はrender、P2 #611は計測用implの分離としてmerge済み（P2 merge `796eb66abd11f8d20b31185f95062ef2c8359848`）。このP3は残るpointer/panel操作を既存の`view/viewport.rs`へ機械的に移す。新しいmoduleや状態所有者を作らない。

#298の保護契約と#300の該当領域の維持判断を継承する。RF0全体の完了待ちは不要。着手時にopenのRF実装PRはない。機能PR #604（リンク）、#609（フォルダopen）、#610（フォルダ内作成）のcurrent diffでは今回の10メソッドと`viewport.rs`を変更していない。settings/open/save接続の分離は#609と共有状態が多いため、このP3には含めない。実装直前にはcurrent mainと関連diffを再確認する。

準備commitは本仕様のみ。製品実装、build/test、GUI、性能測定は作成時点で未実施。以下の受入計画を実行済みの証拠として扱わない。

## 変更対象と具体的な配置

`crates/ui/src/view.rs`の`begin_sidebar_resize`先頭から、`refresh_caret_input_mode_for_keyboard_layout_change`に付くdoc comment直前までを移す。基準では6494〜6764行、10,761 bytes、終端空行込みのSHA-256は`7a4f0f1f8b5c6373ec780bf5e58bdaecda6a95fc45e6a7cf5181ab600dc95e3b`。行番号は基準SHAの識別であり、実装時baseが変われば再抽出する。

移動先は既存`crates/ui/src/view/viewport.rs`。既存implの後ろへ別の`impl EditorView`として追加してよい。既存`use super::*;`を再利用する。親の既存implはsettings/input-mode handlerとwidget組立を引き続き持つため、そのimpl全体を削除しない。

| 移動するメソッド | 移動後の可視性 | 移動範囲外の利用者 |
| --- | --- | --- |
| `begin_sidebar_resize` | `pub(super)` | 親の`sidebar_resizer` |
| `begin_editor_scrollbar_drag` | `pub(super)` | 親の`editor_scrollbar`、既存tests、兄弟`basic_editing_keys`内test |
| `begin_sidebar_scrollbar_drag` | `pub(super)` | 親の`sidebar_scrollbar` |
| `on_panel_mouse_move` | `pub(super)` | 兄弟`render`のlistener |
| `text_autoscroll_direction_for` | `pub(super)` | 親の既存tests |
| `set_text_autoscroll` | `pub(super)` | 親の`apply_row_click`、既存tests |
| `spawn_text_autoscroll` | privateのまま | 移動範囲内からのみ |
| `step_text_autoscroll` | `pub(super)` | 親の既存tests |
| `finish_panel_drag` | `pub(super)` | 兄弟`render`のmouse-up / mouse-up-out listener |
| `show_sidebar_scrollbar_briefly` | `pub(super)` | 兄弟`sidebar`のwheel listener、親の既存tests |

9箇所のprivate→`pub(super)`は親Viewとその子moduleの既存利用を維持する局所調整。crate/public APIへ拡大しない。本文・コメント・signatureの他の部分・順序・attributesを変えない。callerの位置、listener登録順と捕捉を変えない。新helper化、既存viewport methodsの再構成、test移動は行わない。

## 状態所有と維持する契約

型、field、初期化、定数、純粋helperは親へ残す。`SidebarResizeDrag`、`ScrollbarDrag`、`AutoscrollDirection`、`scroll_y_for_thumb_drag`、`sidebar_width_for_drag`、時間・edge閾値を移さない。`sidebar_resizer` / `editor_scrollbar` / `sidebar_scrollbar`の要素組立とkeyboard layout handlerも親へ維持する。座標・layout正本の変更は#307、cache/job寿命の再設計は#368の責務であり、この移動に混ぜない。

| 場面 | 不変条件・境界 |
| --- | --- |
| resize/scrollbar開始 | pointer起点、viewport/content heightのsnapshot、stop_propagation/notifyの位置を維持。editor scrollbar開始時のinertia取消、center取消、pending mode anchor解除の順を保つ |
| panel move | resize→editor thumb→sidebar thumb→本文selection端判定の順。panelがmoveを所有する条件と`event.dragging()`判定、変更時の通知を保持 |
| edge判定 | headerとfind bar予約高さを引く座標系、viewport高さ、狭いviewportで上下edgeが重なる場合の近い方への分岐を保持 |
| autoscroll開始/方向変更 | 同方向なら何もしない。activityのwrapping更新とtask起動、spawn_in/detach、待ち時間とupdate_in失敗時停止を維持 |
| tick | activity/方向一致だけ継続。move_verticalはselectionを延長し、source上のactiveが動かないと方向を解除して終了。document置換による取消と競合しない既存経路を保持 |
| mouse-up/out | `take()`と`||`の短絡を含む既存式をそのまま維持。text selection解除とautoscroll停止、通知、sidebar thumb表示延長の順を変えない |
| sidebar thumb寿命 | visibleとactivity更新、遅延task、同じactivityだけhideする条件、通知位置を維持 |

新しい状態、job owner、timer、callback、永続形式を追加しない。既存の挙動に疑問を見つけても、機械的移動へ無断で修正を混ぜず、観測と原因を分けて報告する。

## 検証と受入

1. 実装時baseから抽出した元範囲と移動先を比較する。9箇所の`pub(super)`だけをprivateへ戻した後、本文・コメント・signature・順序がbyte一致すること。各メソッドが一つだけ存在すること。製品差分は親の10メソッド削除と既存viewportへの追加、および局所可視性調整だけにする。
2. 親/兄弟caller、公開API、test名・属性・本体・入力・期待値・配置を維持する。既存の以下の回帰を同headで実行し、結果を残す。移動内容をなぞるだけの新testは追加しない。

- `starting_a_scrollbar_drag_cancels_a_live_inertia_coast_that_has_not_moved_yet`
- `editor_scrollbar_drag_preserves_document_end_badge_clearance`
- `sidebar_width_drag_respects_panel_limits`
- `sidebar_scrollbar_thumb_shows_on_scroll_and_hides_after_the_delay`
- `sidebar_scrollbar_element_is_hidden_at_rest_and_shown_during_a_drag`
- `text_autoscroll_uses_the_nearest_edge_when_zones_overlap`
- `drag_selection_autoscrolls_across_soft_wrapped_rows_in_one_source_line`
- `drag_selection_autoscrolls_down_while_held_near_the_bottom_edge_and_stops_on_mouse_up`
- `drag_selection_autoscrolls_up_toward_the_document_start_and_then_stops`
- `document_replacement_cancels_text_selection_autoscroll`
- `unresolved_mouse_down_does_not_start_text_selection_drag`
- sidebarあり/なし、行途中/行跨ぎ、soft wrap、日本語UTF-8境界の既存drag selection testsとfind bar開閉中のcaret/scrollbar geometry test

3. 通常/all-featuresを区別し、current-headのmacOS/Windows CIを確認する。

```sh
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --release --locked -p hane
```

P3はfeature配線・計測methodsを変更しない。P2で記録したtiming-probe単独Clippyのbaseline失敗を、このP3で修正・成功扱いしない。実行したcommand/OS/head/base/exit codeと未実施をPRで区別する。trusted workerがtest実行できない場合も未実施を成功とせず、正規verifyとcurrent-head CIで検証する。

4. pointer入力を扱うため、移動同等性とGPUI testに加え、macOS/Windowsのfocused native GUI証拠を計画する。検証アプリの実PID・実行パス・隔離stateと対象SHAを記録する。

| scenario | 観測する結果 |
| --- | --- |
| sidebar幅drag / 本文thumb drag | 境界でclampし、scroll停止後の意図しない慣性の再開や本文位置の跳びがない |
| sidebar thumb drag・wheelとhide | thumbが操作中に見え、mouseup後の遅延hideと再操作時の表示延長を確認 |
| 日本語＋soft wrap本文の上下edge選択drag | held pointerでもselection/scrollが進み、mouse-upとwindow外mouse-upで止まる。保存したsourceは不変 |
| find barあり・狭い高さ | header/find barを含むedge座標と重なり時の方向が正しく、本文選択が逆方向へ進まない |
| selection autoscroll後の文書切替 | 旧taskが新文書のselection/scrollへ適用されないことを確認 |

Windows GUIは接続済みWindows環境から直接操作する正規経路のみ。Mac経由のVM操作を使わない。GUI占有や操作失敗時は`docs/gui-test-recovery.md`に従って復旧する。必須scenario未実施をpassへ読み替えない。新しい性能最適化・処理量変更を含まないため、P3固有の性能再測定は予定しないが、差分が前提を超えれば分離して再判断する。

5. `docs/refactor-execution-plan.md`に旧→新対応、親へ残す理由、実行/未実行と結果参照、後続境界を短く追記する。詳細仕様を複製しない。残るsettings、height/cache、初期化・非同期I/O接続、tests配置の棚卸しは#567で継続し、P3だけで#567/#297をcloseしない。

## 対象外・復旧

意味変更、bug fix、geometry正本変更、状態寿命の再設計、最適化、機能追加、広範なpub化、新Manager/trait、既存test移動/削減、全体整形、Cargo/vendor/workflow/保護パス変更を含めない。PRを一単位でrevertする。新配置へ依存する後続がある場合は逆順に戻す。データ移行と外部設定変更は不要。

選定: Jev `jev-1.14.0`は`design_567_p3`を選択（probability/confidenceとも0.99）。候補は本P3、RF4追加調査、RF0追加調査、待機。CommanderはP2 merge/current main/open RFなし/関係する機能diffを確認した。
