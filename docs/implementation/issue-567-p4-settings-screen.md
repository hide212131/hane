# RF2-A P4: 設定画面と設定操作をprivate moduleへ分離する

対象: Issue #567、親 #297。基準main: `593726cec4d651fc8ed0fbbfa2e2b878599199bd`。作成日: 2026-10-26 JST。

## 目的・開始条件・状態

EditorViewの責務別配置を継続し、設定画面の遷移・組立と既存設定の適用を`crates/ui/src/view/settings_screen.rs`へ機械的に移す。新しいControllerや状態所有者を作らない。P1 #583（render）、P2 #611（計測）、P3 #613（panel pointer）はmerge済み。P3 mergeは`d3e542179365da62798b8072857a7c90ecdc60af`。これらを再実装しない。

#298の保護契約、#300の該当領域の維持判断と受入済みの既存module境界を使う部分着手である。RF0全体や#305の残る全OS入力検証を一律に待たない。#306/#307/#308/#309/#368のsnapshot/geometry/I/O/cache/job寿命の意味変更をこの移動へ混ぜない。今回の対象と旧#301の残作業は#567で継続して追跡する。

観測時にopen PRはなく、実装workerのactive runもない。Release builds / Request releaseの実行は別の責務であり、この設計で起動・変更しない。実装依頼の直前にはcurrent main、関連Issue/PRと同じ対象のrunを再確認する。

本準備commitは仕様書だけ。製品実装・test/build/lint・GUI・性能測定は作成時点では未実施。設計の保存を受入完了と扱わない。

## 正確な移動範囲

親`crates/ui/src/view.rs`の次の2範囲を、元の順序で新moduleの`impl EditorView`へ置く。新moduleは`use super::*;`を使い、親へprivateな`mod settings_screen;`を追加する。親の巨大implには他の責務が残るのでimpl全体を撤去しない。

| 基準範囲（終端空行込み） | 内容 | bytes / SHA-256 |
| --- | --- | --- |
| 3715〜3744行、`toggle_autosave`先頭から`attach_ai_service`に付くdoc comment直前 | autosave操作2、settings accessor2 | 1,014 / `49d3a93b3e04db6f1ba87900b2ea0e2b66a4e468c5b189ddfb8a48da56ce84fa` |
| 3757〜4383行、`open_settings`先頭から`report_error`直前 | 設定遷移・画面・一般設定の16メソッド | 25,786 / `0bacb29a5d699a286998068f21661184af44c4f3eb2c3b82ade0e5df67f249a4` |

行番号・hashは基準SHAのsourceを識別するもの。実装時のbaseが変われば再抽出し、過去の範囲へcurrent codeを戻さない。間にある`attach_ai_service`のdoc commentと本体は親へ維持する。

| 移動するメソッド | 移動後の可視性 |
| --- | --- |
| `toggle_autosave` | 既存`pub(crate)`を維持 |
| `set_autosave` | 既存`pub(crate)`を維持 |
| `settings_open` | 既存`pub(crate)`を維持 |
| `settings_is_open` | 既存`pub`を維持 |
| `open_settings` | 既存`pub(crate)`を維持 |
| `close_settings` | 既存`pub(crate)`を維持 |
| `handle_settings_escape` | 既存`pub(crate)`を維持 |
| `select_settings_category` | 既存`pub(crate)`を維持 |
| `request_leave_settings` | privateのまま |
| `cancel_pending_settings_drop` | 既存`pub(crate)`を維持 |
| `leave_settings` | `pub(super)`へ（親の`handle_external_drop`） |
| `request_settings_category` | `pub(super)`へ（兄弟AI moduleの既存tests） |
| `set_file_context_menu` | privateのまま |
| `file_context_menu_status` | privateのまま |
| `settings_screen_element` | `pub(super)`へ（兄弟`render`） |
| `settings_screen_shell` | privateのまま、`#[inline(never)]`を維持 |
| `general_settings_content` | privateのまま |
| `display_and_save_settings` | privateのまま、doc commentと`#[inline(never)]`を維持 |
| `set_theme` | 既存`pub(crate)`を維持 |
| `set_sidebar_sort` | 既存`pub(crate)`を維持 |

3箇所の可視性調整は親とその子moduleに限り、crate/public APIの拡大はしない。必要な場合、rustfmtによるこの3宣言の改行・末尾commaだけを許容し、PRで具体的な差を示す。他の宣言・本文・コメント・属性・順序は維持する。import整理は必要最小限とし、親の他の子moduleが`use super::*;`で使うimportを消さない。

## callerと所有の境界

