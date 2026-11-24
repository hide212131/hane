# Issue #765 実装準備：Tab・Shift+Tabで移動した番号付き項目の採番

状態： 実装準備（未実装・未検証）。この文書は実装担当への引き継ぎであり、後続の依頼で実装する。

## 司令塔の補足（オーナー判断 2026-11-25）

- **インデント幅は現行規則を維持する。** 下記設計の「番号付き項目が新規の子リストを作る場合は、親マーカー列から4列下げた位置と親のコンテンツ開始列のうち深い方を使う」という規則は採用しない。移動先の列は、現行の Indent / Outdent 計画が決める列（Indent は直前の兄弟の `body_indent_columns` に揃える）をそのまま使う。
- そのため、設計書のテスト表の期待インデントは現行規則で再計算すること。基本ケースは `1. abc\n2. def\n3. g│hi` → `1. abc\n2. def\n   1. g│hi`（3空白）になる。桁数変化ケースの継続行補正は、新旧のコンテンツ開始列の差（インデントの移動量とマーカー幅の変化の合算）で計算する点は設計どおり。
- それ以外の設計判断（採番式、数字部分だけの差し替え、キャレットのバイト境界写像、タスク保持、対象外）はすべて有効。

# Issue #765 実装設計書：Tab・Shift+Tabで移動した番号付き項目の採番

**採用方針:** 移動したサブツリーの先頭項目だけを、移動先での意味的な番号に書き換える。桁数やマーカーの位置が変わったときは、配下の構造が壊れないようにインデントも補正する。子孫・後続項目の番号、元の区切り文字、タスクのチェック状態は変更しない。

設計基準は GitHub の `main` と一致する `9967e9fa`。既存の source-first 編集計画を使い、Issue #714 の改行保持と Issue #728 の Enter の契約を引き継ぐ。新たな先行リファクタリングは不要。本回答は読み取りに基づく設計であり、ファイル変更・テスト実行は行っていない。

## 1. 移動先と番号

項目自身の意味的な番号は **`list_start + ordinal`**。`ordinal` はゼロ始まりなので、直前項目の次に置く番号は **`list_start + ordinal + 1`** とする。元のソースに書かれた直前項目の数字は採番に使わない。`item_prefix` は Enter 用なので、採番式の意味は揃えるが、Tab のマーカー再構築には使わない。`crates/markdown/src/syntax.rs:245`、`crates/markdown/src/list_editing.rs:1032`

| 操作 | 移動先と採番 |
|---|---|
| `Indent`：既存の子リストに追加 | `previous_sibling` のインデックスを取得する。`parent_item == Some(そのインデックス)` かつ `depth == previous.depth + 1` の直下項目から、ソース上で最後の項目を調べる。それが同じ区切り文字の番号付きリストの末尾で、移動元との間に空白以外の別ブロックがなければ、その `list_range` の最終項目の `list_start + ordinal + 1` を使う。末尾項目の `marker_indent_columns` に揃える。 |
| `Indent`：新規の子リスト | 直下項目がない、最後の子リストが箇条書き・異なる区切り文字、または間に別ブロックがある場合は `1`。以前に現れた同じ区切り文字のリストまで遡って採番しない。 |
| `Outdent` | `parent_item` が指す元の親を、新しい直前項目として扱う。親が番号付きなら `parent.list_start + parent.ordinal + 1`、箇条書きなら新規の番号付きリストとして `1`。列は元の親の `marker_indent_columns` に揃える。親側と区切り文字が異なる場合も番号は親側の続きとし、同一の構文リストに結合するために区切り文字を変更しない。 |

子リストの候補は parser が確定した項目に限る。末尾判定には `subtree_range.end` と移動元の開始位置の間のソースを使い、`list_range` の一致は同一リストの識別に使う。これにより、孫、別の子リスト、後続の段落を直前項目に誤採用しない。`crates/markdown/src/list_editing.rs:77`、`crates/markdown/src/list_editing.rs:390`、`vendor/pulldown-cmark/src/firstpass.rs:1569`

**再現例の4空白は明示的に実装する。** 現行の `previous.body_indent_columns` だけでは、`2. def` の本文開始列は **3** であり、数字だけ直しても期待の4空白にはならない。番号付き項目が新規の子リストを作る場合は、親マーカー列から4列下げた位置と、親の構文上のコンテンツ開始列のうち、深い方を使う。既存の子リストに入る場合はそちらの実際の列を優先する。箇条書き項目を移動する経路のインデント規則は変えない。`crates/markdown/src/list_editing.rs:560`

