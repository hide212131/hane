# ADR-0021: LayoutLine と visual 座標系

## ステータス

承認済み

## 日付

2026-08-28

## 背景

R4A で仮想化の単位は Markdown ブロックになったが、カーソル・選択・IME・クリックは
物理行を指したままだった。ブロックの中は「1物理行 = 画面1行」を前提に、行の高さを
`line_height` 一定として扱っている。

この前提は折り返しで崩れる。

- 折り返した段落は画面上で複数行を占めるが、source 上は1行である。「1つ下の行」は
  「次の source 行」ではない。
- 上下移動は `preferred_grapheme_column`（source 行内の grapheme 数）で実装されている。
  折り返し行には自分の source 行が無いので、column では移動先を表現できない。
- クリック位置の解決は行全体を1本の shape 結果として測っていた。折り返し後の
  2行目をクリックしても、1行目の同じ x として解釈される。
- ブロック高さは「行数 × 行高」で、折り返しの分だけ実際より低い。IME の候補ウィンドウ位置
  （`bounds_for_range`）は要素全体の矩形を返しており、カーソル矩形ではなかった。

R4B の目的は、ブロックと描画 run の間に「画面上の1行」を表す型を入れ、カーソルに関する
すべての質問をそこへ集約することにある。

## 決定

### ブロックと run の間に行（`LayoutLine`）を置く

`hane_presentation::layout` を追加し、`VisualBlock` → `LayoutLine` → run の3段にする。
`LayoutLine` は「画面上の1行」であり、物理行1本、または折り返された物理行の1断片である。
各行は次を持つ。

- `line` / `line_id` / `fragment`: どの presented 行の何番目の断片か。
- `wrap`: `Hard`（source の改行で終わる）か `Soft`（折り返しで終わる）か。
- `line_visual_range`: その行が持つ visual text の範囲（行ローカル）。
- `visual_range`: 同じ範囲のブロックローカル表現。各 presented 行の後ろに改行1個分の
  位置を置くので、ブロック内で offset が一意かつ順序を保つ。R4C の cache entry は
  物理行を名指さずに位置を書ける。
- `source_range`: その行が受け持つ source。行はその物理行の source をタイルし、
  物理行はブロックをタイルするので、source の1 byte は必ず1つの行に属する。
- `y` / `height`: ブロック先頭からの位置と高さ。

### 折り返し位置だけをフォントに聞く

レイアウトは `LineShaper` trait を受け取る。UI は window の text system で実装し、
テストは1文字固定幅の `FixedAdvanceShaper`（`hane_presentation::testing`）で実装する。
trait が答えるのは3つだけである。

- `wrap_boundaries`: この幅で折り返す byte offset。
- `x_for_offset`: 断片の左端から見た x。
- `offset_for_x`: 断片内で x に最も近い offset。

「どこで行が始まり終わるか」「行がどの source を受け持つか」「行がどこに座るか」は
すべてレイアウト側で決める。どのフォントでも同じ答えになるので、座標契約は window 無しで
テストできる。

### カーソルの質問は1か所で答える

`BlockLayout` が次を答える。

- `row_for_source` / `point_for_source`: source offset → 行・x・y・高さ。
- `source_for_point` / `source_at_x`: 点 → source offset。
- `vertical_target`: 1行上/下の caret 位置（`To` / `PastEdge` / `Unknown`）。
- `visual_range_on_row`: 選択・IME 下線をその行の範囲へ切り出す。
- `row_bounds_for_source`: フォント無しで答えられる y と高さ（scroll 用）。

soft の切れ目に当たる offset は「次の行の先頭」に属する。したがって折り返し位置の
カーソルは前の行の右端ではなく次の行の左端に出る。行末（改行の直前）の位置を持てるのは
`Hard` で終わる行だけである。

### 上下移動は preferred column ではなく preferred x

