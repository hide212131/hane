# Issue #716 実装引き継ぎ（フェーズ p1）

## 対象

- Issue: #716「閲覧モードでもタスクのチェックボックスだけは切り替えられるようにしたい」
- フェーズ: p1（本 Issue の実装全体。フェーズ分割なし）
- 基準 SHA（準備 commit の親、current main）: `b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b`
- 採用済み設計: `docs/implementation/issue-716-reading-checkbox-design.md`（本 PR に同梱）

## 状態

**未実装・未検証。** この文書と設計書の追加だけが準備 commit であり、製品コードとテストはまだ変更されていない。実装はこの PR への Commander ハンドオフを受けた Claude Code worker が行う。

## 今回の範囲

設計書の確定事項（冒頭の司令塔追記を含む）に従う。要点:

1. `crates/ui/src/view/task_checkbox.rs`: 専用述語 `task_checkbox_toggle_allowed()`（`Editing | Reading` かつ本文 IME なし）を新設し、`press_task_checkbox` と `toggle_task_checkbox` の `body_editable()` ゲートを置き換える。`body_editable()` 自体は変更しない。
2. `crates/ui/src/view/document_find.rs`: 切替後の検索再走査で一致箇所へ自動スクロールしないよう、切替時に限って reveal を抑制する局所メソッドを追加する（再走査と結果更新は継続）。
3. `crates/ui/src/view/reading_mode.rs`: モード説明と既存テストの名称・説明を例外に合わせて整理する（一般編集の拒否アサーションは維持）。
4. 設計書のテスト計画に基づくテストの追加・置換（実プレスでの切替、切替後の一般編集拒否、編集モード復帰後の Undo、選択・スクロールの保持、composition 中の拒否、閲覧モードでの保存、検索 reveal 抑制）。

## 対象外

- 閲覧モードでの Undo / Redo の解禁
- チェックボックス以外の本文編集の解禁
- ホバーの手形ポインター表示（フォローアップ Issue #717。設計書 §6 と layout / render の変更は実装しない）
- チェックボックスの a11y 合成ノード追加（Issue #704 の範囲）
- `crates/editor` と `plan_task_toggle` の製品実装の変更

## 受入条件の柱

- 閲覧モードでチェックボックスをクリックすると両方向に切り替わり、通常編集として記録され dirty になる。
- 切替後も入力・削除・リスト編集・Undo / Redo・置換・貼付などの一般編集は閲覧モードで拒否され続ける。
- 切替で選択とスクロールが保持される（検索再走査の非同期完了後を含む）。切替後に明示的な検索 Next / Previous は従来どおり移動できる。
- 閲覧モードのまま保存できる。
- 既存テスト `reading_mode_and_composition_never_toggle` は新仕様に置換し、composition 拒否は両モード共通のテストとして残す。
