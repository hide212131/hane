# Issue #585: サイドバーからフォルダをゴミ箱へ移動する

## 準備状態

- 基準 main SHA: `3f1f7805ca46507199dae8f8268329e58291d763`（2026-10-24確認）
- Issue: [#585](https://github.com/hide212131/hane/issues/585)
- このcommitはClaude実装待ちの準備文書のみで、製品コード・テスト・受け入れ検証は未実施。

## 今回の範囲

- 作業フォルダ内の子フォルダ行の右クリックメニューへ「ゴミ箱に移動」を追加する。
- 空・非空を問わず、選択フォルダと全配下を復元可能な OS Trash / Recycle Bin へ移す。
- 成功後のWorkFolder tree、選択状態、配下のopen sessionを整合させる。
- dirty / 保存中の配下sessionを保護し、非同期I/Oの失敗・Work Folder切替後の古い結果を安全に扱う。

## 採用済みの経路・関連コード

- ファイル行の既存メニューとOS Trash境界: `crates/ui/src/view/sidebar.rs` の `sidebar_context_trash`、`move_path_to_trash`、`native_move_to_trash`。
- サイドバーのcontext menu描画とfolder row: `crates/ui/src/view/sidebar.rs`。
- WorkFolderのfolder tree・file event・session整合: `crates/session/src/workfolder.rs`、`crates/session/src/session.rs`、`crates/session/src/store.rs`。
- 既存の右クリック操作・安全条件はIssue #236を参照する。キーボード Delete / Backspace はIssue #237に残し、本Issueでは扱わない。
- 関連する整理済み経路として、PR #555で受け入れられたOS watcherを含む現行mainを基準にする。旧経路への実装や同じ削除規則の二重実装はしない。

## 依存関係

着手時に確認した関連open Issueは #237、関連するサイドバー変更PR #555はmerge済みで、Issue #585と同じ実装PR / 実行中runはない。Issue #237はキーボード削除をcontext-menuの削除仕様と合わせて別途有効化する方針であり、今回のcontext-menu機能を始めるための先行条件ではない。`docs/refactor-execution-plan.md#feature-development-policy` に従い、同じ責務の整理が未受け入れであることを理由に待たない。

## 受け入れ・検証

Issue #585の全受け入れ条件を実装後に確認する。関連するGPUI / filesystem回帰テスト、macOS / Windows CI、両OSのfocused GUI scenario（右クリック、メニュー表示、入れ子フォルダのゴミ箱移動、一覧とsession状態）をcurrent headに結び付けて確認する。準備時点ではいずれも未実施。
