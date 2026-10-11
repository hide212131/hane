# RF2-C / Issue #303 — Markdownの機械的分割・実装仕様

作成日: 2026-10-12 JST  
対象: [Issue #303](https://github.com/hide212131/hane/issues/303)  
親: [#297](https://github.com/hide212131/hane/issues/297)  
共通計画: [refactor-execution-plan.md](refactor-execution-plan.md)  
状態: 実装のための設計。製品変更・製品テスト・受入は未実施。

## 1. 今回進める範囲と調査基準

次の**未設計の独立した作業**として #303 を具体化する。#302 は設計済みで、P1だけが受け入れられている。#302 P2以降を完了扱いにせず、同Issueの既存設計による実装を妨げない。#303のIssue本文は、公開APIを維持する範囲で #302 と独立して進めることを認めている。実装を一つずつ進める場合、#302 P2の継続も引き続き選択できる。本書はその優先順位を上書きしない。

| 確認対象 | 2026-10-12の確認結果 |
|---|---|
| main | `cbafef9afeef88865884b4c6ea60fa18c6b18c5c`。文書ブランチ作成直前にも再確認した。 |
| `crates/markdown/src/lib.rs` | blob `db694bf3ff2186598329806989b4b1424ef49ffa`。本書のシンボル一覧はこの内容を基準にする。 |
| `block_index.rs` | blob `a6785b810e8b05850af1da5f172f2ebbe5bff557`。既存の生成処理を維持する。 |
| `list_editing.rs` | blob `707f485a6d795f9a92e63bc2508573242811ec61`。今回の移動対象にしない。 |
| #300 | completedでclosed。PR #386の受入記録は、利用中の公開項目・fallback・テスト補助などを維持すると判断している。不要物を削除したと読み替えない。 |
| #301 | closedだが `not_planned`。全分割が完了したという証拠には使わず、#303の開始条件にも追加しない。 |
| #302 | open。設計PR #454、P1実装PR #456、記録PR #457はmerge済み。P2以降は未着手と記録されている。 |
| open PR | 調査時に取得した #450 の変更14ファイルはAI/UIとその設計書で、`crates/markdown` は含まれない。実装開始時には再取得する。 |
| Rust | `rust-toolchain.toml` は `1.98.1`、componentsは `clippy` と `rustfmt`。 |

#298全体のcloseは一律の開始条件にしない。共通計画が認める部分引き渡しとして、本書のMarkdown対象一覧、維持する契約、変更前・変更後の検証条件を使う。ただし実装担当は、最初にcurrent codeとの一致と変更前テストの結果を確認する。過去の #300/#302 のCIは今回の変更後テストの代わりにはならない。

新構文、parser置換、projectionの統合、snapshotの所有者変更、性能改善、公開APIの削除、UI変更、依存・feature変更は行わない。局所的なファイル配置を具体化するための文書であり、既存方針を変更する新しいADRは作らない。

## 2. 変更しない契約

ここでいうprojectionは、解析結果から表示や編集に必要な情報を取り出したデータを指す。用途の異なるデータを今回まとめ直さない。

| 契約 | そのまま維持する内容 |
|---|---|
| 入力と範囲 | `parse_document` の入力、`Revision`、`SourceRange`、日本語を含むUTF-8のbyte位置を維持する。非ゼロのslice開始位置を足す処理を変えない。 |
| 解析設定 | `ENABLE_STRIKETHROUGH \| ENABLE_TASKLISTS \| ENABLE_TABLES`。オプション追加・削除をしない。依存parserを直接呼ぶ第二の入口も作らない。 |
| 構文木 | `NodeId(0)`のroot、ノード順、親子関係、深さ、未対応eventを落とさない扱い、task markerの反映順、table alignmentを維持する。 |
| 記号と空白 | 記号範囲の導出、重なりのmerge条件、fenceの開始・終了、quote/listの所有者、tabのbyte数と列数の区別、code/line-break paddingを維持する。 |
| 正式な索引 | `BlockIndex`の正式/暫定、revision、増分更新・rebase・tail無効化、各projectionの生成と破棄を変えない。 |
| 表示中の範囲だけの索引 | `LOCAL_BLOCK_LOOKBACK = 2_048`、viewportの一行先まで読む条件、失敗時の空索引、`Confidence::Provisional`、offset由来の`BlockId`を維持する。これは行数の境界であり、任意の長大な一行に対するbyte上限を新たに保証する設計ではない。 |
| fenceの高さ | block相対範囲、sort/dedup、quote所有者の索引、二重計数の除外、空でないselectionの交差確認、最終block末尾の扱いを維持する。`block_is_final`を内部にキャッシュしない。 |
| 編集 | `ListEditProjection`と`ListProjection`を統合しない。編集計画だけを返す既存の境界、`Handled`/`NotApplicable`、Undo/Redoへの接続を変えない。 |
| 利用側 | `hane_markdown::...`の既存公開パスを維持する。presentation/UIがparser内部や新しいprivate moduleへ依存する変更をしない。 |

単にファイルを移すことで速くなった、表示不具合が直った、という報告はしない。移動中に別の不具合を見つけた場合は記録し、意味を変える修正を同じPRへ混ぜない。

## 3. 最終配置

全段階を受け入れた後の製品コードの配置を次に固定する。すべてcrate直下のprivate moduleとし、`pub mod`は追加しない。

```text
crates/markdown/src/
  lib.rs                 module宣言・公開再export・既存の横断テスト
  syntax.rs              構文型とその問い合わせ
  markers.rs             記号・空白範囲の導出と物理行の補助処理
  parser.rs              eventから構文木を作る正式な解析入口
  fence_height.rs        fence行の高さ問い合わせ用データ
  list_projection.rs     list/quoteの表示用データと問い合わせ
  local_block_index.rs   正式な索引がまだない時の局所索引
  block_index.rs         既存の正式索引・各projection生成（維持）
  block_store.rs         既存の索引の格納（維持）
  list_editing.rs        既存のsource編集計画（維持）
```

`lib.rs`の残りの横断テストは無理に各moduleへ振り分けない。ファイルの総行数を減らすためだけのテスト移動は別作業にせず、今回も行わない。製品の型・処理が責務別に見分けられることを完了条件にする。

生成責務は既存の `block_index.rs` に残す。`TableProjection`の3型、`build_list_rows`、`build_list_projections`、fence/table projectionの生成関数を新しいファイルへ移してはならない。すでに別ファイルである領域を再分割すると、正式な索引の寿命まで変更しやすくなるためである。

呼び出しの方向は、`local_block_index -> parse_document + block_index::tiled_blocks`、`parser -> syntax + markers`、`block_index -> parse_document + 各projection + block_store/list_editing`を維持する。`markers`から`parse_document`を呼び直す経路は作らない。型同士の参照と、処理を呼ぶ方向は区別する。

## 4. シンボルの移動表

下表はすべて旧 `crates/markdown/src/lib.rs` からの移動である。型には直前の説明・属性・derive、全フィールド・variant、全implを含める。関数本体は変更しない。行番号よりもシンボル名を正とし、同名のfieldと関数を取り違えない。

| 移動先 | 移すシンボル |
|---|---|
| `fence_height.rs` | `FenceHeightProjection`、`FenceHeightRow`、`FenceQuoteOwnerRows`、`impl FenceHeightProjection`全体。 |
| `list_projection.rs` | `ListProjection`、`ListProjectionItem`、`ListProjectionPrefix`、`QuoteProjection`、`ListProjectionList`、`ListProjectionRow`、`impl ListProjection`全体。 |
| `syntax.rs` | `NodeKind`、`NodeId`、`MarkdownNode`、`TableAlignment`、`TableParse`、`MarkdownTree`、`MarkdownParse`。`impl NodeKind`、`impl MarkdownTree`全体。 |
| `markers.rs` 公開補助 | `FenceDelimiter`、`FenceMarkerEdge`、`fence_delimiter`、`fence_closing_delimiter`、`fence_closes`、`is_table_delimiter`、`has_delimiter_markers`。 |
| `markers.rs` 内部型 | `PrefixCursor`と全impl、`ListItemPrefix`、`PrefixContainer`、`ParsedCodeSpan`、`DerivedMarkers`。 |
| `markers.rs` 内部処理 | `absolute_range`、`consume_containers`、`markdown_line_start`、`markdown_lines`、`ancestor_containers`、`quote_markers`、`code_padding`、`line_break_padding`、`derive_markers`、`list_structural_prefixes`。 |
| `parser.rs` | `heading_level`、`parser_options`、`node_kind_for_tag`、`build_tree`、`parse_document`。`build_tree`内のローカル関数`push`も本体ごと維持する。 |
| `local_block_index.rs` | `LOCAL_BLOCK_LOOKBACK`、`LocalBlockIndex`と全impl、`local_block_index`。 |

`ParsedCodeSpan`と`DerivedMarkers`はparserと記号導出の間で受け渡す既存データである。名前や形を変えず、`markers.rs`に置く。汎用のManagerや共通utility crateは作らない。

### 4.1 問い合わせメソッドの取りこぼしを防ぐ

`FenceHeightProjection`は `from_absolute_rows`、`is_empty`、`inactive_rows_in`、`inactive_rows_before_line`、`is_fence_row_line`、`inactive_rows_in_indices`、`active_quote_rows_in`をまとめて移す。

`ListProjection`は `new`、`fence_marker_edge`、`fence_markers_in`、`container_markers_in`、`quotes_in`、`item_for_marker`、`item_for_source_range`、`item_for_range`、`is_code_block_for_range`、`list`、`prefixes_in`、`row_for_range`をまとめて移す。

`LocalBlockIndex`はprivateな`indexed`と、公開されている`block_at`、`blocks_in`をまとめて移す。

## 5. 公開範囲と再exportを固定する

### 5.1 crate外の名前は増減させない

rootの既存の `pub use block_index::{...}` と `pub use list_editing::{...}` は名前・公開範囲を維持する。新たに必要になる再exportは次の通りである。既存20名と合わせて、調査時点のroot公開名44名を維持する。名前数だけでなく型・variant・field・メソッド・trait実装も比較する。

```rust
pub use fence_height::FenceHeightProjection;
pub use list_projection::{
    ListProjection, ListProjectionItem, ListProjectionList,
    ListProjectionPrefix, ListProjectionRow, QuoteProjection,
};
pub use syntax::{
    MarkdownNode, MarkdownParse, MarkdownTree, NodeId, NodeKind,
    TableAlignment, TableParse,
};
pub use markers::{
    FenceDelimiter, FenceMarkerEdge, fence_closes, fence_closing_delimiter,
    fence_delimiter, has_delimiter_markers, is_table_delimiter,
};
pub use parser::parse_document;
pub use local_block_index::{LocalBlockIndex, local_block_index};
pub(crate) use markers::markdown_lines;
```

最後の `markdown_lines` はcrate内だけの既存経路であり、上記44名には数えない。`block_index.rs`から使う `crate::markdown_lines` は残す。再exportは一時的な互換aliasではなく、内部配置を利用側へ漏らさない恒久的な入口である。wrapper関数や型aliasを新設しない。

### 5.2 ファイル移動で必要になる内部可視性の許可表

Rustでは、rootでprivateだった項目は子moduleから参照できるが、別の子moduleへ移すと兄弟からは参照できなくなる。今回の`pub(super)`はcrate外への公開ではなく、移動前と同じcrate内の参照を保つために限定して使う。

| 対象 | 移動後 | 必要な参照元・理由 |
|---|---|---|
| `MarkdownTree.nodes` | `pub(super)` | `parser::build_tree`が既存の`MarkdownTree { nodes }`をそのまま構築する。constructor追加はしない。 |
| `ListProjectionRow.item_index`、`is_code_block` | 2 fieldだけ`pub(super)` | 既存の`block_index::build_list_rows`が構造体リテラルで構築する。`source_range`は元の`pub`のまま。 |
| `ParsedCodeSpan`と`node`/`content` | 型と2 fieldを`pub(super)` | parserが構築し、記号導出側が読む。 |
| `DerivedMarkers`と5 field | 型と`markers`/`fence_markers`/`fence_marker_edges`/`quote_markers`/`list_item_markers`を`pub(super)` | parserが返却値を`MarkdownParse`へ移す。 |
| `absolute_range`、`derive_markers`、`code_padding`、`line_break_padding`、`list_structural_prefixes` | 5関数を`pub(super)` | parserから既存処理を呼ぶ。 |
| `markdown_lines` | 既存の`pub(crate)`を維持 | 正式索引と記号導出の物理行処理を共有する。 |
| 既存の`FenceHeightProjection::from_absolute_rows`、`ListProjection::new` | 既存の`pub(crate)`を維持 | 呼び出し元とシグネチャを変えない。 |

これ以外のprivateな型・field・メソッドを、分割を通すためにまとめて公開しない。`FenceHeightProjection`の内部field、`ListProjection`のprivate field、`LocalBlockIndex.window`はprivateのままにする。テストだけが必要とする内部参照は、テストを所有moduleへ移して対応する。

root直下の子moduleでの`pub(super)`はcrate内から参照できるため、これをモジュール単位の厳密なアクセス制限と説明してはならない。新たにcrate外へ届かないことと、必要な項目だけに限定したことを確認する。

### 5.3 importと文書リンク

新ファイルは必要な型だけを明示importする。製品コードに`use super::*`を追加しない。rootのcrate-level lint属性はrootに残し、抑制範囲を広げたり警告を追加で隠したりしない。

移動により説明中のリンクが解決できなくなる場合だけ、`[ListProjection]`などを`[crate::ListProjection]`のように修正する。doc linkのためだけの未使用importは増やさない。関数本体、条件、定数、属性、テスト期待値の変更と区別して記録する。

## 6. 実装PRの単位と順序

P1から順番に、直前の段階がmainへ受け入れられてから新しい短期ブランチを作る。6つの製品PRを同時に開かない。各PRは`Refs #303`とし、P1だけでIssueが自動closeされる書き方をしない。

| 段階 | 製品差分 | テスト差分・注意 |
|---|---|---|
| P1 | `fence_height.rs`を追加。3型と全implを移し、rootから`FenceHeightProjection`を再exportする。 | 第7.1節の1テストだけ同moduleへ移す。`block_index.rs`の生成処理は変更しない。新たな可視性変更は不要。 |
| P2 | `list_projection.rs`を追加。6型と全implを移す。 | 第5.2節の`ListProjectionRow`の2 fieldだけ調整する。正式索引側の構築処理を変えない。横断テストはrootに維持する。 |
| P3 | `syntax.rs`を追加。7型と全implを移す。 | `MarkdownTree.nodes`だけ許可表どおり調整する。rootの既存名経由で現行parserとテストが動く配置を保つ。 |
| P4 | `markers.rs`を追加。第4節の記号導出一式を移す。 | 第7.2節の3テストを同moduleへ移す。まだrootに残るparserは明示的なprivate importで呼ぶ。`markdown_lines`のcrate内再exportを追加する。 |
| P5 | `parser.rs`を追加。5関数を移す。 | P4でrootに必要だったparser用importを`parser.rs`へ移し、不要なroot importを除く。公開名は再exportで維持。解析呼出順を変えない。 |
| P6 | `local_block_index.rs`を追加。局所索引一式を移す。 | 第7.3節の5テストを同moduleへ移す。残るroot横断テストが必要とするdocument型はテストmoduleでimportし、製品rootの不要importを除く。 |

P1/P2/P3で`lib.rs`から消すimportは、その段階で本当に不要になったものだけにする。将来のP5/P6を見越した削除をしない。各段階の終点で通常構成とall-featuresの双方を成立させる。

### 6.1 最初のP1の具体的な作業

新しい `crates/markdown/src/fence_height.rs` の製品importは、現行の実装に対して次の3行で足りる。

```rust
use hane_document::{SourceOffset, SourceRange};
use std::collections::HashMap;
use std::ops::Range;
```

`FenceHeightProjection`、`FenceHeightRow`、`FenceQuoteOwnerRows`と全implを、説明・属性を含めて移す。`lib.rs`へ`mod fence_height;`と`pub use fence_height::FenceHeightProjection;`を追加する。型・メソッドの公開範囲は一切変更しない。説明中の別moduleの型へのリンクだけ必要に応じて修正する。

このP1で触れてよい製品ファイルは`lib.rs`と新しい`fence_height.rs`だけである。証拠を保存する文書は別に追加できる。`block_index.rs`、presentation、UI、Cargo設定、fixtureへ修正が必要になった場合は、そのまま範囲を広げず、設計との不一致を確認する。

テストは新ファイルの`#[cfg(test)] mod tests`に同じ本体で置く。`use super::*;`はそのテストmodule内でのみ使用してよい。その他のrootテストを削除・改名しない。

## 7. テストの移動規則

テスト本体、入力bytes、期待値、属性、assertionを維持する。テスト総数の一致だけでなく、名前と本体の対応を残す。以下の移動に伴うテストの完全修飾名の変化は許容し、末尾の関数名は維持する。

### 7.1 P1: `tests::`から`fence_height::tests::`へ

```text
fence_height_projection_answers_physical_line_prefixes
```

### 7.2 P4: `tests::`から`markers::tests::`へ

次の3件はprivateな`markdown_line_start`または`quote_markers`を直接呼ぶ。これらを公開するのではなくテストを移す。新テストmoduleでは必要な`parse_document`、`NodeKind`、document型を明示importする。

```text
markdown_line_scan_preserves_bytes_and_does_not_split_crlf
quote_markers_follow_list_ancestors_without_hiding_literal_greater_than
quote_markers_follow_list_items_with_an_empty_opening_line
```

### 7.3 P6: `tests::`から`local_block_index::tests::`へ

次の5件をまとめて移す。最後のテストはprivateな`window`を参照する。rootのために`window`を公開しない。正式索引との比較に必要な`BlockIndex`はテスト側でimportする。

```text
local_index_sees_a_fence_that_opens_inside_the_lookback_window
local_index_remembers_leading_blank_lines_for_the_first_fenced_block
local_index_resolves_gfm_pipe_tables
local_index_agrees_with_the_formal_index_on_the_visible_window
local_index_covers_only_the_scanned_window
```

これ以外の既存rootテストはその場所に残す。例えば`code_padding_tracks_semantic_spaces_instead_of_container_bytes`、`commonmark_ranges_remain_absolute_for_unicode_and_nested_styles`、`table_parse_retains_delimiter_alignments`、`formal_block_index_exposes_compact_table_projection`、listのmarker/構造空白の各テストは、公開された解析・索引経路を使う横断検証として維持する。

既存の `block_index.rs`、`block_store.rs`、`list_editing.rs` のテストも移動・削除しない。`tests/atx_conformance.rs`、`block_index_contract.rs`、`document_contract.rs`、`line_breaks.rs`とそのfixtureを維持する。テストを通すための期待値の更新は本Issueの許容差分ではない。

## 8. 実装前と各PRの検証

### 8.1 開始時

current main、#303、#298の対象契約、#300の受入記録、#302、関係するopen PRのheadと変更ファイルを取り直す。本書の調査SHAを現在のmainだと仮定しない。変更が対象外なら理由を記録して続行し、対象シンボルや契約が変わっていれば対応表を更新してから移す。

作業前に`git status --short`で利用者の未コミット変更がないことを確認し、作業対象のbase SHAを記録する。変更がある場合は既存作業を壊さず、別worktreeなど既存の分離手段を使う。baselineと候補のテスト一覧を、通常とall-featuresを区別して保存する。

```sh
rustc --version
cargo --version
cargo fmt --version
cargo test --locked -p hane-markdown -- --list
cargo test --locked -p hane-markdown --all-features -- --list
cargo test --locked -p hane-markdown
cargo test --locked -p hane-markdown --all-features
```

Rustの版はその時点のrepo指定に合わせる。本書の時点では1.98.1である。上のテストはbaselineと候補の両方で実行する。今回、この設計作業の環境ではRust製品テストを実行していないため、結果や件数を既知の成功として渡さない。

### 8.2 差分と公開API

```sh
git diff --check
git diff --name-status <base-sha>...HEAD
git diff --color-moved=plain <base-sha>...HEAD -- crates/markdown
cargo fmt --all -- --check
```

`<base-sha>`は各PRの実際の比較元に置き換える。移動した各関数・メソッド・テストの本体が一致することを比較する。許容する差分はmodule宣言、再export、import、第5.2節の可視性、説明リンクだけである。文字列literalの空白まで消す一括正規化で一致を判定してはならない。

rootの公開44名に加え、型定義、enumのvariant、field型、メソッドの引数・戻り型・公開範囲・`const`、derive/trait実装を比較する。名前一覧の一致だけで公開API互換を宣言しない。以下の文書生成をbaselineと候補で確認し、移動で生じた未解決リンクを残さない。

```sh
cargo doc --locked -p hane-markdown --no-deps
cargo doc --locked -p hane-markdown --all-features --no-deps
```

整形や警告が変更前から失敗する場合は、同じtoolchainと同じ対象範囲で前後を比較して記録する。失敗を成功へ読み替えない。無関係な全ファイル整形を本Issueへ混ぜず、必須checkを迂回してmergeしない。

### 8.3 利用側を含む検証

各段階の候補に対して次を実行する。markdown単体だけの成功を完了条件にしない。

```sh
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

macOSとWindowsのcurrent-head CI、および既存workflowで補う必要のある通常/all-featuresの検証を確認する。特定OSのローカル検証だけで他方も合格としない。実装自体はmacOSまたはWindowsのどちらで行ってもよい。本作業のために新しいCI、GUI workflow、評価基盤を作らない。

### 8.4 実画面確認と性能測定の境界

関数本体が一致し、公開API・UI配線・layout・描画・feature・起動経路を変えず、上記の利用側テストを満たした機械的移動では、追加の実画面確認を原則不要とする。各PRでこの条件に当てはまる理由を記録する。「refactorだから不要」という説明だけにしない。

意味を変える修正、描画・focus・入力配線の変更が必要になった場合は別PRに分け、その変更に必要な画面確認を計画する。動作を変えない移動を根拠なくGUI失敗の修正として扱わない。性能改善を目的にしないため新しい比較測定は必須化しないが、性能に影響する変更を混ぜる場合は本書の対象外として #23/#314 の手順に戻る。

## 9. PRの受入とIssueの完了条件

各実装PRの本文には、対象段階、今回移したシンボル、前後のパス、可視性差分、対象base/head、テスト結果とログの場所、CI、レビュー、画面確認要否を記す。過去headの結果をcurrent-headの結果として掲示しない。baseが進んだ場合も、差分や検証条件への影響を確認する。

レビューはその時点の[Commander Policy](aadw-command-policy.md)とユーザーの有効な指示に従う。現行headに対する確認、未解決のblocking finding、許可されたレビュー主体を区別する。自己確認を独立レビューやapprovalと呼ばない。Jev・実装worker・reviewを今回の設計保存で実行済みとは記録しない。

P1からP6までの各受入記録は、`docs/refactor-evidence/rf2c-issue-303/`の下に段階別に保存できる。まだ実行していない結果のファイルを成功の体裁で作らない。各記録から対象commit・コマンド・終了コード・OS/toolchain・結果URLへたどれるようにする。共通実施計画へは設計と受入記録への短い参照を置き、長い仕様を複製しない。

#303は、全6段階が受け入れられ、既存の4つの受入条件を証拠付きで満たした場合にだけ完了とする。設計PRのmerge、P1の成功、あるいはファイルが分かれたことだけでcloseしない。merge前に現行headとbaseを取り直し、必須証拠がそろった候補だけをexpected head付きで受け入れる。

## 10. 後続への引き渡しと戻し方

#306へは、`parser::parse_document`による解析、`BlockIndex`が生成・保持する用途別projection、`local_block_index`による暫定経路、既存の `ListEditProjection` の境界を渡す。本Issueではそれらを一つのsnapshotへまとめない。

#307/#368へは、`FenceHeightProjection`の問い合わせと、正式索引側の生成・保持の分担を渡す。block相対範囲や`block_is_final`の受け渡しをそのまま残す。新たなcache、generation、寿命管理を導入しない。

戻す場合は対象段階のPRをrevertする。後続段階が依存している場合は必要な後続から逆順に戻し、通常/all-featuresと利用側の検証を再実行する。保存形式・設定・利用者データを変えないためデータ移行は不要である。revert時に別機能の変更を巻き戻さないよう、現行差分を確認する。

## 11. 実装担当への最初の指示

**最初はP1だけを実装する。** current factsと変更前テストを取得し、`refactor/303-p1-fence-height`等の専用ブランチで、第6.1節の2製品ファイルだけを変更する。第7.1節の1テストを移し、関数本体の一致、公開API、通常/all-features、workspace、current-head CIとレビューを確認してPRを提出する。未取得の証拠は未取得と記録する。P2以降を同じPRへ足さず、#303をcloseしない。

設計文書と製品受入は別である。本書保存時点で行ったのはGitHub上の現行情報とソースの静的照合であり、コンパイル、製品テスト、GUI、独立レビューの成功は主張しない。

## 12. 根拠

- [調査commitのMarkdown root](https://github.com/hide212131/hane/blob/cbafef9afeef88865884b4c6ea60fa18c6b18c5c/crates/markdown/src/lib.rs)
- [正式索引とprojection生成](https://github.com/hide212131/hane/blob/cbafef9afeef88865884b4c6ea60fa18c6b18c5c/crates/markdown/src/block_index.rs)
- [編集計画の既存境界](https://github.com/hide212131/hane/blob/cbafef9afeef88865884b4c6ea60fa18c6b18c5c/crates/markdown/src/list_editing.rs)
- [Markdownの依存](https://github.com/hide212131/hane/blob/cbafef9afeef88865884b4c6ea60fa18c6b18c5c/crates/markdown/Cargo.toml)、[toolchain](https://github.com/hide212131/hane/blob/cbafef9afeef88865884b4c6ea60fa18c6b18c5c/rust-toolchain.toml)
- [#300の受入記録](https://github.com/hide212131/hane/issues/300#issuecomment-5856926591)、[#302の段階別状態](https://github.com/hide212131/hane/issues/302)
- [調査commitの共通実施計画](https://github.com/hide212131/hane/blob/cbafef9afeef88865884b4c6ea60fa18c6b18c5c/docs/refactor-execution-plan.md)、[Commander Policy](https://github.com/hide212131/hane/blob/cbafef9afeef88865884b4c6ea60fa18c6b18c5c/docs/aadw-command-policy.md)
- [Rust Reference: Visibility and privacy](https://doc.rust-lang.org/reference/visibility-and-privacy.html)。private項目、`pub(super)`、再exportの規則を確認するために参照した。
