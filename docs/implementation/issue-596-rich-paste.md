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

## 並行開発

#591 / PR #679はopen Draftで、clipboard snapshotとPaste dispatchが重なるが、未受入の製品コードはmainへ存在しない。本フェーズはcurrent mainの受入済みinput routingと範囲置換を使い、#679の実装をコピーしない。将来どちらかのmerge後にもう一方を共有snapshot/単一dispatchへ統合し、二重読取・優先順位判定を残さない。画像取り込み・保存・履歴rewriteは本PRの対象外。#297全体は待たない。

## 検証

実装前のplain Copy/Paste（1KiB/100KiB/1MiB）と10MiB文書の短いcopyのbaselineを記録し、実装後に同条件で比較する。未測定値は成功扱いしない。
純粋変換fixture、OS codec/native clipboard test、本文のfocused GPUI回帰、workspace tests/Clippy、current-head最終reviewとmacOS/Windowsの実clipboard GUIを行う。Windowsは接続先Windowsアプリから直接操作し、Mac経由VM操作は行わない。

## 状態

この文書は実装用Draft PRの準備のみ。製品コード・テストは未実装で、baseline・GUI・CI・reviewも未完了。製品変更はClaudeへ正式依頼する。
