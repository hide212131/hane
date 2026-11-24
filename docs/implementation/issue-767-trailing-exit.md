# Issue #767 実装準備：末尾の空項目を抜けると余分な空行が入る問題

状態： 実装準備（未実装・未検証）。この文書は実装担当への引き継ぎであり、後続の依頼で実装する。

# Issue #767 実装設計：末尾の空リスト項目からの離脱

**後続に実内容がない最上位の空項目では、対象項目の接頭辞だけを削除し、既存の改行・空白はそのまま残す。** Enter と Backspace に共通で適用する。後続に実内容がある場合は、間に空行があっても Issue #460 の区切り規則を変えない。

## 原因と変更方針

司令塔の読みどおり、原因は**左側の区切りの追加**にある。`crates/markdown/src/list_editing.rs:938` の `exit_top_level_item` は、右側では `has_following` を確認するが、左側では `has_previous && !previous_line_is_blank(...)` だけで判定する。`"- abc\n- "` では置換範囲が `6..8`、左の区切りが `"\n"`、右の区切りが空になる。そのため `"- abc\n\n"` が残り、キャレットは `6 + 1 = 7`、つまり3行目先頭に移る。

- **末尾の定義:** 項目の物理行の終端を含めた直後、すなわち `after_line` から EOF までが、空、または ASCII のスペース・タブと行終端（LF / CRLF / CR）だけなら対象とする。最後の空白行に行終端がなくても対象とする。Unicode 空白は実内容として扱い、`trim()` は使わない。
- **実装箇所:** `exit_top_level_item` で `source.as_bytes()[after_line..]` を調べ、後続に実内容がない場合は左の区切りを追加しない。右側の既存条件は変更不要。この場合、後続行がなければ `has_following` が偽、空白行があれば `following_line_is_blank` が真なので、右の区切りも空になる。`has_following` 自体は「後続の物理バイトがある」という現行の意味を保つ。
- **置換とキャレット:** 置換範囲は現行の `item.prefix_range` のまま、置換文字列は `""`。接頭辞には対象行のインデント、マーカー、既存の区切り空白、認識済みのタスク接頭辞が含まれるが、行終端は含まれない。`new_caret` は右区切りを追加する前の `line_start + replacement.len()` で計算しているため、左区切りが空なら変更なしで対象行の先頭を指す。たとえば `"- abc\n- \n"` は `"- abc\n\n"`、キャレットはバイト `6`（2行目先頭）。残る2つの LF はどちらも元からある。
- **途中の項目との境界:** 最初の後続行だけでは判定しない。`existing_multiple_blank_lines_after_exit_are_not_collapsed` は空行の先に `- C` があり、#472 のケースも空行の先に本文がある。いずれも後続に実内容があるため、既存の区切り・文字列・キャレットの期待値をそのまま維持する。`crates/markdown/src/list_editing.rs:1768`、`crates/markdown/src/list_editing.rs:1938`

## キーと階層の対象範囲

| 操作 | 現行経路と設計上の結論 |
|---|---|
| **Enter / Backspace** | どちらも `plan_list_edit` の同じ分岐で、空本文の開始位置かつ既存の安全条件を満たす場合に `exit_top_level_item` を呼ぶ。Enter の先行位置判定は本文開始位置を処理しないため、このケースは共通分岐に到達する。UI のキー処理は変更しない。`crates/markdown/src/list_editing.rs:629`、`crates/markdown/src/list_editing.rs:723`、`crates/ui/src/actions.rs:384`、`crates/ui/src/actions.rs:427` |
| **Del（前方削除）** | **対象外。** 本文の `Delete ("delete")` は `EditorCommand::Delete` を直接 dispatch し、`ListEditIntent` に前方削除はない。キャレットのみなら次の書記素を通常削除する。例の `"- abc\n- "` の末尾では何もしない。`"- abc\n- \n"` の空本文位置では直後の LF だけを削除し、`"- abc\n- "` のマーカーは残る。`crates/ui/src/actions.rs:485`、`crates/markdown/src/list_editing.rs:17`、`crates/editor/src/movement.rs:354` |
| **入れ子（`depth > 1`）** | **対象外。** 同じ空項目分岐から `transform_subtree` に進み、マーカーを残して一階層 outdent する。各行の既存終端をそのまま書き戻すため、今回のように区切りを追加する問題はない。次のキー操作で最上位になった空項目を抜けるときは、新しい末尾規則を適用する。`crates/markdown/src/list_editing.rs:654`、`crates/markdown/src/list_editing.rs:2693` |

