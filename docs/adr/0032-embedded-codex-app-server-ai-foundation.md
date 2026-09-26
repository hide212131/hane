# ADR-0032: Codex App Serverを同梱し、OAuthとCustom ProviderでAI連携する

## ステータス

Proposed（設計案を記録。実装・実機検証は未実施）。

ADR作成日: 2026-09-27 (JST)
元設計書: `hane-ai-foundation-design.md`（2026-09-25作成）
ADR作成時に確認したmain: `b4061b9d43cd0c14dc4894cc4e27fa9227bd8199`

利用者の「リポジトリにADRとして記載」という依頼に基づき、先行する設計書の内容を本ADRに収録する。設計判断案、実装上の契約、受入条件を記録するものであり、この文書の追加だけで設計の承認、機能の実装、認証・推論・GUI検証が完了したとは扱わない。

外部仕様とliltoの調査結果は、元設計書に記載された時点の記録を引き継ぐ。今回のADR化では外部仕様の再検証は行っていない。第2節のSHAとバージョンを現在値として再利用せず、実装時に同梱するCodexの版とスキーマで照合する。

本ADRはHane製品に組み込むAI連携基盤を対象とする。[ADR-0031](0031-aadw-v3-jev-bounded-execution.md)の開発運用で使うApp Serverとは別の責務であり、AADWやCommander Policyを変更しない。

## 1. 目的と初期スコープ

HaneにCodex App Serverを同梱し、設定画面から以下を利用できる基盤を作る。

- ChatGPT/Codexの契約による、App Server管理のブラウザOAuthログイン。
- Custom ProviderのBase URL、API key、モデルID設定。
- 両方式の設定を保持したまま、使用する接続先を明示的に切り替える。
- 内包ランタイムの起動、終了、障害表示、モデル選択、固定の短いテスト入力による実応答確認。

初期はChatGPT接続1件とCustom接続1件で十分とする。多数のProvider管理、複数同時実行、チャット履歴画面、文書自動編集、シェル実行、MCP/Skills/Plugins管理、別AIランタイム対応は含めない。内部識別子は表示名と分離し、将来の複数接続追加を妨げない。

「サブスクOAuth」はChatGPT/Codexへのログインを意味する。他社のサブスクリプション認証を汎用的に提供する設計ではない。利用可否は実際のアカウント・契約・組織ポリシーに従う。

## 2. 確認したリポジトリの状態

| 対象 | 確認したmainのexact head | 確認事項 |
|---|---|---|
| hide212131/hane | `96bdb68db19847d1df12ea55a7830cda512b4fd6` | Workspace version 0.19.2、gpui-pre 0.3.6、gpui-component 0.6.6。AI専用crateはworkspace定義にない。[R1] |
| hide212131/lilto | `bd75ef91a874a7a4cf08f71e20650e515a512ed7` | 設定画面、Provider設定、認証、stdio App Serverクライアントがある。[R4–R8] |

Haneの`crates/ui/src/lib.rs`には、タブと設定画面で使用するgpui-component初期化処理がある。`view.rs`にはButton、Checkbox等の使用と設定用サイドバー領域がある。[R2]

Haneの一般設定は`crates/session/src/store.rs`の`Settings`/`SettingsRepository`で扱われ、確認範囲のSettingsはautosave、theme、default_folderを持つ。AI機能追加のために既存保存形式やDocumentSessionを全面変更しない。[R3]

この設計の実装開始時にはmain、関連Issue/PR、ADR、AGENTS.mdを再取得する。上記SHAを未来のcurrent headとみなさない。

## 3. liltoから採用する機能と、そのまま移植しない部分

### 採用する機能

設定画面にOAuthとCustomの選択、接続方式別のモデル設定、認証状態、ロード中・保存結果表示を設ける。Customは表示名、Base URL、API key、モデルIDを入力する。モデル一覧にない選択値を消さず、CustomのモデルIDは手入力できるようにする。[R4, R5]

App Serverを子プロセスとして起動し、stdioで通信する責任分離も参考にする。HOMEを別の場所に置き換えず、CODEX_HOMEだけを専用領域に設定する考え方を採用する。[R6]

### そのまま移植しない部分

