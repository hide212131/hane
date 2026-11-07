# Issue #596: 外部リッチテキストのMarkdown貼り付け（先行フェーズ）

対象: https://github.com/hide212131/hane/issues/596
基準 main: `0e7bbd221db2f377aee9d9992b85b8d5e0362ea7`
採用済み設計: `docs/design/issue-596-rich-clipboard.md`（設計PR #650 merge済み）。

## 今回の契約

利用者の依頼はWindows/macOSで外部アプリからコピーしたリッチテキストをMarkdownとして貼り付け、mainへのmergeとreleaseまで行うこと。ブラウザと文書アプリを代表対象とする。Issue #596の外部入力側を先行フェーズとして実装する。書式付きコピー/exportは後続とし、このPRで親Issueをcloseしない。

- 外部HTMLの段落、H1〜H6、太字、斜体、取り消し線、コード、リスト、引用、HTTP(S)リンク、改行をMarkdownへ変換する。フォント・色・背景・任意CSSは持ち込まない。RTF-onlyの書式復元は既存設計の対象外で、提供されたplainを使う。
- 通常Pasteと本文のプレーンテキストPaste、右クリックメニューを実装する。plain-onlyは原文のまま。Hane内Copyは今回plainのままなので、原文の往復を維持する。将来用metadataを読む場合も設計どおり一致を確認し、信頼の根拠にはしない。
- 本文以外の入力先・閲覧モード・IMEを保護し、選択置換とUndo/Redoを一編集として扱う。非同期jobはsession/instance/revision/selection方向/generation/focusを検証し、古い結果を適用しない。
- 同一世代のsnapshot、CF_HTMLのbyte offset、native所有権、HTML-only、不正HTMLとplain fallback、入力・node/depth・出力上限、安全な破棄を設計に従って扱う。script等を実行・保持せず、ネットワーク取得しない。
- `vendor/gpui`は旧経路のため変更しない。現行platform patchとhane-markdownの責務を使い、UIへ別parserやnative APIを複製しない。

## URLリンク混在の必須条件

通常テキストと複数のリンクが混在する文章を、前後の文章を保って`文章 [表示テキスト](https://example.com/) 文章`へ変換する。日本語ラベル、強調とリンクの入れ子・隣接、句読点、括弧やクエリを含むリンク先を確認する。ラベルをURLへ置換したり段落全体をリンク化したりしない。既存#629とは別の外部HTML入力の契約である。

## 並行開発

#591 / PR #679はopen Draftで、clipboard snapshotとPaste dispatchが重なるが、未受入の製品コードはmainへ存在しない。本フェーズはcurrent mainの受入済みinput routingと範囲置換を使い、#679の実装をコピーしない。将来どちらかのmerge後にもう一方を共有snapshot/単一dispatchへ統合し、二重読取・優先順位判定を残さない。画像取り込み・保存・履歴rewriteは本PRの対象外。#297全体は待たない。

## 検証

実装前のplain Copy/Paste（1KiB/100KiB/1MiB）と10MiB文書の短いcopyのbaselineを記録し、実装後に同条件で比較する。未測定値は成功扱いしない。
純粋変換fixture、OS codec/native clipboard test、本文のfocused GPUI回帰、workspace tests/Clippy、current-head最終reviewとmacOS/Windowsの実clipboard GUIを行う。Windowsは接続先Windowsアプリから直接操作し、Mac経由VM操作は行わない。

## 状態

先行フェーズの製品コードとテストを実装した。実装した責務は次のとおり。

- `crates/markdown/src/clipboard.rs`: 外部HTML→Markdown/プレーン文字の純粋変換（`html_to_markdown` / `html_to_plain_text`）。入力・node・深さ・出力の上限は構築中に適用し、DOMはflatな`Vec`で破棄時に再帰しない。fixtureは`crates/markdown/tests/rich_paste.rs`。
- `vendor/gpui-pre-macos` / `vendor/gpui-pre-windows`: 同一世代のplain/HTML/画像/ファイルを読む`read_rich_clipboard`（macOS `changeCount`、Windows `ClipboardGuard`とCF_HTMLのUTF-8 byte offset）。読取り専用で、書式付きexportは含まない。
- `crates/ui/src/clipboard.rs`: スナップショットと貼り付けの決定表（純粋）。`crates/ui/src/view/clipboard.rs`: 本文Paste/PastePlainTextの共通入口、非同期変換の鮮度検査、範囲置換。`actions.rs` / `view/editor_context_menu.rs`: Paste・Cut・Copy・`PastePlainText`（secondary-shift-v）と本文context menuの接続。

### 設計との差分（要確認）

- 設計は`html5ever` / `markup5ever_rcdom`を直接依存にするとしていたが、実装担当はshell・buildを実行できず`TreeSink`のAPIを検証できないため、上限を構築中に強制できるcrate内パーサーで実装した。依存とlockfileは増えていない。html5ever採用へ置き換えるかはCommanderの判断事項。
- `read_rich_clipboard`は`source_format`引数と`source_metadata`を持たない（Haneの専用metadataは書式付きexportを後続に回したため未使用）。`WriteFailed`も書込みが無いため未定義。
- 書式付きexport（HTML/metadataの書込み）、RTF-onlyの書式復元、表の完全変換は対象外のまま。

### 未実施（このworkerでは実行していない）

`cargo test` / `clippy` / workspace外vendorのnative test、baseline・性能測定、macOS/Windowsの実clipboard GUI、CI・reviewはいずれも未実施で、Commanderが別途確認する。
