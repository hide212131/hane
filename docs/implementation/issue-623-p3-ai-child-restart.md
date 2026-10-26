# Issue #623 実装順3: AI子プロセスと更新再起動へのproxy継承

## 状態

この文書は実装担当向けの準備仕様です。製品コード・テストは未変更です。

- 対象Issue: [#623](https://github.com/hide212131/hane/issues/623)
- 基準main: `746363579ff0ed2713e39d7b14c6d09bf38c2e8d`
- 採用設計: `docs/design/http-proxy.md` の §§4.2、6.2、7、8、9（P7〜P9）
- 先行段階: 共通network core #625と更新CONNECT/curl #628はmainへ反映済み

## 今回の範囲

実装順3として、Haneが起動するpinned AI runtimeと更新後の再起動childへ、同一のnetwork設定を安全に適用します。対象はP7・P8のAI側処理とP9の更新/再起動継承です。設定UI（P1・P10の利用者操作）は実装順4で行います。このPRだけでIssue #623を完了扱いしません。

参照すべき既存入口:

- `crates/network/src/child_env.rs`: `ChildProxyPlan` による環境の正規化、親からのproxy変数除去、loopback除外
- `crates/network/src/context.rs`、`settings.rs`、`environment.rs`、`credential.rs`: immutableな設定/環境snapshotとcredential契約
- `crates/ai/src/runtime.rs`: `RuntimeConfig::spawn_child` の既存 `env_clear` / allowlist
- `crates/ai/src/service.rs`、`connect.rs`: AI childの起動・停止・再接続・世代管理
- `crates/update/src/state_root.rs`: `CHILD_ENV_ALLOWLIST` とtransaction内 `child-env.json`、restart ticket
- `crates/app/src/update_restart.rs`: ticket発行・更新childとのhandoff
- `crates/ai/src/secrets.rs`: 既存OS秘密ストア実装の境界

開始時にcurrent mainとこれらの実コードを読み直してください。上記の名前や構造がmainで変わっていたら、現行実装に合わせてください。

## 実装条件

### AI runtime child（P7）

1. `NetworkSettings` と起動時に取得した `ProxyEnvironment` から、接続操作ごとに一貫したimmutable network snapshotを作ります。実行中にprocess環境を読み直して設定を混ぜません。
2. `RuntimeConfig::spawn_child` は既存の `env_clear`、`CODEX_HOME`、専用working directory、既存allowlistを維持し、snapshotの `ChildProxyPlan` だけからproxy環境を作ります。親環境から引き継ぐproxy変数は大文字小文字を区別せず全て除去し、Directでは残しません。
3. Environment / Manual / Directの各mode、HTTP/HTTPS、Basic認証、除外先、loopbackの扱いは共通network coreに従います。Basic credentialsをDebug、RPC、argv、通常ログ、診断へ出しません。credential取得・設定検証の失敗は明示的なエラーにし、未認証またはDirectへsilent fallbackしません。
4. Haneが起動したpinned runtimeのネットワークだけを対象にします。外部ブラウザ・外部エディタの通信は変更しません。loopback/custom-provider/OAuth callbackは設計どおりproxyを迂回します。
5. 環境変数を組み立てたことだけをP7の通信成功と扱いません。pinned runtimeの実通信観測と、fake child等の補助試験を区別してPRへ記録します。

### 設定変更・再接続（P8のbackend部分）

1. 設定保存成功でnetwork revisionを進め、以後に開始する操作は新revisionだけを使います。進行中操作は開始時snapshotのまま完了またはcancelし、proxy設定やcredentialsを途中で混ぜません。
2. AI childのstop/reconfigure、operation generationにnetwork revisionを含めます。再接続時は旧childの停止完了を確認してから新childを起動し、旧childの遅延応答を新revisionの成功・本文として適用しません。
3. 既存のcancel/停止完了確認を維持します。proxy設定変更だけで自動ログイン、runtime取得、本文変換を始めません。UIからのfocused reconnect確認は設定UI段階で行い、このPRでは未実施なら未実施と記録します。

### 更新・再起動継承（P9）

1. 更新helper・新versionは、通常設定から同じnetwork modeと手動設定を再ロードし、更新後も同じrouteを使います。既存の署名、hash、transaction、限定health、rollbackとstartup境界を弱めません。
2. `child-env.json`、restart ticket、journal、health引数、通常state、logには、proxy URL userinfo、username/password、Proxy-Authorization値を保存しません。URL query等の任意値もログへ出しません。proxy endpointと除外先を保存する場合は、userinfoを必ず分離します。
3. Environment modeの起動時proxyにBasic userinfoがある場合、認証部分はtransactionに束縛した一時OS秘密ストア参照へ分離します。通常ファイルには非秘密の不透明参照だけを保存し、対象transactionの限定child起動時にだけ復元します。成功・中止・rollback・recoveryの全終端経路で参照を一度だけ消費し、不要な秘密残骸を残しません。Manual modeは保存済みOS credential referenceを使い、秘密本文をticketへ複製しません。
4. 一時秘密の保存・取得・復元・cleanupに失敗したら、認証なしproxyまたはDirectへ落としません。旧アプリが終了する前に安全にabortできる段階ではhandoffを中止して旧版を維持します。それ以外は既存transactionのfailure/recovery契約に従い、秘密不足を明示します。
5. Windows Credential ManagerとmacOS Keychainの分岐を含め、ticket/child environmentのschema検証、transaction/install/root結合、one-time claim、cleanup順序をテストします。資格情報の実値をテスト出力へ表示しません。

## 受入証拠

- Route mode別のchild environment、競合する大文字小文字のproxy変数除去、loopback bypass、credential取得失敗を確認するtests。
- network revisionの保存、旧child停止、遅延応答/旧generation破棄、新規操作が新snapshotを使うtests。
- restart ticketと `child-env.json` の秘密非含有、手動proxyのcredential reference再ロード、Environment userinfoの一時参照、失敗時abort、成功/中止/rollback/recoveryのcleanupを確認するtests。fixtureに使うダミー秘密文字列がstate/ticket/journal/log/argvへ現れないことを検索・assertします。
- `cargo test --workspace --all-features`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、macOSとWindowsの通常Release buildを対象headで確認します。
- P7は両OSのpinned runtimeとテストproxyの実通信観測を必要とします。実runtimeの認証/API経路、Custom Provider、モデル取得、本文変換、必要なWebSocket、およびloopback bypassを、fake childの試験だけでpassにしません。実アカウントが必要で未実施の経路はunknownと記録します。
- P8の設定変更→再接続のfocused GUI、P9の両OS更新/restart GUI・fixtureは、対象headごとに実施結果を記録します。未実施項目をpassとしません。

## 対象外

- ネットワーク設定UIと通常設定の入力/保存画面（実装順4）
- 更新HTTPS CONNECT、redirect、curl runtime downloader（実装順2、#628）
- 外部ブラウザ/外部エディタのproxy変更、proxy HTTPS/SOCKS/PAC、TLS検証緩和
- `.github/`、`AGENTS.md`、`CLAUDE.md`、AADW/Commander Policy等の運用基盤
- Issue #623全体のcloseまたはリリース宣言
