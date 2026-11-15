# Issue #728 P1 実装ハンドオフ

このドキュメントは実装準備のための文書であり、**コードはまだ未実装・未検証**です。
設計の正本は `docs/implementation/issue-728-list-enter-design.md`。

## 実装内容

リスト項目の左端と継続行での Enter の意味を、ソース上の行頭記述とキャレット位置から決めるようにする。変更の中心は `crates/markdown/src/list_editing.rs` の計画層で、UI にリストの意味判断は追加しない。改行の終端子は Issue #714 で導入した `preferred_line_terminator`（実際に分割する物理行の位置を渡す）を使い、LF 固定の新経路は増やさない。

### ケース1: マーカーの左端では空行を入れる

- `"- abc"`、キャレット0 → `"\n- abc"`、キャレット1。元の項目のバイト列は変えず、新しいマーカーは生成しない
- 順序付き・タスクも同じ: `"10. abc"` 先頭 → `"\n10. abc"`、`"- [x] abc"` 先頭 → `"\n- [x] abc"`
- ネスト項目では元のインデントを保存する: `"- parent\n  - child"` の child 行頭 → `"- parent\n\n  - child"`（outdent ではない）

### ケース2-1: 継続行で、インデントもマーカーもない左端はリスト外へ進む

- `"- abc\n"`、キャレット6（空の継続行の先頭） → `"- abc\n\n"`、キャレット7。続けて `def` を入力すると `"- abc\n\ndef"` で、`def` はリスト外の段落になる
- lazy continuation の先頭でも境界を作る: `"- abc\ndef"`、キャレット6 → `"- abc\n\ndef"`（後続文字列は削除しない）
- ただし、すでにインデントを持つ既存行でキャレットが空白より前にある場合は、空白を削除して外側へ変換しない: `"- abc\n  def"`、キャレット6 → `"- abc\n\n  def"`（右側は元の項目に属し得る）

### ケース2-2: 継続行に本文用インデントがあれば同じ項目を続ける

- `"- abc\n  def"`、キャレット11 → `"- abc\n  def\n  "`、キャレット14。新しい `- ` は追加しない
- 途中分割でも右側の本文とインデントを維持する: `"- abc\n  def"`、キャレット10 → `"- abc\n  de\n  f"`
- 空白だけの継続行でも同様: `"- abc\n  "`、キャレット8 → `"- abc\n  \n  "`
- 「2スペース」は `- ` の例であり固定幅にしない。parser が確認した項目所属と構造インデントを使い、実際の行頭空白を維持する:
  - `"10. abc\n    def"` 末尾 → `"10. abc\n    def\n    "`
  - `"- parent\n  - abc\n    def"` 末尾 → `"- parent\n  - abc\n    def\n    "`
- タスクの `[x]` がある場合、`body_indent_columns` を継続行の必須最小幅として一律適用しない

### ケース2-3: 明示的なマーカーは別項目、空項目は exit

- `"- abc\n- def"`、末尾 → `"- abc\n- def\n- "`（`+ `、`* `、順序付きでも既存規則どおり）
- 空項目の Enter は既存の exit を優先する: `"- abc\n- "`、末尾 → `"- abc\n\n"`
- 先頭行の本文途中の既存動作は維持する: `"- abc"`、キャレット4 → `"- ab\n- c"`

## 受入条件

- 上記ケース1・2-1・2-2・2-3 の全例を、`crates/markdown/src/list_editing.rs` の計画単体テスト（既存の `replacement` helper で最終文書とキャレット offset を検証）として追加し、全て通ること。再解析した tree でも、外側段落・同じ項目・兄弟項目という期待した所属になることを重要な例で確認すること
- 計画単体では次の観点も覆うこと: `-`／`+`／`*`、順序付き `.`／`)`、チェック済み・未チェックの左端、接頭辞内部の通常編集、継続行の本文先頭・途中・末尾、3行目以降、2スペース以外のインデント、タブ、複数桁番号、マーカーに似た通常本文、引用、fenced／indented code、日本語・絵文字を含む本文と有効な UTF-8 境界、LF／CRLF／混在／終端改行なし、空項目の既存 exit／outdent／保護条件
- `crates/markdown/src/block_index.rs` の編集文脈テスト: 実際の `BlockIndex::list_edit_projection_at` を通し、項目末尾改行の直後、EOF 空行、空白だけの末尾行、継続行がブロック境界にある場合、文書先頭以外にあるリストの絶対 offset、編集直後と背景解析後の index、隣接する通常段落・コードから前のリストを誤取得しないことを確認すること
- `crates/ui/src/view.rs` の UI 統合テスト: 実際の Enter action から計画・適用まで通したケース1と3分類、Shift+Enter の継続動作、Enter → 文字入力を背景解析を待たず連続実行、1回の undo で Enter 前の文書・選択位置へ戻り redo で完全復元、選択範囲・IME・編集不可状態の既存入力制御、CRLF 文書で左端挿入・項目分割・継続・exit を連続実行
- `crates/presentation/src/projection/tests.rs` の表示テスト: 空行が消えずマーカーが重複しないこと、継続行の owner が同じ項目で独立マーカーを持たないこと、外側段落にリストの配置が残らないこと、マーカー開示あり／なし・formal projection あり／なしでソース対応が一致すること、`point_for_source` と hit-test の往復が一致すること、自動折り返しを物理的な継続行と混同しないこと
- 既存テストがすべて従来どおり通ること。追加するテストコードに lint 問題（`unused_mut`、`field_reassign_with_default`、`single_char_add_str` など）を持ち込まないこと。追加したテストコード全体を走査すること
- Commander が実行する focused test: `cargo test -p hane-markdown --no-fail-fast`、`cargo test -p hane-presentation --no-fail-fast`、およびワークフロー側 verify（`cargo test --no-fail-fast`）

## 対象外

- Issue #714 の終端子選択そのものの変更（本件は利用するだけ）
- Issue #729（コードフェンス直後の↑キー）の修正
- 通常の文字入力で既存行を遡及的に再構成すること
- 画面上の x 座標だけで所属を変えること（表示上の折り返しは「2行目以降」に含めない）
