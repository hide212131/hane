# Issue #673 裸HTTP(S) URLのリンク対応・実装準備

- 契約: https://github.com/hide212131/hane/issues/673
- 基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`
- 実装用branch: `codex/issue-673-bare-http-url`
- 関連: #502 / PR #503、#603 / PR #604、#670 / PR #674、#97

これは文書だけの準備commitであり、製品コード・テスト変更、ビルド、テスト実行、GUI確認はしていない。利用者は「URL直接書いてもリンクにならない」と報告した。Windowsの既存MarkdownリンクのCtrl＋クリック不達は別Issue #670であり、この機能追加と混ぜない。

## 読解済みの既存契約

`crates/markdown/src/parser.rs::build_tree` はpulldown-cmarkのoffset event streamからtreeとlink metadataを作る。`LinkType::Inline / Reference / Collapsed / Shortcut` と `is_external_http_url` に該当するdestinationだけが `LinkParse` に入る。`<https://example.com>` のautolinkと裸URLは、今回利用者が求めた直接記述と同じものと仮定しない。現時点でbare URLの自動認識経路は確認できない。

`crates/markdown/src/syntax.rs` の `LinkParse` / `ParseSnapshot.links`、`crates/presentation/src/display.rs` の `LinkTarget` / `VisualLine.links` がsource byte範囲とdestinationを保持する。既存tree nodeとの対応、半開区間、全体offset、reference定義更新、snapshot/cacheの鮮度契約を維持する。新たな候補がtreeのLink nodeを持たない場合の表現は、実装前に現在の利用箇所を調べて決める。架空node IDや既存Markdown nodeの誤用で接続しない。

`crates/ui/src/view.rs` の `row_hit_at_window_point` / `row_hit_at_point` / `link_destination_at` / `apply_row_click` は表示行と実glyphに対応するsource範囲を照合して `cx.open_url` へ渡す。裸URLも同じ入口へ流し、UIでURLを再parseしない。クリック仕様、閲覧pending pressと4論理px閾値は変更しない。

workspaceのGPUIは `gpui-pre = 0.3.6`。active OS backendはpatchされた `vendor/gpui-pre-macos` / `vendor/gpui-pre-windows` で、旧 `vendor/gpui` の実装をactiveと仮定しない。このIssueでOS起動経路を新設しない。

## 実装前に確定する詳細

URL認識方式と具体的な境界期待値は未確定。PR担当がcurrent main・既存依存・既存parser契約と一次仕様を確認し、Jevで方式を選び、採用した詳細をこのbranch文書へ反映してからClaudeへ依頼する。認識libraryを追加する場合は必要性と既存parserとの役割を明記する。大きな共通設計変更が判明した場合は関係するADRを先に整理するが、既存link metadataへ局所的に接続できる限り新ADRを前提にしない。

最低限の仕様決定表:

| 入力/状態 | 必要な期待結果 |
|---|---|
| `https://example.com` / `http://example.com` | URL bytesを変えずdestination化 |
| path/query/fragmentを含むURL、同じ段落に複数URL | 各URLの半開source範囲・正しいdestination |
| 空白・改行・直後の句読点・閉じ括弧 | 隣接本文をURLへ取り込まない。具体例と採用規則を確定 |
| URL内の対応する括弧、UTF-8本文に隣接 | URLの一部を切り落としたりbyte境界を壊したりしない |
| inline/fenced code、HTML領域、既存link/image | 対象外領域を再リンク化しない、既存metadataと重複させない |
| 入力途中→URL完成→末尾削除→再入力 | snapshotと表示・hit-testの範囲が追従する |
| 保存・再読込・背景解析・cache reuse | 原文保持、古い範囲やdestinationを再利用しない |

明示schemeなしのwww、email、file/他scheme、相対ファイルの新対応は含めない。URLをMarkdown link記法へ自動置換しない。source text、保存形式、既存copy/selectionを変えず見た目と操作対象を追加する。

## 検証・受入への対応

受入条件はIssue本文を正とする。parserで境界と除外・offset、presentationで色とsource↔visual対応、UIで両mode/OS modifier条件と編集state不変をfocusedに検証する。soft-wrap、URL末尾glyph、scroll/zoom後を確認し、適切な回帰テストを追加する。全域再設計や無関係な性能最適化を混ぜない。認識処理の増分・背景解析への影響がある場合は同じ入力の変更前基準を取り、全文の毎回走査をUIへ追加しない。

GUIは両OSで編集（macOS Command / Windows Ctrl）、閲覧（無修飾mouse-up）を確認する。編集通常クリックは選択、閲覧の4論理px未満は開く、以上はドラッグ選択、MouseMoveなしreleaseも既存契約を維持する。開くだけでcaret/selection/bytes/revision/Undo/Redoを変えないことをstateテストと前後観測で確認する。ブラウザの実到達URL・対象head/base・binary・PID/path・隔離state・mode・入力と結果を記録し、起動要求だけを到達成功にしない。

Windows GUIは許可済み既存Windowsチャットから直接操作し、Mac経由VMを使わない。GUI操作前にhane-gui-test skillと共有GUIの利用状況を確認する。失敗時は `docs/gui-test-recovery.md` に従い復旧し、unknownをpassにしない。自分の検証アプリだけを終了確認後に片付ける。

## 並行開発と引継ぎ

#670 / PR #674はWindowsの既存Markdownリンク不達を調査する別PR。parser/presentation側の独立した設計は進められるが、click handlingを両PRで同時変更しない。PR #674が修正するactive backendやhit-testの状態を再取得し、最終Windows GUIに必要な経路をcurrent candidateへ統合して確認する。起動不達を裸URL認識の追加で直ったと主張しない。

#598の相対リンク、#614/#615のcursor、#629/PR #630の挿入機能、#567のview分割も責務が重なる範囲だけ再確認する。#297/RF0全体完了は依存にしない。調査時点でこれらのopen PRはなかったが、着手時にcurrent factsを読み直す。

共有checkout `/Users/hide/dev/Codex/hane` には既存の未commit `crates/ui/src/view.rs` 変更がある。受付は `/tmp/hane-url-link-intake-20261104` の独立cloneで文書だけ作成し、共有変更を上書きしていない。後続は独立した作業場所を使い、共有branchのcheckout/reset/stashをしない。

通常の意味判断には既存Jevを使用。公開factsによる `split` と独立文書準備 `prepare` を採用し、要求model `jev-latest` / 実行model `jev-1.14.0` を確認した。受付はDraft PR作成まで。設計詳細の確定、Claudeへの一度の正規依頼、prepare受理確認、実装以降は別PR担当チャットが行う。
