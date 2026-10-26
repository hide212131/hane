# Issue #623 実装順2: 更新HTTPS CONNECTとcurl

## 目的と基準

HTTP proxyが必要な環境で、更新確認・署名付きcatalog・ZIP stagingとAI runtime archive取得が共通network routeに従うようにする。本文書は実装順2の仕様であり、末尾の「実装結果」に実装した範囲と未検証事項を記録する。

- Issue: #623。採用仕様の正本: `docs/design/http-proxy.md` §4、§5、§6.1、§9 P3/P4/P5/P6。
- 基準main: `b3dd52e22061bc79305fbf80082b4821a887dd94`（共通core #625 merge済み）。
- #620は設定画面の機械分離。この段階は設定画面を変更せず、そのGUI受入を着手条件にしない。
- IssueのP1〜P10全体は本PRのmergeだけで完了しない。

## 現sourceと変更責務

| 既存の入口 | 今回の責務 |
|---|---|
| `crates/network/src/` | 既存settings/environment/resolve/credential契約を再利用。必要なら不変snapshotの注入用の小さな型を追加する。process環境・DNS・OS秘密ストアのI/Oをpure coreへ持ち込まない |
| `crates/app/src/https_transport.rs` | `HttpsTransport::get` のorigin DNS/TCP→rustls経路にproxy TCP/CONNECTを追加。`LazyHttpsTransport`のOnceLockはTLS設定だけをcacheする |
| `crates/update/src/download.rs`、`error.rs`、直接のerror変換callsite | `ProxyConfiguration` / `ProxyConnect` / `ProxyAuthentication`を分類し、日本語表示へつなぐ。既存download/検証policyは維持 |
| `crates/ai/src/runtime_install.rs` | `CurlArchiveDownloader`へsnapshot注入、共通routeからcurl commandを構築。実curl subprocessとcancel/pipe管理を扱う |
| `crates/ai/src/service.rs`、`crates/app/src/main.rs`と必要なconstructor callsite | 起動時proxy環境の取り込みと更新/runtime取得への注入。環境captureは入口だけ、通信は既存の明示操作で開始 |
| 各crateのCargo依存と直接関連tests、`update_diagnostics.rs` | `hane-network`等の必要最小限の依存、fixtureとsecret redactionの回帰。無関係な整形/再設計はしない |

`HttpsTransport::new`はOS trust rootsを読み、`get`は現在originの443へDNS/TCP接続する。`resolve_within`は同時resolver thread数を制限し、`DeadlineSocket`はI/O毎に残り時間を再適用する。この仕組みをproxyの任意portへ拡張する。

`CurlArchiveDownloader`は現在unit structで、serviceのruntime install処理から生成される。allowlist確認→curl一hop→header解析→手動redirectを繰り返し、後続でarchive hash/inventoryを検証する。`run_cancelable`は現在stdinをnullにするため、secret configのstdin注入と閉鎖を明示的に管理する必要がある。

## 注入とsnapshot

1. `NetworkSettings`、起動時にcaptureした`ProxyEnvironment`、credential取得契約を一組の不変入力として更新操作とruntime取得へ渡す。defaultはEnvironment。設定なしは従来どおりDirect。テストはmapを注入し、process環境を変更しない。
2. constructor/APIからDirect、Environment、Manualを注入できるようにする。manual Basicのcredential storeは注入し、取得不能を構造化errorにする。OS秘密backend/設定保存/UIは後続なので、未実装storeを「認証なし」として成功させない。
3. アプリ入口で取り込んだproxy環境を両経路で共有し、一つの操作中に再読込しない。毎hopで同じsnapshotから宛先別routeを解決する。今後の設定変更/revision APIへ接続可能な形にするが、今回そのUIやAI再接続を追加しない。
4. 初回に選んだrouteをTLS OnceLockへ永久固定しない。snapshotのDebug/通常state/ログへsecretを含めない。

## 更新CONNECT