1. **OAuthを別の`codex login`プロセスで開始し、auth.jsonの存在・内部構造で判定する方式。** HaneはApp ServerのアカウントAPIを使い、OAuthトークン取得・更新をApp Serverに任せる。ユーザーのグローバルauth.jsonは自動コピーしない。[R7, S1, S2]
2. **API keyの平文設定保存。** liltoのProviderSettingsServiceはapiKeyを含むstateをJSON保存する。Haneは非秘密設定とOS資格情報ストアを分離する。[R5]
3. **SDK時代の引数がApp Serverにそのまま効くという想定。** liltoの`createCodexThreadFromSdk`はapiKey/baseUrlをSDKへ渡す。一方、確認した`createCodexThreadFromAppServer`の起動・thread/turn作成部分には、options.apiKeyとoptions.model.baseUrlの反映がない。既定のcreateSessionはApp Server経路である。静的なコード確認であり、liltoの実環境での失敗を再現したものではない。Haneは独自Provider定義と秘密の注入を明示し、送信先まで試験する。[R8]
4. **idがあるメッセージをすべてレスポンスとみなす処理。** App Serverは双方向通信である。method+idのサーバー要求をresult/error+idの応答と区別する。[R6, S1]
5. **無条件の危険な実行権限。** liltoで確認したdanger-full-accessやapprovalPolicy=neverの設定はHaneの初期既定値にしない。[R8]

## 4. アーキテクチャ上の決定案

### 4.1 内包は「同梱sidecar」とする

Haneと同じ配布物に、検証済みのネイティブCodex実行ファイルと必要な付属ファイルを含める。Haneが絶対パス指定で`codex app-server --listen stdio://`を起動・終了する。ユーザーによるNode.jsやCodex CLIの別途導入を必須にしない。

Rust内部crateとしてcodex-coreを直接リンクする案は初期採用しない。バージョン更新時の依存衝突、障害、プロトコル追従の範囲をHane本体から分離するためである。これは設計上の選択であり、単一実行ファイルへの静的リンクを実現したという意味ではない。

配布物は「Hane + 同梱ランタイム」になる。厳密な単一exe配布は本設計の要件とせず、必要なら別途判断する。

### 4.2 認証する区間を区別する

```text
Hane設定画面 / 将来のAI UI
             |
         hane-ai
             |
    stdioの双方向JSON-RPC
             |
    同梱Codex App Server
             |
        +----+-------------------+
        |                        |
 ChatGPT/Codex              Custom Provider
 管理OAuth認証              API key認証
```

OAuth/API keyは主としてApp Serverから上流サービスへの認証である。Haneと自分で起動したApp Serverとのローカルstdio接続をOAuth認証するのではない。

通信は改行区切りJSON。LSPのContent-Lengthフレーミングは使わず、現行App Server仕様では`jsonrpc`フィールドを付けない。厳密な入出力型は同梱版のスキーマを基準にする。[S1]

### 4.3 最小のRust構成

新規crateはまず`crates/ai`一つだけとする。

```text
crates/ai/src/
  lib.rs        UI向けの操作・イベント・状態
  runtime.rs    同梱バイナリ解決、起動、終了、障害処理
  rpc.rs        stdio、要求対応付け、通知、サーバー要求
  protocol.rs   使用するAPI型とスキーマ整合
  auth.rs       ChatGPTログイン、取消、ログアウト、状態
  provider.rs   接続方式、非秘密設定、Codex設定生成
  secrets.rs    秘密の保存・取得・削除インターフェース
```

実装が小さいうちは複数の責務を同じモジュールに置いてもよい。多数のtraitや汎用Providerプラグイン機構は作らない。秘密ストアとプロセス境界には、試験用の差し替え口を設ける。

UIは生JSONやトークンを保持せず、`AiService`の操作と秘密を含まない状態スナップショットを使用する。プロセスのライフサイクルはアプリ単位とし、設定画面を閉じても認証や実行状態を失わない。GPUIのUIスレッドでブロッキングI/Oをしない。

## 5. ランタイムとプロトコル

状態の基本形は`Stopped → Starting → Initializing → Ready`、異常時は`Failed`、終了時は`Stopping → Stopped`。

Readyになる前に通常要求を送らない。複数の操作が同時に起動を求めても起動・initializeは一度だけにする。GUIからの操作はコマンドキュー経由で処理する。

通信処理では以下を必須とする。

