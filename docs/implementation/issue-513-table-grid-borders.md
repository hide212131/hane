# Issue #513: 表のgridと罫線の境界を揃える

## この文書の位置づけ
Claude実装用PRの準備文書。準備時点では製品コード・テストの実装、build/test/lint、修正後GUI検証は未実施。

- 仕事の契約: https://github.com/hide212131/hane/issues/513
- branch基準main: `df543a7913395f995c500a21a68e59fe696f00d0`
- 原因調査基準: `777689ae02d78c90aabbd1af1a9619aea6f89035`
- 両commit間の変更はAI拡張の設計文書のみで、表の描画・geometry・Commander Policy・Claude実行契約に変更なし。
- 関連: #307、#297。全域リファクタリングを待たず、現行BlockLayoutのgeometryで局所修正する。

## 問題と再現例
ユーザー提供画像では、2列のセル幅は内容に合っているが、横罫線だけが本文右端まで延び、右端の縦罫線が欠ける。画像の実行バージョンは未確認。次の独立したMarkdownで確認できるようにする。

```markdown
| Feature | Status |
| --- | :---: |
| Typora-style editing | ✓ |
| Atomic autosave | ✓ |
| Light / Dark theme | ✓ |
```

## 原因と実装方針
`crates/presentation/src/layout.rs` の `table_column_widths` は内容幅が利用可能幅に収まればその幅を返す。一方 `crates/ui/src/line.rs` の `table_row_element` は、ヘッダー背景こそcell geometryに従うが、横罫線は行全体の左右余白を基準にし、縦罫線は各セル左端だけに描く。

既存cell geometryから同じgrid境界を導き、横罫線・左右外枠・列境界・ヘッダー背景を揃える。行の本文幅を表幅と取り違えず、最後のセルの右端に枠を閉じる。スクロールや切り出しで表示行が変わっても、既存の表geometry契約に従う。

## 読むもの
- `AGENTS.md`、`CLAUDE.md`（存在する場合）
- `docs/aadw-command-policy.md`、`docs/aadw-claude-execution.md`
- `crates/ui/src/line.rs`（`table_row_element` と関連テスト）
- `crates/presentation/src/layout.rs`（表layoutと `table_column_widths`）
- `crates/presentation/tests/layout_contract.rs`
- `crates/ui/tests/` など既存の表描画・UIテスト

## 今回の実装範囲と受入条件
1. 再現例の横罫線がStatus列右端で止まり、右端縦罫線と接続する。ヘッダー背景・外枠・列境界も同じgrid境界に揃う。
2. 1列・複数列、空セル、長文折返し、狭幅、ズーム変更で境界が崩れない。
3. light / dark、通常 / 編集表示の切替、caret・selection・hit test・IMEなど既存契約を維持し、Markdown内容を変更しない。
4. 広い本文内の小さい表と幅制約のある表の両方を含め、今回の描画境界不整合を検出できる回帰テストを追加する。実装をなぞるだけのテストは避ける。
5. 修正に直接必要な製品コードと関連テストのみ変更する。

## その後必要な検証
実装workerはテストを実行できないため、未実施を明示し、必要な検証を結果に残す。trusted finalizer後のcurrent-head workspace tests / Clippyとrequired CIを確認する。受入前に修正headで再現例のfocused GUI検証（light / dark、広幅 / 狭幅、通常 / 編集表示）を行い、画像・観測を残す。current-headの必要なレビューも確認する。これらを文書準備・依頼投稿・patch生成だけで合格扱いにしない。

## 対象外
表の全幅化、Markdown構文変更、表編集機能の再設計、全域座標リファクタリング、AADW・保護パスの変更。並行PRのリンク操作・入力経路などへ範囲を広げない。新規ADRや独自の状態管理・workflowは追加しない。
