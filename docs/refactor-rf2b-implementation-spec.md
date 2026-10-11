# RF2-B：presentationの機械的分割 実装設計書

対象: [Issue #302](https://github.com/hide212131/hane/issues/302)  
親計画: [refactor-execution-plan.md](refactor-execution-plan.md)、[refactor-plan.md](refactor-plan.md)  
作成日: 2026-10-11

## 1. この文書の位置づけ

**実装担当が着手できるように、移動先、移動するシンボル、公開範囲、PRの順序、検証条件を定める。製品コードを変更した記録ではない。** 設計保存時点では、この分割後のビルド、テスト、独立レビュー、実画面検証、性能測定を実施していない。設計用PRの保存・mergeで #302 を閉じない。

仕事の目的・範囲・受入条件は #302、本件の具体的な配置と手順は本書、実装差分と検証証拠は各実装PRを正本とする。既存の設計判断に従う局所的なファイル分割なので、新しいADRは作らない。工程の判断は着手時のdefault branchにある [Commander Policy](aadw-command-policy.md) と [AGENTS.md](../AGENTS.md) に従う。

今回の設計で利用者に選択を求める未決事項はない。一方、実装開始時のmainとの照合、実際のコンパイル、変更前後のテスト比較は、実装時に取得する証拠であって設計だけでは成功としない。

## 2. 調査基準と開始条件

### 2.1 読み取った実装

調査したmainは `0bba41617cf527fbd96bb7e561d1c472d1367120`。以下の基準はこのcommitに固定している。実装時にmainが進んでいたら、同じファイルと関係する呼出元の差分を確認してから本書を適用する。

| 対象 | 基準blob SHA | 確認した境界 |
| --- | --- | --- |
| [presentation/src/lib.rs](https://github.com/hide212131/hane/blob/0bba41617cf527fbd96bb7e561d1c472d1367120/crates/presentation/src/lib.rs) | `f596ee990a78bb907c88ea411876e4e7e7409480` | 位置対応、表示型と方針、行・ブロックの表示生成、共有解析、高さ索引、unit testsを持つ。 |
| [presentation/src/layout.rs](https://github.com/hide212131/hane/blob/0bba41617cf527fbd96bb7e561d1c472d1367120/crates/presentation/src/layout.rs) | `715eb166b111d2169200732d45409257529d8694` | 画面座標と折り返しを扱う既存モジュール。今回の分割対象にしない。 |
| [presentation/src/testing.rs](https://github.com/hide212131/hane/blob/0bba41617cf527fbd96bb7e561d1c472d1367120/crates/presentation/src/testing.rs) | `9a7df528290fccb0ff8cb248363d8d5cc0d39c1f` | `FixedAdvanceShaper`を提供する公開テスト・計測補助。通常構成でも公開を維持する。 |
| [presentation/Cargo.toml](https://github.com/hide212131/hane/blob/0bba41617cf527fbd96bb7e561d1c472d1367120/crates/presentation/Cargo.toml) | `9366e9d24ba99b2b889f4567789decaee6dd100c` | packageは`hane-presentation`。依存は既存のdocumentとmarkdownを保つ。 |

現在のcrate rootは、非公開の`layout`と公開の`testing`を宣言し、`layout`の型・関数・定数および`hane_markdown::TableAlignment`を再公開している。これを維持する。過去のAPI台帳を現在の実装へそのまま当てはめず、今回の基準から公開項目を採り直す。

### 2.2 前段と並行作業

#300は[PR #386](https://github.com/hide212131/hane/pull/386)と[Issueの完了記録](https://github.com/hide212131/hane/issues/300)で、公開API、fallback、テスト補助などを根拠なく削除せず維持する判断を残している。RF2-BでもAPI削除は行わない。#298のRF0-04にあるsource/visual/geometryの契約と検証範囲を、本件の変更前条件として引き継ぐ。RF0全体や #23 の全測定が未完であることを、無関係な領域も含めた一律の待機条件にはしない。

実装の開始条件は、対象範囲の現行コード・公開契約・既存テスト・既知の失敗を確認できることとする。変更前の対象テストに失敗があれば、今回触る契約への影響を切り分ける。原因不明の失敗を正常な基準として固定したり、古い成功記録で代替したりしない。

調査時のopen PRは #450で、headは`5a1470d4473bf10c8f3d4d5dd06f8127c74e129b`だった。取得した変更ファイル一覧には`crates/presentation/`が含まれず、AIとUIおよびその設計書を変更している。本設計はそのブランチやUIの実装を操作しない。これは調査時点の観測であり、各実装PRの開始時には一覧と差分を再取得する。

#301のUI分割、#303のMarkdown分割は、公開された呼び出し方を保つ範囲で独立して進められる。#292/#295の修正を別実装し直さず、受入済みのmainと当該記録を基準にする。#306、#307、#368の意味解析・座標・状態寿命の変更を先取りしない。

## 3. 変更すること、変更しないこと

Markdown原文の位置を画面の位置へ対応させる処理、表示方法を決める処理、表示用の行やブロックを作る処理、数値の高さを管理する処理を、ファイル名から見分けられるようにする。

変えるのは配置とそれに必要な接続だけである。関数名、引数、戻り値、型のフィールド、enumのvariant、derive、trait実装、`const`、条件付きコンパイル、条件分岐、処理順、配列の並び、計算式は保つ。移動先で必要になる明示的なimport、既存公開項目の再export、限定した内部可視性、テストの所属、文書内の参照先だけを変更してよい。

特に、SourceMapの候補配列やsortの最適化、表示ルールの統一、wrapperの削除、解析の追加、snapshotの再利用条件変更、UIでの座標補正、キャッシュ無効化やjobの所有者変更は行わない。依存、feature、toolchain、manifest、lockfile、vendor、workflow、バージョンも変更しない。ファイルが短くなったことを性能改善とは報告しない。

## 4. 最終的な配置

```text
crates/presentation/src/
├── lib.rs                 # crate属性、module宣言、既存公開項目の再export
├── source_map.rs          # 原文と表示の位置対応
├── display.rs             # 表示データと表示方針
├── height_index.rs        # 数値の高さ索引とスクロール基準
├── projection/
│   ├── mod.rs             # 行の表示生成、共有解析、内部の索引・記号判定
│   ├── block.rs           # ブロックの入力、組立、表示範囲・高さの初期計算
│   ├── tests.rs           # 行生成・共有解析の内部テスト
│   └── block/
│       └── tests.rs       # block内部関数へのアクセスが必要なテスト
├── layout.rs              # 既存のまま
└── testing.rs             # 既存のまま、pub mod testingを維持
```

`source_map.rs`、`display.rs`、`height_index.rs`のunit testsはそれぞれの末尾の`#[cfg(test)] mod tests`へ置ける。必要なテストがない空のファイルやmoduleを作る必要はない。integration testsは現在の`crates/presentation/tests/`に残す。

### 4.1 呼び出しの向き

```text
UI・benchmark・integration tests
                 │  従来の hane_presentation::名前
                 ▼
            lib.rs の再export
                 │
       ┌─────────┼──────────┐
       ▼         ▼          ▼
   projection   layout   height_index
       │         │          │
       └─────────┴──────────┤
                            ▼
                         display
                            │
                            ▼
                        source_map
```

実際には各処理が必要な位置型を`source_map`から直接使ってよい。document/markdownへの既存依存は残すが、UI、GPUI、新しいparser実装をこれらのmoduleへ持ち込まない。`display`から`projection`の実行処理へ依存させない。

### 4.2 blockをprojectionの子にする理由

`JoinedParse`は公開型だが、内部の`projection: ProjectionIndex`は非公開である。ブロック組立はこの解析結果や、行を作るための非公開関数を使う。両者を別々の兄弟moduleにすると、共有のためだけに内部型とフィールドを広く公開しやすい。

そこで、行生成と共有解析を`projection/mod.rs`に残し、ブロック組立をその子の`projection/block.rs`に置く。Rustでは子moduleから祖先moduleの非公開項目を参照できるため、`JoinedParse.projection`、`ProjectionIndex`、`SharedParse`などを公開せずに使える。根拠は[Rust Referenceの可視性規則](https://doc.rust-lang.org/reference/visibility-and-privacy.html)を参照する。

親から必要となる`BlockLine`などは、もともと外部公開されている型を`projection`内で再exportして使う。新しい抽象trait、共有用の大きなcontext、getterの増設、型を包むadapterは不要である。

## 5. シンボルの移動表

行番号ではなく、以下の型・関数とその実装全体を移動単位にする。各型の全フィールド、全variant、全impl、属性、直結するコメントも一緒に移す。特定のメソッドだけを元のファイルへ残さない。

### 5.1 source_map.rs

| 移動する項目 | 扱い |
| --- | --- |
| `VisualOffset`、`VisualRange` | `VisualRange::new`を含めて移動。単位と境界の意味を変えない。 |
| `Visibility`、`BoundarySide`、`MarkerEdge` | 全variantとderiveを維持。 |
| `MappingSegment`、`PositionCandidate` | 公開フィールドとその型を維持。 |
| `SourceMap` | `visual_to_source`、`source_to_visual`、`normalize_source`、`normalize_visual`を含むimpl全体。`segments`の公開も維持。 |

ここには表示の種類やMarkdownの構文判断を入れない。`append_segment`や`segments_tile_range`は表示生成の処理なので、名前にsegmentがあっても`projection`側に残す。

### 5.2 display.rs

| 移動する項目群 | 扱い |
| --- | --- |
| `StyleKind`、`StyleRun`、`InlineDisplay` | `StyleKind::display`、`InlineDisplay`の合成処理を含める。 |
| `LineContext`、`BlockKind`、`BlockWeight`、`BlockSurface`、`BlockTint`、`BlockDisplay` | 表示方針とDefault等の実装を一緒に移す。 |
| `TableCellDisplay`、`TableRowDisplay`、`ImagePresentation`、`QuoteRowMetadata` | 既存のデータ構造をそのまま保持する。 |
| `ListId`、`ListItemId`、`ListMarkerKind`、`ListRowRole`、`ListAlignment`、`ListOwnerMetadata`、`ListMarkerMetadata`、`ListStructuralPrefixMetadata`、`ListRowMetadata` | `ListRowMetadata::rebase`を含め同じmoduleに保ち、このprivateメソッドを公開しない。 |
| `VisualLine`、`VisualBlock` | `height`、`display`、`rebase`等の全implを移動。`VisualLine::rule_body_is_collapsed`も落とさない。 |
| `SyntaxDisplay`、`syntax_display`、`block_display_kind` | 構文種別から表示方針への対応を一か所に保つ。`VisualBlock::matches`からprojectionへ逆向きの依存を作らない。 |
| `block_line_context`、`block_is_joinable`、`estimated_height`、`code_line_height` | 表示方針の一部として同居させる。既存のpublic/privateの区別は次節の限定変更を除いて維持する。 |

`SyntaxDisplay`の置き場所を変えるだけで、構文と表示を対応させるmatchを作り直してはいけない。`fenced_line_height`は原文開示の状態も判断するため`projection`に残す。

内部の可視性変更を許すのは、まず次の範囲に限定する。

```rust
pub(super) struct SyntaxDisplay {
    indexed_block: BlockKind,
    pub(super) node_block: Option<BlockKind>,
    pub(super) inline_style: Option<StyleKind>,
    line_context: LineContext,
}

// 本体、引数、戻り値、constの有無は現在のまま。
// 以下は公開範囲だけを示す。
// pub(super) const fn syntax_display(...)
// pub(super) fn block_display_kind(...)
// pub(super) fn estimated_height(...)
```

`indexed_block`と`line_context`は、同じmoduleの既存関数を通して使えるためprivateのままにする。この内部型をcrate rootから外部公開しない。追加の可視性変更が必要になったら、既存呼出元と理由を移動表へ一項目ずつ記録する。コンパイルエラーをまとめて消すための一括`pub(crate)`化は禁止する。

### 5.3 height_index.rs

`HEIGHT_CHUNK_TARGET`、`HeightChunk`とそのimpl、`HeightIndex`とそのimpl、`ScrollAnchor`、`anchored_scroll_y`を移す。

`HEIGHT_CHUNK_TARGET = 128`、負値の扱い、chunkの境界処理、木を再構築する順序、浮動小数点の加算順序、範囲外の扱いを変更しない。`HeightIndex`の内部フィールドと補助メソッドはprivateのままにする。`retree`と`add_sum`にある既存のClippy属性も一緒に移す。

**`block_heights`、`block_heights_with_disclosure`、`block_height_with_disclosure`はここへ移さない。** これらはMarkdownのブロックや非表示行から高さを決める処理であり、数値を管理する索引とは責務が異なる。次節の`projection/block.rs`へ置く。

### 5.4 projection/block.rs

次の項目を一緒に移す。

| 群 | シンボル |
| --- | --- |
| ブロック入力 | `BlockLine`、`BlockWindow` |
| 文書上の範囲と初期高さ | `block_line_span`、`trailing_blank_lines`、`block_heights`、`block_heights_with_disclosure`、`block_height_with_disclosure` |
| ブロックの表示生成 | `present_block`、`present_block_with_list_projection`、`present_block_with_table_projection` |
| 原文開示のまとまり | `disclosure_runs`、`run_uses_shared_parse`、`expected_disclosures`、`merged_disclosure`、`union_disclosure`、`joined_run_disclosure` |
| 結合した行の組立 | `present_joined_run_with_list_projection`、テスト専用の`present_joined_run` |

`expected_disclosures`と実際の表示生成で同じ行のまとめ方を使う関係を保つ。別moduleに同じ開示判定を複製しない。

`parse_joined_block`、`parse_joined_span`、`JoinedParse`、`ProjectionIndex`は親の`projection/mod.rs`に残す。`BlockWindow`の`joined`から親の`JoinedParse`を参照し、親のparse入口では再exportされた`BlockLine`を使う。この型参照は新しい状態の所有者や実行経路を増やさない。

`fence_line_role`、`table_alignments`、`table_row_from_projection`、`table_cells_from_visual_mapping`など、行の種類や表示メタデータを導く既存補助は親に残す。子はこれらをそのまま使う。さらなるファイル分割を本件の必須条件にしない。

### 5.5 projection/mod.rs

上記の移動群、既存`layout`、数値の高さ索引を除いた、現在の行の表示生成と共有解析をここへ移す。具体的には以下を含む。

| 群 | 主な項目と境界 |
| --- | --- |
| 既存の公開入口 | `present_markdown`、`present_markdown_with_disclosure`、`present_polished_line`、`table_delimiter_is_collapsed`、`parse_joined_block`、`parse_joined_span`、`JoinedParse`。wrapperも残す。 |
| 非公開の解析・索引 | `SourceIndex`、`ProjectedMarker`、`ProjectionIndex`、`SharedParse`、`ListParagraphRegion`、`ListItemProjection`とその構築・検索処理。 |
| 行の表示生成 | `present_plain`、`present_raw_source`、`present_rule_line`、`present_markdown_with_list_projection`、`present_markdown_from_parse`、`present_polished_line_with_fence`。 |
| 開示と記号 | `range_touches`、`disclosure_owns_physical_line`、table/listの境界判定、`marker_is_disclosed`、`marker_edge`、`append_segment`、`segments_tile_range`、引用の記号対応。 |
| 構文ごとの既存処理 | listの番号・配置・メタデータ、`FenceLine`とfenceの判定・表示、`StandaloneImage`と画像認識・表示、tableの配置・セル・pipe処理、`formal_quote_metadata`等。 |

これは内部処理を作り直す依頼ではない。第5.1〜5.4節の明示的な移動先に該当しない既存のprojection用項目は、まずこの親moduleにまとめて保つ。関数の細分化、似た判定の共通化、getterへの置き換えは別作業とする。

## 6. 外部からの呼び出しを維持する

外部からの呼び出し方を示す名前と型の組を、公開APIとして維持する。新しいmoduleはすべて非公開で、従来どおり`hane_presentation::SourceMap`などを使えるようにする。新しい型で包まず、同じ定義を`pub use`で再公開する。

最終的なcrate rootの公開項目は次を基準にする。crate文書と既存のcrate属性はこの前に残す。実装開始時に公開項目が増えていれば、その項目も台帳に追記して保つ。この断片を無確認で`lib.rs`全体に上書きしてはいけない。

```rust
mod display;
mod height_index;
mod layout;
mod projection;
mod source_map;
pub mod testing;

pub use hane_markdown::TableAlignment;
pub use layout::{
    BlockLayout, LIST_DEPTH_INDENT, LayoutLine, LayoutPoint, LineShaper,
    LineWrap, QUOTE_BAR_GAP, QUOTE_BAR_WIDTH, QUOTE_DEPTH_INDENT,
    TableCellFragment, TableCellLayout, VerticalMove, layout_block,
    line_visual_start,
};
pub use source_map::{
    BoundarySide, MappingSegment, MarkerEdge, PositionCandidate, SourceMap,
    Visibility, VisualOffset, VisualRange,
};
pub use display::{
    BlockDisplay, BlockKind, BlockSurface, BlockTint, BlockWeight,
    ImagePresentation, InlineDisplay, LineContext, ListAlignment, ListId,
    ListItemId, ListMarkerKind, ListMarkerMetadata, ListOwnerMetadata,
    ListRowMetadata, ListRowRole, ListStructuralPrefixMetadata, QuoteRowMetadata,
    StyleKind, StyleRun, TableCellDisplay, TableRowDisplay, VisualBlock,
    VisualLine, block_is_joinable, block_line_context, code_line_height,
};
pub use height_index::{HeightIndex, ScrollAnchor, anchored_scroll_y};
pub use projection::{
    BlockLine, BlockWindow, JoinedParse, block_height_with_disclosure,
    block_heights, block_heights_with_disclosure, block_line_span,
    expected_disclosures, parse_joined_block, parse_joined_span,
    present_block, present_block_with_list_projection,
    present_block_with_table_projection, present_markdown,
    present_markdown_with_disclosure, present_polished_line,
    table_delimiter_is_collapsed, trailing_blank_lines,
};
```

最終の`projection/mod.rs`は非公開の`mod block;`を宣言し、block側に移した既存公開項目を明示的に再exportする。`pub use ...::*`で内部項目までまとめて公開しない。

API比較では名前だけでなく、関数の全signature、公開フィールド、variant、deriveとtrait実装、`const`、lifetime、通常構成での公開有無を確認する。特に`pub mod testing`を`#[cfg(test)]`へ変えない。`layout.rs`と`testing.rs`の既存importはrootの再exportで解決するため、原則として両ファイルの変更は不要である。

`pub use`は外部互換のための恒久的な公開入口であり、後で削除する移行aliasではない。旧名と新名を並存させる別名aliasやforwarding wrapperを新設しない。内部module名を含むテスト名や診断用の型名文字列が変わり得ることは、外部APIの削除とは区別する。ただしその文字列を永続化や処理分岐に使う呼出元が見つかった場合は、無確認で互換性ありと判断しない。

## 7. 変えてはいけない動作

### 7.1 原文と表示の位置

非表示の記号や、原文にはない表示用文字があるため、原文と表示は単純な一対一対応ではない。複数候補からどちらの境界を選ぶかを表す`Bias`と、候補に付く`affinity`を保つ。

`SourceMap::visual_to_source`では現在の候補の抽出、可視・開示済み候補の優先、sort、前後の選択順をそのまま保つ。`source_to_visual`をそれと対称に書き換えない。正規化の繰り返し回数、隣接するhidden/synthesizedの扱い、`MarkerEdge`も変えない。往復のテストは現在の正規化された位置を比較し、すべての元位置へ戻るという別の期待値に差し替えない。

### 7.2 原文を隠す／見せる条件

`range_touches`、`disclosure_owns_physical_line`、table delimiterの判定、list項目の境界判定は、似ていても同じ条件ではない。次の行の先頭、改行を持つ末尾、改行のない文書末尾、隣接list項目を区別する現行分岐を維持する。

`expected_disclosures`と`present_block`系が共有する、行のまとまり、結合解析を使う条件、画面外の開示範囲の合成、独立画像の扱いを保つ。キャッシュが期待する値だけを独自に計算し直さない。

### 7.3 解析とfallback

`JoinedParse.projection`はprivateのままとする。呼出側のrevision・block identity・source rangeによる再利用条件に手を加えない。BlockIndex側の条件付きrebaseと統一しない。共有解析の再利用をやめて行ごとにparseしたり、表示中の行数を構文の意味の境界にしたりしない。

`parse_joined_block`の非空入力という現在の前提や、`parse_joined_span`の空範囲・読取範囲の扱いを変更しない。`segments_tile_range`の確認と、成立しない場合に原文をそのまま表示するfallbackを保つ。既存のwrapperやテスト専用helperを「不要そう」という理由で削除しない。

### 7.4 表示・高さ・再利用

コードをliteralに扱う優先順位、非表示のfence/table delimiter、開示時の高さの復帰、画像の推定高さ、listの番号と配置、引用の深さとownerを維持する。

`VisualLine`、`VisualBlock`のrebase、source range、revision、disclosure、quote/list/tableの関連範囲を更新する順序を変えない。table projectionを持つブロックのrebaseを拒む現行条件を緩めない。今回、失敗時の巻き戻し方式などを新しく設計しない。

`HeightIndex::splice`は変更範囲の外の測定済み高さを保つ。空の索引、0高さが連続する範囲、最終chunk、文書の一番下での検索、`visible_range`の境界を維持する。数値の高さ索引へMarkdown解析、キャッシュ管理、UIの状態を持ち込まない。

## 8. 実装を分ける順序

同じ大きなファイルを同時に複数担当が分割しない。各PRを受け入れたmainから次を開始し、機能開発の別PRは変更が重ならない範囲で継続する。途中のPRはすべて`Refs #302`とし、最後のPRも全体条件を確認する前にIssueを自動closeさせない。

| 順序 | 一つのPRで行うこと | 主な変更ファイル | 次へ進める条件 |
| --- | --- | --- | --- |
| P1 | 第5.1節の位置対応を移す。 | `lib.rs`、新規`source_map.rs`、対応するテストと移動記録。 | source_map契約、公開入口、全関連テストが変更前と一致する。 |
| P2 | 第5.3節の数値の高さ索引を移す。 | `lib.rs`、新規`height_index.rs`、対応するテストと記録。 | splice、端点、0高さ、既存の利用元が成立する。 |
| P3 | 第5.2節の表示型と方針を移す。 | `lib.rs`、新規`display.rs`、対応するテストと記録。 | 方針の表、型とimpl、限定した可視性変更を確認する。 |
| P4 | 残った行・ブロックのprojection一式を、まず一緒に`projection/mod.rs`へ移す。 | `lib.rs`、新規`projection/mod.rs`と`projection/tests.rs`等。 | rootが宣言と公開入口になり、共有解析の内部を公開せず全テストが通る。 |
| P5 | 受入済みprojectionから、第5.4節のブロック組立だけを子へ移す。 | `projection/mod.rs`、新規`projection/block.rs`、必要なblock用テストと最終記録。 | 最終配置、全移動表、API比較、テストと後続への引き渡しが揃う。 |

P4とP5を分けるのは、crate rootからの移動と、projection内部の境界変更を別々に検証するためである。block側の項目は二段階で位置が変わるが、途中に仮の公開module、仮のadapter、`#[path]`による移行接続は作らない。各段階でコンパイル可能な状態を保つ。

P4は行数が大きくなり得るが、分岐や処理を書き換えない。機械的移動として説明できない差分が混じった場合は、その変更を別PRへ分ける。既存の不具合を発見した場合も、元の期待値を勝手に変えて今回のPRを通さない。

### 8.1 最初の実装担当への具体的な指示

最初の対象はP1だけである。現在のmain、#302、本書、関連PRと作業ツリーを確認し、変更前のpresentationテスト一覧・実行結果と公開項目を保存する。その後、第5.1節の8型と全implを`source_map.rs`へ移し、rootに非公開module宣言と明示的な再exportを追加する。

P1では`display`、`HeightIndex`、projection、layout、UIを整理しない。テストを通すための変更はimport・所属・必要な文書参照だけとし、元のassertionを変えない。第9節の結果、旧→新の移動表、公開項目に差がないこと、対象SHAを実装PRへ提出する。

### 8.2 変更前に不足していたテストを追加する場合

既存テストを確認して、公開入口などに不足があれば、先にテストだけを追加する。移動前の実装でも通ることを確認し、製品挙動を変更しない。テスト追加と移動は少なくともcommitを分け、契約の追加判断を伴う場合は別PRとする。

公開項目の取りこぼし対策には、外部crateとしてrootの全公開名を明示importするcompile testと、必要なsignature・公開フィールドの確認を使える。ただしimport成功だけをAPI全体の同一性証明にはしない。新しい検証用依存やAPI比較基盤の導入を必須にせず、既存のテストとシンボル表を使う。

## 9. テストと受入証拠

### 9.1 既存テストの移動規則

integration testsは元のファイルとcrate root経由のimportを保つ。現在の対象は`source_map_contract.rs`、`layout_contract.rs`、`markdown_features.rs`、`atx_headings.rs`、`line_break_whitespace.rs`およびそれらの`support`である。

unit testsは名前、属性、入力fixture、assertion、本体を保ったまま、検証する責務のmoduleへ移す。privateの`HeightChunk`等へ触るテストは`height_index`の子へ、`ProjectionIndex`や`SourceIndex`を調べるテストは`projection`の子へ置く。P5でblock内のprivate関数へ触るテストは`projection/block/tests.rs`へ移す。子のテストは祖先に残る共有解析も参照できる。

テストのアクセスのために製品型のフィールドやhelperを公開しない。複数群に本当に必要な既存テストhelperだけは、`#[cfg(test)]`の共有補助へ一度移し、複製を避ける。新しいテストframeworkは作らない。

比較は総件数だけでなく、旧テスト名→新テスト名、`#[ignore]`等の属性、fixture、assertionを確認する。module名の変更によるprefixの違いは移動表で説明し、テスト削除・ignore追加・条件付き除外を移動に紛れ込ませない。

### 9.2 契約との対応

| ID | 維持する内容 | 主な既存の検証先と確認方法 |
| --- | --- | --- |
| T1 | hidden/synthesized/expanded、Bias、MarkerEdge、候補の優先、正規化往復。 | `source_map_contract`と該当unit tests。UTF-8の範囲、境界、期待値を前後で比較。 |
| T2 | 見出し、style合成、引用、list、画像、コード、tableの表示方針。 | `markdown_features`、`atx_headings`、表示のunit tests。表示文字列だけでなくkind/style/metadataを比較。 |
| T3 | 共有解析と画面外の記号、開示範囲、独立画像、原文fallback。 | projectionのunit testsと既存契約。`expected_disclosures`と表示生成の一致を維持。 |
| T4 | LF/CRLF、文書末尾、次の行や次のblockに属するcaret、非表示delimiterの高さ。 | `line_break_whitespace`と該当unit tests。既存のfence/table/quote境界ケースを落とさない。 |
| T5 | chunk、splice、測定値の保持、空・0高さ・最終位置・表示範囲。 | `HeightIndex`のunit tests。`height_splice_preserves_measurements_outside_the_changed_blocks`などを維持。 |
| T6 | sourceと画面座標、折り返し、table cell、非表示行、rebase。 | `layout_contract`、`VisualLine`/`VisualBlock`のunit tests。layout本体とFixedAdvanceShaperを変えず確認。 |
| T7 | 公開APIと通常構成の利用、既存のcrate依存方向。 | 第6節の台帳、通常・all-featuresのworkspace tests/Clippy、必要なcompile test。 |

T4の具体的な既存ケースには、`initial_block_heights_keep_the_caret_owned_fence_row_visible`、`inactive_table_delimiter_does_not_reserve_virtual_height`、`selection_outside_a_quote_fence_block_does_not_disclose_its_rows`、`caret_at_the_next_block_start_does_not_disclose_the_previous_quote`、`caret_at_a_quote_owner_end_discloses_fence_rows_before_following_list_text`がある。これらを新しい単純な境界規則に合わせて修正しない。

### 9.3 実行するコマンド

次は実装時に実行するコマンドであり、設計作成時の実行結果ではない。OS、toolchain、base/head、コマンド、終了コード、ログを対応付けて記録する。最初に変更前の基準を取り、各PRの候補で同じ条件の結果を取得する。

```sh
# 現在のテスト名を記録する。移動後はmodule prefixの対応を確認する。
cargo test --locked -p hane-presentation -- --list

# 対象crate全体と、外部からの契約。
cargo test --locked -p hane-presentation
cargo test --locked -p hane-presentation --all-features
cargo test --locked -p hane-presentation --test source_map_contract
cargo test --locked -p hane-presentation --test layout_contract
cargo test --locked -p hane-presentation --test markdown_features
cargo test --locked -p hane-presentation --test atx_headings
cargo test --locked -p hane-presentation --test line_break_whitespace

# 既存利用元を含む、通常構成と全feature構成。
cargo test --workspace --locked
cargo test --workspace --locked --all-features
cargo clippy --workspace --locked --all-targets --all-features -- -D warnings

# 移動による参照切れ・余計な差分を確認する。
cargo doc --locked -p hane-presentation --no-deps
cargo fmt --all -- --check
git diff --check
```

対象crate全体の実行には個別integration testも含まれる。毎回二重に実行することは必須にせず、個別コマンドは失敗箇所の確認や契約別の証拠取得に使う。通常並列実行を基本とし、直列実行だけの成功を通常構成の成功に置き換えない。

macOSとWindowsの既存CIによるworkspace tests/Clippyを確認する。片方のローカルで実装してよいが、片方だけの成功を両OSの成功としない。Linuxのローカルで対象crateを検証できても、native環境の確認を代替したことにはしない。

rustdocのリンク先は移動後も存在する公開入口へ合わせる。`#[cfg(test)]`の関数への既存リンクなど、変更前からの警告があれば前後を比較する。リンクを直すために製品側の`cfg`や公開範囲を変えない。無関係な文書警告を隠すための全体allow追加も行わない。

### 9.4 GUI・性能の適用範囲

本書どおり、関数本体・条件・型契約・UI配線・layoutを変えない移動だけなら、新しいGUI手順や性能測定器を作らない。#302に従い、追加のfocused GUIが不要と判断する場合は、変更範囲とT1〜T7の結果、およびRF0-04の既存記録との対応をPRへ記す。「機械的移動だから何も確認しない」という扱いにはしない。

実画面の配線、native入力、shape/layout、表示や座標に実質的な変更が必要になった場合は、本件の移動から別PRへ分け、その範囲のfocused GUIを行う。画面検証が必要と判断されたものを、静的な画像やunit testだけで代替しない。測定が成立しない場合は未検証のまま記録する。

今回の成果は責務を識別できる配置であり、性能改善ではない。#23の基準、既存の処理量・上限を守るテストは保ち、解析回数、clone、走査、割当、計算順序が変わる変更は本件に混ぜない。実測を行っていない速度やメモリ値は報告しない。

## 10. 各PRに残す記録と完了条件

各PRは本書のP番号と #302、RF0-04を参照し、旧ファイル・シンボル→新ファイルの表、公開範囲の変更一覧、テストの移動表、変更前後の対象SHA、結果とログ、未実施事項を残す。`git diff --color-moved`等は補助に使えるが、移動色が付いたことだけを同等性の証拠にしない。

rootの公開項目は、名前、signature、公開フィールド/variant、trait/derive、cfgを前後比較する。原則として外部公開範囲の差はゼロである。内部可視性は第5.2節の限定変更を明示する。`use`と再export以外の実行コード差があれば、その理由を確認し、意味変更なら移動PRから除く。

既存の全体実行計画には、実装を受け入れた時点で短い進捗と実装PR・証拠への参照を追記する。詳細設計や全テストログを何か所にも複製しない。

- [ ] 第4〜6節の配置と公開入口が成立し、すべての既存シンボルを移動表で追える。
- [ ] 第7節の分岐・処理・型・依存・原文契約を変更せず、新しい一時aliasや二重実装を残していない。
- [ ] テストを減らさず、通常構成・all-features・必要な両OSの検証が対象headで成立し、既存失敗と今回の回帰を区別できる。
- [ ] 現行Policyに従った最終候補の独立レビューと必要な検証があり、未実施・blocked・unknownを成功としていない。
- [ ] 各責務を独立して戻せる記録と、後続への境界説明が揃い、全体の受入後にだけ #302 を閉じる。

設計用PRと途中の実装PRは完了条件を満たした扱いにしない。review件数をゼロにするための無関係な修正を混ぜず、必要なCI・review・GUIの対象headと、影響するbase contextをその時点のGitHubから確認する。

## 11. 戻し方と後続への引き渡し

永続データ、依存、保存形式を変えないため、該当PRのrevertで戻せる。後続の移動がその場所を利用していれば、P5からP1へ必要な範囲を逆順で戻す。後から同じ場所に入った機能修正を機械的に消さず、戻す対象と衝突を確認する。

| 後続 | RF2-Bが渡すもの | RF2-Bでは行わないこと |
| --- | --- | --- |
| #306：解析・projection整理 | `projection/mod.rs`の共有解析・行生成と`projection/block.rs`の組立境界、維持した公開入口。 | snapshotの統一、wrapper削除、再利用条件変更、解析経路の意味変更。 |
| #307：座標の集約 | `source_map.rs`の位置対応、`display.rs`の表示契約、変更していない`layout.rs`。 | UIの独自補正削除、shapeやcaret/IME座標の配線変更。 |
| #368：キャッシュ・高さ索引・job | 数値専用の`height_index.rs`と、構文から初期高さを決めるblock側の境界。 | cache所有者、無効化条件、job寿命、処理上限の変更。 |

次の担当は設計を最初からやり直すのではなく、基準からの差を確認し、まず第8.1節のP1の変更前検証と機械的移動へ進む。設計と現行実装に相違があれば、該当する移動表と理由だけを更新し、本件の範囲を拡張しない。
