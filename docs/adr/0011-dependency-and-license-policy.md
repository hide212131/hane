# ADR-0011: 依存関係とライセンス確認方針

## ステータス

承認済み

## 日付

2026-08-24

## 背景

本プロジェクトは Rust / GPUI を前提にする。RFP では、GPUI 自体は Apache License 2.0 として扱いつつ、配布前に依存関係とライセンスを改めて確認する必要があるとしている。

また、性能と単純さを守るため、本文編集の必須経路に不要な依存を入れないことが重要である。

## 決定

Phase 0 では、依存関係を以下の基準で追加する。

- 本文編集、描画、IME、性能測定に必要なものだけを追加する。
- WebView、Electron、Chromium、DOM editor、Monaco、CodeMirror、ProseMirror は追加しない。
- `document` crate には UI framework 依存を入れない。
- dependency を追加する場合は、目的と使用 crate を commit または ADR に残す。
- 配布前には `cargo tree`、`cargo deny`、license 確認を実施する。

Phase 0 時点では、ライセンス確認は技術上のリスク管理であり、法的判断ではない。

## 許容する依存の種類

Phase 0 で許容する依存は以下に限定する。

- GPUI とその直接必要な依存。
- Text Buffer 実装に必要な rope / tree 系 crate。
- Unicode 位置変換や grapheme 処理に必要な crate。
- benchmark / tracing / measurement に必要な crate。
- fixture generation に必要な開発依存。

## 結果

依存関係が入力 path に入り込む前に用途を確認できる。

一方で、毎回の dependency 追加に小さな確認コストが発生する。これは後から不要依存を剥がすより安い。

## 検討した代替案

### 便利な crate を先に広く入れる

採用しない。

起動時間、メモリ、ライセンス、配布のリスクが増える。

### ライセンス確認を配布直前まで完全に後回しにする

採用しない。

Phase 0 で厳密な法的判断はしないが、明らかに不適切な依存は早期に避ける。
