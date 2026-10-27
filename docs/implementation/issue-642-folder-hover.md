# Issue #642 実装準備

対象: https://github.com/hide212131/hane/issues/642

基準main: `a50c44669d6a116dfbe13439bb258db39d9bf92b`

このcommitはClaude実装待ちの準備文書のみ。製品コードは未実装、テスト・GUIは未検証。

## 範囲

通常ツリーの親フォルダ（子フォルダとroot）行に、hover時の新規フォルダ・新規ファイルの2アイコンを追加する。既存ピンhoverに合わせ、固定幅・日本語tooltip/アクセシビリティ名・イベント伝播防止を適用する。

作成先をクリック行のpathに固定し、別フォルダの選択に依存させない。既存の `new_work_folder_note_in` とフォルダ作成の命名・重複回避・非同期処理・失敗/削除中ガードを利用する。フォルダ作成入口にpath指定が必要なら最小限で抽出し、既存ツールバー動作を維持する。既存H1命名・保存・draft recoveryを維持する。

正本の要求・受入条件はIssue本文。主なコードは `crates/ui/src/view/sidebar.rs`、`crates/ui/src/view/sidebar_pins.rs`、`crates/ui/src/view.rs`。受入済み#608 / PR #610のフォルダ指定新規メモを使う。PR #638は初期化の機械移動で今回の責務とは独立し、#297/RF0全体の完了待ちは不要。

## 検証

対象行のpath指定、別フォルダ選択、閉じた入れ子、root、連続作成、親行へのイベント伝播防止のfocused GPUI回帰を行う。macOS/Windows GUIでhover/クリック・作成先反映・狭幅と長い日本語名・pin共存を確認する。CI、最終レビュー、必要GUIはcurrent head/baseに結び付けて確認する。

## 対象外

ピン一覧/ファイル行の作成ボタン、新しい命名UI、ツールバー/右クリック操作の撤去、汎用リファクタリング。