- callerのHTTPS URL/host allowlist検証後に共通routeを解決する。Directは既存経路、proxyはproxy host/portだけをDNS/TCP接続し、origin DNSをローカルで行わない。
- 同じdeadline付きsocketへ`CONNECT <origin>:443 HTTP/1.1`とHostを書き、認証ありの場合だけProxy-Authorizationを付ける。userinfoをrequest line/Hostへ入れない。Basicの入力を既存coreに従って検証し、header injectionを拒否する。
- HTTP/1.0または1.1の3桁status、CRLF、head合計16KiB、line最大1KiB、最大4個の1xxを厳密に確認。101、200以外の最終status、EOF、過大/不正head、想定外body framingを拒否。1byte readまたは余剰保持によりTLS tunnel byteを捨てない。失敗本文を読み続けない。
- CONNECT後、origin名のSNI/hostnameとOS rootsでrustls TLSを確立。origin GETにProxy-Authorizationを付けず、proxy responseをorigin responseに見立てない。
- DNS/TCP/CONNECT write/read/TLS/GET/headで一つの`Request.deadline_ms`を共有。多address試行や1xxで予算を再開始しない。無応答はTimeout、407はProxyAuthentication、403/502等はProxyConnect、origin証明書不正はTls。設定/secret取得失敗はProxyConfiguration。失敗後Directへretryしない。
- redirectは既存callerへ返し、各hopでURL制約とrouteを再確認する。最大redirect、HTTPS、GitHub allowlist、catalog署名、ZIP hash/size、cancel、part cleanup、helper権限境界を維持。
- error/diagnosticには分類と固定説明だけを出し、proxyの任意header/body、資格情報、環境URLを埋め込まない。既存redactorを追加の防御として回帰検証する。

## curl adapter

- 各hopでallowlist確認後に共通routeを解決する。curl自身へproxy変数の優先順やNO_PROXY判定を再委任しない。
- 最初のoptionを`--disable`とし、proxy変数の大小文字を子環境から除去。証明書/PATH等の既存環境は維持。`--noproxy`は空指定。Directはproxyを明示無効化し、HttpProxyは解決したendpointを明示。
- Basic資格情報はargv、shell、平文一時ファイルへ出さず、`--config -`のstdinからcurlのconfig構文に正しくescapeして渡す。CR/LF/NUL拒否、引用符/backslash/colonを含む合成値の回帰を追加。secretを含むcommand/configをDebugしない。
- stdin書込み後は閉じる。childがstdinを読まない/早期終了/書込み失敗/取消の場合も無制限にblockせず、子終了確認とpipe解放を行う。tarなど既存のstdin-null実行は維持。
- `--proto =https`、手動redirect、`--suppress-connect-headers`、archive size/hash/inventory、connect/stall timeoutを維持。`-k`、`--proxy-insecure`、`-L`を追加しない。
- CONNECT status/407、origin HTTP status、curl終了codeを区別する必要最小限のmetadataを収集。stderr/header dump/config stdinをそのまま診断へ保存しない。任意proxy headerを通常ファイルへ漏らさない。

## 受入証拠

| 条件 | 必要な試験 |
|---|---|
| P3 | 両OSで専用proxy+TLS fixtureによる更新API→署名済catalog/署名→ZIP staging。proxy CONNECT観測とorigin requestを記録し、Direct経路を遮断/拒否しても成功。Directの既存試験も維持 |
| P4 | 分割status/head、1xx上限/101、過大head/line、bare LF、不正status/EOF/framing、407/403/502、DNS/TCP/CONNECT無応答、TLS失敗、bypass。proxyだけのDNS/接続、Direct retryなし、origin認証非漏えい、単一deadlineをassert |
| P5 | 許可/禁止redirect、proxy→bypassなどhop別再解決、許可外hostへのCONNECT未実行、署名/hash/sizeの失敗、cancelとpart cleanupを実経路fixtureで確認 |
| P6 | 両OSの実curl+proxy/TLS fixtureで無認証/Basic/Direct/bypass、競合親環境、合成curlrc、407とorigin statusの区別、secret非argv、stdin EOF、子早期終了/取消/pipe cleanup、runtime archive検証を確認 |

fixtureは実サービス・実ユーザーsecretを使わない。TLS用test root/署名test key、DNS/接続先/portの注入はtest専用の境界へ置き、production allowlist/port/TLS検証を緩めない。curlのDirect fixtureにephemeral port mappingが必要な場合もtest-only seamを使い、同じproduction command構築/route判定を通す。mock command/環境値だけを実通信の証拠にしない。

current headでworkspace all-features tests/Clippyと通常default Release build、両OS required CIを記録する。real curl fixtureがOSツール不足等でskipされた場合は未検証として取り戻し、P3/P6をpassにしない。settings GUI/AI runtime通信はこの段階の試験ではなく、後続段階で必要。

## 実装結果

worker環境ではshell・build・testを実行できないため、以下はすべて**コードを書いただけで未実行**である。P3〜P6はtrusted verifyのCI結果でpassを確認するまで未達として扱う。

