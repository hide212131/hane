# Issue #714 設計書ドラフト

## 現行挙動の確認

確認基準は v0.46.0、作業ツリーの HEAD / origin/main ともに `f445a2933a4c0178ec6207be5856d012d0724b59`。以下の位置はすべて **UTF-8 のバイトオフセット**とする。実装・ファイル変更・テスト実行は行っていない。

### Enter と Shift+Enter

`plan_list_edit` は計画だけを生成し、呼び出し側がソース置換として適用する。

- Enter 継続は `format!("\n{prefix}")` をキャレット位置へ挿入する。元の終端子は後方に残るため、CRLF 行へ LF が混入する。参照: `crates/markdown/src/list_editing.rs:632`
- Shift+Enter も `"\n"` と本文インデントを挿入しており、同じ原因を持つ。参照: `crates/markdown/src/list_editing.rs:577`
- 両分岐とも、新しいキャレットを挿入文字列のバイト長から算出している。この算出方式は維持できる。
- 継続行は現在 `on_opening_line` を満たさず、これらの分岐の対象外になる。今回、この適用条件は変更しない。参照: `crates/markdown/src/list_editing.rs:519`

### 空項目の exit

トップレベルの空項目は `exit_top_level_item` で処理する。既存契約は次のとおり。

- 項目 prefix だけを置換し、既存の終端子を保存する。
- 左右それぞれに段落境界が必要か判定する。
- 左の区切りは直前行の終端子、右の区切りは空項目自身の終端子から作る。
- キャレットは左区切りの後、右区切りの前に置く。

参照: `crates/markdown/src/list_editing.rs:699`

`trailing_empty_top_level_exit_preserves_crlf` は Backspace の CRLF exit を確認している。Enter の連続操作は未カバーである。参照: `crates/markdown/src/list_editing.rs:1104`

ネストした空項目は `transform_subtree` で outdent し、各物理行の終端子をそのまま保存する。参照: `crates/markdown/src/list_editing.rs:617`、`crates/markdown/src/list_editing.rs:1979`

## 確定する意味

### 1. 分割する物理行の終端子を優先する

Enter / Shift+Enter が新設する区切りは、次の優先順位で選ぶ。

| 状態 | 挿入する終端子 |
|---|---|
| 分割対象行に終端子がある | その行の終端子 |
| 対象が無終端の最終行で、直前行がある | 直前行の終端子 |
| 文書内に参照できる終端子がない | LF |

対象行の終端子がキャレットより後ろにあっても、それを採用する。前の行の終端子や文書全体の多数派は優先しない。

たとえば、前行が CRLF、対象行が LF なら、挿入する区切りも LF とする。既存の混在を正規化せず、分割対象行の方式を保存する。

終端子のない最終行には「自身の終端子」がないため、直前行を局所的な補完値として使う。この規則により、CRLF 文書の末尾に最終改行がなくても CRLF を継承できる。

### 2. 空項目 exit の既存契約を維持する

今回の再現は、次の値で固定する。

| 操作 | テキスト | キャレット |
|---|---|---:|
| 初期状態 | `"- abc\r\n"` | 5 |
| Enter 継続 | `"- abc\r\n- \r\n"` | 9 |
| 空項目で Enter | `"- abc\r\n\r\n\r\n"` | 9 |

**exit 後は CRLF が3個になる。** 左の段落境界用に CRLF を追加し、空項目が持っていた末尾 CRLF も保存するためである。キャレットは2個目の CRLF の直後、3個目の直前に置く。ここで `x` を入力すると、`"- abc\r\n\r\nx\r\n"` になる。

Issue 本文の `"- abc\r\n\r\n" 相当` は、混在しないという要求の例として扱い、本設計では上表を厳密な期待値とする。2個へ減らす変更は、既存終端子を保存する exit 契約の変更になるため含めない。

末尾終端子がない `"- abc\r\n- "` の exit は、既存どおり `"- abc\r\n\r\n"`、キャレット9となる。

### 3. Shift+Enter も今回修正する

Enter と同じ終端子選択を適用する。両者は同じ計画層にあり、原因も LF 固定挿入で共通している。共有ヘルパーの利用により一緒に修正でき、リストの意味変更も不要である。

prefix・インデント、空項目判定、適用可能な行の範囲は従来どおりとする。

### 4. 混在済み文書は保持する

- 新設する区切りは対象行の終端子を複製する。
- 既存の終端子は書き換えない。
- exit は左右それぞれの終端子を使う。
- outdent は各物理行の終端子を保存する。

これにより、既存の「自側の終端子」という原則へ揃える。参照: `crates/markdown/src/list_editing.rs:1272`、`crates/markdown/src/list_editing.rs:2013`

## 実装方針（変更する層と分岐）

### 改行選択の共有ヘルパー

`crates/markdown/src/list_editing.rs` に、次の純粋関数を private helper として置く。

