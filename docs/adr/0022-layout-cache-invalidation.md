# ADR-0022: レイアウトキャッシュの無効化と高さ差分更新

## ステータス

承認済み

## 日付

2026-08-29

## 背景

R4B で `BlockLayout` の再利用は始まったが、キャッシュの成立条件は `EditorView` の分岐へ
散らばっていた。また、ブロックの split/join では `HeightIndex` を全再構築し、描画によって
折り返し後の実測高さが確定すると、viewport より上の高さ差がそのまま表示位置のずれになった。

## 決定

### cache entry のキー

`LayoutCacheEntry` は `BlockLayout` と font revision を持つ。再利用には次のすべてを要求する。

- stable `BlockId` が同じ。
- document revision が同じ、または編集が block と交差せず、presentation と layout の
  source range を現在 revision へ rebase できる。
- text column の幅が同じ。
- `WindowShaper` が使う font family、features、fallback、weight、style の hash が同じ。

テーマ変更は presentation と layout を破棄する。幅変更は entry の key mismatch とし、可視 block
から再レイアウトする。画像高さは `VisualBlock` の再生成によって layout revision を変え、同じ
entry を再利用しない。選択、caret、IME、色だけの変更は geometry を変えないため layout を残す。

### 非交差編集

`BlockId` が残る非交差 block は `RevisionDelta` で `VisualBlock` と `BlockLayout` の source range
だけを移送する。文字列と折り返し位置は変わらないため shape/layout をやり直さない。

### 高さ索引と scroll anchor

block 粒度の高さ索引と同じ順序で `BlockId` / `line_count` を保持する。`BlockIndexUpdate` は
置換開始 ordinal、削除数、挿入数を返し、UI は増分 parse の小さな窓だけを比較する。通常の
文字入力で id と line count が変わらなければ高さ索引には触れず、split/join のときだけ同じ窓を
`HeightIndex::splice` する。外側の実測高さは保持する。

`HeightIndex` と UI の `HeightBlock` メタデータ列は 128 block ごとの leaf chunk を持つ。高さ索引は
chunk の高さ合計・要素数に対する Fenwick tree、メタデータ列は要素数 tree を使う。splice は境界
chunk と挿入値だけを再 chunk 化し、tree の再構築も block 全件ではなく chunk 数に比例する。
10万 block の中央 split/join は median 0.002 ms / p95 0.003 ms。

splice では viewport 上端の block id・ordinal・block 内 y を先に記録する。anchor が置換窓の外なら
削除数と挿入数だけで ordinal を移し、窓の中なら挿入された小区間だけを id 検索する。可視 block の
実測高さ更新は構造を変えないため同じ ordinal を使う。どちらも全 block の id 検索は行わない。

### 計測

instrument build の paint record は `layout_cache_hits`、`layout_cache_misses`、
`relayout_blocks` を出力する。1 block の miss は1回の再レイアウトなので、現時点では
`relayout_blocks = layout_cache_misses` である。

## 結果

- block 数が変わる編集でも、無関係な block の実測高さを捨てない。
- viewport より上の高さが見積もりから実測へ変わっても、読んでいる位置が動かない。
- cache の geometry invalidation と paint-only change の境界が明示された。
- 100 MB / 10万段落で cache 指標・入力・scroll・RSS を比較できる。
- 最終版の独立した2回の 100 MB ASCII 入力は p95/p99 3.55/4.41 ms と 4.58/4.60 ms。
  R0 の 4.98/7.65 ms、同一ウィンドウ状態で再採取した R4B の 7.33/7.38 ms の双方以下で、
  15% 相対 gate と 16/33 ms 絶対 gate を通過した。100 MB 改行入力10回も p95/p99 1.97/1.97 ms。

## 棄却した案: `ShapedLine` を直接 paint する

各行断片の GPUI `ShapedLine` を entry に保持し、custom element から直接 paint する実装を試した。
layout と通常 text element の二重 shape は消えたが、同一100 MB文書・同一30 ASCII入力で
`keystroke_to_frame` p95 が R4B の 5.47 ms から 21.49 ms へ悪化し、16 ms gate を超えた。
選択・IME・caret の custom quad を含む prepaint/paint 経路が vsync deadline を外すため、採用しない。

GPUI native text element は維持する。text system 側の layout state を安全に entry へ共有できる
公開APIが導入されるまでは、描画側の shape 再利用を独自 custom paint で置き換えない。
