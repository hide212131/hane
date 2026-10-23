# Issue #549: 左サイドパネル開閉の実装準備

## 対象と基準

対象Issue: https://github.com/hide212131/hane/issues/549
基準main: `f56a70b4f3145d98de70af9cdcdfc44f20793351`
実装branch: `work/issue-549-sidebar-toggle`

これはClaude実装待ちの準備文書。作成時点では製品コードとテストは未変更、製品の動作・GUIは未検証。要求と受入条件S01〜S05はIssue本文を正とする。実装依頼コメントにも、workerが外部URLを開かずに理解できる決定事項を渡す。

利用者はサイドパネルのアイコン・位置・ショートカットをChatGPTアプリに近づけたい。公式CommandsはCommand/Ctrl+BとCommand/Ctrl+Shift+Sを併記している。参考画像の表示はShift+Sだが、HaneのSaveAsとの競合を避けるCommand/Ctrl+B案について、利用者の了承を得た。

## 変更箇所と接続

| 箇所 | 今回の変更 |
| --- | --- |
| `crates/ui/src/actions.rs` | 既存の`command_actions!`へ開閉actionと`secondary-b`を登録する。ボタンからも同じ処理へ接続する。SaveAsの`secondary-shift-s`は維持する。 |
| `crates/ui/src/view.rs` | ウインドウ内だけの開閉状態を追加する。初期は現在と同じ表示。有効なパネルの有無と開閉状態から、描画、境界、有効幅、`main_column_left`、`content_width`、フッターの利用可能幅を揃える。 |
| `crates/ui/src/view/sidebar.rs` と既存アイコン登録 | 共通の開閉ボタンを用意する。単純な左ペインを表す角丸枠＋縦線のSVGを既存の読込経路へ追加できる。外部ライブラリは追加しない。 |
| `crates/ui/src/view/content_search.rs` | 開閉とFiles/Contentモードを分ける。明示的な本文検索actionはパネルを表示して既存の検索開始／focusへ接続する。 |
| `crates/ui/src/view/footer.rs` | `show_settings = work_folder.is_none()`のままでは閉じた時に設定入口が消えるため、パネル非表示時にも既存の設定入口を出す。 |
| WorkFolderのopen成功経路 | 明示的に作業フォルダを開き終えたらパネルを表示する。ダイアログの取消・失敗で表示状態を変えない。 |
| `README.md`、関連する既存テストmodule | キーと開閉を説明し、状態保持・入力先・座標の回帰を確認する。 |

設定ファイルや保存形式へ項目を追加しない。作業フォルダも本文検索パネルもない場合は開閉で空のパネルを作らない。設定画面のナビゲーションは変更しない。

## 配置と座標

32px四方のボタンに20pxアイコンを置き、編集画面のクライアント領域の左8px・上4pxで押せるようにする。表示中のボタンは1個だけ。

パネル表示時は先頭に40pxの固定行を追加する。現在の`work_folder_sidebar`を、40px行の下の残り高を使う`flex_1().min_h(0).overflow_hidden()`の内容枠へ入れる。既存パネルの`h_full().flex_none()`を40px行と直接並べて、合計高がウインドウ高を超えないようにする。

`work_folder_sidebar`と`content_search_sidebar`に渡す高さを`(window_height - 40).max(0)`にする。既存パネル内部の一覧／スクロールバーは同じローカル座標を使うため、`list_top`へ40pxを再加算しない。フィルターと名前変更の入力欄は`InlineRenameInput::prepaint`の実測boundsに従う。手動の座標補正を足さない。

パネル非表示時は`header_element`の`file_menu_button`より前へ同じボタンを置く。`tab_strip`の外の兄弟要素にし、タブのマウス処理で本文の選択を変えない。本文側のheader_heightは従来どおり40pxであり、本文の縦座標へさらに40pxを足さない。

非表示時の有効パネル幅は0。表示時は保存している`sidebar_width + SIDEBAR_RESIZER_WIDTH`。既存の幅自体は変更せず、閉じるときはパネルとリサイズ境界をともに描画しない。進行中のパネルdrag状態を残さない。