```rust
fn preferred_line_terminator(source: &str, at: SourceOffset) -> &str
```

責務は、**`at` を含む物理行の分割時に挿入する終端子の選択だけ**とする。リストの所有者、prefix、exit、キャレット移動は判断しない。

具体的な処理形は次のとおり。

1. `line_start(source.as_bytes(), at.0)` で対象物理行の先頭を得る。
2. `line_end` と `line_end_inclusive` で本文末尾と終端子直後を得る。
3. 終端子があれば、そのスライスを返す。
4. 終端子がなく行頭が0より大きければ、`line_terminator_before(source, start)` を返す。
5. それ以外は `"\n"` を返す。

既存の `line_terminator_before` は「直前行」を読むため、対象行の選択にそのまま使うと混在文書で誤る。最終無終端行の補完に再利用する。

参照: `crates/markdown/src/list_editing.rs:755`、`crates/markdown/src/list_editing.rs:2099`、`crates/markdown/src/list_editing.rs:2115`

入力位置は有効なソース境界で、CRLF の2バイト間を分割位置として渡さない契約とする。今回のリスト分岐は終端子の前で動作する。

### 変更対象

| 分岐・層 | 変更 |
|---|---|
| Enter 継続 | 選択した終端子＋既存の `item_prefix` を挿入 |
| Shift+Enter | 同じ終端子＋既存の本文インデントを挿入 |
| トップレベル exit | ロジックを維持し、Enter の回帰テストを追加 |
| ネストした exit / outdent | 既存の終端子保存処理を維持 |
| UI・editor | 今回の製品コード変更は不要 |

挿入計画は従来どおり空範囲への置換とし、キャレットを `caret + replacement.len()` に置く。CRLF の2バイトを計算へ反映できる。

UI は計画を `replace_range_recorded` で適用するため、既存の undo / redo 経路を使う。参照: `crates/ui/src/view.rs:3596`

### リスト以外の Enter 経路

確認範囲は、本文の Enter / Shift+Enter action、リスト計画の適用と fallback、editor の挿入処理、および関連する固定改行挿入のソース検索とする。

確認結果は次のとおり。

- 本文 Enter / Shift+Enter は、リスト計画が非適用なら `insert_newline` へ進む。参照: `crates/ui/src/actions.rs:359`、`crates/ui/src/actions.rs:394`
- `insert_newline` は `EditorCommand::Insert("\n")` を送る。**通常段落にも同種の LF 固定が存在する。** 参照: `crates/ui/src/view.rs:3663`
- editor の Insert は渡された文字列を挿入する責務である。CRLF を一括挿入した場合の undo テストも既にある。参照: `crates/editor/src/lib.rs:276`、`crates/editor/src/lib.rs:1058`

通常段落、選択範囲置換、リスト計画の非適用経路まで変えると別の編集契約に広がるため、通常 Enter の終端子選択は別件に分ける。#714 の完了を「すべての Enter 経路で CRLF を維持する」とは表現しない。

### Issue #728 との接続

#728 は、リスト左端と継続行で「何を分割・終了するか」を決める変更である。#714 は、その操作で「どの終端子を挿入するか」を提供する。

#728 は同じ `preferred_line_terminator(source, at)` を利用する。対象位置は項目の opening line に固定せず、**実際に分割する物理行の位置**を渡す。継続行の終端子が opening line と異なる場合も、その継続行の方式を選べる。

単なる改行挿入ではない段落 exit を選ぶ場合は、境界の必要性と左右それぞれの終端子を別に判断する。新設ヘルパーで既存 exit の左右を一律に置き換えない。

#728 では次を守る。

- 改行選択規則を分岐ごとに複製しない。
- 左端・継続行の操作を LF 固定の UI fallback に任せない。
- #714 の回帰テストを維持し、#728 の新しい意味に CRLF・混在行のケースを追加する。

共有範囲は当面 `list_editing.rs` 内で十分であり、#728 のためだけに public API や document 層への移設は行わない。

## テスト計画

### 計画単体テスト

追加先は `crates/markdown/src/list_editing.rs` の既存テスト群。`replacement` ヘルパーを利用し、全文とキャレットを比較する。参照: `crates/markdown/src/list_editing.rs:817`

以下では `E` を Enter、`S` を Shift+Enter とする。