- 部分読み込み、UTF-8境界、複数行の一括受信を処理する。送信は単一writerで直列化する。
- request idはプロトコルのstring/integer双方を扱い、応答・通知・サーバー要求を分離する。
- 未知の通知は秘匿化した診断に残して継続できる。未知のサーバー要求は無視して待ち続けず、適切な未対応エラーを返す。
- 承認要求を未実装のまま自動許可しない。仕様上の拒否応答を返す。安全に応答できない場合はターンを停止する。
- RPCの待ち時間と長時間の推論待ちを分離する。タイムアウト後の遅延応答で別要求の状態を壊さない。
- 文字列deltaは集約できるが、承認・完了・エラーイベントを古い順に捨てない。上限到達時は明示的に失敗・停止し、成功と表示しない。
- プロセス停止時に保留要求をすべて解決またはエラー終了させる。stderrを読み続け、パイプ詰まりを防ぐ。
- Hane終了時に子プロセスと必要な子孫プロセスを回収する。macOSとWindowsで検証する。
- 障害後に`turn/start`等を無条件で再送しない。二重課金や二重操作の可能性があるため、配信結果不明と区別する。

同梱版のCLIから以下を生成し、必要部分を保存する。[S1]

```sh
codex app-server generate-json-schema --out ./schemas
```

Codex実行ファイルの版、配布元、ハッシュ、スキーマを一組で固定する。Hane起動時の「最新Codex自動取得」は行わない。experimentalApiは必要な機能に限定し、初期は実験的APIに依存しない範囲を優先する。

## 6. ChatGPT/Codex OAuth

使用する標準的な流れは次のとおり。[S1]

```text
initialize → initialized
account/read
ユーザーがログインを選択
account/login/start {type: "chatgpt"}
返却されたauthUrlを既定ブラウザで開く
account/login/completedをloginIdに対応付ける
account/read / account更新で状態を再取得
モデル一覧を取得
```

localhostのOAuthコールバック処理はApp Serverに任せる。Hane独自のOAuthクライアント、埋め込みWebView、トークン更新処理は初期実装しない。返却URLを開く際にはプロトコルと既知の認証先を検証する。

取消は`account/login/cancel`、ログアウトは`account/logout`を使用する。設定画面を閉じることとログアウトは分ける。画面を閉じたまま認証が完了しても状態を反映する。キャンセル後の遅延通知はloginIdと世代で整理し、最終アカウント状態を再取得する。

OAuth資格情報はCodexのOS keyring保存を優先する。`cli_auth_credentials_store = "keyring"`を用い、利用不可時はエラーを明示する。無断の平文フォールバックは行わない。`auto`は平文へのフォールバックを含むため、同じ保証とみなさない。[S2]

keyringがCODEX_HOMEごとに正しく分離されること、Haneからのログアウトが通常のCodex CLIのログインを消さないことを、採用版で確認する。CODEX_HOME設定だけで全ての資格情報分離が保証されたと判断しない。

## 7. Custom Provider/API key

### 7.1 互換性の境界

現行設定リファレンスのCustom Providerの`wire_api`は`responses`のみをサポートする。したがって「OpenAI互換API全般」ではなく「Codexの使用するResponses API要求に対応する接続先」を対象にする。Chat CompletionsのみのProvider向け変換プロキシは初期範囲に含めない。[S3]

モデル一覧を取得できることと、そのモデルで推論できることは別である。CustomモデルIDの手入力を許可し、保存だけで接続確認済みにしない。

### 7.2 設定生成例

以下はBearer API key型のCustom接続例。プレースホルダーを含む設計用の例であり、そのまま実接続できる設定ではない。[S3, S4]

```toml
model_provider = "hane_custom"
model = "<user-selected-model-id>"

[model_providers.hane_custom]
name = "My Provider"
base_url = "https://provider.example/v1"
wire_api = "responses"
env_key = "HANE_AI_PROVIDER_KEY"
requires_openai_auth = false
```

`HANE_AI_PROVIDER_KEY`の値はOS資格情報ストアから取り出して、このCustom用子プロセスだけに設定する。キー本体をコマンドライン、生成TOML、通常ログに書かない。Hane親プロセスのグローバル環境を書き換えない。