| 項目 | 実装 | 主な場所 |
|---|---|---|
| 注入 | `NetworkContext`（settings・起動時captureした`ProxyEnvironment`・credential storeの不変snapshot）。既定はEnvironment、store未実装は`Unavailable`としてerror。`main.rs`の`network_context()`が`OnceLock`で一度だけ環境を取り込み、更新`LazyHttpsTransport::new`と`AiService::spawn_with_network`が同じsnapshotを共有 | `crates/network/src/context.rs`、`crates/app/src/main.rs` |
| Basic検証 | `BasicCredentials::check_for_basic`（空・`:`を含むuser、制御文字を拒否） | `crates/network/src/credential.rs` |
| 更新CONNECT | 要求ごとに`route`を再解決。proxyはhost/portだけをDNS/TCP、`CONNECT origin:443`、1byteずつ読む厳格な応答検証（HTTP/1.0/1.1・3桁・CRLF・計16KiB・行1KiB・1xx最大4・101拒否・200のみ成功）、origin名のSNIでTLS。単一の`deadline_ms`を共有 | `crates/app/src/https_transport.rs` |
| error分類 | `TransportFailure`/`ErrorCode`に`ProxyConfiguration`/`ProxyConnect`/`ProxyAuthentication`を追加し、UIの日本語表示へ接続 | `crates/update/src/{download,error}.rs`、`crates/ui/src/view/app_update.rs` |
| curl | `CurlArchiveDownloader::new(NetworkContext)`。毎hop `check_allowed_url`後にroute解決、先頭`--disable`、`--noproxy ""`、Directは`--proxy ""`、proxyは`--proxy <endpoint>`、Basicは`--proxy-basic --config -`でstdinへ`proxy-user`を引用符/backslash escapeして渡す。親環境のproxy変数は大小文字を問わず除去。`-w`で`%{http_connect}`を取得し`DownloadFailure`（`ProxyAuthentication`/`ProxyConnect`/`OriginStatus`/`CurlExit`等、秘密なし）を記録 | `crates/ai/src/runtime_install.rs` |
| stdin/pipe | stdinはthreadで書き込み後に閉鎖。子の早期終了・取消・書込失敗でblockせず、子をreapしてpipeを解放 | 同上 `run_cancelable_io` |
| test seam | `hane-ai`の`test-support` feature（`with_test_seams`: CA・`--connect-to`・親環境の代替）、`HttpsTransport`のlookup/direct port。production経路・allowlist・TLS検証は変更しない | 同上、`crates/app/src/test_tls.rs` |

試験: P4（応答検証・分類・単一deadline・Direct retryなし・origin認証非漏えい・hop別再解決・設定error）は`https_transport.rs`のfixture試験、P6は`proxy_curl_tests.rs`の実curl試験（curl不在はskipではなく失敗）と`runtime_install.rs`の単体試験。

未実装・未検証:

- P3/P5の`Fetcher`経由の横断試験を`crates/app/src/proxy_update_tests.rs`に追加した（本番`HttpsTransport`＋`Fetcher`、実proxy＋TLS fixture、`hane_update::testing`の署名key/policy/ZIP・catalog builder）。内容は、API→catalog/署名検証→ZIPのsize/hash確認付きstaging成功（CONNECT 5回・origin request記録・proxy認証がoriginへ届かないこと・origin名が解決不能でも成功）、許可redirect、許可外redirect（別host/http/port/userinfo/類似host）で許可外へCONNECTしないこと、不正署名、hash/size不一致、proxy 407、proxy経由転送中のcancelとpart cleanup。ただしこれらは**コードを書いただけで未実行**であり、trusted verifyのCI結果で確認するまで未達として扱う。test用境界は`https_transport.rs`の`cfg(test)`な`HttpsTransport::with_test_seams`（trust rootと名前表の注入のみ）で、本番のroute/CONNECT/TLS検証/allowlistは変更していない。
- 上記試験は`Fetcher`と`HttpsTransport`の結合に限る。ZIPの展開検査（`scan_archive`/`stage_archive`）はproxy経路の試験に含めていない。
- macOSの実curl（Secure Transport等）で`--cacert`が信頼anchorとして働くか、WindowsのSchannelで`--ssl-revoke-best-effort`が必要十分かは未確認。
- workspace all-features tests/Clippy、default Release build、両OS required CIは未実行。

## 対象外とworker制約

設定保存/移行、OS秘密backend、設定UI、AI app-server child、revision付き再接続、restart ticketのsecret継承は後続。HTTPS/SOCKS proxy/PAC/NTLM等も採用設計どおり対象外。`.github/`、AGENTS/CLAUDE、Commander Policy、trusted skill、release設定など保護パスを変更しない。

通常workerは指定範囲の製品コードと直接関連testsだけを変更し、shell/git/gh/test/build/lint/format/commit/push、次工程の起動、merge、Issue closeを行わない。必要な検証を未実行として報告し、trusted finalizerと独立verifyへ返す。
