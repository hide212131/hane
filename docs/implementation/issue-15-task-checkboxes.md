# Issue #15: Markdownタスクチェックボックスの実装設計

## 位置付け

対象: #15 の残件であるGFMタスクリストの表示・切替・原文開示。通常リストは #126（PR #189 / #210 / #219）の受入済み経路を再利用する。
基準main: `b9676139d450a60cf51857840d0ebf64d5f20c91`。
この文書を追加した時点では製品変更・動作検証は未実施。現在の実装・検証状況はPRのcurrent headと証拠を参照する。

## 現状と変更点

- `crates/markdown/src/parser.rs` は `TaskListMarker` を `ListItem.task` と直下の `TaskMarker` に保持する。文字列の独自正規表現による再判定は追加しない。
- `crates/presentation/tests/markdown_features.rs` のtask fixtureは、通常bulletの後に生の `[ ]` / `[x]` を残している。非開示時にチェック状態を視覚化する。
- `crates/markdown/src/list_projection.rs`、`crates/presentation/src/projection/mod.rs`、`projection/block.rs`、`display.rs`、`source_map.rs`、`layout.rs` を既存のprojection・marker・geometry契約に沿って拡張する。具体的なシンボルは実装時のコードに照合する。
- UI側は `crates/ui/src/view.rs` の行イベント・`apply_list_edit_intent` の編集経路、`view/render.rs`、`line.rs`、`shape.rs` を確認し、最小限の専用モジュールへ切り出せる。新たな全域リファクタリングは含めない。

## 振る舞い

1. パーサーがタスクと認識した `- [ ]` / `- [x]` / `- [X]`、同等の `*` / `+`、番号付きリスト、入れ子、引用内を対象にする。`[X]` もチェック済み。コード内、通常段落の角括弧、壊れた・未完の構文はタスクUI化しない。
2. 非開示時には通常リストの記号・番号を維持し、その後のtask markerを未チェック／チェック済みのコントロールとして描く。原文のASCII角括弧をそのまま見せたり、表示だけを変更して操作できない状態で完了にしたりしない。完了本文への取り消し線等は追加しない。
3. caret、選択、IMEの対象範囲がtask markerへ入ると、既存の開示ポリシーに沿い元の `[ ]` / `[x]` / `[X]` を直接編集できる。開示した原文上の通常クリックはcaret移動として扱う。IME中のコントロール操作は本文を変更しない。
4. 編集モードの非開示checkboxに対する修飾キーなしの左クリックを一回のtoggleとして扱う。チェック領域外の本文クリック、選択drag、右クリックでtoggleしない。開示更新や再描画によって同一操作が二重実行されないようイベントを消費する。
5. toggleは現在の文書で対象マーカーを再検証した上で、中央の1バイトだけを ` ` → `x`、`x` / `X` → ` ` と置換する。行番号だけ、表示glyphだけ、子孫検索だけで対象を決めない。親・兄弟・子タスクと取り違えない。周辺の空白、bullet/番号、本文、改行コードは完全に保持する。表示だけ・再読込だけで `[X]` を小文字へ書き換えない。
6. 既存の `replace_range_recorded` 相当の記録付き編集と `after_input` 経路を使い、一操作一Undo単位とする。前後の入力と混ざらず、Redoで同じ結果へ戻る。長さが変わらないため既存のcaret・選択のsource offsetを保持し、dirty、再解析、描画、保存に通常編集として反映する。
7. 閲覧モードは既存の `body_editable()` に従い表示のみ。クリックでチェック状態、文書revision、dirty、履歴を変えない。IMEや文書切替などで古いイベントが到着しても別文書へ適用しない。

## 位置対応・鮮度の不変条件

- checkboxの表示メタデータには該当する所有itemと実source rangeを結び付ける。TaskMarkerは当該ListItemの直接の子だけを使用する。
- SourceMapを通してsynthetic表示から実マーカーへ対応させる。編集できる架空のsource位置は作らない。開示・非開示の境界と本文開始位置の往復契約を保つ。
- 描画、クリック領域、caret、選択、折返し、ズーム、横/縦スクロールは既存BlockLayoutのgeometryを共有する。固定pixelのUI側offsetで合わせない。checkboxの見た目・hit領域は重ならず、日本語本文や複数桁番号でも本文開始と継続行が整列する。
- document instance/session/revisionとprojectionの鮮度を検証する。旧snapshotのrangeを現文書へそのまま適用しない。現在の同一文書で再解決できない場合は無変更で終了する。既存strict-match/rebase契約を緩めない。
- 全文再解析をpaintやマウス移動ごとに追加しない。既存のlocal/global list projectionを共用し、viewport部分解析・親行が画面外のネストでも同じ結果を返す。

