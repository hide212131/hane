# ADR-0004: Source と Visual の位置対応モデル

## ステータス

承認済み

## 日付

2026-08-24

## 背景

本エディタでは Markdown ソースを唯一の正としながら、通常表示では Markdown 記号を隠す。

例:

```markdown
これは **重要です**。
```

通常表示:

```text
これは 重要です。
```

ユーザーが見た目上の「重」と「要」の間をクリックした場合、Markdown source 上では `**重|要です**` に対応する必要がある。

この対応関係は、カーソル、選択、IME、Undo/Redo、Markdown 記号の段階表示で共通して使われる。

## 決定

Source ↔ Visual の対応は、`presentation` crate が生成する `VisualBlock` と `SourceMap` に集約する。

概念構造は以下とする。

```text
VisualBlock
├── block_id
├── source_range
├── revision
├── visual_text
├── style_runs
├── hidden_source_ranges
├── source_to_visual_segments
└── visual_to_source_segments
```

`source_to_visual_segments` と `visual_to_source_segments` は、単一 offset の map ではなく range segment として保持する。

```text
MappingSegment
├── source_range
├── visual_range
├── boundary_behavior
└── visibility
```

`visibility` は以下を持つ。

| 値 | 意味 |
|---|---|
| `Visible` | source と visual の両方に現れる |
| `HiddenMarkup` | Markdown 記号として通常表示では隠れる |
| `Synthesized` | 箇条書きの bullet など、visual 側に生成される |
| `ExpandedMarkup` | カーソル近傍で Markdown 記号を段階表示している |

Phase 0 では完全な Source ↔ Visual 対応は作らない。ただし、太字表示実験でもこの model の簡易版を使う。

## Position と Range の単位

`SourcePosition` は ADR-0003 の `SourceOffset` と同じ UTF-8 byte offset とする。

`VisualPosition` は `visual_text` 内の UTF-8 byte offset とする。画面上の x/y 座標ではない。

`SourceRange` と `VisualRange` はどちらも半開区間 `[start, end)` とする。

画面上の click / drag / arrow movement は、GPUI の hit test と text layout からまず `VisualPosition` を得て、その後 `SourceMap` で `SourcePosition` へ変換する。

## 非一対一境界の規則

隠れた Markdown 記号の前後には、同じ visual position に複数の source offset が対応する場合がある。

そのため、mapping は候補を1つに潰さず、`PositionCandidate` の集合として扱う。

```text
PositionCandidate
├── source_offset
├── visual_offset
├── affinity
├── side
└── reason
```

`affinity` は以下を持つ。

```text
Affinity
├── Before
└── After
```

`side` は以下を持つ。

```text
BoundarySide
├── Leading
└── Trailing
```

候補選択の既定規則は以下とする。

| 操作 | 候補選択 |
|---|---|
| マウスクリック | visual に最も近い visible 文字境界を優先し、同距離なら現在 cursor affinity を維持 |
| 左右 arrow | 移動方向に対応する次の正規 cursor position を選ぶ |
| Shift + arrow | anchor は固定し、active end だけ正規 cursor position へ移動 |
| selection drag | drag 開始側を anchor とし、hit test 側を active end として正規化 |
| hidden markup 内への programmatic 移動 | markup を段階表示するか、最も近い editable source position へ正規化 |

`HiddenMarkup` は visual range がゼロ長になり得る。`Synthesized` は source range がゼロ長になり得る。

ゼロ長 segment は有効だが、隣接 segment と同じ意味に潰してはならない。クリック、矢印移動、selection extension で境界候補を復元するために保持する。

## 正規化

Source ↔ Visual の往復は、常に完全な恒等変換になるとは限らない。

特に hidden markup、synthesized marker、段階表示前後では、複数の source position が同じ visual position に対応する。

そのためテスト特性は以下とする。

```text
normalize_source(source_to_visual_to_source(p)) == normalize_source(p)
normalize_visual(visual_to_source_to_visual(p)) == normalize_visual(p)
```

`normalize_source` は、編集可能な最も近い canonical source position と affinity に正規化する。

`normalize_visual` は、表示上同一とみなす canonical visual position に正規化する。

## テスト方針

以下を単体テストの必須対象にする。

- ASCII の太字、斜体、インラインコード。
- 日本語を含む太字。
- 絵文字を含む装飾範囲。
- Markdown 記号をまたぐ選択。
- source → visual → source が正規化後に一致すること。
- visual → source → visual が正規化後に一致すること。
- hidden markup の境界での affinity。
- zero-length source segment と zero-length visual segment。

## 結果

位置対応を `presentation` に集約することで、`editor` は Markdown 構文を直接解釈せずにカーソル移動と選択を扱える。

一方で、mapping model はエディタの中核複雑性になる。Phase 0 では簡易版を使いつつ、後から full model に移行できる shape を維持する。

## 検討した代替案

### 表示文字列だけを編集して保存時に Markdown へ戻す

採用しない。

RFP の「Markdown ソースを唯一の正とする」に反する。

### カーソル移動時に都度 Markdown を再解析する

採用しない。

入力遅延とカーソル移動が parser に依存し、巨大文書で性能が不安定になる。
