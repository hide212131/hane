# Issue #766 実装準備：入れ子リスト項目の表示位置がキャレット位置でずれる問題

状態： 実装準備（未実装・未検証）。この文書は実装担当への引き継ぎであり、後続の依頼で実装する。

# Issue #766 実装設計：親項目の開示で子項目の横位置を動かさない

**採用方針:** 子のマーカーが合成表示のままなら、親の開示で現れる字下げを、子の既存インデントの内側に描く。子のマーカーと本文は動かさない。子自身のマーカーが開示された場合は、現行の「生マーカーの実測幅が座標の基準」という契約を維持する。

## 1. 原因

再現 `- abc\n  - def` で動いているのは **`expanded_prefix_width` だけ**。親の本文末尾はソースオフセット `5`、子行の先頭の空白は `6..8`、子自身の `- ` は `8..10`。joined run はオフセット `5` の開示を子行の投影にも渡すが、行やマーカーを一律には開示しない。`marker_is_disclosed` は構造プレフィックスと項目マーカーをそれぞれの所有項目で判定する。したがって、子行の先頭空白は親が所有するので開示される一方、子のマーカーは合成のまま残る。根拠は `crates/presentation/src/projection/block.rs:1023`、`crates/presentation/src/projection/mod.rs:361`、`crates/markdown/src/lib.rs:1092`。

次の値は、既存コードを `layout_contract` の `FixedAdvanceShaper`（1文字8 px）で追った結果。テストの実測結果ではない。

| 子行の値 | 開示なし | 親の本文末尾を開示 |
|---|---:|---:|
| `visual_text` | `• def` | `  • def` |
| 親が所有する空白の `Visibility` | `HiddenMarkup` | `ExpandedMarkup` |
| `marker.synthesized` | `true` | `true` |
| `marker.visual_range` / `body_visual_start`（UTF-8バイト） | `0..4` / `4` | `2..6` / `6` |
| 集約ラベル幅 / 現在のマーカー実測幅 | `16` / `16` | `16` / `16` |
| `marker_column_width` / `marker_body_gap` | `16` / `0` | `16` / `0` |
| `expanded_prefix_width` | `0` | `16` |
| `marker_x_origin` フィールド / 実際のマーカー描画原点 | `16` / `16` | `16` / `32` |
| `body_x_origin` | `32` | `48` |

`disclosed_marker_width` は変数名に反して合成マーカーも測る。この再現では両状態とも `max(aggregate_marker_width, disclosed_marker_width)` を通り、列幅の基準は切り替わらない。また、現行の `marker_x_origin` は意味上の列原点であり、描画側は `text_x_origin` から空白とマーカーを順に描く。**`marker_x_origin` だけを比較するテストでは、この不具合を検出できない。** 根拠は `crates/presentation/src/layout.rs:1700`、`crates/ui/src/line.rs:1312`。既存の `list_marker_discloses_for_every_ancestor_whose_source_range_the_caret_enters` も、親だけを開示したときに子行が `  • inner` になる契約を固定している（`crates/presentation/src/projection/tests.rs:1147`）。

## 2. 不変条件と対象範囲

- **固定する:** 引用・タスクを含まない通常の入れ子箇条書きの開始行で、先頭の構造インデントが ASCII 空白のみ、かつ子自身のマーカーが両状態で合成のままの場合。親の開示だけで、子の実際のマーカー描画原点、本文原点、本文の折り返し予算を変えない。空白の表示とそのソース対応は維持する。
- **固定しない:** 自身のマーカーが開示される行。カーソルが直接その行にある場合だけでなく、子孫に入った結果として祖先自身のマーカーが開示される場合も、現行の生マーカー幅・生の字下げを維持する。リスト全体の geometry や全バイトのキャレット座標を開示不変にはしない。
- **既存テストの期待値変更はなし。** `disclosed_markers_use_their_source_visible_width_without_a_virtual_gap`、`disclosed_empty_markers_put_the_terminal_caret_after_raw_source`、`disclosing_a_short_ordered_marker_drops_only_the_inactive_alignment_gap` は生マーカー幅と仮想ギャップなしを固定する。`disclosed_structural_prefix_width_is_included_in_list_body_geometry`、`disclosed_continuation_prefix_does_not_readd_marker_width`、`disclosed_list_prefix_is_subtracted_only_on_the_first_wrap_fragment` は自身の字下げ・継続行・折り返しを固定する。`nested_list_rows_use_semantic_depth_after_opening_prefixes_are_hidden` の非開示時の期待値も維持する。いずれも `crates/presentation/tests/layout_contract.rs:1389`、`crates/presentation/tests/layout_contract.rs:1759`、`crates/presentation/tests/layout_contract.rs:2310` のリスト契約群。

