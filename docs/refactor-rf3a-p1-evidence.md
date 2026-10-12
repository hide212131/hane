# RF3-A P1 検証記録

Issue: [#304](https://github.com/hide212131/hane/issues/304)  
実装PR: [#477](https://github.com/hide212131/hane/pull/477)  
設計正本: [入力先判定の実装仕様](https://github.com/hide212131/hane/blob/cbf468b3d622f9c3f0f2823fdb5f03bc9cc58ee0/docs/refactor-rf3a-input-routing-implementation-spec.md)  
作業ブランチ: `refactor/rf3a-p1-input-routing-20261012`  
開始base/current base: `c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e`

## P1 の範囲と候補差分

通常編集キー14 action（Backspace/Delete、左右移動と選択、Home/Endと選択、SelectAll、Copy/Cut/Paste）の入力先判定をprivateな `view/input_routing.rs` にまとめた。優先順はsidebar filter、inline rename、本文。各actionは一度だけ対象を解決し、既存Editorまたは既存のfilter/rename helperへ渡す。Cutは同じ解決結果をコピーと削除の両方に使う。

変更ファイルは `crates/ui/src/actions.rs`、`crates/ui/src/view.rs`、新規 `crates/ui/src/view/input_routing.rs`、本記録。本文のselection/IME/Undo/Redo、filter/renameの状態所有者は移していない。特別キー（P2）、platform入力とIME callback（P3）、短い入力欄の内部状態共通化、list planner/source-first transactionの変更は含まない。

## 変更前の契約テスト

| 確認 | 結果 | 対象SHA・証拠 |
|---|---|---|
| 追加した契約テスト | GPUIテスト7件。本文/filter/renameの14 action、pending renameのCut、Unicode grapheme境界を確認 | `f538017afe51ba9b4b7091cdfbd455177bf9d73f` |
| 既存テストの削除・ignore・改名 | PR差分に該当なし | [PR #477の差分](https://github.com/hide212131/hane/pull/477/files) |
| 変更前候補のworkspace all-features tests | 成功（`cargo test --workspace --all-features --locked`） | [exact-head run 38248422587](https://github.com/hide212131/hane/actions/runs/38248422587) |
| 変更前候補のworkspace Clippy | 成功（`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`） | [exact-head run 38248422587](https://github.com/hide212131/hane/actions/runs/38248422587) |

変更前候補はテスト追加後・routing変更前のhead `f538017afe51ba9b4b7091cdfbd455177bf9d73f`。上記runはそのheadに対して実行した。

## P1 routing候補の検証

| 確認 | 結果 | 対象SHA・証拠 |
|---|---|---|
| workspace all-features tests | 成功（`cargo test --workspace --all-features --locked`） | `44762e39485da9d95fe539449802aff9abf0b15e`、[exact-head run 38251386798](https://github.com/hide212131/hane/actions/runs/38251386798) |
| workspace Clippy | 成功（`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`） | `44762e39485da9d95fe539449802aff9abf0b15e`、[exact-head run 38251386798](https://github.com/hide212131/hane/actions/runs/38251386798) |
| Claude worker・trusted finalizer・exact-head guard | 成功 | [run 38251386798](https://github.com/hide212131/hane/actions/runs/38251386798) |
| PR-triggered通常CI | `action_required`、jobなし。成功として扱わない | [run 38252382588](https://github.com/hide212131/hane/actions/runs/38252382588) |

この証拠記録の更新は文書だけを変更する。上のrouting候補headの製品コードとテストは更新前後で同一であり、最後の受入では記録更新後のPR headに結び付く通常CIも確認する。

## 受入に残る確認

| 確認 | 状態 |
|---|---|
| UI crate通常構成のテスト一覧・tests | 未実施 |
| UI crate all-featuresのテスト一覧 | 未実施 |
| workspace通常構成 locked tests | 未実施 |
| rustfmt / `git diff --check` | 未実施 |
| current-head PR CI | 未完了。Draft解除後に必要なcheckを確認 |
| CodeRabbit current-head full review | 未実施。現在のDraftではreviewされていない |
| macOS focused GUI・日本語IME | 未実施 |
| Windows focused GUI・日本語IME | 未実施 |
| final head/base・差分・復旧方法の記録 | 未完了 |

P1はまだ受入済みではない。このPRはDraft、Issue #304はopenのまま。未実施の確認をpassとして扱わず、必要な証拠が揃ってからP1を受け入れる。