| 呼出元・残す場所 | 維持する接続 |
| --- | --- |
| `actions.rs` | settings入力時のaction判定、Escape、autosave切替と既存の設定を開く経路 |
| `view/render.rs` | settings時のearly returnと`settings_screen_element`呼出位置 |
| `view.rs`のexternal drop | `pending_settings_drop`とprotected状態を記録して`leave_settings`へ進む既存経路 |
| `view/ai_settings.rs`、`view/ai_prompts.rs` | ready route、dirty/leave確認、close/category確定、held drop取消。AI状態・入力Entity・保存処理・testsは各moduleに維持 |
| `view/app_update.rs` | `restore_app_update_local`と`app_update_card`を再利用。更新check/download/restartの実装は移さない |
| `view/sidebar.rs` | `set_sidebar_sort`の既存利用を維持し、並替えを別実装しない |
| `crates/app/src/main.rs` | `settings_is_open`と`attach_ai_service`の既存公開入口・意味を維持 |
| 親Viewと既存子moduleのtests | 名前・属性・本体・期待値・入力・module pathを維持 |

`SettingsCategory`、`Settings`、`settings_open/category/focus_handle/error/store_error`、`file_context_menu_state/busy/generation`、`pending_settings_drop/protected`、`ai_settings`、`ai_prompts`、`restart_ai_service`、theme・cache・height・sessions/work_folder等の型/field/初期化/所有者は親へ維持する。`store_settings`は他のcallerも持つ既存共通helperとして親に残し、設定保存形式やエラー経路を変えない。`settings_layout_is_narrow`とその既存testも親に残す。

## 維持する不変条件と境界

| 場面 | 維持する現在の処理 |
| --- | --- |
| open | 本文IME、filter/rename composition、rename取消のguardを先に評価。component初期化、AI selection reset、filter blur、settings session開始、OS登録状態の問合せ、local更新復元、focus/notifyの順を保持 |
| OS状態の遅い結果 | 開いたsettingsと一致するgenerationだけ受理。busy/conflict/unsupported判定、実行順、error処理、spawn/detachを維持 |
| close | settings/categoryとAI sessionの終了、busy解除/generation更新、本文focus、notifyの後でheld dropをtake。protected状態をtakeして既存`apply_external_drop`へ一度渡す順を保持 |
| Escape・カテゴリ | AI入力がEscapeを所有する場合のreturn、dirty確認と保存/破棄/継続、ready route消費順を維持。category確定時のheld drop取消とpending leave取消を省略しない |
| shellとstack | bodyを作ってからshellを組む。Windowsの1 MiB UI stack対策の2つの`#[inline(never)]`とhelper境界を保持。ひとつの巨大render関数にinlineしない |
| 幅・描画 | narrow判定の入力、一般ページだけの縦scroll、AIページのoverflow/min-height、navigation配置、element ID/debug selector/listener順を維持 |
| theme | 同じ値ならreturn。設定値/解決theme/component同期/cache clear/height再構築/保存/notifyの順を維持し、geometry正本や無効化規則を変えない |
| autosave・sort | 同値no-op、autosave切替は既存debounceを使い開始済みwriteをcancelしない。sortは既存treeをin-placeで並替えて保存しrescanしない。session/選択/dirty本文を維持 |
| 保存失敗 | 既存store helperを使い、起動中の適用と永続化の成功を混同しない。設定保存エラー表示を維持 |

外部folder/file dropの受理規則、未保存文書の保護、更新による再起動、AI接続/プロンプトの仕様、OS登録内容を変える機能追加はしない。既存不具合を見つけた場合も機械移動へ無断で修正を混ぜず、観測と原因を分けて報告する。

## 実装と比較の受入条件

1. 製品差分は親のmodule宣言と2範囲削除、新moduleへの20メソッド追加、上記3可視性調整に限る。型・field・caller・共通helper・tests・AI/update実装・外部公開APIは不変。各メソッドの定義は一つだけとする。
2. 実装時baseの各範囲と、移動先の対応範囲を比較する。比較用コピーで3メソッドの可視性を戻し、宣言改行が実際に変わった場合はこの3宣言に限り改行と末尾commaを含めbaseの正確な宣言へ戻す。外側の区切り空行だけを除き、本文・コメント・復元signature・属性・順序がbyte一致すること。全空白削除、本文やコメントの正規化で差を隠さない。
3. 二つの`#[inline(never)]`、原文コメント、独立したframe境界が残ること。移動とimport/可視性・宣言整形をPRで区別する。signatureの意味、task寿命、処理順、型名解決を変更しない。
4. `docs/refactor-execution-plan.md`へP4の旧→新、親に維持する理由、後続境界と実施/未実施・証拠参照を短く追記する。詳細仕様を複製しない。本仕様の作成時点の履歴と現在の状態を区別する。

## 検証計画

既存testを移動・削減・期待値変更しない。以下は現行assertを持つ代表例であり、今回の実行結果ではない。

