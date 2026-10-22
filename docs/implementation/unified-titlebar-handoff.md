# Issue #542 実装への引き継ぎ

## 入口

- 仕事の契約: [Issue #542](https://github.com/hide212131/hane/issues/542)
- 具体的な実装仕様: [unified-titlebar.md](unified-titlebar.md)
- 配置図: [unified-titlebar-layout.svg](assets/unified-titlebar-layout.svg)
- 作業手順: [AGENTS.md](../../AGENTS.md)、[Commander Policy](../aadw-command-policy.md)、[Claude実行手順](../aadw-claude-execution.md)、[ADR-0030](../adr/0030-gpui-kit-incremental-local-adoption.md)

この文書を作成した2026-10-21時点では、設計保存までを行い、製品コードとテストの実装、workerの起動、実機確認は行っていない。基準mainは `29f7943ce225016ebf3f76c1115857d887388103`。このSHAを将来の実装headとして使わない。

## 実装者へ渡す要約

目的は、Windows / macOSでOSのタイトルバーとファイルタブを一段にし、アプリのLight / Dark / System設定と配色を揃えること。

1. 既存の `gpui-component 0.6.6::TitleBar` を使う。高さ40px、macOS標準ボタン位置 `(9, 12)`・左予約80px、Windows右ボタン34px×3、移動用余白48px以上。最小サイズ330×240論理px。新規の＋／タブ閉じるボタンは作らない。
2. 共通外枠を上部＋残りの内容にし、編集画面では既存メニュー／タブ、設定画面では「設定」を置く。`settings_screen_shell` の `#[inline(never)]`、戻る処理、モーダルの遮蔽を保つ。非対象OSの既存配置を維持する。
3. タブとメニューは `.occlude()` と左mousedown／二度押しの伝播停止でDragから分離する。右3ボタンへ独自click処理を追加しない。48px余白とOSボタンを操作領域の囲いに入れない。
4. themeの初回同期を行う。native appearanceは既存の `gpui::App::set_window_appearance`（`gpui-pre 0.3.6`）で、Light／Dark→`Some(...)`、System→`None`。GPUI更新・vendor変更は行わない。macOSではoverrideの設定後とrender時の両方で `cx.window_appearance()` を読み、古い `window.appearance()` キャッシュへ戻らない。component themeも同期する。
5. 本文の開始Yは `40 + find_height`、本文高は `H - 40 - find_height - footer_height`。40を二重に引かず、sidebar高だけ `H - 40` へ変更。`InputCapture` の実boundsを保ち、IME候補へ40を手動加算しない。終了は通常ビルドの既存 `flush_pending_drafts` 経路へ渡す。

要約と詳細設計が将来食い違った場合は、Issueの要求と詳細設計の正本を整合させてから実装する。PRコメントだけで仕様を上書きしない。

## 着手時に確認すること

current main、Issue本文、設計PRの採用状況、関連PR、対象の実行中runを読み直す。設計PRをそのまま実装用PRへ転用しない。

直接重なるのは **PR #538 / Issue #536**。設計時点のheadは `beb51eca223d15f3ebec5b7a4a4367596c51f643`、open。上部の「ファイル」メニュー、一般設定の `set_theme`、フッター32pxが入るため、採用後のcurrent mainを基準にする。未採用なら上部の担当範囲を先に調整し、同じ責務の製品変更を並行して開始しない。未採用PRを丸ごとコピーする指示ではない。

PR #541は設計時点では別責務。#297全体、#305全体の完了は待たない。採用済みの入力先整理・短い入力欄を使い、#307の文字配置内部の再設計を混ぜない。

## Commanderから実装workerへ渡す手順

実装開始を依頼されたセッションで、既存の[Claude実行手順](../aadw-claude-execution.md)に従う。

1. 採用された設計がcurrent mainにあるか確認する。未採用なら設計の扱いを決めてから進める。設計文書を実装branchの `pr-head/` から読める状態にする。URLだけをworkerへ渡さない。
2. current mainから実装用branchを作る。`docs/implementation/issue-542-unified-titlebar.md` などにIssue、基準SHA、採用設計、今回の範囲、未実装・未検証を記した準備commitを作り、実装用Draft PRを作成する。本文は「Claude実装待ちの準備PR」、`Refs #542` とする。
3. current headと実行中run、mainにある現行workflowの契約を再確認し、実装用PRのConversationへ現在の40桁headを入れた依頼を投稿する。設計保存PRへ起動コメントを投稿しない。
4. `prepare` 成功で依頼が受理されたと扱い、workerの開始はrunの実状態で別に確認する。`skipped` は未開始。失敗理由を読まずに同じ依頼を再送しない。workerのshell／Git／認証制限を緩めない。

以下は**将来の実装用PRへ投稿するための雛形**。この文書自体は起動指示ではない。`<実装PRの現在の40桁head>` を実値へ置き換え、残りの前提もcurrent stateへ合わせる。

実際に投稿するときは、コードフェンスを外し、`@claude` と各機械用項目をそれぞれ一行ずつ貼り付けてください。

```text
@claude
AADW_COMMANDER_HANDOFF_V3
AADW_ACTION: implement
AADW_TARGET_HEAD: <実装PRの現在の40桁head>
AADW_FAILURE_CLASS: initial
AADW_ROOT_CAUSE: issue-542-unified-titlebar-initial
AADW_EVIDENCE: https://github.com/hide212131/hane/issues/542

Issue #542の統合タイトルバーを実装してください。

pr-head/docs/implementation/unified-titlebar.md を具体仕様の正本とし、
同ディレクトリの unified-titlebar-handoff.md と対象コードを読んでください。
AGENTS.md、docs/aadw-command-policy.md、docs/aadw-claude-execution.md に従ってください。

受入条件:
- Windows / macOSで40pxの一段の上部とし、編集・設定の両画面に残す。
- Light / Dark / System、初回同期、明示設定からSystemへの復帰が一貫する。
- メニュー、既存タブ操作、移動用余白、OSボタンを分離し、狭幅で押し出さない。
- 本文・sidebar・検索・AI画面・実IMEの座標を二重にずらさない。
- 通常ビルドの終了フックと下書き復元、タブの未保存確認を既存経路で保つ。

設計で指定した既存TitleBar・WindowOptions・appearance APIを利用してください。
PR #538の採用済みメニュー／一般設定を保持し、footer高さを固定しないでください。
GPUI更新、vendor変更、保存仕様追加、本文内部geometryや入力routingの刷新は対象外です。

担当は製品と必要な対象テストの編集です。使用可能な道具はRead/Edit/Write/Glob/Grep。
shell、Git、test/build/lintの実行、push、GUI操作は行わず、trusted finalizerと後続CI／GUIへ渡してください。
未実行の検証を成功と書かず、変更ファイル・残る確認を日本語で報告してください。
```

通常実装モデルやworkerの制限は既存手順を正とし、この文書で別の実装経路を新設しない。current-head CIが成功するまで検証済みと扱わない。設計時点のSHAや過去PR本文の検証SHAを新しい候補の証拠として再利用しない。

## 実装後の検証と返却

詳細設計のT01〜T10に対応させ、既存workspace tests／ClippyとmacOS・Windowsのrequired CI、必要な通常・release build、実画面を確認する。既存の計測で影響する性能を比較し、無関係な全回帰や新しい測定基盤を足さない。

Windows実画面は `papa-win` のWindows側ChatGPTから直接操作する。Keyhac hookはショートカット試験中OFF、終了時ON。確認用プロセスを終了し、OS・DPI・アーキテクチャ・build/PID・隔離state・fixtureを記録する。macOSの標準ボタン／二度押し／全画面復帰、WindowsのSnap／resizeはソース調査だけで合格にしない。

通常ビルドの終了・下書き復元を必ず含める。既存のWindows IME問題 #523 は同条件のbaselineと比べ、新しい座標ずれと区別する。#524の遅延条件は入力待ち・routingへ変更が及ぶ場合に追加する。

返却内容は、変更理由と動作、current head／必要なbase context、対象テスト・CI・実画面の証拠、受入条件ごとの結果、未確認の条件。既存ポリシーに従ってレビュー・統合を判断する。設計文書の保存や準備PRの作成だけでIssueをcloseしない。