## 状態と入力を守る

開閉だけではwork folder、文書、session、各ScrollHandle、フィルター、検索InputState、検索結果を作り直さない。ファイル読込／フォルダscan／保存／文書解析を新しく起動しない。検索欄を閉じるために`leave_content_search`や`toggle_content_search_mode(Files)`を呼ばない。これらは検索モードや結果を変更するため。

閉じる前に、名前変更中・確定処理待ち・未確定の日本語入力・既存modal/AI/設定の制限を確認する。拒否する場合は何も変更せずその呼出しを終える。変換完了後に遅れて実行する予約や待機状態は追加しない。必ず`blur_*`、実focus移動、表示状態更新より前に確認する。

本文／フィルターの既存composition検査に加え、フォーカス中の本文検索InputStateも確認する。`document_find.rs::document_find_input_is_composing`は`EntityInputHandler::marked_text_range`を読む既存例であり、同じ方式を再利用できる。本文内検索の未確定文字も表示変更で失わせない。

閉じる時は既存の`blur_content_search_focus`などで隠れる入力先のフラグを解除する。検索InputState等が実際のGPUI focusを持つ場合は、`window.focus(&self.focus_handle, cx)`も行う。本文のテキスト・選択・dirty・undo/redoは変更しない。再表示は保持したモードと内容を描画するだけにし、再検索は開始しない。本文検索キーによる明示的な開始は別として既存処理を使う。

## 今回必要な確認

既存のGPUIテスト方式に合わせ、値の反転だけを調べるテストにしない。次を一まとまりで確認する。

1. 実際のaction/keyとクリックによる開閉、モード・幅・scroll・本文・選択・dirty・編集履歴の保持、追加読込が起きないこと。
2. フィルター／検索入力／検索結果から閉じた後の実focusと入力配送。検索キーの再表示、設定入口、名前変更・IME中のno-op。
3. 表示・非表示のボタンboundsが同じこと、最小幅160pxと狭いウインドウでの既存操作、一覧の最下行と固定フッター。既存の`sidebar_scrolls_rows_without_moving_fixed_controls`などを参照する。
4. 開閉を挟んだ本文の折り返し、クリック・ドラッグ選択、入力座標。パネル内の入力boundsとスクロールバー位置を確認する。

初回のworkerはshell/test/build/lintを実行できない。必要な検証を実行したと報告せず、変更と未実行項目を返す。push後のtrusted verify、既存macOS/Windows CI、current-headのレビューをCommanderが確認する。

画面の受入はmacOS/Windowsで、両テーマ、クリック・Command/Ctrl+B、検索からのfocus、本文位置、IMEを対象に行う。既存の`/gui-validate head`と`/gui-validate sidebar-chrome head`には開閉操作やBキーの確認がなく、その成功だけで本機能をGUI合格にしない。この機能のためだけに新しいworkflow・汎用GUI基盤・厳密な時間計測基盤を作らない。

Windowsはpapa-win側のChatGPTアプリから直接確認する。KeyhacのフックをOFFにしてキーを確認し、終了後ONへ戻す。検証用の実行パス・PID・隔離stateを記録し、プロセス終了を確認する。この準備環境には対象OSのnative GUI操作機能がないため、実画面の結果は未確認のまま区別する。

## 範囲と並行作業

#542 / 設計PR #543の全幅タイトルバー統合、#237のツリーキー操作、全域リファクタリング、AI機能、配布、workflow、vendor、依存版は変更しない。#543は作成時点で設計用Draftであり、本Issueは既存レイアウトに限定する。統合直前にcurrent mainと双方の差分を確認し、同じボタンを二重に置かない。

#304/#305の採用済み入力経路を使う。PR #541はブロック表示引数、PR #546はAIパネル幅で、今回の可視性とは別責務。単に同じview.rsを使うことや#297全体の未完了を待機理由にしない。

Jevの既定CLIはこの環境に存在しない。Commander Policy §2.9に従い、取得したcurrent factsで準備actionを一つずつ判断した。Jev応答・製品実装完了・検証成功を作り上げていない。
