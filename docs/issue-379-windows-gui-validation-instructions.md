# Issue #379 Windows GUI確認手順（T31）

この文書は、別のWindowsセッションでIssue #379のWindowsネイティブGUI確認を行うための手順書です。対象は現在の実装PR #397のT31、および画面入力・フォーカスに関係するT25〜T27のWindows確認です。

実装時の確認事項として、設計PR #392の仕様ではAI設定を全画面で表示し、設定フォームの入力・IME・Tab移動・Escape・Ctrl+SをEditorへ漏らさないこと、設定表示中に外部ファイルを開いても新しい文書状態を古いsnapshotで巻き戻さないことを求めています。設計PRが未マージの場合は、その時点のPR headから仕様を読み直してください。

## 実行前に確認すること

開始時にGitHubとWindows側の作業ツリーから最新状態を取り直します。本書作成時に観測した値は開始条件として固定しません。

- Issue #379、PR #397、設計PR #392の状態とコメント。
- PR #397のcurrent head、base branchのcurrent head、差分、Actions、submitted reviews。
- branch `feat/379-chatgpt-codex-oauth-ai-settings` と作業ツリーの変更。
- `AGENTS.md`、`docs/aadw-command-policy.md`、設計PR #392の実装仕様。
- Windows OS版、アーキテクチャ、実行するアプリのcommit/build。

PR #397が別のheadへ進んでいた場合は、必ずそのcurrent headをWindowsで確認します。古いheadやGitHub ActionsのWindows jobだけをWindows GUIの証拠にしません。対象PRが閉じている、branchが変わっている、または対象binaryがcurrent headと結び付かない場合は続行せず、差分と blocker を報告します。

この作業はGUI検証です。製品コードを変更しません。再現する不具合があれば結果と手順を記録し、独断で修正・pushせずCommanderへ返します。

## テスト環境の隔離

1. Windows側でcurrent PR branchのcleanなcheckoutを使います。既存の未コミット変更を上書き・削除しません。
2. Windowsネイティブ環境で対象commitからアプリをビルドします。既存のWindows CI/release workflowとCargo package定義を確認し、実行ファイルが対象commitから作られたことを記録します。
3. 普段使いのHane stateやworkspaceを使わず、環境変数 `HANE_STATE_DIR` を新しい一時ディレクトリに設定します。Markdown確認用にも空の一時workspaceと、テスト用の小さなMarkdownファイルを用意します。
4. 一時stateを指定した状態でHaneを起動し、一時workspaceだけを開きます。認証情報や通常の設定が一時領域から漏れていないことを確認します。
5. 作業後は一時テスト領域だけを片付けます。Haneの通常state、通常workspace、Windowsの既定アプリ設定、file association、シェル拡張は変更・削除しません。

例（PowerShell。パスは衝突しない一時名に変更してください）:

```powershell
$testRoot = Join-Path $env:TEMP 'hane379-t31-windows'
$env:HANE_STATE_DIR = Join-Path $testRoot 'state'
$workspace = Join-Path $testRoot 'workspace'
New-Item -ItemType Directory -Force -Path $env:HANE_STATE_DIR, $workspace | Out-Null
```

Repository rootでWindows向けの既存手順に従って `hane` packageをビルドします。例えば、開発用binaryを使う場合は `cargo build --locked -p hane` の後に `target\debug\hane.exe` を起動できます。実際の起動方法が対象headで変わっていたら、現行のpackage/workflowを優先してください。

## 実施するシナリオ

各シナリオについて、実結果を画面で確認し、pass / fail / 未実施を記録します。未実施をpassに読み替えません。

### W1 — AI設定の表示と移動（T31 / T24）

1. Haneの設定を開き、AIページへ移動します。
2. AIページがEditor横の別panelではなく、既存設定画面のナビゲーション内に表示されることを確認します。
3. ChatGPT/CodexとCustom Providerを切り替え、選択中の設定フォームと、保存済み設定／未保存draftの表示が区別されることを確認します。
4. 画面をスクロールできる場合、保存操作が見える状態で操作できることを確認します。
5. 未保存の変更を作り、設定カテゴリを離れようとします。「保存して移動／破棄して移動／編集を続ける」の表示を確認します。「編集を続ける」と「破棄して移動」を試し、期待する画面とdraft状態になることを確認します。
6. ChatGPTのOAuthログイン、Custom Providerへの接続、応答確認／Probeは実行しません。

### W2 — Windows IME、Tab、Escape、Ctrl+S（T25）

