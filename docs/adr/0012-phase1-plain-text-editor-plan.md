# ADR-0012: Phase 1 Plain Text Editor 実装計画

## ステータス

承認済み

## 日付

2026-08-25

## 背景

Phase 0 は巨大文書、IME、仮想スクロール、入力 latency の技術検証を完了し、Phase 1 への判断を Go とした。一方、安定 60 fps、100 MB 読込時の memory headroom、可視要素の再利用、空 shell RSS は未達または余裕が小さい。

Phase 1 の目的は Markdown presentation を進めることではなく、Markdown sourceをそのまま表示する高速な plain text editor を完成させることである。

## 決定

Phase 1 は Phase 0 と同じく、設計判断、実装、単体テスト、実UI検証、性能測定、report の順で進める。

実装順序は以下とする。

1. Phase 0 report の4つの先行改善を Phase 1 の開始ゲートとして固定する。
2. file open を一時 `String` を介さない streaming Rope 構築へ変更する。
3. revision/range単位の可視行cacheを導入し、cursor・selectionだけの更新で行文字列を再構築しない。
4. 自動scroll測定を固定周期timerからdisplay-linked frame callbackへ変更する。実操作はnative scroll eventをcoalesceして1 eventにつき1回だけpaintを要求する。
5. source editだけを保持するUndo/Redoを実装する。IME compositionはcommit時に1 transactionとする。
6. Shift選択、mouse drag選択、行頭・行末移動、document先頭・末尾移動、Copy/Cut/Pasteを実装する。
7. selectionとIME marked rangeを文字範囲へ正確に描画する。
8. Unicode、連続入力grouping、IME、cache invalidation、巨大文書を自動テストする。
9. Phase 0と同じ測定条件で入力、scroll、startup、RSSを再測定する。
10. `docs/phase1/report.md` に結果、Pass/Fail、Phase 2判断を記録する。

## Phase 1での非目標

- Markdown記号を隠すpresentation。
- Markdown block parserの拡張。
- 見出し、太字、画像等の装飾描画。
- Save、Save As、Recent Files、自動保存のUI。
- 設定、theme、packaging。

既存の太字parser/presentation実験はPhase 0の検証資産として残すが、Phase 1の本文描画経路では使用しない。

## 完了条件

- Markdown sourceが記号を含むplain textとして表示される。
- grapheme単位のcursor移動、上下移動、行頭・行末、文書先頭・末尾が動作する。
- keyboardおよびmouse dragで正確な範囲選択ができる。
- Copy/Cut/Pasteがsource selectionに対して動作する。
- 日本語IMEのcomposition、commit、cancelが動作し、commitを1回でUndo/Redoできる。
- 通常の連続入力、Backspace、Deleteが自然な単位でUndo/Redoできる。
- native scrollおよびdisplay-linked測定で可視範囲だけを描画する。
- 変更されていない可視行のpresentation cacheがrevision deltaを通して再利用される。
- 100 MB fileを一時的な文書全体`String`なしでRopeへ読み込む。
- workspace test、clippy、formatがpassする。
- Phase 1測定reportがある。

## 結果

Phase 2へ進む前に、source editorとして必要な入力操作と性能上の土台を完成させる。Markdown presentation由来の複雑さを持ち込まずに、Undo/Redoとselectionの正確性を検証できる。