## 検証と証拠

- Markdown: 正常なtask markerと偽陽性、直下所有者、未完構文、Unicode前後、LF/CRLF、混在/入れ子リストを検証。
- Presentation: task fixtureを更新し、非開示/開示、選択とIMEでの開示、source↔visual境界、viewport部分解析、折返しと番号幅の契約を検証。単に古い期待値を削除して済ませない。
- 編集/UI回帰: 実際の行イベントからtoggleし、対象1バイトだけが変わること、親子取り違えなし、文書切替・stale revisionで誤編集なし、周辺入力から独立したUndo/Redo、閲覧モード・IME中無変更、本文クリック/drag非toggleを確認。
- 保存: toggle→保存→再読込で状態が一致し、対象外のbyteが不変であること。`[X]` のUndoで元の大文字が復元されること。
- 必須チェック: `cargo test --workspace --all-features`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`。その他は既存CIのrequired checksに従う。worker内で実行せずfinalizer/Commanderの検証結果を区別する。
- focused GUI: 未チェック/チェック済み/大文字、通常・番号付き・入れ子・引用、クリック→Undo/Redo→原文開示→保存再読込、折返し・スクロール・ズーム、IME中・閲覧モードを確認。対象headとbase context、fixture、観測結果、画面証拠をPRへ残す。未実施は未実施と記録する。
- 性能: 大きいネスト付きtask文書で基準mainと候補headの同条件描画・toggle/scrollを測定し、更新範囲と不必要な全文処理が増えていないか確認する。未測定を改善済みとしない。

## 並行作業・対象外

#605のリスト深さの数値調整、#307/#368のgeometry/cache責務整理は既存契約を利用し、全体完了を待たない。#622（#589）の本文置換は同じ編集履歴のAPIを使うため、current diffが変わった際に競合と履歴契約を確認する。#620の設定画面分離、#625のnetwork coreとは責務を分離する。変更前または提出前にcurrent mainと関連PRを再確認する。

通常リストの再実装、自動番号振直し、drag並替え、タスク集計、専用保存形式、閲覧モードの編集許可、AADW/.github/AGENTS/CLAUDE変更、独立したリファクタリングは対象外。新しい専用ショートカットは要求しない。直接のMarkdown編集は引き続き可能。

## 実装メモ（Claude Code worker）

実装は上記の設計に沿って次の構成にした。worker内ではshell・テスト・lint・GUIを実行していないため、検証結果はfinalizer/CI/Commanderの観測を正とする。

- `hane_markdown::MarkdownParse::task_markers`: パーサーの直下 `TaskMarker` を、所有itemの bullet/番号マーカー直後の `[ ]` / `[x]` / `[X]` 三バイトとして再確認した結果。確認できない候補はタスクUI化しない。文書全体の `ListProjection` にも `ListProjectionItem::task` として載せ、viewport部分解析でも同じ結果になる。
- presentation: 非開示時は bullet/番号のsynthesized labelに続けてチェックボックス字形（`☐` / `☑`）を同じsegmentに含め、実バイトは隠しsegmentとして対応付ける。リスト全体の整列幅（`marker_labels`）も同じ字形で測る。開示時は通常の `ExpandedMarkup` になり、コントロールは出ない。`ListMarkerMetadata::task` が実source範囲と字形のvisual範囲を持つ。
- `BlockLayout::task_checkbox_at_x`: 描画と同じ行geometryで字形範囲だけをhit testする。折返しの継続行・開示行は答えない。
- UI（`crates/ui/src/view/task_checkbox.rs`）: 修飾キーなしの左押下だけを扱い、閲覧モード・IME中は何もしない。現在のsession・revisionで `plan_task_toggle` が再検証し、中央1バイトだけを `replace_range_recorded` + `after_input` で置換する（独立した1 Undo単位、選択offsetは不変）。押下で消費するためcaret移動・dragは起きない。
- 未実施: workspace test/clippy、focused GUI、保存再読込のGUI確認、性能比較。チェックボックスは字形で表現しており、フォント由来の見た目は実機で未確認。

## 引渡し

Claude Codeが製品コードと直接関連する回帰テストを実装する。Commanderはこの文書とIssueの受入条件を実装用PRへ渡す。開始時とpush直前のexact-head検証は既存trusted runner/finalizer契約に従う。workerは次工程、モデル切替、merge、Issue closeを決めない。
