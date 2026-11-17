# Issue #745 実装ハンドオフ: 更新時の未保存ブロックを「保存して再起動」に

## 位置づけ

本 PR は実装前の準備 PR である。現時点では未実装・未検証であり、Claude による実装を待つ。
設計の正本は `docs/implementation/issue-745-save-and-restart-design.md`（同梱）。

## スコープ

設計書 §2 の方式で実装する。

- 提示判定: ブロッカーが DirtyDocument のみ、かつ dirty な全セッションがファイルパスを持つ場合だけ、更新帯と設定カードの主アクションを「保存して再起動」にする。判定は副作用のない共通関数に集約する（描画時に prepare_update_restart を呼ばない）
- 保存: 対象は SessionId 昇順に逐次、SaveIntent::Current のみ。完了通知付き保存（save_session の内部共通化＋一回限りの完了通知）を新設し、`Saved` のときだけ次へ進む。`Queued` は中止扱い
- 合流: 全件 Saved の後、セッション構成・revision・path の変化検査と restart_blockers の再収集を行い、空なら一括保存状態を解除して既存の start_update_restart(Some(candidate_id)) を呼ぶ
- 状態: EditorView に一括保存状態（operation_id、candidate_id、開始時 snapshot、対象一覧、進行位置）を追加し、初期化する。通常再起動入口にも一括保存中のガードを入れる
- 失敗時: 最初の失敗で中止し、対象ラベル・パス・理由を restart_message に表示する。成功済みの保存はそのまま、本文と dirty は保持する

## 受入条件

- 設計書 §6 のチェックリストをすべて満たすこと
- 設計書 §5 の追加テスト16件を実装し、既存の回帰テスト群を維持して通すこと
- `cargo test -p hane-ui --no-fail-fast` と `cargo check --workspace` が通ること
- crates/session の dirty 定義・保存キュー・保存チケットの意味は変更しないこと

## 対象外

- IME・保存中・PendingEdit・AI フォーム等、DirtyDocument 以外のブロッカーの扱いの変更
- dirty な無名メモを対象に含めること（下書き保存済みでも対象外のまま）
- 自動保存の挙動変更、service.rs の I/O・外部変更検査の再実装

## 注意

- 設定値を切り替えるテストを書く場合は、プロセス共有の実設定ストアではなく `StateStores::memory()` 等の隔離されたストアを使うこと（過去に共有ストア汚染で大量のテストが連鎖失敗した事故がある）
- 非同期テストは時間待ちに依存せず、完了を明示的に解放できる FileService の fake を使うこと（設計書 §5 の注記）
