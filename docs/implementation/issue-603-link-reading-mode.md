# Issue #603 実装準備: 閲覧モードでリンクをクリックして開く

- Issue: https://github.com/hide212131/hane/issues/603
- Parent: #13
- 関連: #502 / PR #503（修飾キーでHTTP(S)リンクを開く）, #598（作業フォルダ内の相対Markdownリンク）
- 基準main: 68c438aea3ce39e12b25aa4ad3e2f5e16294190d
- 実装branch: work/issue-603-link-reading-mode

この文書はClaude実装用Draft PRの準備資料です。このcommitでは準備文書だけを変更し、製品コードとテストは変更しません。ビルド、テスト、GUI確認も未実施です。

## 目的と範囲

Markdown本文中のHTTP(S)リンクを開く操作をモードごとに分けます。

- 編集モード: macOSはCommand+左クリック、Windows/LinuxはCtrl+左クリックでOSの既定ブラウザへ開く。修飾キーなしのクリックは従来どおりカーソル移動・選択に使う。
- 閲覧モード: 修飾キーなしの左クリックでOSの既定ブラウザへ開く。OS標準の修飾キー付きクリックも従来どおり開ける。
- 対象: parserが認識するMarkdown inline/reference linkの絶対HTTP(S) destination。
- 対象外: 相対パス、file URL、HTTP(S)以外のscheme、画像、リンク文字列以外、HTML link、preview、WebView。

リンクを開く操作ではcaret、selection、本文bytes、revision、Undo/Redoを変更しません。リンク外のクリックはそれぞれのモードで現在の動作を保ちます。Shift/AltやOS標準ではない修飾キーを含む組み合わせは、既存の編集・選択動作を保ちます。

## current codeの境界

基準SHAのmainで確認した経路:

1. `crates/ui/src/view.rs` の `EditorView::on_row_mouse_down` はvisible rowの実際のhit-test結果を受け、`apply_row_click` の結果が `OpenLink` の場合に `cx.open_url` を呼びます。
2. 同ファイルの `link_destination_at` はpresentation済みlineのlink範囲とdestinationを参照し、`apply_row_click` は `glyph_offset` を使ってリンク文字上のクリックだけを開きます。
3. 現状の `apply_row_click` は `is_link_open_modifier` がtrueの場合だけリンクを開きます。macOSはCommand、Windows/LinuxはCtrlを要求し、他の修飾キーとの併用は除外します。
4. `EditorView` はwindow単位の `EditorMode` を保持し、`crates/ui/src/view/reading_mode.rs` がEditing/Readingと本文の編集可否を管理します。
5. destination、link range、visible-row/source mapping、OSの既定ブラウザhandoffはIssue #502 / PR #503で受け入れ済みです。

modeに応じたactivation条件を既存のrow-click経路へ接続してください。座標からリンクを推測したり、UI側でMarkdownを再parseしたり、新しいリンク状態/cacheを追加したりしないでください。

## 受け入れ条件

- macOS編集モードではCommand+クリック、Windows/Linux編集モードではCtrl+クリックでHTTP(S) destinationを既定ブラウザへ渡す。
- 閲覧モードでは修飾キーなしの左クリックでHTTP(S) destinationを既定ブラウザへ渡す。既存のCommand/Ctrl+クリックも維持する。
- inline link/reference link、複数行・soft-wrap linkでクリック位置に対応する正しいdestinationを開く。
- linkを開く操作はcaret/selection/document bytes/revision/Undo/Redoを変更しない。
- 編集モードの通常クリック、Shift/Alt併用、ドラッグ選択、および閲覧モードの非リンク上の動作を維持する。
- 非link、画像、相対URL、file URL、HTTP(S)以外を外部ブラウザで開かない。
- UI回帰テストでmode別activation条件と編集stateの不変を確認する。既存のIssue #502 destination/hit-test保護も維持する。
- macOSとWindowsのfocused GUI scenarioで編集モードのmodifier clickと閲覧モードのplain clickを確認し、結果と対象headを記録する。

## 依存と並行作業

- Issue #502 / PR #503は外部HTTP(S) link handoffの既存経路として受け入れ済みであり、本Issueはmode別activationだけを追加します。
- Issue #598は作業フォルダ内の相対`.md` linkをHane内で開く別機能です。外部HTTP(S)の対象判定を利用しつつ、相対linkのrouting・modifier条件を変更しないでください。実装開始時に#598のcurrent Issue/PRとcurrent mainを読み直してください。
- 変更責務は既存のEditorView click handlingとその回帰テストに限定します。#297全体や無関係なrefactorの完了は待ちません。
