# Issue #642 実装準備

対象: https://github.com/hide212131/hane/issues/642

基準main: `a50c44669d6a116dfbe13439bb258db39d9bf92b`

初回準備commit時点では、この文書のみで製品コードは未実装、テスト・GUIは未検証だった。現在の状態は末尾の実装・同期記録とPR上のcurrent evidenceを参照する。

## 範囲

通常ツリーの親フォルダ（子フォルダとroot）行に、hover時の新規フォルダ・新規ファイルの2アイコンを追加する。既存ピンhoverに合わせ、固定幅・日本語tooltip/アクセシビリティ名・イベント伝播防止を適用する。

作成先をクリック行のpathに固定し、別フォルダの選択に依存させない。既存の `new_work_folder_note_in` とフォルダ作成の命名・重複回避・非同期処理・失敗/削除中ガードを利用する。フォルダ作成入口にpath指定が必要なら最小限で抽出し、既存ツールバー動作を維持する。既存H1命名・保存・draft recoveryを維持する。

正本の要求・受入条件はIssue本文。主なコードは `crates/ui/src/view/sidebar.rs`、`crates/ui/src/view/sidebar_pins.rs`、`crates/ui/src/view.rs`。受入済み#608 / PR #610のフォルダ指定新規メモを使う。PR #638は初期化の機械移動で今回の責務とは独立し、#297/RF0全体の完了待ちは不要。

## 検証

対象行のpath指定、別フォルダ選択、閉じた入れ子、root、連続作成、親行へのイベント伝播防止のfocused GPUI回帰を行う。macOS/Windows GUIでhover/クリック・作成先反映・狭幅と長い日本語名・pin共存を確認する。CI、最終レビュー、必要GUIはcurrent head/baseに結び付けて確認する。

## 対象外

ピン一覧/ファイル行の作成ボタン、新しい命名UI、ツールバー/右クリック操作の撤去、汎用リファクタリング。


## 実装・main同期記録（2026-10-27）

- Claude実装とGPUI行ボタン回帰テストを含む同期前headは `b4e88fee72a57c1d230071c1d604f9f3ff461508`。従来のCI成功はこのhead時点の証拠であり、同期後headの成功には流用しない。
- main `497419203834f2a7aca413d1cb7758b37eb9dff6` を通常mergeで取り込み、競合なし。旧base `a50c44669d6a116dfbe13439bb258db39d9bf92b` からmainの変更は `.github/scripts/rust_ci_path_filter.py`、`.github/tests/test_rust_ci_path_filter.py`、`.github/tests/test_windows_gui_lock_ci_wiring.py`、`.github/workflows/ci.yml` のみ。
- 同期前headとmerge後の製品ソース・Cargo設定の差分はない。macOS GUI対象の製品コード、描画・入力経路、依存関係は不変であり、CI workflow/testの変更は当該GUI観測に影響しない。そのため[同期前headのmacOS focused GUI証拠](https://github.com/hide212131/hane/pull/643#issuecomment-6440973148)をbase-independentな証拠として維持する。新headで再実施したという意味ではない。
- 元の通常default Release候補SHA256は `f736d26523fe8d5c3938169db58dc80999db0db30cfbf47269e9e4fcbc90cd5d`。GUI観測とcleanupの対象はこの既存候補で、新候補の起動は行わない。
- #640 head `bff07de51c2daa0ba49df2fd9b1c791ec25c92e5` のCI run `40511138058` でWindows専用9 tests/macOS専用60 testsが実行・成功し、CI前提の待機条件は解消した。一方Windows実機はcanonical lock owner未特定でUnknown。共有Windowsデスクトップの操作・候補起動・lock試行・buildは開始しない。
- 同期後current-head CIとreview状態はPRで改めて取得する。旧headのCI/reviewを新headの証拠にしない。Windows GUIはUnknown、PRはDraftを維持し、PR merge・リリースは行わない。
