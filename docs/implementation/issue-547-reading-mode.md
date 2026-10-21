# Issue #547: 現在の表示を保つ閲覧モード

## 目的と適用条件

要求の正本は [Issue #547](https://github.com/hide212131/hane/issues/547)。既存のMarkdown描画を使い、本文を書き換えずに読めるモードを追加する。クリックや選択によって記法を開く処理も止める。別のプレビュー画面・HTML変換・分割表示は作らない。

調査基準はmain `f56a70b4f3145d98de70af9cdcdfc44f20793351`。PR #541（Issue #306 P2）が今回使用する表示入口を `BlockPresentationRequest` にまとめるため、その受入・main反映だけを製品実装の先行条件とする。旧9引数入口や互換wrapperへ機能を追加しない。#306の残りや#297全体の完了は待たない。

この文書の初回commitは実装用Draft PRの準備であり、製品実装・テスト・画面確認の完了を意味しない。現在の状態と実装対象SHAはPRを参照する。

## 利用者から見た仕様

### 表示とボタン

既存のタブ行の右端に、モードを切り替えるアイコンボタンを一つ置く。スクロールするタブ領域の外側で、縮まない領域を確保する。既存のファイルメニューボタンと同様に、本文のクリック処理へイベントを流さない。タブの選択・本文の選択範囲をボタン押下だけで動かさない。

| 現在のモード | ボタンの形 | ツールチップ・操作名 | 押した後 |
| --- | --- | --- | --- |
| 編集 | 輪郭線の目と丸い瞳 | 閲覧モードに切り替え | 閲覧 |
| 閲覧 | 右上に傾いた鉛筆の輪郭 | 編集モードに切り替え | 編集 |

Microsoftの[公式紹介](https://techcommunity.microsoft.com/blog/onedriveblog/introducing-markdown-support-in-sharepoint-and-onedrive/4512174)（2026-04-21）と[掲載画像](https://techcommunity.microsoft.com/t5/s/gxcuf89792/images/bS00NTEyMTc0LVNmdXVDag?revision=17)の目・鉛筆を参考にする。画像内の左右分割ボタンは今回追加しない。内部の正式なコンポーネント名を確認したとは扱わない。

既存のButton/tooltip/focusとテーマを使い、通常時・hover・focusで押せることが分かるようにする。絵文字は使わない。既存の同梱アイコンが形状を満たすなら再利用し、足りない場合だけ小さなSVGを `assets/icons/` に追加して `crates/ui/src/icons.rs::AppAssets` 経由で配布する。追加のパッケージや画像生成は不要。アイコンは16〜18px程度、押せる領域は既存の上部ボタンにそろえる。

本文の文字、行間、余白、配色、幅、倍率とMarkdownの対応範囲は今の表示を使う。閲覧中は、編集のために表示していた記法だけが閉じた状態になる。本文の入力カーソルと入力モード表示は出さず、文字選択の色は表示する。

### 状態の範囲

`EditorView` が `Editing / Reading` の列挙型を一つ持ち、初期値は `Editing` とする。モードは同じウインドウ内で共通にし、タブの切り替え、新規文書、再読込で勝手に編集へ戻さない。文書、各Editor、設定ファイルへ複製・保存しない。

閲覧中もクリック・ドラッグ・Shift選択・全選択・移動・スクロール・コピー・検索・倍率変更・既存のリンク操作を使える。コピーは既存の選択からのコピー仕様を使い、新しいHTML/見た目専用コピーは作らない。検索は元の文書を対象にするが、隠れたリンク先や記法に一致しても閲覧中は記法を開かず、該当ブロックへの移動と件数表示を維持する。

本文以外の検索欄、フィルター、ファイル名変更、設定の入力を止めない。ファイルの保存、既存の自動保存、外部変更の再読込も既存の経路を使う。閲覧切り替え自体では保存予約を増やさない。

## 変更する責務と入口

### 本文変更の禁止

`EditorView` に本文の編集可否を返す小さな共通メソッドを置き、次の実在する入口で確認する。入力先の判定と本文の編集可否は分け、既存の `InputTarget` を不要に増やさない。

| 入口 | 変更方針 |
| --- | --- |
| `view.rs::insert_text / insert_newline` | 閲覧中は本文変更前に戻り、`after_input` を呼ばない |
| `view.rs::dispatch` | 本文変更を伴うEditorCommandだけを拒否する。移動・選択は許可する |
| `view.rs::apply_list_edit_intent` | Enter/Shift+Enter/Tab/Shift+Tab/Backspaceによる置換前に止め、呼出側の通常入力へのfallbackでも本文を変えない |
| `actions.rs` のCut/Paste等 | Cutはクリップボードへの書込み前に止める。フィルター等の別入力先の編集は維持する |
| `view/input_routing.rs::body_input_capture_enabled` | 閲覧中は本文のOS入力先を登録しない |
| `input.rs` の `replace_text_in_range / replace_and_mark_text_in_range / unmark_text` | `InputTarget::Body` の分岐でも可否を検査し、登録解除前から遅れて届いた呼出しを拒否する。検索等の別入力先を止めない |
| `view/ai_selection.rs::ai_selection_replace` | 対象sessionの `replace_range_recorded` 直前で拒否する。通常dispatchを通らない直接書込みにも適用する |

本文の文字入力、削除、単語削除、改行、リスト編集、貼り付け、切り取り、Undo/Redo、AI置換をすべて止める。既存の `body_key_ready` を一律にfalseにして移動・選択まで止める変更はしない。`Editor` や `DocumentSession` の汎用変更API全体を閲覧専用にする設計は不要。

`capture.rs::InputCapture::paint` は現状、OS入力登録と `mark_frame_painted / record_frame_instrumentation` を同じ条件で実行している。入力登録の可否と描画計測の可否を分け、閲覧であることだけを理由に既存の描画計測を止めない。従来のrename・サイドバー・AI等に関する条件は維持し、本文の編集禁止から計測まで無効になる回帰を直接確認する。記録されなかったframeを処理量ゼロと解釈せず、新しい計測基盤は追加しない。

AI処理中や結果表示中に閲覧へ切り替える場合は、既存の `EditorView::close_ai_selection(window, cx)` を使い、状態のresetと本文へのフォーカス復帰を行う。処理世代の検査で遅れた結果を現在の文書へ適用しない。`AiSelectionState::reset` だけを直接呼んで入力先を失わない。閲覧中のAIからの本文置換は無効にし、切替前に取得した結果にも同じ条件を適用する。外部AI送信をテスト目的で発生させない。

調査基準では、タスクのチェック・表・日付バッジに独立したクリック編集入口はない。`context_menu.rs` はWindowsファイル関連付けであり、本文編集メニューではない。存在しない編集機能を追加せず、実装開始時に追加の直接書込みが増えていないか確認する。

### 日本語変換中の切り替え

本文でcompositionが存在する間だけ切り替えを無効にし、「文字の変換を確定または取り消してから切り替えてください」とツールチップ等で理由を示す。プログラム上の切替入口でも同じ条件を確認する。押せないボタンへのmousedownでフォーカスを奪い、自動確定・取消を起こさない。

変換確定または取消後は切り替えられる。切り替えの都合で未確定文字を削除したり、OSの変換を推測して二重確定したりしない。検索欄等のcompositionと本文のcompositionは区別する。実OSの動作は両OSで確認する。

## 表示と位置計算を一致させる

`BlockPresentationRequest` に編集時／閲覧時の表示方針を表す具体的な値を一つ渡す。`EditorView` のモードから作り、表示側に独立した可変状態を複製しない。別renderer、汎用context、隠れたglobal state、`forced_disclosure` の特殊値は使わない。

`line.rs` の `range_disclosure / block_context / disclosure_for_line` と `expected_block_disclosures` が同じ方針を使う。閲覧中はカーソル、選択、composition、検索のforced disclosureで記法を開かない。見えている文字のsource map・hit test・選択は今の仕組みを使い、選択位置を偽装して記法を隠さない。

`view.rs` の `cached_block / layout_around / neighbor_row_target / target_in_neighbor / neighbor_block_window` 等の描画と移動に使う全経路に同じ方針を渡す。描画だけを変え、隣のブロックへの移動計算が編集時の高さを使う状態を残さない。

`line.rs::row_element` と表行の描画では、本文caretを出す可否を明示的に扱う。`caret_input_mode=None` だけではcaret本体は消えないため、それだけで実装完了にしない。選択色は引き続き描く。

### 切り替えとキャッシュ・高さ

切り替え処理は本文の変更処理から分ける。`after_input` や文書を置換する `on_document_replaced` をそのまま流用しない。本文revision、未保存状態、Undo/Redo履歴、タブと選択を維持し、スクロールは既存の元文書位置を基準にする仕組みで保つ。

切替前の表示・レイアウト・行所有情報・caret geometry・測定済み高さが、新しい表示方針と混ざらないように更新する。切り替え直前の選択ドラッグや自動スクロールは止め、切替後は新しい表示の座標で選択できるようにする。通常のスクロール位置を先頭へ戻す実装はしない。

特に `active_height_disclosure` を閲覧中 `None` にするだけでは不十分。現行の `background_parse.rs` は選択が閉じる条件を `Some(empty range)` で判定し、`install_disclosure_heights_preserving_measurements` は `None` なら早期returnする。`install_heights` も通常は既存の実測高さを残す。表区切り行・コードの囲み記法が開いていたときの高さが残らないよう、モード変更をこれらの処理へ一貫して反映する。

背景の解析・高さ計算・画像読込が完了するときも、現在のモードと整合させる。sourceだけに依存する解析結果は再利用できるが、旧モードの高さや記法展開を現在値へ戻してはならない。既存のjob世代・revision・文書instance・disclosure検査を利用し、必要な表示方針の照合だけを足す。モードの往復で古い非同期結果が再び採用されるケースも確認する。

切替時に表示に関係する再計算は必要だが、巨大文書全体の同期parseやshapeを追加しない。既存の同期join上限、viewport単位処理、背景job数の上限を維持する。

## 確認すること

下表を同じ安定したcurrent headで確認する。既存テストの期待値を弱めず、今回の条件を直接確認するテストだけを追加する。`view/input_routing.rs`、`view/basic_editing_keys.rs`、表示・高さ・AIの既存テストの足場を使う。

| ID | 条件 | 期待する観測 |
| --- | --- | --- |
| R01 | 目→閲覧、鉛筆→編集、タブ切替、新規文書、再読込 | ウインドウ内のモードを維持し、明示操作だけで切り替わる。初期状態は編集 |
| R02 | 見出し・太字・リンク・画像・表・コード・引用・リストをclick/drag/Shift/矢印/全選択/検索 | 閲覧中は記法を開かず、選択色・コピー・スクロール・検索を使える。本文caret/入力モード表示なし |
| R03 | 通常文字、Enter、Backspace/Delete、単語削除、Tab、Cut/Paste、Undo/Redo、OS文字入力callback | 閲覧中は本文bytes/revision/dirty/履歴を変えない。Cutでクリップボードも変更しない。編集へ戻すと既存操作ができる |
| R04 | フィルター、文書検索、全体検索、ファイル名変更の入力 | 閲覧中も既存の入力と選択・コピー・貼り付けを使える |
| R05 | 本文日本語変換中の押下→確定または取消→切替、遅れたcommit/marked/unmark | 変換中は切り替わらず文字を失わない。切替後は遅れたcallbackで本文を変更しない。両OS実画面で確認 |
| R06 | AI処理中／結果表示中の切替、古い結果の完了・置換 | 閲覧中に置換できず、編集へ戻した後にも古い結果を復活させない。切替直後から本文の選択・コピーを使える。mockで確認し外部送信しない |
| R07 | 表区切り・コード記法を開いた状態から往復、倍率変更、幅変更、背景高さ計算中の往復、画像読込 | 不要な高さが残らず、見えている位置とhit test/選択が一致する。旧モードの完了処理で表示を戻さない |
| R08 | 未保存本文を持つ文書の切替、保存中の切替、外部更新の再読込 | 内容と既存の保存・競合処理を維持する。モード切替だけでrevision・保存予約を増やさない |
| R09 | 多数タブ・狭い幅・ライト／ダーク | ボタンに到達でき、本文やタブを押した扱いにならず、目・鉛筆と説明が表示される |

既存のworkspace tests（通常とall-features）、Clippy、macOS/Windows CIを確認する。通常ビルドで両OSの対象を絞ったGUI確認を行い、R01/R02/R05/R07/R09を画像と実操作で確認する。Windowsは既存の接続先 `papa-win` のChatGPTアプリから直接操作する。ショートカットを確認する場合はKeyhacのhookを切り、終了時に戻す。Mac経由VM操作は使わない。

表示処理量の確認は既存のfixtureと計測を使い、変更前mainと候補を同じ幅・倍率・文書で比較する。特に大きい文書で切替が全文同期処理を増やしていないことを、既存の解析／layout観測から確認する。新しい性能基盤や閾値の緩和は追加しない。未実施の実画面や計測を単体テストで代替したとは扱わない。

## 実装担当へ渡す範囲

主な対象は `crates/ui/src/view.rs`、`line.rs`、`input.rs`、`actions.rs`、`capture.rs`、`view/input_routing.rs`、`view/background_parse.rs`、`view/ai_selection.rs` と直接対応するテスト。必要なら小さな `view/reading_mode.rs` にモード・切替処理をまとめてよい。アイコンが必要なら `icons.rs` と `assets/icons/` に限定して追加する。

PR #541がmainに入ったことを確認し、そのmainを取り込んだ実装用PRのcurrent headへ一度だけ初回依頼を投稿する。依頼にはこの文書のbranch内パスとIssueの受入条件を含める。Jevが利用できない環境ではCommander Policy §2.9に従い、取得できていない応答を作らない。

製品コードと直接関連するテストは既定のClaude担当が変更する。workerはshell/test/build/lint/commit/push、次工程の起動、merge、Issue closeを行わない。trusted finalizer後の検証・必要なCI・current-head review・GUIは別に確認する。

保存形式、OSのファイル権限、設定永続化、新ショートカット、サイドバー開閉、統合タイトルバー、フッタ全体の再設計、#546のパネル幅、vendor/依存更新、`.github/`等の運用基盤は今回変更しない。
