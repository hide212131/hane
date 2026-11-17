# Issue #744 実装ハンドオフ: タブを閉じる確認に「保存せずに閉じる」を追加

## 位置づけ

本 PR は実装前の準備 PR である。現時点では未実装・未検証であり、Claude による実装を待つ。
設計の正本は `docs/implementation/issue-744-discard-close-design.md`（同梱）。

## スコープ

設計書 §2 の方式で実装する。

- `SessionSet::close_discarding_changes` を新設する（削除・最後のタブの置換は通常 `close` と private helper で共用。通常 `close` の dirty 拒否は維持）
- `close_tab_now` の後処理を `finish_tab_close` に抽出して通常／破棄で共用し、新ハンドラ `confirm_discard_and_close_tab` を追加する
- `TabCloseConfirm` に表示世代（generation）を追加し、同じ文書への確認の再表示でも stale click を区別する。既存 fixture（clipboard.rs、app_update.rs のテスト等）の構築も追随させる
- ダイアログのボタンは左から「キャンセル／保存せずに閉じる／保存して閉じる」。新ボタンの selector は `tab-close-confirm-discard`。三択が文字切れ・重なりなく収まるようダイアログ側の幅を調整する
- 保存処理中は破棄ボタンを無効化し「保存処理中です」と表示する。ハンドラ側でも保存中でないことを再検査する

## 受入条件

- 設計書 §6 のチェックリストをすべて満たすこと
- 設計書 §5 のテストを追加すること:
  - セッションテスト（crates/session/tests/conflict_rules.rs）: `explicit_discard_closes_a_dirty_session_without_writing`、`explicit_discard_of_the_last_session_creates_a_clean_untitled`、`explicit_discard_of_an_inactive_session_keeps_the_active_session`
  - GPUI テスト（crates/ui/src/view.rs）: §5.2 の7件と §5.3 の stale click 4件
  - 既存の回帰テスト群（§5.4）を維持して通すこと
- `cargo test -p hane-ui -p hane-session --no-fail-fast` と `cargo check --workspace` が通ること

## 対象外

- 閉じる入口の追加（タブの×ボタン新設など）
- 保存・Save As 経路そのものの動作変更
- 確認表示中の通常編集で確認を自動失効させる仕様変更

## 注意

- 設定値を切り替えるテストを書く場合は、プロセス共有の実設定ストアではなく `StateStores::memory()` 等の隔離されたストアを使うこと（過去に共有ストア汚染で大量のテストが連鎖失敗した事故がある）。ファイル不変テストでは autosave を明示的に無効化すること（設計書 §5.2 の注記）
