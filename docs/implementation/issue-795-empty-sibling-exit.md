# Issue #795 設計: 連続する空項目からの離脱

出典: Codex（GPT-6.2 Sol / medium）による設計（2026-11-28）。司令塔の補足と Jev 判断は末尾の「司令塔補足」にあります。

# Issue #795 設計書 — 連続する空リスト項目からの離脱

対象は `origin/main` の `31439efac2374bb8d134b068586ee7becc3c4443`。Enter と Backspace の最上位リスト項目からの離脱に適用する。

## 推奨案

**案 (c) を採用する。** キャレットがある空項目から離脱するとき、その直後に物理行として連続する、同じリストの空の兄弟項目をすべて畳む。対象はパーサーが認識した、マーカーと ASCII スペース・タブだけの項目に限る。チェックボックス、子孫、継続本文を持つ項目や、既存の空行に達した時点で止める。前方の空項目は保持する。後方は離脱後の入力に吸収される問題があり、前方は既存の左側境界で保護できるため、この操作を対称にする必要はない。

- **報告ケース** `- abc\n- │\n- ` → `- abc\n\n│`。`│` はキャレットで、保存バイトに含めない。Hane は末尾改行が作る空の最終行も描画するため、`abc` の下は区切りの空行とキャレット行の **2 行**になる。提示案の表記どおり `- abc\n\n│\n` とすると、さらに空の最終行が描画される。`ghi` を打つと `- abc\n\nghi` となり、独立した段落になる。[末尾行の描画契約](../../crates/ui/src/view.rs#L12000)
- **後ろに内容があるケース** `- abc\n- │\n- \n- C` → `- abc\n\n│\n\n- C`。`ghi` を打つと `- abc\n\nghi\n\n- C` になり、空の兄弟を除いた後は現行の両側セパレータ契約と同じになる。既存の空行が右側境界を満たす場合は、その空行を再利用する。[既存の両側セパレータテスト](../../crates/markdown/src/list_editing.rs#L1952)
- **末尾判定**は、操作前から末尾だった場合と、畳んだ結果として末尾に達した場合を区別する。前者は #767 の規則どおりマーカーだけを除去する。後者は右側セパレータを省き、直前に非空行があれば左側セパレータを作る。両方を省くと `- abc\nghi` となり、`ghi` が `abc` の項目の継続行になる。[ADR-0029 §8](../../docs/adr/0029-source-first-list-editing.md#L373)

### 吸収の検証

ローカルの `markdown-it` の CommonMark モードで、右側だけを省いた `- abc\n\nghi\n- ` は、報告例の `-` が下線として解釈され、`ghi` がレベル 2 の Setext 見出しになることを確認した。後続を `* ` や `1. ` にすると、マーカーは `ghi` の段落の継続行になる。右側の空行を残した場合は `ghi` と後続リストが分離する。推奨案の末尾出力と `- C` が続く出力でも、`ghi` がトップレベルの段落になることを確認した。Hane が同梱するパーサーにも、Setext 下線の判定と、空項目による段落中断を禁止する判定がある。[CommonMark の Setext 見出し規則](https://spec.commonmark.org/0.31.2/#setext-headings) · [空リスト項目の例](https://spec.commonmark.org/0.31.2/#example-285) · [同梱パーサーの判定](../../vendor/pulldown-cmark/src/firstpass.rs#L2132)

もう一つ保護が必要なのは、空項目の列の先にインデントされた別ブロックがある場合。たとえば `- A\n- │\n- \n\n  x` の `x` は元はリスト外の段落だが、空項目をまとめて消すと、打鍵前の再解析で `A` の項目に入ることを確認した。このケースを避けるため、**折り畳み後に残る最初の非空行がスペースまたはタブで始まる場合は、折り畳みを取り消して従来の離脱を使う**。この保守的な条件により、一部の安全なインデント付き入力でも従来の結果になる。

## 変更箇所と処理の流れ

1. [`plan_list_edit`](../../crates/markdown/src/list_editing.rs#L524) の Enter／Backspace 共通分岐で、既存の `item_at` が返す項目インデックスと `ListEditProjection` を `exit_top_level_item` に渡す。空本文の開始位置、パーサーでの認識、引用、対象項目自身の子孫など、既存の離脱条件を通過した最上位項目にだけ適用する。
2. [`exit_top_level_item`](../../crates/markdown/src/list_editing.rs#L1033) から、投影のソース順で後続を調べる。候補の `prefix_range.start` が直前の項目行の `line_end_inclusive` と一致し、`list_range`、`depth`、`parent_item` が同じで、`ordinal` が連続することを要求する。`is_parser_item` が真の候補だけを採用する。生のマーカー文字列や、回復候補にも付く `previous_sibling` 単独では削除を決めない。投影外には削除範囲を広げない。
3. 各候補は `body_range` が空、`task` が `None`、引用外で、`exit_is_safe_to_collapse` の最上位判定を満たすことを要求する。この判定は、実際の子ノードから求めた `subtree_range.end` が開始行の終端を越えないことを確認する。パーサーの先読みで既存空行まで含み得る `item_range.end` は使わない。`- [ ] `、`- [x] `、`- [X] ` はタスクとして保持し、文字どおりの `- [] ` は本文を持つ項目として保持する。最初の対象外行で探索を止め、残る最初の非空行にインデントがある場合は、上記のとおり今回の折り畳み全体を見送る。[既存の保護判定](../../crates/markdown/src/list_editing.rs#L1003) · [subtree の算出](../../crates/markdown/src/list_editing.rs#L3023)
4. 1 行以上畳む場合は、**対象項目の行頭から最後に畳む項目の行終端直後まで**を、ひとつの `MarkdownEditPlan::Replace` で置換する。左側は直前行の終端を使い、既存の左側空行があれば再利用する。残るバイトがある場合はキャレット行を対象項目自身の行終端で閉じ、直後に非空行があれば同じ終端で右側セパレータを作る。文書末尾まで畳んだ場合はキャレット行を閉じるための行終端を追加しない。折り畳みが成立しない場合は、従来の `prefix_range` の置換を使う。
5. キャレットは左側の区切りを出力した直後に置き、UTF-8 のバイト位置で返す。削除範囲の外側の改行・空白を保持し、CRLF は一組として扱う。裸の CR と LF が新しい継ぎ目で CRLF に融合しそうな場合だけ、置換文字列側の行終端を CRLF にして必要な行数を保つ。たとえば、残る直前行が裸の CR で終わり、次に出力する終端が LF なら、その LF を CRLF として出力する。キャレットは確定した終端長から計算し、CRLF の途中には置かない。
6. UI は既存の `apply_list_edit_intent` から `replace_range_recorded` を一度呼ぶ。1 キー操作で 1 revision、1 Undo になる。Undo は畳んだ行を含む元のバイト列とキャレットを復元し、Redo は離脱直後の状態を再現する。その後の打鍵は別の履歴単位になる。[UI の適用経路](../../crates/ui/src/view.rs#L3750) · [履歴の記録](../../crates/editor/src/lib.rs#L370)

## ADR-0029 との関係

**後続項目の保持規則への限定的な契約変更**になる。§5 は後続リストの分離を、§8 は後続項目の原文とリスト所属の保持を明記しており、空の兄弟項目は現行文面の保持対象に含まれる。§5・§8 に、隣接するマーカーだけの空の兄弟項目を畳める条件と、折り畳みで末尾に達しても必要な左側境界を作ることを追記する。後続に実内容がある場合の両側境界と、操作前から末尾にあった項目に対する #767 の例外は維持する。`empty_top_level_exit_contract_matrix` を含む既存テストの期待値変更は不要で、新しい分岐の契約テストを追加する。[ADR-0029 §5](../../docs/adr/0029-source-first-list-editing.md#L284) · [ADR-0029 §8](../../docs/adr/0029-source-first-list-editing.md#L373) · [既存の契約テスト](../../crates/markdown/src/list_editing.rs#L2367)

## テスト計画

[`crates/markdown/src/list_editing.rs`](../../crates/markdown/src/list_editing.rs#L2367) に次を追加する。全行を Enter と Backspace の両方で実行し、単一の `Replace`、範囲、置換バイト列、キャレットを照合する。`│` は保存しない。

| テスト名案 | 入力 | 期待結果 |
|---|---|---|
| `exit_folds_empty_sibling_at_eof` | `- abc\n- │\n- ` | `- abc\n\n│`。範囲 `6..11`、置換 `"\n"`、キャレット `7` |
| `exit_preserves_boundaries_after_folding_before_content` | `- abc\n- │\n- \n- C` | `- abc\n\n│\n\n- C`。範囲 `6..12`、置換 `"\n\n\n"` |
| `exit_folds_the_entire_adjacent_empty_run` | `- A\n- │\n- \n- \n` | `- A\n\n│`。後続 2 項目を畳み、空の表示行は計 2 行 |
| `exit_preserves_mixed_line_endings` | `- A\r\n- │\n- \r\n- C` | `- A\r\n\r\n│\n\n- C`。全行 CRLF の派生ケースも検証 |
| `exit_keeps_cr_and_lf_from_merging` | `- A\r- │\n- \r\n- C` | `- A\r\r│\r\n\n- C`。キャレット位置と行数も照合 |
| `exit_folding_stops_at_task_items` | `- A\n- │\n- \n- [ ] ` | `- A\n\n│\n\n- [ ] `。`[x]`、`[X]`、`[]` も原文を保持 |
| `exit_folding_preserves_a_sibling_subtree` | `- A\n- │\n- \n- \n  - child` | `- A\n\n│\n\n- \n  - child`。`child` は残った空の開始行を持つ項目の子。`  text` に替えた継続本文も検証 |
| `exit_reuses_blank_lines_without_scanning_past_them` | `- A\n- │\n- \n \t\n- ` | `- A\n\n│\n \t\n- `。既存の空行以降も保持 |
| `exit_skips_folding_if_indented_suffix_could_reattach` | `- A\n- │\n- \n\n  x` | `- A\n\n│\n\n- \n\n  x`。打鍵前にも `x` がリスト外に残る |
| `exit_keeps_previous_empty_siblings` | `- A\n- \n- │\n- ` | `- A\n- \n\n│`。前の空項目は残る |
| `exit_preserves_remaining_ordered_marker_bytes` | `3. A\n4. │\n5. \n6. C` | `3. A\n\n│\n\n6. C`。打鍵後の後続リストは `6` から始まる |

上の構造テストでは Hane の `parse_document` で、離脱直後の保護対象と、`ghi` を挿入した後の独立 `Paragraph` を確認する。別リストの空項目、非 ASCII 空白を本文に持つ項目、パーサー未確認の回復候補も削除対象外として固定する。

[`crates/ui/src/view.rs`](../../crates/ui/src/view.rs#L13471) に `empty_sibling_exit_is_one_undoable_edit` を追加する。実キー経路で報告ケースを再現し、離脱が 1 revision で、`abc` の下に空の表示行が 2 行あり、キャレットが最終行の先頭にあることを確認する。直後に `ghi` を入力し、Undo 1 回目で打鍵だけ、2 回目で離脱全体が戻ることと、Redo で双方がバイト単位・キャレット位置ともに復元されることを検証する。

## 却下した案

- **(a) 両側セパレータと後続空項目を常に維持する。** 保護対象がある場合の動作としては必要だが、報告された余分な空行を解消できない。
- **(b) 右側セパレータだけを外す。** 次の打鍵によって Setext 見出し化または段落への吸収が起こり、残した空リスト項目の構造を守れない。
- **後続を 1 項目だけ畳む。** さらに空項目が残ると、右側の区切りは引き続き必要になり、3 項目以上のケースで同じ問題が残る。
- **表示上だけ空行を詰める。** 保存される原文には余分な空行が残り、見えている行とキャレット移動・再読込時の行構造が一致しなくなる。

実装・ファイル変更は行っていない。Hane の Rust テストと GUI 検証は実装時に行う。設計判断に使う Jev CLI は名前解決エラーで応答を取得できなかったため、上記は対象コミットのコード、契約テスト、および CommonMark の照合に基づく提案。
## 司令塔補足（採用判断と進め方）

- 本設計を採用します。推奨案は (c)「後ろに連続する空の兄弟項目をすべて畳む」です。
- 意味判断（Jev、jev-1.14.0）に方針選択を委ね、`collapse_following_empty_run` が confidence 1.0 で選ばれました（却下: 1つだけ畳む／右セパレータを外す／現状維持）。設計書中の「Jev CLI は名前解決エラーで応答を取得できなかった」は設計セッションの読み取り専用サンドボックス側の制約によるもので、司令塔の実行環境からは Jev が正常に応答しています。この補足が正です。
- 実装は1フェーズで行います。変更の中心は `crates/markdown/src/list_editing.rs` の `exit_top_level_item` と、その呼び出し（`plan_list_edit` の Enter / Backspace 共通分岐）です。ADR-0029 §5・§8 への追記（隣接するマーカーだけの空の兄弟項目を畳める条件と、折り畳みで末尾に達した場合の左側境界）も同じ PR に含めます。
- 既存の契約テスト（`empty_top_level_exit_contract_matrix` を含む）の期待値は変更しません。新しい分岐の契約テストを設計書のテスト計画どおり追加します。
- 本書は実装前の準備文書です。この時点では未実装・未検証です。
