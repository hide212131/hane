# Issue #580: AIプロンプト本文入力欄の表示改善

## 実装準備

- 対象Issue: [#580](https://github.com/hide212131/hane/issues/580)
- 対象フェーズ: AIプロンプト設定の本文入力欄の表示改善
- 基準main SHA: `667f4baca1ae7362c3d43e0738895a20da4c322c`（2026-10-24確認）
- 採用済み機能の背景: [Issue #449](https://github.com/hide212131/hane/issues/449)、[PR #450](https://github.com/hide212131/hane/pull/450)、`docs/ai-selection-actions-implementation-spec.md`
- 対象コード: `crates/ui/src/view/ai_prompts.rs` の `AiPromptsPage::editor_section`
- 状態: この準備commitには製品コード・テストの変更は含めない。実装・検証は未実施。

## 今回の範囲

設定画面「AIプロンプト」の本文Textareaを、初期表示から複数行を確認・編集できる高さにする。既存のTextareaと選択・保存経路を利用し、長い本文も編集できるようにする。

## 受入条件

- [ ] 本文入力欄で複数行を同時に確認できる。
- [ ] 複数行本文の入力、別プロンプトへの選択切替、保存が従来どおり動く。
- [ ] 標準的なウィンドウサイズで入力欄が操作でき、長い本文も編集できる。
- [ ] 変更に対応する回帰確認とcurrent-head CIの結果を記録する。
- [ ] GUI表示を実際のHaneで確認し、対象headと画面証拠をPRへ記録する。

## 対象外と不変条件

タイトル入力欄、保存形式、プロンプトの選択・実行、AI接続・認証、文書編集経路は変更しない。プロンプト本文の内容や保存値は改変しない。

## 依存関係

元機能のIssue #449 / PR #450は受け入れ済みで、current mainに既存Textarea経路がある。今回の表示サイズ変更はIssue #305の短文入力モデルや未完了RF0の責務・状態・契約と重ならないため、それらへの依存はない。変更前の性能測定対象にも該当しない。