認証が`api-key`等の専用ヘッダーを要求する接続先は、必要になった時点で詳細設定を追加し、`env_http_headers`等のCodex設定に明示的に変換する。非秘密のquery_paramsも独立した項目として扱う。接続先の仕様を確認せずBearerと専用ヘッダーを同時送信しない。[S3, S4]

Custom Provider用キーを一律に`account/login/start(type: apiKey)`へ渡す設計にはしない。アカウントAPIのOpenAI API keyログインと、任意のCustom Provider認証を区別する。[S1, S3]

### 7.3 保存と更新

非秘密設定はバージョン付き`ai-settings.json`等に保存する。一般設定の`settings.conf`は維持する。秘密はCredentialRefで参照する。

```text
AiSettings
  schema_version
  active_connection: ChatGpt | Custom
  chatgpt.model_id
  custom.id / name / base_url / model_id / credential_ref
```

保存済みキーは設定画面に再送しない。UIは「保存済み」と表示し、明示的な「変更」「削除」操作を持つ。空欄の意味を「保持」と「削除」で混同しない。

資格情報の更新では、現在の`credential_ref`を上書きしない。Haneが新しいCredentialRefの識別子を秘密の保存前に割り当て、更新操作全体を再起動後も追跡できる永続credential operation journalを使う。journalには新旧CredentialRef、操作状態、設定世代など復旧に必要な非秘密情報だけを保存し、秘密値は保存しない。

1. 新しいCredentialRefの識別子をHane側で先に割り当てる。OS資格情報ストアにはHane専用のservice/namespaceと、この識別子を組み合わせた名前で保存できる契約にする。
2. 新CredentialRefを`pending_new`、旧CredentialRefを切替成功後の削除対象としてoperation journalへatomicに永続化する。journalの保存に失敗した場合は秘密を作成しない。
3. 新しい資格情報を、journalに記録済みの新CredentialRefへ保存する。ここで旧CredentialRefと旧キーは変更しない。保存に失敗した場合もjournalを残し、起動時の復旧処理で存在有無を確認して整理する。
4. 新しいCredentialRefを参照する非秘密設定を一時ファイルへ書き、同一ファイルシステム上でatomic replaceする。失敗時は新CredentialRefの削除を試みる。削除が失敗してもjournalを残すため、追跡不能にはしない。
5. 設定のatomic replaceが成功して永続化されたらjournalの状態を`settings_swapped`へatomicに更新する。その後に旧CredentialRefを削除し、新CredentialRefをactiveとして確定する。削除やjournal更新の途中で終了してもjournalを残す。
6. Hane起動時にoperation journalを処理する。現行設定が新CredentialRefを参照していれば新側を保持して旧側の削除を再試行する。現行設定が旧CredentialRefを参照していれば旧側を保持し、未参照の新側を削除する。どちらとも整合しない場合は秘密を自動採用せず、未参照のHane管理資格情報として安全側に回収または診断する。
7. 復旧と削除が完了した後だけjournalから操作記録を除く。OS資格情報ストアが列挙を提供する場合は、Hane専用namespace内の未参照項目との照合も補助的なgarbage collectionとして行うが、列挙機能だけには依存しない。

キー削除も同じoperation journalを使い、旧CredentialRefを削除予定として永続化してから`credential_ref`を外した非秘密設定をatomic replaceし、その成功後に資格情報を削除する。これにより新規資格情報の作成直後、設定切替直後、削除失敗のいずれで異常終了しても、Haneが作成した資格情報を次回起動時に追跡できる。設定エラーを黙ってデフォルト接続へ戻さない。

## 8. 接続方式と保存領域の分離

概念的には以下の領域を用いる。実パスはHaneのOS別アプリデータ解決に従う。

```text
<Hane app data>/ai/
  ai-settings.json
  codex-chatgpt/    ← ChatGPT用CODEX_HOME
  codex-custom/     ← Custom用CODEX_HOME
  probe-workspace/ ← 接続確認専用の空の作業領域
```

両接続のCODEX_HOMEを分離し、同時に動くランタイムはまず1個とする。切替時は実行中ターンの完了またはユーザー操作による中断を待ち、旧プロセスを終了して新設定で起動する。

HOME/USERPROFILEをこれらの領域へ置き換えない。外部Codexの設定やauth.jsonを取り込まない。ChatGPT資格情報をCustom接続先へ送らない。Custom障害や利用上限時に、別課金経路へ自動フォールバックしない。

