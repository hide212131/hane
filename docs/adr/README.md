# Architecture Decision Records

このディレクトリには現行実装に適用される判断を置く。現在の全体像は
[architecture](../architecture.md) を参照する。完了した Phase 0–4 の実装計画は、判断の経緯として
[`docs/history/adr/`](../history/adr/) に保存する。

現行のリファクタリング作業は [`docs/refactor-plan.md`](../refactor-plan.md) と
[`docs/refactor-execution-plan.md`](../refactor-execution-plan.md) を正とする。

## Active

| ADR | 内容 |
|---|---|
| [ADR-0001](0001-phase0-technical-validation.md) | Phase 0 技術検証方針 |
| [ADR-0003](0003-text-buffer.md) | Text Buffer と位置単位 |
| [ADR-0005](0005-revision-and-background-work.md) | Revision とバックグラウンド処理 |
| [ADR-0007](0007-ime-composition.md) | IME Composition モデル |
| [ADR-0011](0011-dependency-and-license-policy.md) | 依存関係とライセンス確認方針 |
| [ADR-0013](0013-undo-redo-transactions.md) | Undo/Redo transaction |
| [ADR-0014](0014-gpui-memory-baseline.md) | GPUI memory baselineと空Editor RSS目標 |
| [ADR-0018](0018-block-index.md) | revision 付き Block Index |
| [ADR-0019](0019-document-session-and-file-service.md) | DocumentSession と File I/O 境界 |
| [ADR-0020](0020-block-virtualization.md) | ブロック単位の仮想化と描画 |
| [ADR-0021](0021-layout-lines-and-visual-coordinates.md) | LayoutLine と visual 座標系 |
| [ADR-0022](0022-layout-cache-invalidation.md) | レイアウトキャッシュの無効化と高さ差分更新 |

## Superseded / amended

| ADR | 現在の参照先 |
|---|---|
| [ADR-0002](0002-crate-boundaries.md) | [architecture](../architecture.md)、[ADR-0019](0019-document-session-and-file-service.md)、[ADR-0021](0021-layout-lines-and-visual-coordinates.md) |
| [ADR-0004](0004-source-visual-mapping.md) | [architecture](../architecture.md)、[ADR-0021](0021-layout-lines-and-visual-coordinates.md) |
| [ADR-0006](0006-presentation-blocks-and-virtual-scroll.md) | [ADR-0020](0020-block-virtualization.md)、[ADR-0021](0021-layout-lines-and-visual-coordinates.md)、[ADR-0022](0022-layout-cache-invalidation.md) |
| [ADR-0008](0008-markdown-parsing-strategy.md) | [ADR-0018](0018-block-index.md)、[architecture](../architecture.md) |
| [ADR-0009](0009-performance-harness.md) | [architecture](../architecture.md)、[baseline](../baseline/README.md) |

## History

以下は各Phaseの実装順序を記録した歴史資料であり、現在の設計や着手順の根拠にはしない。

| 資料 | 内容 |
|---|---|
| [ADR-0010](../history/adr/0010-phase0-implementation-plan.md) | Phase 0 実装順序 |
| [ADR-0012](../history/adr/0012-phase1-plain-text-editor-plan.md) | Phase 1 Plain Text Editor実装計画 |
| [ADR-0015](../history/adr/0015-phase2-markdown-presentation-plan.md) | Phase 2 Markdown Presentation実装計画 |
| [ADR-0016](../history/adr/0016-phase3-typora-editing-plan.md) | Phase 3 Typora-style Editing実装計画 |
| [ADR-0017](../history/adr/0017-phase4-polish-plan.md) | Phase 4 Polish実装計画 |
| [Phase 0 report](../history/phase0/report.md) | Phase 0 測定・判断記録 |
| [Phase 1 report](../history/phase1/report.md) | Phase 1 測定・判断記録 |
| [Phase 2 report](../history/phase2/report.md) | Phase 2 測定・判断記録 |
| [Phase 3 report](../history/phase3/report.md) | Phase 3 測定・判断記録 |
| [Phase 4 report](../history/phase4/report.md) | Phase 4 測定・判断記録 |
