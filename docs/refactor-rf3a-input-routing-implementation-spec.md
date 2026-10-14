# RF3-A / Issue #304：入力先判定の集約・実装仕様

作成日: 2026-10-12 JST  
対象: [Issue #304](https://github.com/hide212131/hane/issues/304)  
親計画: [refactor-execution-plan.md](refactor-execution-plan.md)  
状態: 設計を保存した段階。製品実装、Rustテスト、実画面確認、独立レビューは未実施。

## 1. 次に進める作業と開始条件

**#302の完了後は、#304「入力先判定の集約」を進める。** #302のP1〜P5は受入済みで、最後のPR #475はmerge済み。今回の調査mainは `c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e` であり、文書ブランチ作成直前にも再確認した。完了済みの#302や#303を再実装しない。

#304を選ぶ理由は、既存計画のRF3に対応し、本文・フィルタ・名前変更に加えて増えた検索欄やAI画面の入力抑止を、同じ修正先から確認できるようにするためである。#305はこの境界を使って短い入力欄の内部を整理できる。#306は別の解析・表示の整理であり、#304全体の先行条件にしない。今回もリスト挙動、座標、解析、保存の変更は混ぜない。

#301はclosedだが `state_reason=not_planned` であり、全UI分割が受入済みという根拠にしない。一方、調査mainには `view/inline_rename.rs`、`view/sidebar_filter.rs` が存在し、文字編集と名前変更の処理が分かれている。この受入済みmainの配置を使い、#301全体の再開や全Viewの分割を新しい開始条件にしない。#298の対象契約、変更前の関連テスト、今回の操作表を確認して部分着手する。

仕事の目的と受入条件はIssue、具体的な接続と移行は本書、実装差分と検証結果は実装PRを正本とする。既存のADR-0007・0013・0029の責務を変更しない局所整理なので、新しいADRは作らない。レビュー・mergeの運用は開始時のAGENTSとCommander Policyを参照し、本書に複製しない。

### 1.1 調査したファイル

以下のblobは上記mainのもの。行番号は固定の指示にせず、シンボルと比較元commitを使う。

| ファイル | blob | 確認した責務 |
|---|---|---|
| `crates/ui/src/actions.rs` | `33ebc687f288ef04d178382f5221f2a2984322f1` | キー登録、共通settings guard、通常編集・特別なキーの入力先分岐。 |
| `crates/ui/src/input.rs` | `15e5abccb1bc3887a27bc2595e661c79dd00f503` | EditorViewのplatform文字入力8メソッド、短い入力欄の描画・登録。 |
| `crates/ui/src/capture.rs` | `76e2be44d424730b0f6ea955118a1d42cb3cfd23` | 本文の入力handler登録と描画時の入力計測。 |
| `crates/ui/src/view/sidebar_filter.rs` | `4ea6576143e22083be6fd93a689088d667c02e2b` | filterの文字列・選択・composition・focus、検索も含む既存判定。 |
| `crates/ui/src/view/inline_rename.rs` | `2b6982707b56190b59f67643d52009898d21b4ec` | renameの文字編集・composition・pending・ファイル操作。 |
| `crates/ui/src/view.rs` | `aa95406b5979f276a0e171e5626f366ac1af21a2` | module接続、Editorへのdispatch、list intent、after_input、文書と表示状態。 |
| `crates/ui/Cargo.toml` | `9cae10b8dde87595726cacff932ae0431802c6a7` | packageは `hane-ui`。GPUIの既存test-supportを利用できる。 |

### 1.2 並行PRとの境界

調査時のopen PRは#471（リスト終了の設計）と#472（#460の製品修正）。#472の取得したheadは `72421e6da722ca29d1e7286ee4724ced278011c8`。PR本文中の別headを現在値と取り違えない。

#472の変更ファイルは `crates/editor/src/lib.rs`、`crates/markdown/src/list_editing.rs`、`crates/ui/src/view.rs`、`docs/adr/0029-source-first-list-editing.md`。#304はリストの計画生成、記録付き置換、原文の区切り方を変更せず、受入済みの `apply_list_edit_intent` を呼ぶ。#472の作業ブランチには書き込まない。

P1の通常キー集約は独立して始められる。Enter/Backspace接続を受け入れる前に、#472の最新状態とmainとの差分を再確認する。同じhandlerやテストを実際に変更中なら、その最小範囲だけ順序を調整する。未受入のリスト修正を#304へ丸ごと取り込んだり、今回の都合で#472をmergeしたりしない。#472でのWindows GUI免除は#460固有であり、#304には転用しない。

## 2. 今回の目的と非対象

入力した文字や押したキーを、どの編集欄へ渡すかを一つの場所で決める。この処理を入力の振り分け（input routing）と呼ぶ。**文字列をどう編集するかではなく、どこへ渡すかを整理する。**

本文は既存のEditor、フィルタとrenameは各既存module、本文検索・文書内検索・AI画面の入力は各既存の入力部品が引き続き所有する。独自のInputState、汎用trait、Manager、イベントバス、文書全体の状態機械を作らない。新しい永続状態、focusの複製、履歴、generationは追加しない。

次は非対象とする。

- #305の文字列・selection・UTF-16変換の共通化、filter/renameの部品置換。
- #237のサイドバー移動、新しいショートカット、Tabの一般的な挙動への変更。
- Enter/Backspaceによるリスト構造、Undo単位、IMEモデル、保存・rename規則の変更。
- 検索・AIの新機能、AI接続・prompt・結果適用の変更、画面の全面再配置。
- layout・caretの座標計算、キャッシュ・解析・非同期処理の寿命、性能最適化。

既存コードの名前と実際の意味が違う箇所を、移行時に黙って意味変更しない。バグ修正が必要と判明した場合は、再現例と変更する契約を分離する。本Issueのために新たなバグを正しい期待値として固定しない。

## 3. 現行コードから確認した重要な差

### 3.1 「フィルタにfocus」の既存関数はフィルタ専用ではない

`sidebar_filter_is_focused()` は `sidebar_filter_focused || content_search_input_is_focused() || content_search_results_focused()` を返す。一方、`sidebar_filter_selection()`やfilterの書換えは、生の `sidebar_filter_focused` を確認している。

従って、「この関数がtrueならfilterの文字列を編集してよい」という置換は不可。新しい判定内部では生のfilterフラグと検索のフラグを分けて読む。既存の広い条件は、必要な互換動作として明示的に計算する。

### 3.2 キーとplatform文字入力の順序が同じではない

通常の編集キーは、検索・AI等の抑止を先に確認した後、filter→rename→本文の順で選ぶ。`EntityInputHandler`の通常の文字入力はrename→広いsidebar条件→本文であり、`selected_text_range`だけはrename→生のfilter→本文である。

現行の合成状態を含めて同等性を検査するため、入口の種類を明示して一か所で定義する。一つの優先順へ乱暴に統一しない。同時成立が実操作で生じるかはfocus遷移テストでも確認するが、「起きないはず」として既存の分岐を削らない。合成状態での現行結果を、利用者向けに望ましいfocus仕様と宣言することもしない。

### 3.3 何もしないことと、次の入力先へ回すことは違う

renameがpendingのとき、`replace_inline_rename_text`等は本文へ回さずに止まる。空選択のCopy/Cut、filter/renameでのUndo/Redoも本文へ渡さない。

入力先を一度選んだら、その処理がfalseや空結果を返したことを理由に別の入力先へ進めない。本文内のlist plannerが `NotApplicable` を返して通常編集へ戻る場合は、**同じ本文の内部処理**として例外的に既存どおり残す。

## 4. 配置と具体的な境界

新規製品ファイルは `crates/ui/src/view/input_routing.rs` とする。`view.rs`にprivateな `mod input_routing;` を追加する。必要な型だけを `pub(crate) use input_routing::{...};` で `actions.rs`/`input.rs`から参照させる。crate外の公開APIは増やさない。

| 場所 | 変更後の責務 |
|---|---|
| `actions.rs` | 既存actionの宣言、キー登録、listener、共通settings guard。対象actionは新しい振り分けメソッドを一回呼ぶ。 |
| `view/input_routing.rs` | 一時的な判定材料、入口ごとの入力先解決、actionの対象別dispatch、特別なキーの順序、登録可否の判定。 |
| `input.rs` | GPUI traitの実装、既存の引数・戻り値変換、対象別の既存処理への接続、描画。優先順位をここに再実装しない。 |
| `capture.rs` | 既存と同じ条件の判定結果を使ってhandlerを登録。計測本体は変えない。 |
| `sidebar_filter.rs` / `inline_rename.rs` | 文字列、selection、composition、個別の編集、rename pendingとI/O。入力結果や通知を維持する。 |
| `view.rs` | module宣言と必要なcrate内再export。本文のdispatch/list/after_input、mouse処理、共有描画データ生成は維持する。 |

`sidebar_filter_is_focused`は複数機能の状態をまとめるため、その**既存メソッドの定義**を最終段階で `input_routing.rs`へ移す。既存名と戻り値は維持する。新しい判定からこの旧メソッドへ戻る循環参照を作らず、生のフラグから一度だけ計算する。filterの文字編集メソッドや、単にrename状態の有無を返す `inline_rename_active` は所有moduleに残してよい。

公開範囲を広げるのは、新しい入口メソッドと渡す小さなenumだけに限定する。EditorViewの状態フィールドをpubにしない。子moduleから親の非公開項目を使えるRustの規則を利用する。必要な新しい補助型・補助関数は原則private、テストはその子moduleに置く。

### 4.1 新しい入口の形

次は新設する具体的なinterface案であり、現存APIではない。名前の微調整はよいが、対象と戻り値の意味は保つ。未使用variantを先の段階のためだけに先行追加しない。

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalInputTarget {
    Body,
    SidebarFilter,
    InlineRename,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoutedInputAction {
    Backspace,
    Delete,
    Horizontal { right: bool, extend: bool },
    LineBoundary { end: bool, extend: bool },
    SelectAll,
    Copy,
    Cut,
    Paste,
    Vertical { down: bool, extend: bool },
    DocumentBoundary { end: bool, extend: bool },
    Undo,
    Redo,
    Newline { shift: bool },
    Indent { outdent: bool },
    StepFileTab { previous: bool },
    FindMatch { previous: bool },
    Escape,
}
```

`EditorView::handle_routed_input_action(action, window, cx) -> ()` がキー操作を受ける。通常操作では副作用なしで対象を決め、同じ同期callback内で実行する。clipboard読取・書込を挟んでも、Cutの対象を取り直さない。

platform用には `input_target_for_text_callback() -> LocalInputTarget` と `input_target_for_selection_query() -> LocalInputTarget` を用意する。登録側には `body_input_capture_enabled() -> bool` と、既存の短い入力欄のcaret判定と同じ意味のqueryを用意できる。これらの判定本体は同じmoduleに置く。

判定材料は既存のrename有無、生filter、検索入力、検索結果、文書内検索入力、AIのキー抑止などのboolだけを持つ小さな値にする。本文・path・promptを複製せず、EditorViewフィールドとして保持せず、awaitやTaskへ渡さない。settings guardは既存のmacroの一か所を保ち、通常キーごとに複製しない。別に定義されている `OpenAiSelection` の入口・キー・内部guardは維持する。

## 5. 判定表

表のFは生の `sidebar_filter_focused`、Rはrename状態の存在、SI/SRは本文検索の入力/結果、Dは文書内検索入力、Aは `ai_selection_blocks_editor_keys()` を表す。Wは既存の広いsidebar条件 `F || SI || SR`。これらはコード内の略語名を強制するものではない。

| 入口 | 先に止める条件 | 入力先・扱い |
|---|---|---|
| 通常の文字編集・clipboardキー | 共通settings guard。その後 `SI || SR || D || A` | Fならfilter、次にRならrename、それ以外は本文。 |
| 本文専用の選択上下・文書端・Undo/Redo | 上と同じ。その後FまたはR | 本文だけへ渡す。短い欄で未対応ならそこで終了。 |
| platform文字入力7メソッド | 既存のhandler登録とfocusの契約を維持。新しい画面guardを一括挿入しない | R→W→本文。 |
| `selected_text_range` | 同上 | R→F→本文。Wへ広げない。 |
| 本文InputCaptureの登録 | 現行どおりR、W、Aのいずれかなら登録しない | 他のとき既存と同じfocus handle、boundsで登録し、計測を実行。 |
| 短い入力欄のcaret描画 | 現行の `focus_handle.is_focused(window)` と `R || W` | 描画判定のみ。本文の入力許可と同一視しない。 |

platformのWは互換性の記録であり、検索のテキストをfilterに転送する新経路ではない。検索やAIの入力部品は各自のhandlerで処理する。単体テストでEditorViewへ直接callbackを呼んだ結果と、実画面でどのhandlerが登録されるかを別に確認する。

`InputCapture`には現行でDの明示guardがない。Dを追加するだけでも登録や計測の動作を変えるので、本Issueでは条件を勝手に増やさない。既存のfocus handleによる分離を実画面で検証する。late unmarkやcomposition終了の経路もあるため、全platform callbackへ一律の `is_focused` guardを追加しない。

## 6. 通常キーと本文処理の操作表

全操作は、対象を解決してから既存のメソッドへ渡す。文字列の編集アルゴリズムや通知処理を新moduleへコピーしない。

| 操作 | filter / rename | 本文 |
|---|---|---|
| Backspace | 対象の `backspace_*` | `apply_list_edit_intent(Backspace)` がfalseのときだけ `dispatch(Backspace)`。 |
| Delete | 対象の `delete_*` | `dispatch(Delete)`。list intentを新しく追加しない。 |
| 左右、Shift+左右 | 現行のmove/select helper。選択方向とgrapheme境界を維持 | `MoveLeft/MoveRight { extend }`。 |
| Home/End、Shift付き | 現行のmove/select helper | `MoveToLineStart/MoveToLineEnd { extend }`。 |
| SelectAll | 対象のselect all | `EditorCommand::SelectAll`。 |
| Copy | その対象の選択文字列を取得 | `editor().selected_text().ok()`。 |
| Cut | 同じ対象から取得し、clipboardへ書いてから対象のdelete | clipboardへ書いてから `dispatch(Backspace)`。list-aware Backspaceへ変えない。 |
| Paste | `replace_*_text(None, &text, cx)` | `dispatch(EditorCommand::Insert(&text), cx)`。`insert_text`へ置き換えない。 |
| Shift+上下 | 処理せず終了 | `move_vertical(down, true, window, cx)`。 |
| 文書先頭/末尾、Shift付き | 処理せず終了 | 既存の `MoveToStart/MoveToEnd { extend }`。 |
| Undo/Redo | 処理せず終了。新しい短文履歴は作らない | 既存Editorの履歴を使う。 |

Copy/CutはNoneまたは空文字ならclipboardも本文も変更しない。Pasteはclipboardから文字列を取れない場合に何もしない。filter/renameのCR/LF除去は既存helperに任せ、本文の改行やsource bytesへ適用しない。

入力先を決めるguardを `EditorView::dispatch` や `Editor::dispatch` の内部へ押し込まない。これらにはキー以外からの正当な呼出元があり、その意味まで変わるためである。`after_input`を新しい共通finallyとして呼ばず、既存の対象別処理が行う回数を維持する。

## 7. 特別なキーの順序

これらは通常の編集キーのguardだけでは表せない。**既存の短絡評価順と副作用を新moduleの対象別メソッドへ移す。** 判定のために `handle_*` や `dismiss_*` を事前呼出しし、その後もう一度実行してはならない。

特別なhandlerがfalseを返して次へ進むとき、後続の判定材料はその呼出し後に取得する。呼出し前の状態値を使って後のfocusを推測しない。入力先が決まった後は取り直さない。

### 7.1 Enter / Shift+Enter

Enterは、SRなら `handle_content_search_enter` を一度呼んで終了、SIまたはDならローカル処理せず終了、Aなら `ai_selection_handle_enter` を一度呼んで終了する。その後F→R→本文の順。

filterではcompositionがあればcommitだけ、なければ何もしない。renameではcompositionがあればcommitだけ、それ以外は `confirm_inline_rename`。本文ではIMEがないときだけ既存list Enterを試し、未処理なら `insert_newline`。

Shift+EnterはDまたはAなら終了、SI/SRでも終了する。その後はfilter/renameと同じ確定処理、本文は `ListEditIntent::ShiftEnter` の未処理時だけ `insert_newline`。文書内検索の入力部品が行う前の一致への移動を親で二重実行しない。

### 7.2 Tab / Shift+Tab

最初に `ai_selection_handle_tab(forward, window, cx)` を一度呼び、trueなら終了する。falseのときだけ後続を調べる。Dなら終了する。

TabはSI→結果へfocus、SR→入力へfocus。Shift+TabはSI/SR→入力へfocus。その後、FまたはRなら終了し、本文だけで既存Indent/Outdent intentを呼ぶ。失敗時に通常Tab文字を挿入する新しいfallbackを作らない。

### 7.3 上下キー

選択を広げない上下キーは、最初に `ai_selection_handle_vertical_arrow(down, cx)` を一度呼ぶ。true、またはその後のDなら終了。SRなら検索結果の選択を移す。SIでは上は終了、下は結果へfocusする。F/Rなら本文移動はせず、それ以外だけ `move_vertical(down, false, window, cx)` を呼ぶ。

Shift+上下は第6節の本文専用処理であり、この候補移動を呼ばない。AのboolだけでAI専用handlerの返り値を代用しない。

### 7.4 タブ切替・検索の次/前

NextFileTab/PrevFileTabはSI/SR/D/Aで終了し、F/Rまたは本文IME中でも終了する。それ以外で既存のnext/prevを呼ぶ。登録済みキーとmacOS/Windowsの修飾キーを変えない。

DocumentFindNext/PreviousはSI/SRで終了、W/Rで終了、`document_find_input_is_composing(window, cx)`がtrueなら終了する。それ以外で既存の次/前を呼ぶ。現在この分岐にないAのguardを足さず、Dのfocusそのものだけでも止めない。通常編集の共通guardを使い回して検索入力中のF3等を壊さない。

### 7.5 Escape

次の順を維持する。各段階で処理した場合はそこで終わる。

1. settingsが開いていれば `handle_settings_escape`。
2. `ai_selection_should_leave_on_escape()`ならAI画面から離れる。falseでもAなら終了し、本文のcomposition cancelへ進めない。
3. `dismiss_file_tab_context_menu`、次に `dismiss_tab_close_confirm`を、それぞれ一度だけ試す。
4. `content_search_should_leave_on_escape()`なら検索から離れ、次に文書内検索の同等条件を確認する。
5. Fならcomposition取消またはblur、Rならcomposition取消またはrename取消。それ以外でのみ `perform_cancel_composition`。

AI入力の変換を取り消したEscapeで本文の `after_input` を呼ぶと、まだ開いているAI画面を無効化し得る。この既存の防止分岐を削らない。検索/AI/settingsの `should_leave` の内部を新moduleへ複製せず、既存関数を呼ぶ。Escapeに関係しない画面の閉じ方を変更しない。

Open/OpenFolder/Save/SaveAs、検索画面を開く操作、Rename開始、ResetZoom、ToggleAutosaveは対象固有の仕事として既存入口に残す。元から一か所のsettings guardとrenameの操作中guardを維持し、通常の文字編集用routeへ無条件に置き換えない。

## 8. platform文字入力の8メソッド

入力先の解決だけを共通のqueryへ置き換え、trait実装と実処理は維持する。

| メソッド | 保持する内容 |
|---|---|
| `text_for_range` | 対象ごとのUTF-16範囲変換と `actual_range.replace`。エラー時のNone。 |
| `selected_text_range` | 第5節の専用条件。選択範囲だけでなくreversedを保つ。 |
| `marked_text_range` | 同じ対象のmarked rangeを返す。別の欄にあるcompositionを代用しない。 |
| `unmark_text` | filter/renameは既存commit。本文は `editor_mut().commit_composition()`。after_inputを追加しない。 |
| `replace_text_in_range` | 本文のrange指定なし・IMEなしだけ `insert_text`。それ以外は `commit_text`、既存のerror処理とafter_input。 |
| `replace_and_mark_text_in_range` | 元のrangeとnew_selected_rangeをそのまま対応する既存helperへ渡す。本文のerrorとafter_inputを維持。 |
| `bounds_for_range` | renameのrender stateとfilterの共有render state、shape、reversedに応じたcaret端点を維持。本文のcaret geometry・fallback boundsも維持。 |
| `character_index_for_point` | 対象ごとの既存helper。本文は現行selection.activeからのUTF-16位置を返す。新しいhit testに変えない。 |

`InlineRenameInput`はfilterにも使われる既存elementである。描画・shape・bounds・ `window.handle_input` の位置と順序はそのままにする。共有render state生成、`set_text_input_bounds`、UTF-16/grapheme helper、mouse処理の大規模移動は不要。描画データの組立と入力先判定は区別し、既存描画APIを再実装しない。

`InputCapture::paint`は判定式だけをquery呼出しへ変え、許可された場合に実行するhandler登録→mark_frame_painted→metrics更新の順・回数を保つ。入力計測だけを画面条件の外へ移す変更も禁止する。

## 9. テストを先に追加する

既存のテスト一覧と実行結果を変更前に保存する。新テストはまず旧handlerにも通る形で追加し、そのcommitと移行commitを分ける。既存helperやGPUI test-supportを使い、新しい依存や専用テスト基盤を作らない。

新しい純粋な判定のテストは `view/input_routing.rs` の子に置ける。既存のprivate action handlerを直接検査する必要があるテストは `actions.rs` の `#[cfg(test)] mod tests` に置くか、既存GPUIのaction配送経路を使う。テストのためにhandlerやViewの状態を外部公開しない。大きくなる場合だけ `view/input_routing/tests.rs` を分ける。

### 9.1 必須の自動テスト

| ID / 提案テスト名 | 具体的な確認 |
|---|---|
| R01 `key_target_matrix_preserves_existing_guards` | F/R/SI/SR/D/Aの全64組合せで通常キーと本文専用キーの対象を第5節と比較。settingsあり/なしも確認。R/F同時trueを除外しない。期待値は移行前の分岐から独立に記載し、新resolverを期待値にも呼ぶテストにしない。 |
| R02 `platform_target_matrix_preserves_entrypoint_contracts` | R/F/SI/SRの全16組合せでtext系とselection queryの違いを確認。検索がtrueかつF=falseの差も落とさない。 |
| R03 `short_field_actions_do_not_mutate_document` | 本文「本文B」、filter「絞込F」、rename「名前R」を区別し、選択・削除・Paste・Copy/Cutを各対象で実行。対象以外の本文bytes、revision、selection、dirty、Undo/Redo、active sessionが不変。filterの既存scroll reset等は許容。 |
| R04 `cut_uses_one_resolved_target` | 文字列取得と削除が同じ対象。一つの選択を一回だけ削除。空選択はclipboardを変更しない。本文Cutはlist plannerを呼ばない。 |
| R05 `pending_rename_never_falls_through_to_body` | pending=trueのrenameで文字入力、削除、Paste、Enter、Escapeを実行。個別の現行pending規則を保ち、本文は不変。false/trueを変更有無として解釈して本文へ回さない。 |
| R06 `unicode_ranges_survive_routed_input` | 本文/filter/renameで `A😀e\u{301}Z` のUTF-16範囲1..3がemojiのbyte範囲1..5に対応。そこを「羽」に置換すると `A羽e\u{301}Z`。byte caretは4、短い欄のUTF-16 caretは2。前後両方向の選択、左右・削除、composition commit/cancelも既存の境界規則で確認。 |
| R07 `enter_and_escape_only_finish_current_composition` | filter/rename変換中のEnterは確定だけ、renameのI/O開始は次のEnter。Escapeは変換取消、次のEscapeで離脱。本文は既存のIMEと記録付き編集を維持。 |
| R08 `search_and_ai_controls_do_not_double_dispatch` | 検索/AI入力と結果のEnter/Shift+Enter、上下、Tab/Shift+Tab、Copy/Paste/Undo。専用handlerを一回だけ呼び、本文や別の入力欄へ漏れない。文書内検索のShift+Enter/F3を二重移動させない。 |
| R09 `escape_preserves_overlay_order` | settings→AI→tab menu→close confirm→検索→文書内検索→短い欄→本文の順。AI変換取消のEscapeで本文のafter_inputを呼ばない。各dismiss関数の呼出回数も確認。 |
| R10 `body_list_and_history_pipeline_is_unchanged` | 本文の通常改行、非空リストのEnter/Shift+Enter/Tab/Backspace、選択置換、Undo/Redoで既存のsource/selectionを維持。#472が受入済みならその回帰ケースも既存テストのまま実行し、同じ修正を再実装しない。 |
| R11 `input_registration_and_bounds_keep_their_owner` | handler登録、短い欄のcaret、reversed選択のbounds、実際の文字入力先を区別して検査。focus切替で本文handlerが検索/AIのhandlerを奪わない。 |
| R12 `focus_transitions_keep_input_isolated` | 本文→filter→rename→本文、検索入力→結果→本文、文書内検索→本文、AI表示/取消、settings表示/離脱を実操作で追う。pending renameでfocus変更が拒否された場合も本文へ入力しない。 |

R06の期待値は設計上の例であり、テスト実行済みの結果ではない。範囲変換の既存ルールを変える必要が見つかった場合は、本体の移動と混ぜず切り分ける。

状態だけを直接作ったテストで判定表を検査し、さらに実際のfocus遷移と登録されたactionで同じ結果になることを確認する。合成状態が実際の遷移でも発生して別の欄を書き換える場合は、再現と原因を記録する。既存と今回の回帰を分け、必須契約の不成立を「たぶん起きない」として受け入れない。

## 10. 3つの実装PR

同じ大きな変更を並列に進めず、各段階を受け入れたmainから次のブランチを作る。中間PRは `Refs #304` とし、Issue全体を早期closeしない。

| 段階 | 変更すること | 終了条件 |
|---|---|---|
| P1：通常キー | 新moduleと最小enum/queryを追加。Backspace/Delete、左右と選択、Home/Endと選択、SelectAll、Copy/Cut/Pasteの14 actionを委譲する。まず既存handlerの契約テストを追加し、別commitで振り分けを変更。 | R01/R03/R04/R05/R06、対象の既存テスト、workspace/CI/review、変更範囲の実画面確認。特別なキーとplatform callbackはまだ変更しない。 |
| P2：特別なキー | 本文専用操作、Enter/Shift+Enter、Tab/Shift+Tab、上下、Escape、tab切替、検索の次/前を第6・7節の順序で委譲。 | R07〜R10とR12の対象部分。専用handlerが二度走らないこと、本文の履歴/list処理が同じこと。 |
| P3：platformと最終接続 | 8 callbackとcaptureの判定を新queryへ接続。広いsidebar判定の既存メソッド定義を新moduleへ移す。 | R02/R06/R11/R12、全操作表、両OSの必要な実画面、残る旧分岐と所有状態の点検、Issue全体の受入。 |

P1で触れる製品ファイルは `actions.rs`、`view.rs`のmodule宣言/再export、新規 `view/input_routing.rs`。テスト・証拠文書は追加できるが、editor/markdown/presentation、検索/AI本体、保存/rename本体、Cargo/vendor/workflowは変更しない。

P2も原則この範囲。P3では `input.rs`、`capture.rs`、`view/sidebar_filter.rs`が加わる。既存の共有render helper、文字変換、編集helperはそのまま再利用する。`view.rs`全体の整形やテスト大移動を混ぜない。

移行中に新旧の入口が共存する期間はPR単位で明示し、P3完了時には対象のキー/8 callbackが新しい判定を使うことを確認する。移行用feature flag、二重writer、旧ロジックへの実行時fallbackを追加しない。

## 11. 検証・受入・戻し方

### 11.1 変更前後の検証

current main、#304、関連PRのhead/差分、作業ツリーを取得し直す。repo指定toolchainを使い、OS・版・比較元SHA・候補SHA・実行コマンド・終了コードを記録する。本書のSHAを作業時の最新値と決めつけない。

```sh
rustc --version
cargo --version
cargo fmt --version
cargo test --locked -p hane-ui -- --list
cargo test --locked -p hane-ui --all-features -- --list
cargo test --locked -p hane-ui
cargo test --locked -p hane-ui --all-features
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
git diff --check
```

テスト一覧は変更前からの削除・ignore追加・名前変更と、今回追加した件数を分けて比較する。全件数だけの一致を証明にしない。通常構成をall-featuresの成功で代替しない。関連する現行macOS/Windows CIが実際に走ったことと、実checkoutがhead単独かmerge結果かを記録する。過去の#302/#303の成功は今回の候補の検証に数えない。

#302に既存format差分とrustdoc warningの記録があるが、今回のbaseでも同じか再確認する。失敗を成功とせず、原因不明の必須check failureを残したままmergeしない。無関係な全体整形を混ぜない。

### 11.2 実画面確認

**#302のファイル移動とは違い、本件は入力の振り分けを変更するため、実画面確認が必要。** macOSとWindowsの変更対象経路で実際のキー入力と日本語IMEを確認する。P1/P2/P3ごとにその差分へ必要な範囲を絞り、P3では全体を確認する。既存のGUI validatorまたは許可されたローカル操作を使う。新しい汎用GUI基盤を作らない。

最低限、本文/filter/renameへの日本語入力、変換確定/取消、選択置換、Copy/Cut/Paste、名前変更pending中の入力保護、検索/AI欄のEnter/Tab/Escape、本文のUndo/Redoと保存後の再読込を確認する。AI画面の入力隔離を確認するために実サービスへ本文を送る必要はなく、既存のfakeや送信前画面を利用してよい。ただしnative IMEの成功はfakeやunit testだけでは証明しない。

検証用の文書・work folder・アプリ状態を隔離する。利用者の既存文書やAI設定を変更しない。画面、操作、確認したsource、対象head、OSを対応付ける。未実施・blocked・unknownをpassへ変えない。別Issueに対するOS免除は転用しない。

### 11.3 完了条件と証拠

`docs/refactor-rf3a-p1-evidence.md`、P2/P3の同名規則の文書を実装時に追加できる。今回の設計保存では未実行の証拠文書は作らない。各PRにはbefore/afterの呼出関係、今回委譲したaction/callback、残る入口、公開範囲、状態所有者、対象SHA、テスト/CI/review/GUI、未実施項目を残す。

最終的に、通常キーの入力先優先順と外部画面の抑止が新moduleを正とし、特別なキーとplatform入口の相違が明示され、本文と短い欄の状態を複製していないことを確認する。`rg`等で対象メソッドの利用箇所を列挙し、機能内部の正当な状態確認と、残存する入力先の独自判定を区別する。名前の出現を機械的にゼロへする作業ではない。

#304の5つの受入条件を満たし、P1〜P3の必要な検証を受け入れてからcloseする。設計保存やP1成功だけで閉じない。#305へは `LocalInputTarget` と既存の対象別編集helper、#306へは変更していない本文list/Editor接続を引き渡す。

戻す場合は対象PRをrevertする。後続が依存する場合はP3→P2→P1の必要な範囲を逆順に戻す。保存形式・設定形式・利用者データは変えない。無関係な#472等を巻き戻さず、戻した候補で関連テストを再実行する。

## 12. 次の担当への最初の指示

最初は**P1だけ**を実装する。現在のmainと本書の差分、#304、open PRを確認し、通常の14 actionについて変更前テストを先に追加・実行する。次に `view/input_routing.rs` へ入力先判定と対象別呼出しをまとめ、既存の本文/helper処理・キー・settings guardを維持する。特別なキー、日本語IME callback、文字列モデルの共通化を同じPRへ足さない。

この設計作業ではGitHub上の現行情報とソースを静的に確認した。実行環境のPATHにCargo・rustc・Jev CLIは見つからず、製品コードのコンパイル、Rustテスト、Jevによる判断、実装worker、GUI、独立レビューは実行していない。設計を保存したことと、製品の受入を分けて報告する。

## 根拠資料

- [#302の完了記録](https://github.com/hide212131/hane/issues/302)、[#304の目的と受入条件](https://github.com/hide212131/hane/issues/304)、[#301の状態](https://github.com/hide212131/hane/issues/301)、[#306の依存](https://github.com/hide212131/hane/issues/306)。
- [調査mainのactions](https://github.com/hide212131/hane/blob/c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e/crates/ui/src/actions.rs)、[platform入力](https://github.com/hide212131/hane/blob/c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e/crates/ui/src/input.rs)、[入力登録と計測](https://github.com/hide212131/hane/blob/c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e/crates/ui/src/capture.rs)。
- [filter](https://github.com/hide212131/hane/blob/c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e/crates/ui/src/view/sidebar_filter.rs)、[rename](https://github.com/hide212131/hane/blob/c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e/crates/ui/src/view/inline_rename.rs)、[View](https://github.com/hide212131/hane/blob/c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e/crates/ui/src/view.rs)。
- [ADR-0029の本文編集境界](https://github.com/hide212131/hane/blob/c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e/docs/adr/0029-source-first-list-editing.md)、[並行するリスト修正PR #472](https://github.com/hide212131/hane/pull/472)。
- [Rust Reference: Visibility and privacy](https://doc.rust-lang.org/reference/visibility-and-privacy.html)（2026-10-12確認）。新しい子moduleとcrate内公開範囲の根拠。