将来の各会話はconnection_idと設定世代に紐付ける。接続先やキーの変更後、以前の会話を新しい接続先へ暗黙に継続しない。新しい会話にするか、明示した切替操作を必要とする。

CODEX_HOMEはOS sandboxではなく、状態保存先の分離である。Codexには設定の優先順位やproject configがある。初期はユーザーの文書フォルダをcwdにせず、Hane管理領域で起動する。Provider/送信先/認証方式はHaneから明示してeffective configを検査する。組織の強制ポリシーは回避せず、競合はエラー表示する。[S5]

## 9. 設定画面

既存の全画面設定と「←アプリに戻る」を維持し、カテゴリ「AI」を追加する。gpui-componentを使い、設定フォームをEditorの巨大なview.rsへさらに集積させない。AI用の子View/フォーム状態として切り出す。

| セクション | 項目・操作 |
|---|---|
| ランタイム | 同梱版、停止/起動中/利用可能/エラー、再起動 |
| 使用する接続 | ChatGPT/Codex契約、Custom Provider/API key |
| ChatGPT | ログイン、ログイン取消、状態更新、ログアウト、モデル選択 |
| Custom | 表示名、Base URL、API key変更/削除、モデルID、保存 |
| 応答確認 | 固定テスト入力を表示、送信先表示、「テスト送信」、応答、停止、エラー |

ChatGPTアカウント情報はサーバーが返せる範囲だけ表示する。固定のプラン一覧やモデル名をハードコードしない。

状態は「設定保存済み」「アカウント認証済み」「モデル選択済み」「最終テスト成功」を分ける。Custom接続でOpenAIアカウントがnullでも、それだけをログイン失敗としない。逆にOpenAI認証不要という応答だけを接続成功とは扱わない。

「保存」は推論を開始しない。「テスト送信」はAPI利用料金またはサブスク利用枠を消費し得ることを画面に明示する。現在開いている文書や未保存の内容をテストへ自動添付しない。

設定画面の開閉、認証・モデル取得の非同期完了でDocumentSession、dirty、undo/redo、選択、カーソル、スクロールを壊さない。入力フォーカスが背後のエディタへ漏れないことを日本語IMEを含めて検証する。

## 10. 権限・機密情報・実応答テスト

App Serverは単なる文字生成プロキシとして扱わない。初期の接続確認では、ユーザーの文書フォルダやMCP/Skills/Pluginsを利用しない。機能フラグとsandbox/permission設定を組み合わせ、実際のツール構成を採用版で確認する。

`approval_policy = "never"`は「ツールが無効」ではない。read-onlyも「任意の読み取りが起きない」保証とは区別する。接続確認のためにdanger-full-accessへ緩めない。shell、書込み、外部ツール等を無効化できたことを確認するまで本番のテスト送信を公開しない。設定・監視だけで保証が不足する場合は、プロセスレベルの隔離を追加する。[S3, S5]

API key環境変数の子ツールへの継承を防ぐ。環境変数フィルタの設定名はCodexの版によって差があるため、固定名を前提にしない。App Serverの`generate-json-schema`はRPCプロトコルのschema生成であり、config schemaの取得手段として扱わない。設定互換性の最終判定は、Haneへ実際に同梱するCodex実行ファイルを、生成した設定と`--strict-config`で起動する検証とする。2026-09-27時点の公開Config Referenceでは新しい形式として`shell_environment_policy.filters`、旧形式として`shell_environment_policy.exclude` / `include_only`が記載されているが、公開リファレンスが同梱版の受理範囲と一致するとは仮定せず、同じ設定レイヤーで新旧形式を併用しない。[S3]

新しい形式を採用版がstrict configで受理する場合は、例えば次のように明示的にCustom Providerの秘密を除外する。

```toml
[shell_environment_policy]
ignore_default_excludes = false

[shell_environment_policy.filters]
"HANE_AI_PROVIDER_KEY" = "exclude"
```

採用版が`filters`をstrict configで受理せず、旧形式を受理することを実起動で確認した場合は、例えば次のように設定する。

```toml
[shell_environment_policy]
ignore_default_excludes = false
exclude = ["HANE_AI_PROVIDER_KEY"]
```