| テスト名 | 入力・キャレット・操作 | 期待テキスト | 期待キャレット |
|---|---|---|---:|
| `enter_continuation_preserves_crlf` | `"- abc\r\n"`, 5, E | `"- abc\r\n- \r\n"` | 9 |
| `enter_split_mid_body_preserves_crlf` | `"- abcd\r\n"`, 4, E | `"- ab\r\n- cd\r\n"` | 8 |
| `enter_continuation_preserves_lf` | `"- abc\n"`, 5, E | `"- abc\n- \n"` | 8 |
| `enter_continuation_preserves_bare_cr` | `"- abc\r"`, 5, E | `"- abc\r- \r"` | 8 |
| `enter_continuation_at_unterminated_eof_inherits_crlf` | `"- A\r\n- bc"`, 9, E | `"- A\r\n- bc\r\n- "` | 13 |
| `enter_continuation_without_any_terminator_defaults_to_lf` | `"- abc"`, 5, E | `"- abc\n- "` | 8 |
| `enter_mixed_endings_uses_current_lf` | `"- A\r\n- bc\n"`, 8, E | `"- A\r\n- b\n- c\n"` | 11 |
| `enter_mixed_endings_uses_current_crlf` | `"- A\n- bc\r\n"`, 7, E | `"- A\n- b\r\n- c\r\n"` | 11 |
| `shift_enter_preserves_crlf` | `"- abc\r\n"`, 5, S | `"- abc\r\n  \r\n"` | 9 |
| `shift_enter_mixed_endings_uses_current_lf` | `"- A\r\n- bc\n"`, 8, S | `"- A\r\n- b\n  c\n"` | 11 |
| `shift_enter_at_unterminated_eof_inherits_crlf` | `"- A\r\n- bc"`, 9, S | `"- A\r\n- bc\r\n  "` | 13 |
| `ordered_enter_preserves_crlf_and_next_number` | `"3. A\r\n4. B"`, 4, E | `"3. A\r\n4. \r\n4. B"` | 9 |
| `task_enter_preserves_crlf_and_unchecks_marker` | `"- [x] A\r\n"`, 7, E | `"- [x] A\r\n- [ ] \r\n"` | 15 |
| `enter_crlf_uses_utf8_byte_offsets` | `"- 日本\r\n"`, 8, E | `"- 日本\r\n- \r\n"` | 12 |

補完規則はヘルパー単体でも、LF / CRLF / CR の直前行、終端子のない単一行、空文書を確認する。混在した無終端最終行では、文書先頭や多数派より**直前行**を選ぶケースを含める。

### exit と連続操作

| テスト名 | 入力・キャレット・操作 | 期待テキスト | 期待キャレット |
|---|---|---|---:|
| `enter_crlf_continuation_then_exit_preserves_boundaries` | `"- abc\r\n"`, 5, E→再解析→E | `"- abc\r\n\r\n\r\n"` | 9 |
| `enter_exit_unterminated_empty_item_preserves_crlf` | `"- abc\r\n- "`, 9, E | `"- abc\r\n\r\n"` | 9 |
| `enter_exit_only_empty_crlf_item_preserves_terminator` | `"- \r\n"`, 2, E | `"\r\n"` | 0 |
| `enter_exit_mixed_endings_preserves_each_side` | `"3. A\r\n4. \n5. C"`, 9, E | `"3. A\r\n\r\n\n\n5. C"` | 8 |

連続操作テストでは、1回目のテキストとキャレット9も assert し、その結果から projection を再構築する。exit 後に `x` を挿入し、`"- abc\r\n\r\nx\r\n"` になること、再解析後の `x` がリスト外の段落になることまで確認する。

既存の `trailing_empty_top_level_exit_preserves_crlf` は Enter / Backspace の両方で実行する形へ拡張できる。既存の LF、保護対象の NoOp、ネスト outdent、混在終端子テストも維持する。

実装後の主検証は `cargo test -p hane-markdown` とする。

### UI 統合

計画単体が主検証だが、連続操作で projection 更新と履歴が関与するため、既存の source-first list editing テスト付近へ focused GPUI テストを1件追加する。参照: `crates/ui/src/view.rs:11716`

確認内容は次のとおり。

1. `"- abc\r\n"`、キャレット5から Enter を2回適用する。
2. 中間値・最終値・キャレットが上表に一致する。
3. Undo で1回目の結果、もう一度 Undo で初期状態へ戻る。
4. Redo で両段階を復元する。

このドラフトでは UI テスト・GUI 操作は未実施であり、結果の evidence とはしない。

## 未確定事項

- #714 の改行選択、Shift+Enter の包含、exit の最終テキストとキャレットは本案で固定する。Issue 本文へ反映する際は、exit が **3個の CRLF・キャレット9** となる理由も記載する。
- 通常段落の LF 固定を扱う follow-up の Issue 番号と、将来の共有ヘルパー移設先は未確定。#714 のリスト修正の前提にはしない。
- #728 の継続行に関する受入条件は同 Issue の設計で確定する。本案は操作の意味を先取りせず、物理行に基づく改行選択を共有する。
## Commander 追記（2026-11-15、設計確定）

ドラフト作成時点では Jev 判断は未取得だったが、その後 Commander が Jev で確認した: 空項目 exit の期待値（既存契約を保存し CRLF 3個・キャレット9）は Jev が確率 0.96 で支持。本設計を確定とする。