| 対象 | 既存testと主張 |
| --- | --- |
| 基本遷移・幅 | `settings_screen_replaces_editor_and_escape_returns_to_it`、`narrow_settings_keep_general_controls_inside_the_window`、`settings_layout_stacks_only_when_content_would_be_squeezed` |
| 入力・drop | `ctrl_tab_does_not_switch_tabs_while_settings_is_open`、`empty_file_drop_on_settings_screen_is_ignored`、`file_drop_on_settings_screen_returns_to_editor_without_discarding_dirty_text`、`external_open_does_not_close_an_open_settings_page` |
| AI接続のdirty遷移 | `escape_prompts_before_discarding_a_dirty_ai_settings_draft`、`switching_category_from_a_dirty_ai_settings_draft_targets_the_clicked_category` |
| AIプロンプトのheld drop | `a_drop_behind_dirty_prompts_is_applied_once_after_discard`、`a_folder_drop_held_while_unsaved_is_refused_after_the_session_is_saved`、`continue_editing_cancels_the_held_drop`、`edits_made_during_a_save_then_leave_keep_settings_and_the_held_drop`、`an_edit_between_save_completion_and_route_consumption_keeps_settings` |
| Windows stack | `ai_settings_render_with_windows_stack_budget`と通常Windows起動。unit成功で実機起動を代替しない |
| sidebar順序 | `sidebar_sort_menu_updates_order_and_resorts_without_a_rescan`、`sidebar_sort_menu_stays_visible_and_clickable_at_minimum_sidebar_width` |

同headの通常/all-featuresを区別し、current-headの両OS CI、workspace外vendorの既存対象、通常Releaseを確認する。

```sh
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --release --locked -p hane
```

trusted workerが実行できない検証は未実施として返し、正規verifyとcurrent-head CIを観測する。current-head CI成功前に高コストなreview/GUI検証を始めない。必要な検証が既存CIにない場合は不足を明示して補う。P2で記録したtiming-probe単独Clippyのbaseline失敗を本P4で修正・成功扱いしない。

### macOS / Windowsのfocused native GUI

実際の候補head/base、通常構成の検証アプリ、実PID・実行パス・隔離stateを記録し、現在のGUI担当・占有状況を確認してから正規経路で実施する。過去PRの順番を固定したまま流用せず、current queueへ接続する。Windowsは接続先アプリから直接操作し、Mac経由のVM操作は使わない。

| scenario | 必須の観測 |
| --- | --- |
| G1 本文→設定→本文 | dirtyな日本語本文・caret/selectionを保持。戻るbutton/Escape後に本文focusへ復帰し、通常編集とundoが成立。本文IMEとfilter/rename composition中の拒否も確認 |
| G2 狭幅とカテゴリ | 一般/AI/AIプロンプトと戻る導線が見え、狭幅でもtheme/autosave cardがwindow内にある。Windowsでは切替を繰り返してstack overflow/終了しない |
| G3 一般設定の適用 | 隔離stateでtheme/autosave/sortを切替。描画と選択表示、保存エラーなしの再起動後の値、sort後の選択/expanded/dirty内容を確認。終了時に検証用変更を戻す |
| G4 dirty設定と離脱 | AI接続またはプロンプトの未保存編集で戻る/カテゴリ切替を要求し、確認から継続/破棄で正しい遷移。設定入力のIME中Escapeは入力側に留まり、本文へ漏れない |
| G5 設定中のdrop | disposableなファイル/フォルダで、dirty設定中のdropが保留され、継続で取消、破棄後に一度だけ受理。dirty/saving文書を持つfolder dropが保護され、本文内容を失わない |
| G6 一般画面の状態表示 | Windows連携状態/エラー欄/更新cardが表示され既存の状態を反映。新しい更新download/restartやOS登録変更をこの配置確認のために追加実行しない。遅い結果のguardは既存source/関連testsと照合 |

未実施/fail/blocked/unknownをpassにしない。操作・画面取得の失敗は`docs/gui-test-recovery.md`に沿って復旧し、必要なシナリオを再実行する。P4は処理量/アルゴリズムを変更しないため新しい性能再測定は予定しないが、通常Release/Windows stack条件とnative GUIを免除しない。差分が前提を超えれば別の意味変更として分離して再判断する。

## 残作業・対象外・復旧

settingsの局所配置受入と、#567全体の受入は区別する。height/cache共有helper、初期化・非同期I/O要求接続、関連testsの配置を含む残る棚卸しは#567で継続し、移動/処理済み/根拠付き維持/別担当の意味変更を確定する。P4だけで#567/#297をcloseしない。

意味変更、機能追加、bug fix、最適化、設定形式変更、AIサービス所有変更、新しいruntime/trait/Manager、全体整形、test移動、Cargo/vendor/workflow/保護パス変更は対象外。戻す場合はこの一責務のPRをrevertし、新配置へ依存する後続がある場合は逆順に戻す。製品データ移行・外部設定変更は不要。

選定記録: Jev `jev-1.14.0`は`design_567_p4`を選択（probability 0.98 / confidence 0.97）。候補はP4、RF4追加調査、RF0追加調査、待機。CommanderはP3 merge、current main、open PRなし、caller/可視性/stack境界と既存assertを確認した。
