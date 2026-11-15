# Issue #714 P1 実装ハンドオフ

このドキュメントは実装準備のための文書であり、**コードはまだ未実装・未検証**です。
設計の正本は `docs/implementation/issue-714-crlf-design.md`。

## 実装内容

`crates/markdown/src/list_editing.rs` のリスト Enter / Shift+Enter / 空項目 exit が挿入する改行を、固定の `"\n"` から「分割する物理行自身の終端子」へ変える。

- 純粋関数 `preferred_line_terminator(source, at)` を同ファイルに新設する（責務は終端子の選択だけ）。
  - `at` を含む物理行に終端子があればそれを返す（キャレットより後ろにあっても採用）。
  - 無終端の最終行では直前行の終端子（`line_terminator_before`）を補完に使う。
  - 参照できる終端子が無ければ `"\n"`。
- Enter 継続: 選択した終端子＋既存の `item_prefix` を挿入する。
- Shift+Enter: 同じ終端子選択を適用する（prefix・インデントは従来どおり）。
- 空項目 exit: 左右それぞれの終端子を保存する既存契約を維持する。
- 混在済み文書の既存終端子は書き換えない。

## 受入条件（テストで固定する期待値）

計画単体テスト（`crates/markdown/src/list_editing.rs` の既存テスト群に追加）:

| テスト名 | 入力・キャレット・操作 | 期待テキスト | 期待キャレット |
|---|---|---|---:|
| `enter_continuation_preserves_crlf` | `"- abc\r\n"`, 5, Enter | `"- abc\r\n- \r\n"` | 9 |
| `enter_split_mid_body_preserves_crlf` | `"- abcd\r\n"`, 4, Enter | `"- ab\r\n- cd\r\n"` | 8 |
| `enter_continuation_preserves_lf` | `"- abc\n"`, 5, Enter | `"- abc\n- \n"` | 8 |
| `enter_continuation_preserves_bare_cr` | `"- abc\r"`, 5, Enter | `"- abc\r- \r"` | 8 |
| `enter_continuation_at_unterminated_eof_inherits_crlf` | `"- A\r\n- bc"`, 9, Enter | `"- A\r\n- bc\r\n- "` | 13 |
| `enter_continuation_without_any_terminator_defaults_to_lf` | `"- abc"`, 5, Enter | `"- abc\n- "` | 8 |
| `enter_mixed_endings_uses_current_lf` | `"- A\r\n- bc\n"`, 8, Enter | `"- A\r\n- b\n- c\n"` | 11 |
| `enter_mixed_endings_uses_current_crlf` | `"- A\n- bc\r\n"`, 7, Enter | `"- A\n- b\r\n- c\r\n"` | 11 |
| `shift_enter_preserves_crlf` | `"- abc\r\n"`, 5, Shift+Enter | `"- abc\r\n  \r\n"` | 9 |
| `shift_enter_mixed_endings_uses_current_lf` | `"- A\r\n- bc\n"`, 8, Shift+Enter | `"- A\r\n- b\n  c\n"` | 11 |
| `shift_enter_at_unterminated_eof_inherits_crlf` | `"- A\r\n- bc"`, 9, Shift+Enter | `"- A\r\n- bc\r\n  "` | 13 |
| `ordered_enter_preserves_crlf_and_next_number` | `"3. A\r\n4. B"`, 4, Enter | `"3. A\r\n4. \r\n4. B"` | 9 |
| `task_enter_preserves_crlf_and_unchecks_marker` | `"- [x] A\r\n"`, 7, Enter | `"- [x] A\r\n- [ ] \r\n"` | 15 |
| `enter_crlf_uses_utf8_byte_offsets` | `"- 日本\r\n"`, 8, Enter | `"- 日本\r\n- \r\n"` | 12 |

exit と連続操作:

| テスト名 | 入力・キャレット・操作 | 期待テキスト | 期待キャレット |
|---|---|---|---:|
| `enter_crlf_continuation_then_exit_preserves_boundaries` | `"- abc\r\n"`, 5, Enter→再解析→Enter | `"- abc\r\n\r\n\r\n"` | 9 |
| `enter_exit_unterminated_empty_item_preserves_crlf` | `"- abc\r\n- "`, 9, Enter | `"- abc\r\n\r\n"` | 9 |
| `enter_exit_only_empty_crlf_item_preserves_terminator` | `"- \r\n"`, 2, Enter | `"\r\n"` | 0 |
| `enter_exit_mixed_endings_preserves_each_side` | `"3. A\r\n4. \n5. C"`, 9, Enter | `"3. A\r\n\r\n\n\n5. C"` | 8 |

- 連続操作テストでは中間値（`"- abc\r\n- \r\n"`、キャレット9）も assert し、exit 後に `x` を挿入すると `"- abc\r\n\r\nx\r\n"` になり、再解析後に `x` がリスト外の段落になることも確認する。
- UI 統合テストを1件追加する（`crates/ui/src/view.rs` の既存 source-first list editing テスト付近）: `"- abc\r\n"` キャレット5から Enter を2回適用し、中間値・最終値・キャレットが上表に一致すること、Undo 2回で初期状態へ、Redo で両段階が復元されること。
- 既存テスト（LF の挙動、NoOp、ネスト outdent、混在終端子、`trailing_empty_top_level_exit_preserves_crlf`）は全て維持する。
- Commander が実行する主検証: `cargo test -p hane-markdown --no-fail-fast` と、UI 統合テストを含む `cargo test --no-fail-fast`（ワークフロー側 verify）。

## 対象外

- Issue #728 の意味変更（リスト左端・継続行の Enter の意味）。本件は終端子の選択だけを共有ヘルパーとして提供する。
- 通常段落など、リスト以外の Enter 経路の LF 固定（設計で follow-up と判断済み。本件の前提にしない）。
- 既存の混在済み文書の正規化。
