# Issue #502 実装準備: Markdownリンクを外部ブラウザで開く

- Issue: https://github.com/hide212131/hane/issues/502
- Parent: #13
- 基準main: e82b9a9fe42280bfa78497746bcd7e21c73430c5
- 準備branch: work/issue-502-markdown-link-open

この文書はClaude実装用Draft PRの準備資料です。現時点では準備commitのみで、製品コード・テストは変更していません。ビルド、テスト、GUI確認も未実施です。

## 目的と範囲

本文でMarkdownリンクを、OS標準の修飾キー+左クリックでOSの既定ブラウザへ渡します。

- macOS: Command+左クリック
- Windows / Linux: Ctrl+左クリック
- 対象: parserが認識するMarkdown linkのHTTP(S) absolute destination
- 対象外: 相対パス、file URL、他scheme、画像、HTMLリンク、preview、WebView

修飾キー付きでリンクを開く操作は、caret、selection、文書bytes/revision、Undo/Redoを変更しません。通常クリック、Shift+クリック、ドラッグ選択は現在の編集動作を保ちます。Modifier clickがリンク外なら通常の編集動作を保ちます。

## current codeの境界

基準SHAのmainで確認した呼び出し境界:

1. crates/markdown/src/parser.rs の node_kind_for_tag は pulldown-cmark Tag::Link を NodeKind::Link にしますが、destinationは保持しません。
2. crates/markdown/src/syntax.rs の MarkdownNode は kind/source_range/parent/children/depth を持ちますが、destination metadataはありません。
3. crates/presentation/src/display.rs の InlineDisplay.link_color はリンクの見た目をUIへ伝えますが、destinationは伝えません。
4. crates/ui/src/view.rs の EditorView::on_row_mouse_down は visible rowのwindow positionを既存layout/source mapからSourceOffsetへ変換し、Editor selectionを更新します。row elementのLeft button handlerは同じ関数へ接続されています。
5. vendor/gpui/src/app.rs の App::open_url はplatformの既定browserへURLを渡すAPIです。

Parserからvisible layout/hit-testまでdestinationとリンクのsource対応を保持し、既存source mappingを使ってクリック位置がリンク表示文字列上か判定してください。表示座標から構文位置を推測したり、毎回Markdownを別経路で再parseしたりしないでください。destinationをどの型・cacheに保存するかはcurrent codeと既存のsnapshot/cache contractを確認して決めてください。

## 受け入れ条件

- macOSのCommand+クリック、Windows / LinuxのCtrl+クリックでHTTP(S) destinationを既定browserへ渡す。
- inline linkとreference link、複数行・soft-wrapしたlinkで正しいdestinationを開く。
- 通常クリック、Shift+クリック、ドラッグ選択の既存動作を維持する。
- Linkを開いてもcaret/selection/document bytes/revision/Undo/Redoを変えない。
- 非link、画像、相対URL、file URL、HTTP(S)以外を誤って開かない。
- Parser/UIの回帰テストでdestination対応、OS modifier判定、編集stateの不変を確認する。
- macOSとWindowsのfocused GUI scenarioを実施し、default-browser handoffと実施結果を記録する。

## 依存と並行作業

#13本文は「richer link interaction and destination previews」を未分割候補に挙げています。本Issueはdestination previewを含めず、外部browser handoffだけに分けます。

PR #495（Issue #494）はcurrent mainの crates/ui/src/view.rs を含むUI経路を変更中ですが、そのPR patchはon_row_mouse_down、offset_at_row_x、Left-row mouse handlerを変更していません。したがってmain上の既存hit-testへ接続できる限り先行merge不要です。実装開始時にPR #495とmainのcurrent headを取り直し、同じクリック状態を別経路へ二重実装しないでください。#297全体完了は依存にしません。

## 検証

回帰tests、workspace tests / Clippy、focused macOS・Windows GUI scenarioをIssue #502の条件に沿って実施してください。未実施や実行環境がない確認は、実施済みのように扱わず結果に記録してください。