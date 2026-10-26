# Issue #629: クリップボードURLのリンク挿入・実装引き継ぎ

- 対象Issue: https://github.com/hide212131/hane/issues/629
- 基準main: `71c976961f5348c6860cf570bf3d35646283d4f4`
- 採用設計: [本文選択へのURLリンク挿入](../design/clipboard-url-link.md)、設計PR #627（merge済み）
- フェーズ: 本文メニュー・URL判定・リンク生成・既存編集経路との接続、および直接関連する回帰テスト。

この準備commitは文書だけであり、製品機能・テストは未実装、実OS検証は未実施。Claude Codeによる実装先を準備する。設計採用・依頼受理を製品完成とは扱わない。

## 守る契約

設計書のA1–A10をすべて受入条件とする。URLはメニューを開いた時のsnapshotを使い、本文選択を保持する。Markdownの適用可否と生成は既存parserを使う。文書instance・revision・向き込みSelectionと編集可否・IMEを実行直前に確認し、不一致なら編集しない。

既存 `Editor::replace_range_recorded(range, replacement, selection_after)` と `EditorView::after_input` を利用する。リンク専用の置換/history APIを重複追加しない。IME中にこのAPIを呼んで変換をcommitしない。独立したUndo/Redo・Selection復元と通常の保存/recovery/解析更新を保つ。

関連PRの再確認: #622 head `901eb21c897983e3da4a9939eda30ac195df714c` のreplace plan、#626 head `f8ec7dfc04f946e275b7f98898fb6fab38b9f3fc` のcheckboxはいずれも同じ置換APIを利用している。両PRは準備時点でopen・未mergeで、リンク機能はそのbranchのコードを複製しない。右クリックから左クリックの `on_row_mouse_down` を呼ばず、checkbox toggle・リンク遷移へ伝播させない。実装開始とpush前にcurrent headを照合する。

## 対象外と検証

書式付きclipboard #596、file drop #598、リンクcursor、proxy #628、設定画面、全域refactorのファイル移動、保護された運用基盤は対象外。他PRの担当・branch/headを変更しない。

関連Editor/Markdown/GPUIテストとCIのbuild/test/lint、current-headレビュー、macOS/Windowsの実clipboard・IMEと保存再読込を設計どおり確認する。workerが実行できない検証を実施済みとしない。Issue全体はcloseしない。
