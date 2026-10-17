# Issue #460: 空リスト項目の終了処理 — 実装仕様

## 1. 状態と実装の入口

対象は [Issue #460](https://github.com/hide212131/hane/issues/460)。本書は具体的な変更箇所、入力条件、原文の変換、検証方法の正本とする。目的・範囲・受入条件の正本はIssue、恒久的な責務分担は [ADR-0029](adr/0029-source-first-list-editing.md) とする。

**設計保存時点では製品コード未変更だった。** その後の実装・検証状態はIssue #460と製品PR #472を参照する。Issue ownerは2026-10-12に受入条件を明確化し、focused GUI確認はmacOSのみ必須とした。Windows実画面確認は不要。Windows/macOS CIとWindows改行形式のテストは維持する。この指定は以下に残る「両OSのfocused GUI」記述より優先する。

調査日: 2026-10-12。コード読解基点は `ca9b1b61e0317101f5f994f7aa3259431056584d`。保存ブランチの基点は、調査中に進んだmain `b3e8d307f57ab0de7e321261e5ecc29a99a7f039`。両者の差分はPR #450のAI選択操作の導入で、`actions.rs`、`view.rs`、`capture.rs`の入力制御などが変わっている。旧ファイルを丸ごと復元して実装しない。

最初の実装作業は、**第8節のE01を現在のHaneの編集計画とparserを使う失敗テストにし、原文・カーソル位置・追加入力後の所属を確認すること**。設計から作り直さず、次に第5節の終了処理へ進む。設計保存PRには製品コードを追加せず、製品修正は別PRにする。

## 2. 今回変えること、変えないこと

空の最上位リスト項目をEnterまたは後退削除で解除した後、普通の文字を入力しても前の項目の続きへ戻らないようにする。必要な区切りを終了操作で原文へ入れる。利用者に追加のEnter操作を求めない。

原文を描画の都合で解釈し直さず、次の経路を維持する。

```text
本文のキー入力
  → EditorView::apply_list_edit_intent
  → current sourceのListEditProjection
  → plan_list_edit / MarkdownEditPlan
  → Editor::replace_range_recorded
  → after_input / 既存の解析・表示・保存
```

最上位になる前の空の入れ子項目は、既存の一階層戻す処理を使う。既存のEnterによる次項目生成、Shift+Enterによる項目内改行、Tab/Shift+Tabによる階層変更を作り直さない。PowerPoint全操作の完全互換、複数項目選択の新規操作、非空項目の新しい解除・結合、番号の自動書換え、チェックボックスUI、引用内リストの終了機能の新設は対象外とする。

後退削除は既存の`Backspace` action（WindowsのBackspace、macOSの通常のDelete）を使う。前方削除の`Delete` actionへ終了機能を追加しない。入力途中の原文を直接編集する操作と、空項目の本文開始位置から行う終了操作を分ける。

空行の原文を保持したまま、通常段落と同じ横位置で文字を入力できることが要求である。段落の縦余白をPowerPointと同じピクセル値にする変更や、空行を隠す専用描画は追加しない。

## 3. 確認したコードと原因

以下の行番号は読解基点での目安。実装ではシンボル名で探し、現在の配置を使う。

| 場所 | 確認した事実 | 今回の扱い |
|---|---|---|
| `crates/markdown/src/list_editing.rs` / `plan_list_edit`（約460〜600行） | 空項目のEnter/Backspaceを同じ分岐で扱い、最上位では`prefix_range`を空文字へ置換する。 | 終了の変換と適用条件を修正する中心。 |
| 同ファイル / `ListEditItem`、`build_list_edit_projection` | parserが認識した項目と、入力途中の記号を文字列から回復した項目を同じ型へ入れる。現在はその由来を残していない。 | 第4節の小さな識別情報を追加する。 |
| 同ファイル / `body_range`、`subtree_range` | `body_range`は開始行の本文だけ。開始行が空でも子項目・継続本文を持ち得る。 | 開始行の空白判定だけで最上位を解除しない。 |
| 同ファイル / `line_start`、`line_end`、`line_spans` | CRとLFを認識する。`line_spans`はCRLFを一組として進む。 | 新規区切りもCRLFを途中で分割しない。 |
| `crates/markdown/src/block_index.rs` / `list_edit_projection_at`（約950行） | bufferとindexのrevision一致を確認し、現在のブロックを同期解析する。 | API・鮮度条件を変えずに利用する。 |
| `crates/ui/src/actions.rs` | Enter/Backspaceは既存のリスト編集入口へ渡す。前方Deleteは通常編集。設定・検索・AI指示などに入力を渡す制御がある。 | 製品変更は原則不要。最新の入力制御を保持する。 |
| `crates/ui/src/view.rs` / `apply_list_edit_intent` | IME中は構造編集しない。NoOpはキーを消費し、ReplaceをEditorへ一回渡して`after_input`を呼ぶ。 | 一回の適用経路を維持する。 |
| `crates/editor/src/lib.rs` / `replace_range_recorded`（約264行） | 履歴の区切り、原文置換、選択の更新、Undo記録を行う。 | Markdownを知らない既存処理をそのまま使う。 |

現在の終了後に`ghi`を入れると、原文は`- abc\n- def\nghi`となる。CommonMarkでは`ghi`は前項目の段落継続になり得る。修正結果は`- abc\n- def\n\nghi`とする。[S1]

前方だけでなく後方の境界も必要である。例えば`3. A\n\nghi\n5. C`では、`5. C`が通常段落の続きになる。途中の空項目を解除する場合は、必要なら後方にも区切りを用意する。後続の原文記号や番号を変更して解決しない。[S1]

なお、現行plannerは`document.full_text()`で全文Stringを作り、UIにはindexがない場合の`BlockIndex::from_buffer`もある。本書はそれを高速化済みとは扱わない。今回、新しい全文コピー・全文再解析を追加しないことと、既存のコストをなくすことは別である。

## 4. 適用条件と安全側の扱い

### 4.1 parserによる項目と入力途中の回復を区別する

`ListEditItem`に、例えば`pub is_parser_backed: bool`を一つ追加する。名称は同じ意味なら調整できる。

- `parsed.list_item_markers`から作る項目は`true`。
- 後半の`raw_list_marker`による回復から作る項目は`false`。
- sortや親・兄弟の再計算でこの値を推測し直さない。表示用`ListProjection`へ複製しない。

**新しい最上位終了処理はparserが認識した項目だけに適用する。** 単に原文の行頭に`- `があっても、コード内や通常文の入力途中である可能性がある。回復した最上位項目に新しい段落境界を作る処理を適用せず、`NotApplicable`として通常の文字編集へ返す。

既存の入れ子空項目の回復と一階層戻す処理は維持する。一階層戻した直後に最上位の`- `が原文へできた場合、次の操作ではcurrent sourceから再構築し、通常のparser認識を使う。背景解析を待つ追加条件にしない。この連続操作は必ずテストする。

関数の引数・戻り値と既存re-export名は維持する。ただし公開structへのフィールド追加は、struct literalで構築する利用者にとって変更になる。実装開始時に`rg -n 'ListEditItem' crates`で全構築・比較箇所を確認し、必要な初期値だけを追従する。公開APIが完全にbyte-identicalとは報告しない。新しい公開enumや汎用状態管理は不要。

### 4.2 操作位置

新しい終了処理の対象は、選択範囲がなく、項目の開始行上で、空本文の開始位置にカーソルがある場合とする。`-`だけで空白がない有効な空項目では、記号の直後もこの位置になる。

Backspaceの対象を`caret <= body_start`だけで広げず、本文開始位置との一致を確認する。記号の途中・記号より前からの操作は通常の原文編集へ返し、行全体を突然消さない。Enterも空項目の接頭辞の途中では通常の改行へ返す。Tab系の位置判定まで一緒に変えない。

日本語変換中かどうかは既存Editor/UIが判断する。Markdown層へIMEの型や状態を渡さない。

### 4.3 本当に空か確認する

開始行の本文が空、またはASCIIのspace/tabだけであることを確認する。`trim()`のUnicode空白判定を、削除してよい本文の判断には使わない。全角空白、改行以外の制御文字、見えにくいUnicode文字は本文として残す。

最上位を解除するときは、開始行の終端から項目の末尾までに本文・子項目・コード等がないことも確認する。`item_range`と`subtree_range`の有効な範囲を照合し、後続部分がspace/tab/CR/LFだけの場合に限る。別の兄弟項目や別段落へ範囲を広げて調べない。

次のように開始行だけが空の親項目は、最上位の終了対象ではない。

```text
- <caret>
  - child
```

同じく`- <caret>\n  body`も空項目ではない。この位置の終了操作では、`Handled(NoOp { selection_after: 元の選択 })`として文書・カーソル・履歴を変えない。原文を選択して編集する通常操作は妨げない。非空親項目を段落へ変換する新仕様は追加しない。

### 4.4 他の構造を巻き込まない

最上位終了に使う接頭辞のうち、行頭から当該リスト記号までがspace/tabだけであることを確認する。親がなく`depth == 1`であることとも照合する。字下げの列数だけで最上位と判定しない。

`> - <caret>`のように接頭辞へ引用記号が含まれる場合、今回の終了支援は行わずNoOpにする。引用の`>`を削除して外へ出たり、`>`を空白へ置換したりしない。引用内の空項目に対する安全な保護であり、引用内リストの終了機能を実装したとは扱わない。同一行へ親・子の記号を重ねた構造も、他の記号を含む接頭辞を一括削除しない。

有効な原文範囲・文字境界・親子関係を確認できない構造操作はNoOpにする。通常文やコードでリスト操作自体が成立しない場合のNotApplicableと区別する。NoOpをReplaceの空文字置換で代用せず、余計なrevisionやUndoを増やさない。

## 5. 原文とカーソルを作る規則

### 5.1 一回の置換で前後を分ける

終了対象の物理行について次を求める。すべて原文UTF-8のバイト位置で、半開区間を使う。

```text
p = 対象行の先頭
q = 対象行の内容の終端（改行文字を含まない）
previous_eol = 対象行の直前の改行列（存在すればLF / CRLF / CR）
current_eol  = qから始まる対象行の改行列（末尾改行なしなら存在しない）
```

前の物理行があり、その内容がspace/tabだけではない場合、`left_separator = previous_eol`とする。それ以外は空文字にする。前に既にある空行の文字や個数を削らない。

対象行の次に物理行があり、その内容がspace/tabだけではない場合、`right_separator = current_eol`とする。それ以外は空文字にする。末尾改行の直後が文書末なら「後続本文がある」とは扱わない。

結果は次の一つの計画である。

```text
range           = p..q
replacement     = left_separator + right_separator
selection_after = caret(p + byte_len(left_separator))
```

元の`current_eol`は置換範囲の外に残す。右側の区切りを生成する場合、カーソルはreplacement全体の末尾ではなく、左側と右側の区切りの間に置く。これにより、後から文字を入力すると後続のリストとは空行を挟んで分かれる。

例えば、`- A\n- <caret>\n- C`は`- A\n\n<caret>\n\n- C`にする。単にreplacementの末尾へカーソルを置くと、後方の区切りの下へ入力してしまうため不可。

新規helperは`plan_empty_top_level_exit`等の非公開関数として同じファイルに置く。上の計画だけを返し、bufferの変更、parserの追加呼出し、UI更新、履歴の直接操作をしない。

### 5.2 改行と空白

既存の行末を削除・正規化しない。左側に追加する区切りは直前の改行列、右側に追加する区切りは対象行の改行列をそのまま使う。混在改行でも文書全体をLFやCRLFへそろえない。

空行判定はASCIIのspace/tabに限定する。CRLFは一つの行末として進み、CRとLFの間をカーソル位置やrange端にしない。既存の空白だけの空行も、追加済みの区切りとして扱う。

これは終了する編集操作の規則であり、通常Enterが現在使う`Insert("\n")`を文書全体で変更するものではない。終了後に明示的にEnterを押した場合は既存の通常改行処理へ戻る。

### 5.3 文書の途中と番号

後続の同階層項目は、通常段落を入力した時点で別リストになる。後続の記号・番号・本文・子孫の原文は書き換えない。

後続の原文が`1. C`なら、新しく分かれたリストの開始番号は1となる。元リストの表示上の連番を維持するために原文を再採番する処理は追加しない。通常の自動継続で生成済みの`5. C`なら、開始番号5の新しいリストとして残る。

文書が`- `だけなら空文書にする。文書先頭へ不要な区切りを作らない。前後に既に空行があれば増やさず、既存の複数空行を一つに詰めることもしない。

前後の非空ブロックとの間には一つの空行を確保する。直前が見出し等で空行なしでも構文上区別できる場合まで、最小改行数を推定する専用parserは作らない。局所的で一貫した区切り方を優先する。

## 6. 空行時と入力後を混同しない

空行だけの時点では、parserが空のParagraphノードを作らない場合がある。また、文書途中の空行が前後のリストのsource rangeに含まれる場合もある。**終了直後に必ず空のParagraphノードが存在する、または`item_at`が必ずNoneになる、という受入条件を作らない。**

終了直後は、指定した原文・カーソル位置、対象の空記号が消えたこと、次の通常入力へ余計な記号や字下げを付けないことを検査する。入力後は実際に再解析し、追加文がListItemの子孫ではないParagraphであることを検査する。

Viewに「この空行は通常段落」という持続状態を追加しない。次回の文字入力やIME確定時に接頭辞を注入する方式も復活させない。保存時や再読込時にだけ区切りを補完しても不可。

## 7. 変更ファイルと非変更範囲

| ファイル | 実装PRで行うこと |
|---|---|
| `crates/markdown/src/list_editing.rs` | 項目の由来を表す1フィールド、終了条件、安全側の結果、前後の区切りを作る非公開helper、同居unit testsを追加・変更する。 |
| `crates/markdown/tests/list_editing_contract.rs`（新規案） | 第8節の原文・位置・解析結果のcontract testsを置く。既存の同目的テストがあればそちらへ追加してもよい。 |
| `crates/ui/src/view.rs`の既存テスト部 | 実際のUI→planner→Editor→after_inputによる連続入力・Undo/Redo・IMEの統合テストを追加する。テストだけの変更と製品メソッドの変更を区別する。 |
| `crates/presentation/tests/layout_contract.rs`等の既存契約テスト | 正しい原文から通常段落の横位置・SourceMap・caretの関係を検査する。既存fixture/helperを再利用する。 |
| `docs/adr/0029-source-first-list-editing.md` | 第10節の方針で、記号削除だけを指示する記載と空項目の定義を修正する。 |
| 本書、Issue #460、必要な既存の検証記録 | 実装結果と証拠への参照を更新する。 |

新crate、依存ライブラリ、設定、ファイル形式、独自parser、リスト専用EditorCommand、専用キャッシュ、AADWのworkflowは追加しない。`actions.rs`、`capture.rs`、`view.rs`の製品入力経路、`editor`、`session`、`presentation`の製品ロジックは原則変更しない。

正しい原文を渡しても表示や入力位置に独立した不具合が残る場合は、失敗する最小テストと変更箇所を示す。見た目だけのx補正や、未確認の広範な整理で埋め合わせない。今回の受入を妨げる場合は解消までIssueを閉じず、必要な設計追記を行う。

## 8. 決定的な原文テスト

`<caret>`はカーソルの説明表記で原文には含まれない。`\n`、`\r\n`、`\r`、`\t`はそれぞれ実際の改行・タブを表す。E01〜E16は原則、EnterとBackspaceの両方で同じ結果を検査する。

| ID | 操作前 | 終了直後の期待原文と位置 |
|---|---|---|
| E01 | `- abc\n- def\n- <caret>` | `- abc\n- def\n\n<caret>` |
| E02 | `- <caret>` | `<caret>` |
| E03 | `- abc\n- def\n\n- <caret>` | `- abc\n- def\n\n<caret>` |
| E04 | `- abc\n \t\n- <caret>` | `- abc\n \t\n<caret>` |
| E05 | `- abc\n- <caret>\n` | `- abc\n\n<caret>\n` |
| E06 | `- A\n- <caret>\n- C` | `- A\n\n<caret>\n\n- C` |
| E07 | `3. A\n4. <caret>\n5. C` | `3. A\n\n<caret>\n\n5. C` |
| E08 | `- A\n\n- <caret>\n\n- C` | `- A\n\n<caret>\n\n- C` |
| E09 | `- A\r\n- <caret>` | `- A\r\n\r\n<caret>` |
| E10 | `- A\r- <caret>\r- C` | `- A\r\r<caret>\r\r- C` |
| E11 | `- A\n- <caret>\r\n- C` | `- A\n\n<caret>\r\n\r\n- C` |
| E12 | `   - <caret>` | `<caret>` |
| E13 | `- 日本🙂\n- <caret>` | `- 日本🙂\n\n<caret>` |
| E14 | `- A\n-<caret>` | `- A\n\n<caret>` |
| E15 | `- <caret>\n2. C` | `<caret>\n\n2. C` |
| E16 | `- A\n\n\n- <caret>` | `- A\n\n\n<caret>` |

特にE01はrange `12..14`、replacement `"\n"`、変更後caret `13`。E06はrange `4..6`、replacement `"\n\n"`、caret `5`。E11はrange `4..6`、replacement `"\n\r\n"`、caret `5`。E13はUTF-8 range `13..15`、caret `14`とし、文字数で計算していないことを固定する。

各ケースでは計画をRopeBufferへ適用し、原文と位置を比較した後、その位置へ`ghi`を挿入してHaneの`parse_document`で解析する。該当本文がParagraphに含まれ、祖先にListItemがないことを確認する。E01では元リストに`abc`と`def`の2項目だけが残ることも確認する。数値NodeIdが別parseで一致することを要求しない。

E06/E07/E15では後続項目がリストとして残ることも検査する。E07は後方の区切りを省略すると失敗するため、前方だけの修正を検出できる。空行時のASTだけを比較してテストを終えない。

### 追加する保護・連続操作のテスト

| ID | 内容 | 期待 |
|---|---|---|
| G01 | `- A\n  - B\n  - <caret>`でBackspaceを2回、その後文字入力 | 1回目は最上位の空項目、2回目は原文の区切りを生成。A/Bの親子関係を維持し、追加文だけがリスト外。Enterでも同様。 |
| G02 | `- <caret>\n  body`、`- <caret>\n  - child` | 最上位終了はNoOp。子の文字・記号、カーソル、revision、Undoを保持。 |
| G03 | `> - <caret>`、`> - A\n> - <caret>` | 終了支援はNoOp。引用接頭辞を消さない。引用終了の成功ケースとは数えない。 |
| G04 | フェンス内の`- <caret>`、通常文中の未成立記号 | 新しい最上位終了計画を返さない。文字編集に必要な通常操作は行え、空行を構造的に増やさない。 |
| G05 | 非空本文、全角空白やNBSPだけの本文、非空選択、記号の途中のcaret | 本文を空と誤認して行全体を消さない。通常の文字編集を維持。 |
| G06 | `-`/`*`/`+`、`.`/`)`、開始0/非1/先頭ゼロ、marker後のspace/tab | 有効なparser-backed空項目で同じ終了規則。既存番号・記号の原文を正規化しない。 |
| G07 | 既存のtask情報を持つ項目と、子項目にtaskがある親 | 自分の認識済みtask prefixだけを対象とする。子taskを自分の空本文と誤認して削除しない。parserがtaskと認識していない`[ ]`等の文字を勝手に構造記号として消さない。 |
| G08 | Enter→Tab→空項目の一階層戻し→最上位終了を連続実行 | 背景正式解析を待たず、毎回current sourceを使う。回復用項目からの遷移が成立。 |
| G09 | Shift+Enter後の本文継続、既存ファイル`- A\nB`の読込 | 意図した項目内継続を維持。既存文書の継続行を読込時に通常段落へ自動整形しない。 |
| G10 | 原文先頭以外にあるブロックのprojection、無効なrange、CRLF境界 | 絶対byte offsetを正しく使う。範囲外panic・文字途中のcaret・原文損失を起こさない。 |

最初のテストでは現在の期待値を確認し、元から存在する別の不具合と本修正の回帰を分ける。ただしE01等の主要条件や原文損失を、既存不具合として放置して完了にはしない。既存の回復処理を大きく作り直す必要が生じた場合は、再現と最小変更を記録して設計を更新する。

## 9. 統合・実画面・保存の受入

### 9.1 EditorとUIの統合

通常のUIテストから`apply_list_edit_intent`を呼び、実際の`replace_range_recorded`と`after_input`を通す。計画を手で原文へ反映したテストだけで完了にしない。

最低限、E01/G01/E07で終了→通常入力→Undo/Redoを確認する。終了直後のUndoは元の記号・全空白・改行・選択を一回で戻す。通常入力をUndoした後にもう一回Undoすると終了操作が戻り、Redoで同じ位置へ進む。NoOpでは履歴が増えない。

終了後に日本語を`replace_and_mark_text`で入力し、確定と取消を両方試す。確定後の文字はリスト外、取消後は終了済みの原文と位置へ戻る。変換中Enterがリスト終了や追加改行を重ねないことも確認する。通常の貼り付け、移動して戻る操作、追加Enter、別タブへ移って戻る操作で、一時的なリスト接頭辞を復活させない。

PR #450の導入後のmainを使い、AI指示欄・設定・検索・フィルター・名前変更欄のEnter/Backspace/Tabが本文へ漏れないことを既存テストと必要な追加テストで確認する。このテストに外部AIへの実送信は不要である。

### 9.2 表示と保存

正しい終了後原文を、Haneの通常の表示・レイアウト経路へ渡す。文字入力後の通常段落の本文・caret・選択・クリック・日本語変換候補位置が同じ横位置の計算を使うことを検証する。画面全体の固定ピクセル値ではなく、同じ幅・字体・倍率での通常段落の位置を基準にする。

空行時は第6節の区別を守り、原文位置と実際の入力開始位置を確認する。見た目のsnapshotだけで原文の所属を保証しない。文字入力後の構造、見える位置、保存byteの3つを対応付ける。

既存FileService/DocumentSessionの保存経路で一時ファイルへ保存し、閉じて読み直して再解析する。E01とCRLFのE09を含め、保存結果が編集バッファと一致し、通常段落の所属が保持されることを確認する。Stringの再連結だけをファイル保存試験の代わりにしない。保存処理の製品変更は原則不要。

### 9.3 実機で行う順序

macOSの対象headで、以下をfocused scenarioとして確認する。Windows実画面確認はIssue ownerの後続指定により不要である。通常の起動・撮影だけで代替しない。

1. E01の空項目で後退削除し、カーソルの横位置を確認して英数字と日本語を入力する。Enterによる終了も確認する。
2. G01の入れ子を一階層ずつ戻して終了し、G09のShift+Enterとの違いを確認する。
3. E07の途中で通常文を入力し、後続の番号付きリストが通常文へ吸収されないことを確認する。
4. Undo/Redo、貼り付け、IME取消、保存・再オープンを行い、原文と画面を対応付ける。
5. AI指示欄など本文以外へフォーカスした際は本文が変わらないこと、前方Deleteを新しい終了操作へ変えていないことを確認する。

証拠には対象head、比較したmain、OS、シナリオ、期待/実結果、原文・画像・ログへの参照を残す。未実行のOSや失敗・不明なケースをpassとしない。既存のnative GUI検証手段を使い、新workflowや専用Gateを作らない。

## 10. ADR-0029へ反映する内容

本設計保存ではADR本体を実装済みに更新しない。実装PRで第5節と第8節の終了規則を整合させる。元の判断はIssue #460による変更として追跡できるようにする。新ADRは不要。

第5節の「top-levelの場合はline endingを残してmarker prefixだけを削除する」を、次の意味へ置き換える。

> 本文・子孫を持たない空の最上位項目を終了するときは、その項目の接頭辞を除去し、続けて入力する通常文が前後のリストに属さないよう、必要な段落区切りを原文へ即時に生成する。既存の空行と改行列を保持し、文書先頭・末尾へ不要な区切りを作らない。記号の除去、区切りの生成、カーソル移動を一回の記録付き編集で行う。Viewの一時的な文書構造では代用しない。

空項目の定義に「開始行だけが空でも、継続本文・子孫があれば最上位終了の空項目とは扱わない」を加える。第8節も同じ終了helperを参照し、EnterとBackspaceに別の終了規則を書かない。

コード内の回復候補とparserが認識した項目の区別、および引用等を破壊しない保護は本書を参照する。Tab全般、通常の継続、Editor/Markdown/UIの責務分担を再定義しない。

## 11. リファクタリングと並行作業

Issue #303は完了済み。Issue #302は調査開始時P2まで受入済みで、P3のPR #469が進行中だった。取得した#469の変更ファイルは`crates/presentation/src/display.rs`、`crates/presentation/src/lib.rs`、`docs/refactor-rf2b-p3-evidence.md`で、今回のMarkdown終了処理と直接重ならない。統合時には現在の状態を再取得する。

Issue #304は入力先の整理、Issue #306は解析・projection契約の整理である。今回は既存の入口とparserを使うため、これら全体の完了を先行条件にしない。構築する型が増える箇所は小さく保ち、#306が同じListEditItemを変更中ならその差分だけを調整する。

Issue #302/#307は表示・座標を実際に変更する必要が生じた場合の調整先。Issue #297全体の完了は待たない。リファクタリングPRにこの動作修正を混ぜない。

調査中にmergeされた#450は、AIポップアップのキー制御、入力capture、編集時の候補失効などを追加した。`actions.rs`、`view.rs`、`capture.rs`を旧版で上書きせず、最新の入力制御を使う。無関係なAI通信や設定を変更しない。

## 12. 実装順と完了判定

作業は一つの小さな製品PRを基本とし、次の順でコミットと検証を進める。機械的なファイル分割や別原因の修正を混ぜない。

| 順序 | 作業 | 次へ進む条件 |
|---|---|---|
| P0 | current main/Issue/関連PRを取得。現行toolchainで変更前テスト。E01の失敗テストを追加。 | 失敗が原文境界の不足を検出している。別の環境エラーと区別できる。 |
| P1 | 項目の由来、空・保護条件、前後区切りとcaretを実装。E01〜E16/G群を追加。 | Haneの実parserで追加入力後の所属まで正しい。 |
| P2 | Editor/UI、IME、Undo/Redo、表示、保存・再読込のテスト。ADRを更新。 | 原文・位置・履歴・保存が一致し、入力制御を壊していない。 |
| P3 | CI、最終候補のレビュー、macOSのfocused GUI。Windows GUIはIssue ownerの指定により不要。必要な指摘を原因単位で修正。 | 現行Commander Policyの受入条件を満たす。設計保存だけでは達成しない。 |

実装環境で実行する基礎コマンドは次を基準とする。リポジトリの固定toolchainと現在の既存検証方法を使い、既存環境の違いを隠すためにtoolchainや依存を変更しない。

```sh
cargo fmt --all -- --check
cargo test -p hane-markdown --locked
cargo test -p hane-markdown --all-features --locked
cargo test --workspace --locked -- --test-threads=1
cargo test --workspace --all-features --locked -- --test-threads=1
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
git diff --check
```

通常構成・対象crateのlintも既存CIに合わせて確認する。既存テストを減らさず、最終候補のmacOS/Windows CIとレビューを確認する。行移動だけのPRに対するGUI不要判断を、本件の動作修正へ流用しない。巨大な文書については既存のコピー・解析回数を増やしていないことを確認し、同一fixtureの変更前後を必要範囲で比較する。性能改善を今回の成果として約束しない。

完了時はIssueの受入条件へP1〜P3の証拠を対応付ける。独立した改善提案まで全件解消することを目的にしないが、本文損失、誤った所属、カーソル不整合、Undo/保存の不一致、必要な証拠不足は残してmergeしない。

## 13. 実施済み確認と未検証事項

実施済みなのはGitHub上の現行コード・Issue・設計方針の読解、調査中のmain更新差分の確認、本書の具体化である。補助としてPythonの小さな原文置換モデルでE01〜E16のrange/replacement/caretを計算し、markdown-it-pyのCommonMark設定で`ghi`がリスト外となることを16ケース確認した。後方区切りを省いた番号付きリストの例では、後続の番号が通常文へ吸収されることも確認した。

**この補助モデルはHaneのRust実装ではなく、GFMのtask認識・Haneの編集projection・Undo・描画を検証していない。製品テスト成功の証拠として使わない。** E01〜E16の期待値をHane自身で固定する作業はP0/P1に残る。

本環境にcargoと既定のlocal Jev CLIは見つからなかった。GitHubへのコンテナからのcloneも名前解決で失敗したため、コード読解には接続済みGitHubツールを使った。Jev判断やRust実行を取得したとは記録しない。設計文書の保存はCommander Policy第2.9節の代替規定に従う。環境に関する記述は本セッションの事実であり、次の実装環境へ引き継いで決めつけない。

残る確認は実装上の検証であり、利用者が決める未決仕様ではない。主に、parser-backed項目の追加情報の全構築箇所、即時の入れ子回復、空行時の実際の表示位置、taskの現行認識、最新入力制御、macOSのnative IMEを確認する。Windows固有の改行形式等はRust/CIで確認する。仕様を満たせない具体的な問題が出た場合は、失敗例と最小変更を設計・Issueへ反映する。

## 14. 戻し方と次担当への引き継ぎ

ファイル形式・設定・保存データの移行は行わない。製品修正のrevertで処理を戻せる。修正後に保存された空行は標準Markdownのまま残し、revertのために利用者の文書を一括編集しない。

次担当はcurrent `AGENTS.md`と`docs/aadw-command-policy.md`、Issue #460、本書、ADR-0029を読む。設計保存PRが未mergeなら本書の最新commitを参照し、設計だけを採用するか、製品PRへ文書commitを引き継ぐかを明記する。設計PRを製品実装済みの証拠にしない。

既存の実装担当・レビュー担当の分担を使い、実装、検証、最終候補のレビュー、必要なGUI確認を経て受入条件がそろった場合に製品PRをmergeし、Issue #460をcloseする。本セッションはその直前の設計保存までで止める。

## 参考資料

- [S1: CommonMark 0.31.2 — list items / lists](https://spec.commonmark.org/0.31.2/#list-items): 空の開始行と空項目の区別、lazy continuation、段落とリストの境界。parserの規則は変更しない。
- [S2: Microsoft — 箇条書きまたは段落番号を追加する](https://support.microsoft.com/ja-jp/powerpoint/add-bullets-or-numbers-to-text): Enterの後にBackspaceで箇条書きを解除し、通常のテキストへ戻る説明。
- [S3: Microsoft — Add text, bullets, and sub-bullets](https://support.microsoft.com/en-us/powerpoint/add-text-bullets-and-sub-bullets): Enterと行頭のTab/Shift+Tab。PowerPoint全版・すべての空入れ子操作を実機検証した根拠ではない。
- [読解基点のlist_editing.rs](https://github.com/hide212131/hane/blob/ca9b1b61e0317101f5f994f7aa3259431056584d/crates/markdown/src/list_editing.rs)
- [読解基点のblock_index.rs](https://github.com/hide212131/hane/blob/ca9b1b61e0317101f5f994f7aa3259431056584d/crates/markdown/src/block_index.rs)
- [読解基点のEditor](https://github.com/hide212131/hane/blob/ca9b1b61e0317101f5f994f7aa3259431056584d/crates/editor/src/lib.rs)
- [保存基点のactions.rs](https://github.com/hide212131/hane/blob/b3e8d307f57ab0de7e321261e5ecc29a99a7f039/crates/ui/src/actions.rs)
- [保存基点のview.rs](https://github.com/hide212131/hane/blob/b3e8d307f57ab0de7e321261e5ecc29a99a7f039/crates/ui/src/view.rs)
- [調査中のmain差分](https://github.com/hide212131/hane/compare/ca9b1b61e0317101f5f994f7aa3259431056584d...b3e8d307f57ab0de7e321261e5ecc29a99a7f039)