`Editor` は `preferred_visual_x` を持ち、`move_vertical_to(target, extend, preferred_x)`
だけが上下移動の入口になる。移動先の決定は `EditorView::move_vertical` が行い、
caret のブロックを（その前後1行を含めて）レイアウトして `vertical_target` を引く。
ブロックの端に達したときだけ隣のブロックの1行を present してその先頭/末尾行を見る。
`EditorCommand::MoveUp` / `MoveDown`（grapheme column 版）は、ブロック境界がまだ無い
起動直後のフォールバックとして残す。

`Editor::set_selection` と上下以外のコマンドは preferred x を落とす。短い行を通過しても
狙っている x は呼び出し側が持ち続けるので、その次の行で元の桁に戻る。

### 高さと caret 矩形はレイアウトから取る

`HeightIndex` の更新値は `VisualBlock::height()` ではなく `BlockLayout::height()` に
なった（行粒度のときは `line_height_of`）。折り返した行はその行数分の高さを占める。
`bounds_for_range` は最後のフレームが描いた caret 矩形（`CaretGeometry`）を返すので、
IME の候補ウィンドウがカーソル位置に出る。

### レイアウトはブロック単位でキャッシュし、編集で rebase する

`layout_cache: HashMap<BlockId, BlockLayout>` を `block_cache` と同じ保持規則で持つ。
presentation が再利用できたフレームでは、幅と revision が一致する限りレイアウトも
再利用する。ブロックに触れない編集では `BlockLayout::rebase` で source range だけを
移送するので、打鍵ごとに画面内の全ブロックを shape し直すことはない。

## 結果

- 折り返しを含めて、カーソル・選択・IME・クリック・上下移動が同じ座標変換を通る。
- source offset の往復契約が行の上で閉じた。`layout_contract.rs` は、
  段落・引用・リスト・コード・表を含む文書の全 source offset について
  「source → 点 → source」が同じ offset に戻ることを固定する（source map が隠す
  offset は編集可能位置ではないため対象外）。
- 上下移動が行単位になった。折り返し段落の中を1行ずつ動き、短い行を通過しても桁が戻る。
- ブロック高さが実際の行数を反映するようになった。
- `hane-bench buffer` に `viewport block layout` を追加した（10万ブロック文書の
  viewport 14 ブロックを present + layout して median 0.048 ms / p95 0.051 ms、
  固定幅シェイパ）。既存シナリオは R4A と同水準。
- 一方で、レイアウトは1フレームに1回テキストを shape する。GPUI 自身も描画時に shape
  するため、可視行の shape は二重になる。R4C の cache entry へ shape 結果を持たせるまでは
  この重複が残る。

## 検討した代替案

### `VisualLine` をやめてブロックを1本の visual text にする

採用しない。source map・disclosure・style run はいまも物理行単位で作られており、
そこまで壊すと R4B が R3.25 の表示契約の作り直しになる。ブロックローカル座標は
`line_visual_start` の加算で表現できるので、両方の座標を `LayoutLine` が持てば足りる。

### 折り返しを GPUI の要素に任せる

採用しない。`div` にテキストを入れれば GPUI は折り返すが、どこで折り返したかは
こちらに返らない。カーソル位置・選択矩形・行高がすべて推測になる。折り返し位置を
先に決めて、行ごとに要素を作る方が、描画とヒットテストが同じ答えを使う。

### 上下移動を `Editor` の中で完結させる

採用しない。移動先はフォントと幅に依存する。`hane-editor` は GPUI 非依存であり、
文書だけからは折り返し位置を知り得ない。view が解決して結果を `move_vertical_to` で
渡す形にすると、editor は「caret と preferred x を持つ」責務だけを持つ。

### 行の高さを行ごとに実測する

採用しない。実測は描画後にしか得られず、フレーム内で高さが変わると scroll が揺れる。
行の高さは presented 行の高さ（`VisualLine::height`）を断片ごとに数える定義にした。
画像行は折り返さないので従来どおり1行である。