採番前に既存の `NoOp` / `NotApplicable` 判定を維持する。未完成マーカーの回復情報だけでは移動先を確定できない場合、推測した番号を書かず、既存の空白移動に委ねる。計算した番号が parser の上限である9桁を超える場合は、無効なマーカーを書かず選択を維持した `Handled(NoOp)` とする。`vendor/pulldown-cmark/src/scanners.rs:307`

## 2. 先頭行と継続行の書き換え

- **先頭行:** インデントを移動先の列に合わせ、`marker_range.start` から連続する ASCII 数字だけを差し替える。直後が元の `.` / `)` であることを確認し、区切り文字以降はコピーする。`marker_range` 全体や `prefix_range` 全体は置換しない。これにより `[ ]`、`[x]`、`[X]` とその後の本文を保持できる。`crates/markdown/src/list_editing.rs:192`
- **2行目以降:** 一様シフトだけでは不十分。**新旧の構文上のコンテンツ開始列の差**を、同じサブツリー内の継続行・別段落・子孫行に適用する。例えば `10.` → `1.` を伴う4列の Indent では、旧4列にある継続行は新7列に置く。逆に `1.` → `10.` で1列足りなくなると、空行後の段落や子リストが項目外へ出得る。直後の段落行が lazy continuation として残る場合があっても、それを構造保持の根拠にはしない。`vendor/pulldown-cmark/src/scanners.rs:307`、`vendor/pulldown-cmark/src/parse.rs:1460`
- **列幅:** 構文上の開始列はマーカーと構文上の区切り空白から求める。チェックボックスまで含む `body_indent_columns` や、数字のバイト長差だけでは代用しない。既存の `PrefixCursor::list_item` の空白規則を再利用し、タブストップを考慮する。継続行は旧開始列からの相対列を保つ。lazy continuation は不足していた列数を維持し、負の列にはしない。先頭空白にタブがあれば、必要な範囲だけ空白で組み直して列を合わせる。マーカー直後のタブが移動で構文上の空白幅と本文側の境界を変える場合は、同じ境界を保つのに必要な空白だけを補正する。チェックボックスと非空白の本文はそのままコピーする。`crates/markdown/src/markers.rs:99`、`crates/markdown/src/markers.rs:143`
- **置換範囲:** 現行の `subtree_range` に対する1回の `MarkdownEditPlan::Replace` を維持する。子孫は先頭空白だけを調整し、番号には触らない。空行を埋めず、`split_physical_lines` が返す各行の終端子をそのまま戻す。置換範囲外のソースは変更しない。`crates/markdown/src/list_editing.rs:2693`、`crates/markdown/src/list_editing.rs:2733`

## 3. キャレット

`caret ± delta` では足りない。`delta` は列の差であり、空白のバイト数や番号の桁数とは一致しない。**先頭行の旧バイト境界から新バイト境界へ**、次の規則で写像する。

| 旧キャレット位置 | 移動後 |
|---|---|
| マーカーより前のインデント内 | 旧インデント末尾からの距離を移動後のインデント内で維持し、範囲外は行頭に丸める。行頭は行頭に、マーカー開始境界は新マーカー開始境界に写す。旧インデントがゼロの場合、その境界はマーカー開始として扱う。 |
| 数字の内部 | 数字先頭からのバイト位置を維持し、新しい数字の末尾を上限にする。旧数字の末尾は新数字の末尾に写す。 |
| 区切り文字以降 | 編集していないソース上の同じ境界を指す。通常は先頭空白と数字の実バイト長差を足す。区切り空白を補正した場合はその差も含め、チェックボックス内・本文中の位置を保持する。 |

オフセットは絶対 UTF-8 バイト位置とし、`selection_after` の anchor / active は同じ値にする。Tab / Shift+Tab が先頭行だけを対象とする既存の判定は維持する。`crates/markdown/src/list_editing.rs:524`

## 4. 変更箇所と対象外