設定名がschemaと一致しない場合は起動時に失敗させ、未知の設定を無視したまま接続確認を続けない。設定の存在だけでなく、実プロセスで秘密がshell等の子プロセスへ漏れないことを検証する。MCPや別経路の子プロセスも将来追加時に対象とする。[S3]

ログはランタイム版、操作名、request/thread/turn ID、経過時間、結果コードを中心にする。生のauthUrl、トークン、API key、Authorization、文書内容、Provider応答の生エラー本文を無条件に保存しない。URLは原則HTTPSを要求し、認証情報入りURLを拒否する。ローカル開発用HTTPは明示的に扱う。

## 11. 最小の実装単位

### PR 1: ランタイムと通信

新規hane-ai、同梱版を参照する起動管理、stdio RPC、initialize barrier、状態表示の土台、fake serverテストを実装する。承認要求を受けても許可しない。使用する実行ファイル・スキーマ・配布対象OSを固定する。

### PR 2: 設定・認証・Custom経路

既存設定へのAIカテゴリ追加、ChatGPT管理OAuth、キーの安全保存、Custom設定生成、接続切替を実装する。OAuthとCustomの値が実際のランタイムに届くことを記録できるmock Providerで確認する。

### PR 3: 実応答確認と配布検証

制限した環境の固定テスト入力で応答・中断を確認し、macOS/Windowsの配布物で試す。署名、必要な同梱helper、ライセンス表示、パスに空白/日本語がある環境、終了時のプロセス回収を検証する。AIが使えない状態でも通常のMarkdown編集は可能にする。

この分割は目安。基盤、認証、配布の完了条件を保てるならPR数を増やさない。

## 12. 受入条件

| 分類 | 必須の証拠 |
|---|---|
| 通信 | 分割JSON、文字境界、応答順序逆転、string/integer ID、initialize同時要求、サーバー要求、タイムアウト、異常終了の自動テスト |
| OAuth | 新規ログイン、取消、画面を閉じた後の完了、アプリ再起動、再認証、ログアウト。通常のCodex CLI資格情報への影響がないこと |
| Custom | 保存したBase URLへ送られること、正しい認証ヘッダー、モデルID、キー差替え、削除、401/403/429、非互換Responses、無効TLSの試験 |
| 分離 | ChatGPTトークンのCustom送信なし、別接続への暗黙フォールバックなし、誤ったeffective configを拒否、秘密のログ・設定流出なし |
| 実応答 | 両方式で固定テストの応答を受信し、成功/失敗/中断を区別。モデル一覧取得だけを完了証拠にしない |
| 権限 | 接続確認で文書読取り・変更、shell、外部ツールを実行しないことを採用版で確認 |
| UI | 設定開閉と非同期イベントで文書内容、dirty、undo/redo、selection、scroll、IME/focusに退行なし |
| 配布 | Codex CLI/Node未導入のmacOS・Windows環境で起動、認証、応答確認、終了。必要な依存は実配布物から解決 |

実装PRのcurrent exact headに対して、CI、レビュー、実機結果を記録する。未実施の実機OAuthやGUI検証を成功と扱わない。

## 13. 将来の文書AI機能への接続

後続の要約・校正では、UIが明示的に選択したテキストまたは文書スナップショットをAIサービスへ渡す。`document_id / revision / selection / text`を組にし、保存済みファイルだけを真実としない。

編集は「提案 → 差分プレビュー → ユーザー適用 → Haneの編集コマンド/Undo履歴」の経路を優先する。適用前にrevisionを確認し、生成待ちの間に変更された文書へ古い差分を自動適用しない。この段階でApp Serverからの直接ファイル変更や承認UIを別設計する。

## 14. 残る判断・未検証事項

採用するCodexの正確なリリース番号、OS/CPU別の配布ファイルとhelper、keyringの名前空間分離、接続確認時のツール無効化の具体的な組合せ、実際のCustom接続先・モデルのResponses互換性は、実装開始時に採用版で確定する。本調査はソースと公開仕様の確認であり、これらの実動作保証ではない。

## 15. 代替案と採用しない理由

第3・4・6・7節の判断理由を、代替案との比較として整理する。

