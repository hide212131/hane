# Issue #559 実装準備

対象: https://github.com/hide212131/hane/issues/559
基準main: `7de394127383b50641b7345aee4866b6c3b16be0`

サイドパネルのファイル一覧ラベルで末尾の `.md` 拡張子を非表示にする。受入条件・対象外はIssue #559を正本とする。現時点では未実装・未検証。

既存の `crates/ui/src/view/sidebar.rs` の `file_name_label` と日付バッジ描画を確認し、一覧の表示だけを変更する。`crates/session/src/workfolder.rs` の実ファイル名・パス保持、`crates/session/src/date_badge.rs` の日付抽出契約、名前変更・保存・フィルタ照合は維持する。

通常名、日付バッジ付き、途中の `.md`、大文字拡張子、日本語名、フォルダ名を確認する。関連テストとrequired CI、対象headのfocused GUIで一覧・フィルタ・ネスト・クリックして開く動作を検証する。

#555の一覧更新と#550のパネル可視状態は別責務として扱う。統合時にはcurrent mainとの差分を再確認する。新しいADRや全域リファクタリングは今回の先行条件にしない。
