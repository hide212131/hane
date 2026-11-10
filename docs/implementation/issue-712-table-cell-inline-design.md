## 概要

Issue #712 は、非アクティブな表セルにも既存のインライン投影処理を適用して修正する。

表専用のセル分割・パイプ隠蔽・列情報の生成は維持する。各セルの生ソースをそのまま追加している部分を、既存の `present_markdown_from_parse` による投影へ置き換える。これにより、太字・イタリック・インラインコード・打ち消し・インラインリンクについて、通常段落と同じマーカー隠蔽、スタイル、マッピングを利用する。

調査対象は HEAD `27b5ebc2e04d4c25ec30aca624fe0e9fe4b106ba`。本書は修正設計であり、ファイル変更、テスト実行、GUI検証は行っていない。

## 根本原因

[projection/mod.rs](../../crates/presentation/src/projection/mod.rs#L2380) の `present_polished_line_with_fence` は、次の条件で表専用経路を選ぶ。

- `context == LineContext::Table`
- キャレット・選択による開示が、その物理行を所有していない

この経路の `present_table_line` は、セルの生ソースを `Visibility::Visible` のセグメントとして追加し、行全体に `StyleKind::Table` を付与する。インラインのマーカー解析とスタイル生成を行わないため、`**h1**` がそのまま表示される。

通常経路の `present_markdown_with_list_projection` は、`parse_document` と `ProjectionIndex` を経由して `present_markdown_from_parse` を呼ぶ。こちらはマーカーを `HiddenMarkup` にし、構文ノードから `Bold` 等のスタイルを生成する。

したがって、原因は描画側の太字適用漏れではなく、表専用経路でインライン投影が欠落していることにある。

## 修正方針（具体的な変更箇所と関数）

### 現行の関数と再利用範囲

[projection/mod.rs](../../crates/presentation/src/projection/mod.rs#L2971) の現行シグネチャは次のとおり。

```rust
pub(crate) fn present_table_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    header: bool,
    alignments: &[TableAlignment],
) -> VisualLine
```

通常経路は次の関数を使う。

```rust
fn present_markdown_with_list_projection(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
    local_bare: LocalBareLinks,
) -> VisualLine

fn present_markdown_from_parse(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    shared: &SharedParse<'_>,
) -> VisualLine
```

`SharedParse` は次の情報を持つ。

```rust
struct SharedParse<'a> {
    parsed: &'a MarkdownParse,
    projection: &'a ProjectionIndex,
    list_projection: Option<&'a ListProjection>,
    source: &'a str,
    local_bare: LocalBareLinks,
}
```

**採用方針は、セル投影用の小さなアダプターを追加し、`present_markdown_from_parse` を再利用する方式とする。** 通常経路の大規模な共通化や、装飾ごとの独自実装は行わない。

### セルをインライン文脈で解析する

セル文字列をそのまま通常 presenter に渡すだけでは不十分である。セル内の `# title`、`---`、`- item`、バッククォート列などが、見出し・水平線・リスト・コードブロックとして解釈され得る。

この誤認を避けるため、セルを一時的な二列表の第1セルに包んで既存 parser に渡す。

```text
| {セルの生ソース} | x |
| --- | --- |
```

前後の補助空白、パイプ、第2セル、区切り行は解析用であり、出力しない。セル末尾のバックスラッシュが補助パイプをエスケープしないよう、セルと補助パイプの間に空白を置く。

処理は次の順序とする。

1. 元ソースのセル範囲を、現行の `unescaped_table_pipes` で確定する。
2. セルごとに仮想表を `parse_document` で解析する。
3. `ProjectionIndex::new` と `SharedParse` を構築する。
4. 仮想表中の**元セルに対応する範囲だけ**を、`disclosure: None` で `present_markdown_from_parse` に渡す。
5. 得られた表示テキスト、SourceMap、インラインスタイルを行へ合成する。

追加する内部関数の案は以下とする。

```rust
fn present_inactive_table_cell(
    line_id: u64,
    revision: Revision,
    source_range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualLine
```

返却する `VisualLine` は内部の運搬用とし、行へ移すのは `visual_text`、`source_map`、インラインの `style_runs` だけとする。ブロック種別、表情報、見出し相当のメタデータは移さない。

仮想表として解析できたこと、および対象範囲が第1セルに属することを確認する。成立しない場合は、そのセルだけ生ソース表示に戻し、文字を欠落させない。

### `present_table_line` の変更

現行のセル追加箇所を、上記アダプターを使う内部ヘルパーへ集約する。

- セルの表示テキストを行末へ追加する。
- セルの各セグメントとスタイルの visual 座標に、追加前の行長を加算する。
- `TableCellDisplay` を、追加後の表示範囲で生成する。
- 行全体の `StyleKind::Table` とセル内のスタイルを共存させる。

`InlineDisplay` は重なるスタイルのフラグを合成する設計であり、`Table` 自体はインライン装飾を指定しないため、`Bold` 等を重ねられる。

`present_table_line` のシグネチャは維持する。表示側と計測側に別々の装飾ロジックを追加しない。

### リンクの扱い

[projection/block.rs](../../crates/presentation/src/projection/block.rs#L318) は、正式な `TableProjectionRow.links` から `LinkTarget` を補完している。クリック対象の正本はこの既存経路に維持し、セルの局所解析で得たリンクを無条件に行へ追加しない。

通常の `[label](url)` は、セル解析でマーカー隠蔽と `Link` スタイルを生成できる。

別行の定義を使う参照リンクは、セル単独の解析では解決できない。今回の変更では参照リンクの新たな構文隠蔽は追加せず、既存の正式な対象範囲とリンク先を維持する。そのラベルへの `Link` スタイルは、既存補完処理を拡張して付与する。現在の「bare URL のみ」という条件を、重複スタイルを避けながら正式なリンクラベル全体へ広げる。

参照定義の解決やリンク安全性の判定を、セル presenter に再実装しない。

## マッピングとレイアウトへの影響

### SourceMap とセル範囲

現行の主要な型は以下である。

```rust
pub struct MappingSegment {
    pub source_range: SourceRange,
    pub visual_range: VisualRange,
    pub visibility: Visibility,
    pub marker_edge: Option<MarkerEdge>,
}

pub struct TableCellDisplay {
    pub column: usize,
    pub source_range: SourceRange,
    pub visual_range: VisualRange,
    pub alignment: TableAlignment,
}
```

`source_range` はマーカーを含む元ソースの範囲に維持する。`visual_range` は、マーカーを隠した後の行内表示テキストの範囲に変更する。既存の前後空白は維持し、trim を追加しない。

例えば、改行なし・source 開始位置0の次の行を考える。

```text
| **h1** | h2 |
```

期待値は次のとおり。

| 項目 | 修正後 |
|---|---|
| 行の visual text | `" h1  h2 "` |
| 第1セル source range | `[1, 9)` |
| 第1セル visual range | `[0, 4)` |
| Bold の visual range | `[1, 3)` |
| 第2セル source range | `[10, 14)` |
| 第2セル visual range | `[4, 8)` |

第1セルの `**` は、削除するのではなく、visual 幅0の `HiddenMarkup` セグメントとして保持する。開始・終了の `MarkerEdge` も既存処理の結果を保持する。

仮想表の source 座標は、対象セルの仮想開始位置を引いてから元セルの絶対開始位置を加える。visual 座標には行内の追加位置を加える。すべてUTF-8のバイトオフセットとして扱い、仮想文字列の位置を返却値へ漏らさない。

### hit-test とキャレット

現行の変換は次の形である。

```rust
pub fn visual_to_source(
    &self,
    visual: VisualOffset,
    affinity: Bias,
) -> Option<PositionCandidate>

pub fn source_to_visual(
    &self,
    source: SourceOffset,
    affinity: Bias,
) -> Option<PositionCandidate>
```

`BlockLayout::source_at_x` は、セルの配置から visual 位置を求めた後、SourceMap で source 位置へ戻す。リンク用の `glyph_source_at_xy` も同じ SourceMap を使う。

壊してはいけない条件は以下とする。

- 可視文字内部の visual→source が、元ソースの同じ文字を指す。
- セル内装飾の `MarkerEdge` を失わず、境界の `Bias` を通常経路と同様に扱える。
- 隠れたマーカー内部は同じ visual 位置へ縮退する。全source位置の完全な往復一致は要求しない。
- セル境界・空セルで複数のsource位置が同じvisual位置になる場合、既存のセル選択とaffinityの規則を維持する。
- リンクの最後の文字の右半分でも、glyph hit-test がリンクラベル内のsource位置を返す。

キャレット移動後は元のsource位置から再投影し、その行を既存の開示経路へ切り替える。短縮後のvisual位置を編集位置として保存しない。

### 列幅と alignment

[layout.rs](../../crates/presentation/src/layout.rs#L1334) の `table_column_widths` は、画面外の正式な行も `present_table_line` で投影し、`table_cell_intrinsic_widths` で測定する。

今回の修正によって、計測も次の条件になる。

- 非表示の `**`、バッククォート、リンク先文字列等は幅を消費しない。
- 太字、イタリック、等幅などのフォント指定は shaper を通じて幅へ反映する。
- ヘッダ用の既存ウェイトも維持する。

**列幅は表示テキストと実際のスタイルを基準に計算する方針を採用する。** 生マーカーを含む幅を確保すると、表示内容と計測が食い違うためである。

ただし、現在表示中の開示行も列幅計算に参加する。開示によって幅が増える可能性は既存方針のままとし、今回、開示時の列幅固定は追加しない。

alignment の値、セルpadding、列幅配分方式は変更しない。中央・右寄せでは、短縮後の表示幅に基づいて文字原点が変わる。描画とhit-testが同じ配置情報を使うことを検証する。

## エッジケースと判断

| ケース | 判断 |
|---|---|
| `\|` | 元ソースの既存分割を使い、同じセルに保持する。バックスラッシュの奇偶判定を維持する。エスケープ文字自体の表示改善は今回追加しない。 |
| セル境界をまたぐ `**` | セルごとに解析し、別セルのマーカーと対応させない。`| **a | b** |` はセルをまたぐ太字にしない。 |
| 未閉鎖の `**` | parser が装飾と認めない部分を隠さない。正規表現で一律除去しない。 |
| コードスパン内の `**` | コード区切りだけを隠し、内部の `**` は表示する。`InlineCode` を付け、`Bold` は付けない。 |
| コード内のパイプ | 今回は表の既存分割規則を維持する。未エスケープのパイプの構造解釈を変更しない。 |
| セル内リンク | インラインリンクの表示と正式な `LinkTarget` を、同じsource範囲とSourceMapで結び付ける。 |
| 参照リンク | 別行定義によるクリック対象を維持し、正式なラベルへスタイルを補完する。新たな参照構文隠蔽は対象外とする。 |
| ヘッダと本文 | インライン処理は同一とし、既存のヘッダ背景・ウェイトを重ねる。本文の `#` 等をブロック構文へ昇格させない。 |
| 空セル・不足列・余剰列 | 現行の補完・列数制限を維持する。余剰部分のsource coverageも欠落させない。 |
| 日本語・絵文字・CRLF | バイトオフセットを維持し、行末処理をセル投影へ混ぜない。 |

## 変更しないもの

- キャレット・選択が行を所有する場合の通常経路と、その開示条件。
- `TableProjection` の構造認識、列数、正式なセル範囲、alignment の判定。
- 区切り行の非表示、開示、仮想高さの扱い。
- Tab等によるセル移動、行列追加・削除、選択、undo / redo の編集ロジック。
- リンク先の解決・許可スキームの判定。
- インライン構文の対応範囲そのもの。

## テスト計画

### presentation 単体テスト

[projection/tests.rs](../../crates/presentation/src/projection/tests.rs#L1906) の既存表テストを基礎に、次を追加する。

1. **装飾**
   - 太字・イタリック・打ち消し・コード・インラインリンクをヘッダと本文で検証する。
   - visual textから対応マーカーが消え、期待するスタイル範囲が付くこと。
   - 行全体の `Table` スタイルが残ること。
   - 入れ子装飾と、日本語・絵文字を含む装飾を検証する。

2. **マッピング**
   - 上記 `"| **h1** | h2 |"` の具体値をassertする。
   - 開始・終了マーカーが幅0で、正しい `MarkerEdge` を持つこと。
   - セグメントが元ソースを欠落・重複なく覆うこと。
   - 非ゼロの絶対source開始位置でも正しく変換すること。
   - 可視文字内部の逆変換、境界の両 `Bias`、正規化後の安定性を検証する。

3. **構文境界**
   - `\|`、連続バックスラッシュ、セルをまたぐマーカー、未閉鎖マーカー、コード内の `**`。
   - `# title`、`---`、`- item` がセル内でブロック表示へ変化しないこと。
   - 空セル、不足列、余剰列、末尾改行あり／なし。

4. **リンクと開示**
   - 既存の `table_row_reference_link_keeps_destination_defined_on_another_line` を維持する。
   - インラインリンク・参照リンク・bare URL混在時の対象範囲、リンク先、スタイル重複を検証する。
   - 同じ行を非アクティブ／キャレットあり／選択ありで投影し、通常経路の結果を維持すること。

### layout テスト

- 非アクティブ行の列幅がマーカーの生文字数で増えないこと。
- 太字・等幅のスタイルが計測に渡ることを、スタイルを区別するテスト用shaperで確認する。
- 同じ正式projectionと非開示条件で、画面外の装飾行も列幅に反映され、スクロール前後で幅が変わらないこと。
- 左・中央・右寄せ、折り返し、狭いviewport、空セルを検証する。
- `source_at_x_with_bias` と `glyph_source_at_xy` が、短縮した表示から正しいsource文字へ戻ること。
- 開示行の既存計測と区切り行の高さを維持すること。

### UI経路の回帰テスト

`hane-gui-test` の方針に従い、`crates/ui/src/view.rs` の `gpui::TestAppContext` を使った実イベントテストを追加する。

- 非アクティブセルの文字を座標クリックし、期待するsource位置へキャレットが移る。
- 開示後も意図した文字位置へ挿入され、隣接セルやマーカーを誤編集しない。
- リンクラベルの先頭・末尾文字をクリックし、既存のリンク操作が正しい対象を選ぶ。
- セル移動、行列編集、undo / redo、選択表示を既存操作で確認する。
- 中央・右寄せ、折り返し後も描画位置とクリック位置が一致する。

### macOS実機GUI確認

1. 対象headのSHAを記録し、そのheadから **Debugビルド** を作成する。実行ファイルの絶対パスと由来を記録する。
2. 一意のfixtureと `HANE_STATE_DIR` を使い、専用Haneを起動する。実PIDと実行パスを記録する。
3. 初回操作前に `agent-desktop skills get desktop` と `agent-desktop skills get platform` を読む。
4. 太字・イタリック・コード・打ち消し・リンク、日本語、alignment、折り返しを含む表を開く。キャレットを表外へ置く。
5. スクリーンショットで、マーカー隠蔽、文字装飾、列幅、寄せ位置を確認する。
6. セルクリック→入力→undo→表外クリックを行い、開示と非開示の往復を確認する。保存後のfixture bytesも照合する。
7. 対象SHA、Debug profile、OS、PID、実行パス、操作前後の画像を証拠として残す。

GUI障害時は `docs/gui-test-recovery.md` に従って復旧し、元シナリオを再実行する。起動成功だけを受入証拠にしない。

## リスク

- **仮想表の座標変換ミス**  
  誤クリック・誤編集につながる。非ゼロsource開始位置、UTF-8、セル先頭末尾、逆方向hit-testを必須の回帰条件とする。

- **補助文字の解析結果への混入**  
  対象セルだけを投影し、仮想表の構造を確認する。補助文字のセグメント・スタイルを出力しない。解析不成立時は生ソースへ戻す。

- **参照リンクの表示差**  
  正式な対象情報を保持する。参照構文の新規隠蔽をこの修正へ混ぜず、リンクスタイルとクリック対象の整合を検証する。

- **大きな表での再解析コスト**  
  計測でもセル投影が増える。代表的な大規模表で確認し、問題が観測された場合に行単位の解析共有を検討する。最初から永続キャッシュは追加しない。

- **開示時の幅変化**  
  マーカー表示とフォント差により列幅が変わり得る。表示とhit-testの一致を必須とし、列幅固定は別の設計課題とする。

設計判断用のJev CLIは、既定providerの認証未設定で応答を取得できなかった。本書はその結果を代作せず、確認したコードと既存Policyに基づく設計案として記載した。


## Commander 追補（2026-11-10）

- 実装の基底は origin/main `3fc60dd7`（v0.45.0 リリース準備後）。設計書が調査対象とした `27b5ebc2` はローカル main の旧参照であり、本書が参照する presentation の該当コード（`present_table_line`、行表示の分岐、`table_column_widths`）は基底でも同一であることを Commander が確認した。
- Jev 判断を実施済み: 設計妥当性 0.73、次の action は「この設計で実装」を確率 0.96（confidence 0.94）で選択。本書末尾の「Jev CLI は認証未設定」という記載は、設計実行環境（読み取り専用サンドボックス）のソケット制限による誤記録であり、実際の Jev CLI は導入済みで上記のとおり応答を得ている。
- 受入条件と実装範囲は `docs/implementation/issue-712-table-cell-inline-handoff.md` を正とする。