| 代替案 | 初期に採用しない理由 |
|---|---|
| Codexの内部Rust crateをHaneへ直接リンクする | Codexの依存関係と更新の影響をHane本体へ持ち込まず、同梱した別プロセスとの通信を境界にするため |
| 外部にインストールされたCodex CLIを必須にする | 配布したHaneだけで認証・接続・応答確認を行えることを完了条件にするため |
| Hane独自のOAuth処理とauth.json読取りを実装する | トークン管理を重複させず、App ServerのアカウントAPIを境界にするため |
| liltoの実装をそのまま移植する | UI・機能構成は参考にするが、秘密の平文保存、認証判定、Custom設定の反映、双方向要求、権限設定はHaneの契約に合わせて実装するため |

## 16. 影響とトレードオフ

同梱した別プロセスを使うことで、Codexの更新・障害とMarkdownエディタ本体の責務を分離する。一方で、配布ファイル、必要なhelper、署名、ライセンス表示、プロセス終了、同梱版とスキーマの整合はHane側で管理する必要がある。単一実行ファイルへの静的リンクを保証する設計ではない。

認証・モデル選択・実応答確認を基盤として独立させるため、後続の要約・校正・編集機能を追加できる。ただし本ADRだけではそれらを実装済みとせず、文書スナップショット、差分確認、revision確認、Undoへの適用は後続の設計・検証対象にする。

安全な秘密保存と接続先の明示を優先し、keyring利用不可、設定競合、非互換Responsesなどを自動フォールバックで隠さない。AIが利用できない場合も、通常のMarkdown編集を妨げない。対応Providerの範囲は第7節、初期スコープは第1節、残る判断は第14節によって限定する。

依存関係・配布の確認は[ADR-0011](0011-dependency-and-license-policy.md)、後続実装の検証とmerge判断は[Commander Policy](../aadw-command-policy.md)を参照する。本ADRの追加を理由に既存の運用規則を置き換えない。

## 参照資料

以下は元設計書が2026-09-25確認として記録した参照資料である。今回のADR化では再検証していない。公開ドキュメントは変化するため、実装時は同梱版のスキーマと照合する。

- R1 Hane Cargo.toml: `https://github.com/hide212131/hane/blob/96bdb68db19847d1df12ea55a7830cda512b4fd6/Cargo.toml`
- R2 Hane UI: `https://github.com/hide212131/hane/blob/96bdb68db19847d1df12ea55a7830cda512b4fd6/crates/ui/src/lib.rs` および同SHAの `crates/ui/src/view.rs`（先頭230行確認）
- R3 Hane settings store: `https://github.com/hide212131/hane/blob/96bdb68db19847d1df12ea55a7830cda512b4fd6/crates/session/src/store.rs`（先頭220行確認）
- R4 lilto settings UI: `https://github.com/hide212131/lilto/blob/bd75ef91a874a7a4cf08f71e20650e515a512ed7/src/renderer/components/settings-modal.ts`（先頭260行確認）
- R5 lilto Provider設定: `https://github.com/hide212131/lilto/blob/bd75ef91a874a7a4cf08f71e20650e515a512ed7/src/main/provider-settings.ts`
- R6 lilto App Server client: `https://github.com/hide212131/lilto/blob/bd75ef91a874a7a4cf08f71e20650e515a512ed7/src/main/codex-app-server-client.ts`
- R7 lilto auth: `https://github.com/hide212131/lilto/blob/bd75ef91a874a7a4cf08f71e20650e515a512ed7/src/main/auth-service.ts`
- R8 lilto runtime: `https://github.com/hide212131/lilto/blob/bd75ef91a874a7a4cf08f71e20650e515a512ed7/src/main/agent-sdk.ts`（1–475行、550–630行、625–890行確認）および同SHAの `src/main/agent-sdk-config.ts`
- S1 OpenAI App Server: `https://developers.openai.com/codex/app-server/` → `https://learn.chatgpt.com/docs/app-server`
- S2 OpenAI Authentication: `https://developers.openai.com/codex/auth/` → `https://learn.chatgpt.com/docs/auth`
- S3 OpenAI Configuration Reference: `https://developers.openai.com/codex/config-reference/` → `https://learn.chatgpt.com/docs/config-file/config-reference`
- S4 OpenAI Advanced Configuration: `https://developers.openai.com/codex/config-advanced/` → `https://learn.chatgpt.com/docs/config-file/config-advanced`
- S5 OpenAI Config Basics: `https://developers.openai.com/codex/config-basic/` → `https://learn.chatgpt.com/docs/config-file/config-basic`