## 追加する回帰テスト

キャレットは **0 始まりの UTF-8 バイト位置**。併記する行・列は1始まり。`両方` は `Backspace` と `Enter` をそれぞれ独立に検証する。

| ケース | 入力文字列／操作前キャレット | Intent | 期待文字列 | 操作後キャレット |
|---|---|---|---|---|
| LF・項目末尾に改行なし | `"- abc\n- "`／`8` | 両方 | `"- abc\n"` | `6`（2行1列） |
| LF・項目末尾に改行あり | `"- abc\n- \n"`／`8` | 両方 | `"- abc\n\n"` | `6`（2行1列） |
| CRLF・項目末尾に改行なし | `"- abc\r\n- "`／`9` | 両方 | `"- abc\r\n"` | `7`（2行1列） |
| CRLF・項目末尾に改行あり | `"- abc\r\n- \r\n"`／`9` | 両方 | `"- abc\r\n\r\n"` | `7`（2行1列） |
| 番号付き・項目末尾に改行なし | `"3. A\n4. "`／`8` | 両方 | `"3. A\n"` | `5`（2行1列） |
| 番号付き・項目末尾に改行あり | `"3. A\n4. \n"`／`8` | 両方 | `"3. A\n\n"` | `5`（2行1列） |
| 後続が空行・空白だけ | `"- abc\n- \n\n \t"`／`8` | 両方 | `"- abc\n\n\n \t"` | `6`（2行1列） |
| 入れ子の末尾：変更なしの対照 | `"- A\n  - B\n  - "`／`14` | 両方 | `"- A\n  - B\n- "` | `12`（3行3列） |

末尾ケースでは出力文字列とキャレットに加え、計画が `prefix_range` だけを空文字列に置換することも確認する。UI では既存の実キー Backspace テストを使い、操作直後の原文とキャレットを確認する。

## 変更するファイルと既存テストの扱い

- **`crates/markdown/src/list_editing.rs:938`** — 製品コードの変更は `exit_top_level_item` のみ。テストは上表を追加し、旧 EOF 挙動を固定するケースと `empty_top_level_exit_contract_matrix` の **E01 / E05 / E09 / E12 / E14** の期待値を改める。途中の項目、文書先頭、既存空行を再利用するケースは変更しない。
- **`crates/ui/src/view.rs:12637`** — テストのみ。旧 EOF の文字列・座標を前提とする表示、実キー、CRLF、Undo/Redo、タスク項目、UTF-8 の期待値を新仕様に合わせる。連続 outdent テストは最初の操作を変えず、2回目の最上位離脱だけ更新する。
- **`docs/adr/0029-source-first-list-editing.md:284`** — 空項目の Enter と Backspace の契約に末尾の例外を明記する。**末尾離脱直後に文字を打つと、直前の項目の lazy continuation になり得る。** マーカーだけを消す新仕様と、末尾でも入力後に必ず独立段落になるという旧保証は両立しない。旧 EOF テストの独立段落アサーションは末尾ケースに限って外し、独立段落を検証する IME 等のテストは必要なら後続本文のある入力で検証を維持する。途中の #460 受入テストは変更しない。

`plan_list_edit` の分岐・安全ガード、`transform_subtree`、UI キーバインド、前方削除、履歴処理は変更しない。`crates/editor/src/lib.rs:980` の履歴テストはリストプランナーを呼ばず、明示的に渡した置換を検証するため、今回の変更対象外とする。

## 司令塔の確認コマンド

```sh
cargo test -p hane-markdown list_editing::tests::
cargo test -p hane-ui real_backspace_keystroke_exits_
cargo test -p hane-ui list_exit
cargo test -p hane-ui source_first_list_editing_
cargo test -p hane-ui top_level_
```

調査はローカルの `9967e9fa` を読み取り専用で実施した。ファイル変更・テスト実行は行っていない。GitHub の Issue 本文は公開 Web から取得できなかったため、Issue の要件は依頼文、現行挙動と既存テストの期待値はローカルソースを根拠にした。