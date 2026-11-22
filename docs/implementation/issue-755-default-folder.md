# Issue #755 実装ハンドオフ — 設定画面でデフォルトのフォルダを表示・変更する

## 状態

未実装・未検証。この文書と設計書だけを置いた準備コミットであり、機能はまだ存在しない。

## 設計

設計書は `docs/implementation/issue-755-default-folder-design.md`（このフォルダ内）。設計担当は Codex GPT-6.2 Sol、基準は `main` の `411c8089`。

## 実装の要点

- 設定画面「一般」の「保存」の直後に「デフォルトのフォルダ」のカードを追加する。値は保存済みパス（未設定なら「未設定」）、操作ボタンは1つ（「変更…」／未設定時は「選択…」）
- フォルダ選択（フォルダのみ・単一選択）で確定したら `settings.default_folder = Some(path)` を設定し、既存の `store_settings` で保存する。設定画面は開いたまま、表示を更新する
- **現在の作業フォルダは切り替えない**（`switch_to_work_folder` はセッション一式を置き換えてタブが閉じるため呼ばない）。新しいデフォルトは次回の通常起動から効く
- キャンセル・空の結果では何も書かず、エラーも出さない。ダイアログの失敗はカード内に表示する。保存失敗は既存の共通エラー表示（`settings_store_error`）を使う
- 未設定に戻すクリア操作、パスの手入力は作らない。設定ファイル形式と起動時の分岐は変更しない

## 受入条件

設計書のテスト計画にある GPUI テスト5件を実装し、通ること:

- `general_settings_shows_the_saved_default_folder_or_unset`
- `changing_default_folder_saves_without_replacing_open_sessions`
- `cancelling_default_folder_picker_preserves_the_saved_folder`
- `default_folder_picker_error_keeps_the_previous_value`
- `default_folder_save_error_is_visible_and_same_path_can_be_retried`

テストは `StateStores::memory()` ＋ `EditorView::from_sessions` で本番の設定ストアから隔離すること（共有ストアを使うと並行テストへ漏れる）。司令塔は `cargo check --workspace` と対象クレートのテストを `--no-fail-fast` で再実行して確認する。