1. 一時stateでCustom Providerの接続名など、秘密ではないテキスト欄を選びます。API key欄には入力しません。
2. 利用可能なWindows日本語IMEで日本語を入力し、変換中も設定欄が再生成・フォーカス喪失せず、確定した文字が欄に入ることを確認します。
3. IME変換中にEscapeを押します。まず変換が取り消され、同じEscapeで設定ページやアプリ全体が閉じないことを確認します。
4. Tab / Shift+Tabで設定欄とボタン間を移動します。フォーカス順が画面の表示順と合い、現在のフォーカスが見えることを確認します。
5. テスト用Markdownに短い文字列を入力し、Editorの保存・dirty状態を記録します。AI設定の入力欄にフォーカスを移し、ダミーの接続名を入力してCtrl+Sを押します。
6. Editorの本文、保存済み／dirty状態、選択、undo可能状態が変わらず、Editorへキー入力や保存操作が漏れないことを確認します。AI設定側で保存操作が起きた場合はその挙動も記録します。

日本語IMEがWindows環境にない場合はこのシナリオを「未実施」とし、環境を明記します。別OSや自動UI testで置き換えません。

### W3 — 設定表示中に外部ファイルを開く（T26）

1. AI設定ページを表示した状態で、Windowsの既存のHane外部open経路を使い、一時workspace内の別Markdownファイルを開きます。
2. 既定アプリ設定やfile associationの変更、shell extensionのインストールが必要なら実施せず、「未実施」とします。
3. 外部open後もAI設定が不意に閉じないことを確認します。
4. Editorへ戻り、新しいMarkdownファイルが現在の文書として表示されることを確認します。設定表示前の文書内容へ巻き戻らないことを確認します。

### W4 — Editor状態の保持（T27）

1. テスト用Markdownの本文、dirty状態、undo可能状態、選択範囲、スクロール位置を記録します。
2. AI設定を開閉し、接続方法を切り替え、秘密でない設定欄へダミーテキストを入力して取り消します。
3. Editorへ戻り、記録した文書状態が意図せず変更されていないことを確認します。
4. W3で別ファイルを開いた場合は、その新しい文書が維持されていることを追加で確認します。

## 禁止事項

- OAuthログイン、ログアウト、実アカウント操作、Custom Provider実接続、応答確認を行わない。T29/T30は今回のWindows GUI確認範囲外です。
- API key、token、OAuth URL、email、実endpointなどの秘密値を画面記録・チャット・ログ・スクリーンショットへ残さない。ダミー値も記録に含めません。
- 通常のHane state/workspaceや他の利用者のファイルを使わない。
- file association、既定アプリ、shell extension、セキュリティ設定を変更しない。
- PR #397のDraft解除、CodeRabbitレビュー起動、merge、Issue closeを行わない。
- GUI Validatorとして製品コードを修正しない。再現した問題は停止して証拠とCommander判断が必要な事項を報告します。

## 証拠と報告

記録には次を含めます。

- 対象Issue、PR、branch、検証開始時・終了時のhead SHA、base SHA。
- Windowsのedition/version、architecture、Hane binaryのbuild元commit。
- 使用した実行手順、各シナリオのpass / fail / 未実施、観測結果、再現手順。
- Hane標準設定や個人ファイルに触れていないこと。
- サニタイズ済みスクリーンショットが必要な場合は、一時workspaceとダミー情報だけを表示した状態で撮影します。
- 問題があれば、製品不具合／検証環境の障害／未確定のどれかを区別します。原因が分からない場合は未確定とします。

対象headが検証中に変わった場合は、そのGUI結果をcurrent PR headの証拠として使いません。結果を保存する場合は `docs/issue-379-t00-evidence.md` にWindowsの観測事実と該当headを追記し、PR #397の本文にも同じ内容を反映します。T31全体はcurrent-head CI、必要なreview evidence、macOS/Windows GUIの全範囲が揃うまで完了扱いにしません。

## 別セッションへ渡す依頼文

このリポジトリの `docs/issue-379-windows-gui-validation-instructions.md` と、冒頭で確認した最新の `AGENTS.md`、Commander Policy、設計PR #392の仕様に従って、Issue #379 / PR #397のWindowsネイティブGUI確認を実施してください。作業対象はT31およびこの文書のW1〜W4です。Windows環境とPRのcurrent headを最初に検証し、隔離state・一時workspace・current commitのHane binaryを使ってください。実OAuth・Custom接続・Probe、製品コード変更、PRのDraft解除、CodeRabbit起動、merge、Issue closeは行わず、pass / fail / 未実施と証拠・再現手順を日本語で報告してください。結果を記録する場合は、検証したheadがPR #397のcurrent headと一致することを確認してから証拠文書とPR本文へ反映してください。
