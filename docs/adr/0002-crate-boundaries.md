# ADR-0002: Crate 境界と依存方向

> **Amended (R5):** 現行の crate graph と追加された `session` / `layout` 境界は
> [architecture](../architecture.md)、[ADR-0019](0019-document-session-and-file-service.md)、
> [ADR-0021](0021-layout-lines-and-visual-coordinates.md) を正とする。本 ADR は当時の判断記録として保持する。

## ステータス

承認済み

## 日付

2026-08-24

## 背景

本エディタでは、入力遅延と巨大文書対応を最優先する。UI フレームワーク、Markdown 解析、文書編集、IME、描画最適化が密結合すると、性能問題の切り分けとテストが難しくなる。

RFP では `document` と `editor` は GPUI への依存をできるだけ持たないことを求めている。これは画面なしで大量の編集・位置変換テストを実行するために必要である。

## 決定

初期実装では以下の crate 境界を採用する。R3.75 で `session` crate を追加した。
更新後の境界と依存方向は [ADR-0019](0019-document-session-and-file-service.md) を参照する。

```text
crates/
├── app/
├── document/
├── markdown/
├── metrics/
├── presentation/
├── editor/
├── ui/
└── benchmark/
```

依存方向は以下に固定する。

```text
app
 ├── ui
 ├── editor
 └── benchmark

ui
 ├── editor
 ├── metrics
 └── presentation

editor
 ├── document
 └── metrics

presentation
 ├── document
 └── markdown

markdown
 └── document

benchmark
 ├── document
 ├── metrics
 ├── markdown
 ├── presentation
 └── editor
```

`document` と `metrics` は最下層とし、GPUI、Markdown parser、OS UI API に依存しない。

各 crate の責務は以下とする。

| crate | 責務 |
|---|---|
| `app` | 起動、ウィンドウ生成、アプリライフサイクル、OS 連携 |
| `document` | Text Buffer、Edit、Anchor、Revision、Undo/Redo の基礎 |
| `markdown` | Markdown 解析、block index、source range 抽出 |
| `metrics` | 依存を持たない rolling window と percentile 集計 |
| `presentation` | Markdown source から visual block / style run / mapping を生成 |
| `editor` | Cursor、Selection、IME state、command dispatch |
| `ui` | GPUI element、描画、入力イベント接続、スクロール |
| `benchmark` | 性能測定、fixture 生成、計測レポート |

Phase 0 では全 crate を完全実装しないが、この依存方向に反するコードは入れない。

## Phase 0 の最小実装範囲

`document` の責務には Undo/Redo の基礎を含めるが、Phase 0 で完成した Undo/Redo 機能は作らない。

Phase 0 の `document` で実装する最小範囲は以下とする。

- Text Buffer interface。
- `SourceOffset`、`SourceRange`、`Revision`、`Anchor`。
- 単一 edit と、その edit を戻すための inverse edit 生成。
- IME composition cancel に必要な snapshot / inverse edit 保持。
- 後続 Undo/Redo 実装で使う transaction id の型定義。

Phase 0 で実装しないものは以下とする。

- ユーザー操作としての Undo / Redo command。
- 連続入力を自然な単位へまとめる履歴 grouping。
- ファイル保存後の履歴管理。
- 複数 cursor / 複数 selection に対応した履歴統合。

## 結果

編集ロジックを UI なしでテストできる。GPUI の API 変更や描画実験が `document` に波及しにくくなる。

一方で、小さいプロトタイプでも crate 間の型変換が発生する。Phase 0 では抽象化を増やしすぎず、責務境界を守るための最小限の型だけを導入する。

## 検討した代替案

### 単一 crate で開始する

採用しない。

Phase 0 は小さいが、IME、buffer、layout、paint、benchmark が同じ場所に混ざると、性能問題の責務が曖昧になる。

### GPUI の型を全 crate で直接使う

採用しない。

`document` と `editor` を画面なしで検証できなくなる。GPUI 依存は `ui` と `app` に閉じ込める。