- `crates/markdown/src/list_editing.rs:524` — `plan_list_edit` の `Indent` / `Outdent` から、移動先を解決する `ordered_move_target` と、番号付き移動を組み立てる `transform_ordered_subtree`（いずれも新規・非公開）を呼ぶ。既存の `transform_subtree` は箇条書きと空項目の Enter / Backspace のために残す。同ファイルの `tests` に回帰テストを追加する。
- `crates/markdown/src/markers.rs:143` — `PrefixCursor::list_item` を呼び、既知のマーカー列から構文上のコンテンツ開始列を得る内部ヘルパーを追加する。空白規則を複製せず、parser や表示用の既存ロジックは変えない。
- `crates/ui/src/view.rs:26031` — 番号付き項目について実キー dispatch の回帰テストを1件追加する。Tab、Shift+Tab、Undo でソースと選択が一致することを確認する。UI の製品コードやキーバインドは変更しない。

**対象外:** 子孫・後続項目の一括採番、区切り文字の統一、箇条書きの移動規則、Enter / Shift+Enter / Backspace の挙動、引用コンテナの移動規則の見直し、非空選択への対応、表示ラベル・保存形式・Undo の仕組みの変更。公開型と `ListEditProjection` のフィールドは増やさない。

## 5. 回帰テスト

`|` はキャレットで、入力・期待ソースには含まない。括弧内は先頭からの **0始まり UTF-8 バイト位置**。`\n` と `\r\n` は実際の改行を表す。

| ケース | 入力 | Intent | 期待結果 |
|---|---|---|---|
| 基本・4空白 | `1. abc\n2. def\n3. g│hi` (18) | `Indent` | `1. abc\n2. def\n    1. g│hi` (22) |
| 既存の子リスト・ソース番号と意味番号が異なる | `1. A\n\n    5. N\n    99. M\n8. B│` (29) | `Indent` | `1. A\n\n    5. N\n    99. M\n    7. B│` (33) |
| 親側の続き・後続番号を温存 | `3. A\n   1. B│\n77. C` (12) | `Outdent` | `3. A\n4. B│\n77. C` (9) |
| 2桁→1桁・継続行・UTF-8 | `9. A\n10. 日│本\n    cont` (12) | `Indent` | `9. A\n    1. 日│本\n       cont` (15) |
| 1桁→2桁・別段落と子孫を保持 | `9. A\n   1. B│\n\n      P\n\n      7. C\n9. Z` (12) | `Outdent` | `9. A\n10. B│\n\n    P\n\n    7. C\n9. Z` (10) |
| `)` 区切り | `1) A\n2) B│` (9) | `Indent` | `1) A\n    1) B│` (13) |
| 異なる区切り文字の子リストには結合しない | `1. A\n\n    5) N\n2. B│` (19) | `Indent` | `1. A\n\n    5) N\n    1. B│` (23) |
| 子孫の番号はそのまま | `9. A\n10. B│\n\n    7. C\n    42. D` (10) | `Indent` | `9. A\n    1. B│\n\n       7. C\n       42. D` (13) |
| CRLF | `1. A\r\n2. B│\r\n` (10) | `Indent` | `1. A\r\n    1. B│\r\n` (14) |
| 未チェックタスク | `1. A\n2. [ ] B│` (13) | `Indent` | `1. A\n    1. [ ] B│` (17) |
| チェック済みタスク・接頭部内キャレット | `9. A\n10. [X│] B` (11) | `Indent` | `9. A\n    1. [X│] B` (14) |
| 数字内キャレット | `9. A\n1│0. B` (6) | `Indent` | `9. A\n    1│. B` (10) |
| 先頭項目 | `1. A│\n2. B` (4) | `Indent` | 同一ソース・同一選択 (4)、`Handled(NoOp)` |

表の `│` も同じキャレット記号。文字列と位置に加え、再パースして、移動項目の親・`list_range`・`ordinal`、継続段落と子孫の所属、タスクのチェック状態を検証する。実キー dispatch テストでは `9. A\n10. B│` (10) → Tab で `9. A\n    1. B│` (13) → Shift+Tab で元のソースと位置 (10) → Undo で入れ子状態と位置 (13)、を確認する。

## 6. 司令塔の確認コマンド

新規テスト名は `ordered_move_` で始め、GUI テストは `ordered_move_through_real_key_dispatch` とする。

```sh
cargo test -p hane-markdown --lib list_editing::tests::ordered_move_
cargo test -p hane-markdown --lib list_editing::tests::
cargo test -p hane-ui --lib ordered_move_through_real_key_dispatch
```

まず新規ケース、次に既存のリスト編集全体、最後に GPUI が実行できる環境で実キー経路を確認する。受入時は対象 head の Debug ビルドで、再現例の Tab・Shift+Tab と桁数変化後の本文キャレットを確認し、planner テストの結果と区別して記録する。