## 3. 変更するファイルと計算

1. **`crates/presentation/src/display.rs:342` — `ListRowMetadata`**  
   開示に依存しない `leading_structural_space_count: Option<usize>` を追加する。行頭から自項目のマーカー直前までを構造プレフィックスが連続して覆い、そのソースが ASCII 空白だけの場合に空白数を持つ。タブや構造外の文字を含む場合は `None`。隠れた空白も数える。
2. **`crates/presentation/src/projection/mod.rs:2113` — `list_row_metadata` と呼び出し元 `present_markdown_from_parse`**  
   物理行のソースと既存の構造プレフィックス範囲から上記メタデータを一度作る。開示後の `visual_text` から逆算せず、ローカル parse と文書全体の投影で由来が異なり得る `item_id` の数値比較にも頼らない。`marker_is_disclosed`、`list_marker_metadata`、source map の生成は変更しない。
3. **`crates/presentation/src/layout.rs:1611` — `line_geometry`**  
   対象分岐だけ、`marker_x = max(list_depth_x(depth), 全先頭空白の整形幅)` とする。全空白の幅はメタデータと `LineShaper` から求め、開示前後で同じ値を使う。`P` を実際に表示されている行頭からマーカー開始までの整形幅とすると、`text_x_origin = marker_x - P`、`body_x_origin = marker_x + marker_column_width` とする。つまり先頭空白をマーカーの**手前**に描き、本文原点へ二重加算しない。固定の深さインデントより空白幅が大きいフォント・ズームでは、両状態で同じ幅を先に確保するため、負の描画原点に頼らない。現行のマーカー列幅とマーカー後のギャップ計算は維持し、右寄せは導入しない。先頭断片の予算は更新後の `text_x_origin` から算出するので、表示された空白幅だけ広がる。後続断片の原点と予算は従来どおり本文原点から算出する。

**変更しないもの:** `list_marker_widths` の集約、joined parse と開示判定、Markdown パーサー、GPUI の描画・入力処理、source map API。番号付き・タスク・引用付きリスト、タブを含む字下げ、継続行、開示された自項目のマーカーは既存経路に残す。

## 4. 追加テスト

配置先は `crates/presentation/tests/layout_contract.rs:2310` の入れ子リスト geometry 契約群。既存の `present` ヘルパーで `BlockIndex` とリスト投影を通し、手組みの `VisualLine` は使わない。

- **`inactive_nested_bullet_geometry_is_stable_when_parent_is_disclosed`** — `- abc\n  - def` を `None` と `Some(5)` で比較する。子の空白が `HiddenMarkup` → `ExpandedMarkup` になり、子のマーカーは両方 `synthesized == true` と確認する。`body_x_origin == 32` と `marker_x_origin == Some(16)` に加え、`visual_spans_on_row` に子の `marker_visual_range` を渡し、**実描画に対応する左端も両方 `16`** と確認する。`def` 先頭の `point_for_source` → `source_at_x` の往復も固定する。同じ fixture を1文字10 pxでも比較し、インデントを超える空白幅で原点がずれないことを確認する。
- **`inactive_nested_bullet_wrap_is_stable_when_parent_is_disclosed`** — 子本文を長くして狭幅で比較し、本文のソース上の折り返し境界、後続断片の `text_x_origin == body_x_origin`、プレフィックスとマーカーが途中で分断されないことを確認する。開示で変わる visual byte offset 同士は比較しない。
- **`nested_bullet_keeps_raw_geometry_when_its_own_marker_is_disclosed`** — 同じ子の `def` 内にカーソルを置き、`synthesized == false`、マーカー後の仮想ギャップなし、既存の生プレフィックス込みの本文原点（8 px shaper で `48`）を確認する。

## 5. 司令塔の確認コマンド

```sh
cargo test -p hane-presentation --test layout_contract inactive_nested_bullet_
cargo test -p hane-presentation --test layout_contract nested_bullet_keeps_raw_geometry_when_its_own_marker_is_disclosed
cargo test -p hane-presentation --test layout_contract
cargo test -p hane-presentation --lib projection::
```

読み取り対象はローカルの `9967e9fa`。コード・ファイルの変更、テスト実行は行っていない。GitHub の Issue 本文は取得できなかったため、受入条件は依頼文に提示された内容を基準とした。