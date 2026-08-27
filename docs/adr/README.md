# Architecture Decision Records

このディレクトリは、Rust + GPUI Markdown Editor の設計判断を記録する。

ADR は実装前にすべてを固定するためではなく、実装時に迷うと性能・正確性・依存境界へ影響する判断を明示するために使う。

ADR-0001 から ADR-0011 はPhase 0、ADR-0012からADR-0014はPhase 1、ADR-0015からADR-0017は
Phase 2以降の実装判断を記録する。ADR-0018以降は `docs/refactor-plan.md` の各フェーズで
必要になった判断を記録する。

## ADR 一覧

| ADR | 内容 |
|---|---|
| [ADR-0001](0001-phase0-technical-validation.md) | Phase 0 技術検証方針 |
| [ADR-0002](0002-crate-boundaries.md) | Crate 境界と依存方向 |
| [ADR-0003](0003-text-buffer.md) | Text Buffer と位置単位 |
| [ADR-0004](0004-source-visual-mapping.md) | Source と Visual の位置対応モデル |
| [ADR-0005](0005-revision-and-background-work.md) | Revision とバックグラウンド処理 |
| [ADR-0006](0006-presentation-blocks-and-virtual-scroll.md) | Presentation Block と可変高さ仮想スクロール |
| [ADR-0007](0007-ime-composition.md) | IME Composition モデル |
| [ADR-0008](0008-markdown-parsing-strategy.md) | Markdown 解析方針 |
| [ADR-0009](0009-performance-harness.md) | Performance Harness と測定基準 |
| [ADR-0010](0010-phase0-implementation-plan.md) | Phase 0 実装順序 |
| [ADR-0011](0011-dependency-and-license-policy.md) | 依存関係とライセンス確認方針 |
| [ADR-0012](0012-phase1-plain-text-editor-plan.md) | Phase 1 Plain Text Editor実装計画 |
| [ADR-0013](0013-undo-redo-transactions.md) | Undo/Redo transaction |
| [ADR-0014](0014-gpui-memory-baseline.md) | GPUI memory baselineと空Editor RSS目標 |
| [ADR-0015](0015-phase2-markdown-presentation-plan.md) | Phase 2 Markdown Presentation実装計画 |
| [ADR-0016](0016-phase3-typora-editing-plan.md) | Phase 3 Typora-style Editing実装計画 |
| [ADR-0017](0017-phase4-polish-plan.md) | Phase 4 Polish実装計画 |
| [ADR-0018](0018-block-index.md) | revision 付き Block Index |

## Phase 0 実装時の参照順

Phase 0 の実装を始める場合は、以下の順に読む。

1. [ADR-0001](0001-phase0-technical-validation.md)
2. [ADR-0010](0010-phase0-implementation-plan.md)
3. [ADR-0002](0002-crate-boundaries.md)
4. [ADR-0003](0003-text-buffer.md)
5. [ADR-0005](0005-revision-and-background-work.md)
6. [ADR-0007](0007-ime-composition.md)
7. [ADR-0006](0006-presentation-blocks-and-virtual-scroll.md)
8. [ADR-0009](0009-performance-harness.md)

## 実装開始に必要な最小判断

現時点で、Phase 0 の実装開始に必要な判断は ADR として揃っている。

- 何を作り、何を作らないか。
- crate をどう分けるか。
- Text Buffer の正式位置単位を何にするか。
- 入力 path で何をして、何をしないか。
- background job を revision でどう扱うか。
- 継続入力中の stale job を coalescing / cancel / 非重複範囲の部分 publish でどう扱うか。
- IME composition をどこで保持するか。
- IME composition の cancel / commit / undo 単位をどう扱うか。
- `SourceOffset`、`SourceRange`、`Anchor` の境界契約をどうするか。
- 画面外描画をどう避けるか。
- 可変高さ仮想スクロールの計算量と scroll anchoring をどう扱うか。
- 何を測定し、どの値を見て Phase 1 へ進むか。
- GPUI の IME / paint 観測点を最初に spike すること。

実装中に新しい設計判断が必要になった場合は、既存 ADR を更新するか、新しい ADR を追加する。

## Phase 1 実装時の参照順

1. [Phase 0 report](../phase0/report.md)
2. [ADR-0012](0012-phase1-plain-text-editor-plan.md)
3. [ADR-0013](0013-undo-redo-transactions.md)
4. [ADR-0014](0014-gpui-memory-baseline.md)
5. [ADR-0003](0003-text-buffer.md)
6. [ADR-0005](0005-revision-and-background-work.md)
7. [ADR-0007](0007-ime-composition.md)
8. [ADR-0006](0006-presentation-blocks-and-virtual-scroll.md)
9. [ADR-0009](0009-performance-harness.md)

## Phase 2 実装時の参照順

1. [Phase 1 report](../phase1/report.md)
2. [ADR-0015](0015-phase2-markdown-presentation-plan.md)
3. [ADR-0008](0008-markdown-parsing-strategy.md)
4. [ADR-0004](0004-source-visual-mapping.md)
5. [ADR-0005](0005-revision-and-background-work.md)
6. [ADR-0006](0006-presentation-blocks-and-virtual-scroll.md)
7. [ADR-0014](0014-gpui-memory-baseline.md)
8. [ADR-0009](0009-performance-harness.md)

## Phase 3 実装時の参照順

1. [Phase 2 report](../phase2/report.md)
2. [ADR-0016](0016-phase3-typora-editing-plan.md)
3. [ADR-0004](0004-source-visual-mapping.md)
4. [ADR-0008](0008-markdown-parsing-strategy.md)
5. [ADR-0005](0005-revision-and-background-work.md)
6. [ADR-0007](0007-ime-composition.md)
7. [ADR-0009](0009-performance-harness.md)

## Phase 4 実装時の参照順

1. [Phase 3 report](../phase3/report.md)
2. [ADR-0017](0017-phase4-polish-plan.md)
3. [ADR-0004](0004-source-visual-mapping.md)
4. [ADR-0005](0005-revision-and-background-work.md)
5. [ADR-0006](0006-presentation-blocks-and-virtual-scroll.md)
6. [ADR-0007](0007-ime-composition.md)
7. [ADR-0009](0009-performance-harness.md)
