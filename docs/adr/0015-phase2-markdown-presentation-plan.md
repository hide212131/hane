# ADR-0015: Phase 2 Markdown Presentation 実装計画

## ステータス

承認済み

## 日付

2026-08-25

## 背景

Phase 1 は plain text editor の入力、selection、IME、Undo/Redo、clipboard、巨大文書、仮想scrollを完成させ、Phase 2への判断をGoとした。

Phase 2の目的は、Markdown sourceを唯一の正として維持したまま、見出し、太字、斜体、取り消し線、インラインコード、コードブロック等を画面上で装飾することである。Markdown記号の段階表示と完全なSource ↔ Visual位置変換はPhase 3の対象である。

## 決定

Phase 2はこれまでと同じく、設計判断、実装、単体テスト、実UI検証、性能測定、reportの順で進める。

1. 正式なMarkdown解析器として`pulldown-cmark`を導入し、`into_offset_iter()`のbyte rangeをHaneの`SourceRange`へ変換する。
2. `markdown` crateはrevision、解析範囲、block kind、inline spanを返し、Document BufferとUIを直接変更しない。
3. Phase 2ではMarkdown記号をsourceどおり表示し、`SourceMap`を恒等写像に保つ。記号を隠す`HiddenMarkup`はPhase 3で有効化する。
4. `presentation` crateはparser resultからblock kind、style run、推定高さを持つ`VisualBlock`を構築する。
5. UIは可視行とoverscanだけを局所解析し、revision/range単位のcacheを継続利用する。1文字入力から全文parseを同期実行しない。
6. 見出しはlevelに応じたweight/size、太字、斜体、取り消し線、コード、linkをそれぞれnative GPUI text styleで描画する。
7. blockの推定高さを`HeightIndex`へ反映し、見出し等の可変高さでも仮想scrollとcursor追従を維持する。
8. Unicode、nested inline、source identity、編集後cache invalidation、選択、IMEを自動テストする。
9. Phase 1と同じ測定条件で入力、scroll、startup、RSSを再測定する。
10. `docs/phase2/report.md`に結果、Pass/Fail、Phase 3判断を記録する。

## Phase 2での非目標

- Markdown記号の段階表示。
- hidden markupをまたぐ完全なSource ↔ Visual位置変換。
- 画像のdecode/display、表、数式、Mermaid、埋め込みHTML。
- Save、Save As、Recent Files、自動保存、設定、packaging。
- 入力ごとの全文同期parse。

## 完了条件

- 見出し、太字、斜体、取り消し線、インラインコード、コードブロック、linkが装飾される。
- Markdown source byteとvisual byteの位置がPhase 2では恒等対応する。
- cursor、selection、mouse hit test、IME、Undo/Redoが装飾中もsourceに対して動作する。
- 可視範囲外の行をpaint用に構築しない。
- 変更されていない可視行cacheをrevision deltaで再利用する。
- workspace test、clippy、formatがpassする。
- Phase 2測定reportがある。

## 結果

表示装飾と編集位置の正確性を分離して検証できる。Phase 3では同じparser rangeと`SourceMap`を使い、cursor近傍だけMarkdown記号を展開する。
