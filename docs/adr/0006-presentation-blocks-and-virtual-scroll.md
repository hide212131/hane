# ADR-0006: Presentation Block と可変高さ仮想スクロール

> **Amended (R5):** 現行のブロック単位仮想化は ADR-0020、layout/caching は ADR-0021/0022 と
> [architecture](../architecture.md) を正とする。

## ステータス

承認済み

## 日付

2026-08-24

## 背景

巨大 Markdown 文書全体を1つの GPUI element として描画すると、画面外の文書量が描画負荷に直結する。

RFP では、画面に見えているブロックとその周辺だけを GPUI に渡すことを求めている。一方で Markdown の各ブロックは高さが異なるため、固定高さ list だけでは不十分である。

## 決定

文書を `VisualBlock` の列として扱い、描画は可視範囲と overscan に限定する。

```text
Document
├── BlockIndex
├── VisualBlock[]
└── HeightIndex
```

`VisualBlock` は最低限以下を持つ。

```text
VisualBlock
├── block_id
├── source_range
├── revision
├── estimated_height
├── measured_height
├── layout_cache
├── style_runs
└── invalidation_state
```

スクロール位置から可視 block を求めるために、block height の prefix sum を扱う `HeightIndex` を導入する。

`HeightIndex` は Phase 0 では Fenwick tree または同等の計算量を持つ構造で実装する。

必要な計算量は以下とする。

| 操作 | 計算量 |
|---|---:|
| block height update | `O(log n)` |
| block index から scroll y を取得 | `O(log n)` |
| scroll y から block index を検索 | `O(log n)` |
| total document height | `O(1)` または `O(log n)` |

Phase 0 では、段落単位または固定行 chunk 単位で block を作り、可変高さへの移行を妨げない形にする。

## 描画範囲

描画対象は以下に限定する。

```text
visible viewport
+ overscan_before
+ overscan_after
```

overscan はフレーム落ちを避けるために設けるが、文書サイズに比例して増やさない。

## Scroll Anchoring

推定高さが実測高さに置き換わると、viewport より前の累積高さが変わり、表示位置が跳ねる可能性がある。

これを避けるため、scroll state は raw `scroll_y` だけでなく anchor を持つ。

```text
ScrollAnchor
├── block_id
├── intra_block_y
└── visual_position_hint
```

viewport 先頭に最も近い visible block を anchor block とする。

viewport より前の block height が補正された場合、同じ `block_id + intra_block_y` が画面上の同じ位置に残るよう `scroll_y` を補正する。

anchor block 自体の高さが変わった場合は、`intra_block_y` を block height 内に clamp する。

## Invalidation

edit が発生した場合、以下だけを invalid にする。

- edit range を含む block。
- Markdown 構造上、影響が及ぶ可能性のある近傍 block。
- 高さが変わった場合の HeightIndex。

全文 layout cache を破棄しない。

Phase 0 では、block invalidation の最大探索範囲を以下に制限する。

- 通常テキスト edit は、edit を含む block と前後1 block。
- 改行を含む edit は、edit 開始 block から edit 終了 block と前後2 block。
- Markdown delimiter の簡易 presentation 実験では、同一 block 内だけを再解析する。
- 範囲が上限を超える場合は、visible range と overscan を優先して再構築し、全文再構築は background job に送る。

Phase 2 以降で block 構造にまたがる Markdown 構文を扱う場合は、この上限を parser strategy の ADR で再検討する。

## 結果

巨大文書でも、画面外の文書量によって毎フレームの element 構築・layout・paint 負荷が増えにくくなる。

一方で、可変高さの block index と height index は実装が複雑になる。Phase 0 では chunk 単位で開始し、測定結果を見て block 粒度を調整する。

## 検討した代替案

### 文書全体を1つの GPUI element として描画する

採用しない。

100 MB 文書で layout と paint の負荷が文書サイズに比例しやすい。

### 完全な固定高さ list にする

採用しない。

Markdown では見出し、段落、コードブロック、画像、表により高さが変わる。Phase 0 の一部では使えても、製品設計としては不十分である。
