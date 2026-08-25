# ADR-0016: Phase 3 Typora-style Editing 実装計画

## ステータス

承認済み

## 日付

2026-08-25

## 背景

Phase 2はCommonMarkのsource rangeを使ったnative presentationを完成させたが、Markdown記号はsourceどおり表示し、SourceMapを恒等写像にしていた。Phase 3ではRFPの主要価値であるMarkdown記号の段階表示と、非一対一のSource ↔ Visual変換を完成させる。

## 決定

1. Markdown sourceを唯一の正とし、通常表示では見出し、quote、list、太字、斜体、取り消し線、inline code、linkの構文記号だけを`HiddenMarkup` segmentへ畳む。
2. caret、selection、IME marked rangeが構文範囲へ入った場合、その構文記号を`ExpandedMarkup`として表示する。文書sourceは変更しない。
3. hidden segmentはzero-length visual rangeとして保持し、削除や隣接segmentへの統合をしない。
4. collapsed境界のclickはvisible content境界をcanonical positionとし、source/visual往復は`normalize_source`と`normalize_visual`後の一致を保証する。
5. UIのcursor、selection、mouse hit test、IMEはすべてSourceMapを通す。presentation cache keyにはdisclosure rangeを含める。
6. 2,048行の局所fence探索はbackground index完成までのfallbackとする。正式なindexは共有Rope snapshotから1 jobだけ構築し、revision一致時だけpublishする。
7. 連続入力中のbackground requestはcoalesceし、入力pathでは全文parseの完了を待たない。
8. Phase 2と同じformat、clippy、test、実UI、latency、scroll、startup、RSS gateを回帰確認する。

## Phase 3での非目標

- 画像、表、数式、Mermaid、埋め込みHTML。
- Save、Save As、Recent Files、自動保存、設定、packaging。
- 箇条書きmarkerのsourceに存在しないglyphへの置換。
- 入力ごとの全文同期parse。

## 完了条件

- 通常表示で主要Markdown記号が隠れ、active構文だけ段階表示される。
- hidden/expanded境界、Unicode、selection、IMEのSource ↔ Visual変換を自動テストする。
- 2,048行を超えるfenced blockをbackground indexが判定する。
- background resultはrevision一致時だけpublishされ、同時実行は1 jobに制限される。
- workspace test、clippy、formatがpassする。
- Phase 3測定reportがある。

## 結果

通常表示とsource編集を分離せず、Markdown sourceの完全性を維持したままTypora-style editingを成立させる。Phase 4ではこのmapping基盤を画像、表、保存UI等へ拡張する。
