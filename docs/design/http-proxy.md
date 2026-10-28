# HTTPプロキシ対応の実装設計

## 1. 目的と状態

社内ネットワークなど直接インターネットへ接続できない環境で、Haneのネットワーククライアント機能をHTTPプロキシ経由で利用できるようにする。アプリ更新だけに限定せず、AIランタイム取得、ChatGPT接続、Custom Providerへの通信も対象とする。

本書は実装担当への設計であり、製品実装・通信試験は未実施。設計PRの作成・mergeは機能の完成を意味しない。仕事の範囲と受入条件は [Issue #623](https://github.com/hide212131/hane/issues/623)、具体仕様は本書、実装結果は後続の実装PRを正本とする。

調査基準: main `4f11a6675381838a33653ba0fb0ad62b364f34c9`（2026-10-26）。実装開始時にはcurrent main、対象Issue、関連PRと実行中runを再確認する。

2026-10-28の追加設計: [Issue #649](https://github.com/hide212131/hane/issues/649)の[更新通知設計](application-update-notifications.md)は、本体更新metadataだけを起動後・使用中1時間ごとに確認する。下記P10の起動時無通信にはこの確認だけを例外として追加する。自動確認も既存のnetwork snapshotを通し、AI/runtime/Skillsや設定の表示・保存を新しい通信トリガーにしない。通知機能の製品実装は未実施。

## 2. 現在の通信経路

| 機能 | 現在の入口 | 現状と変更点 |
|---|---|---|
| 更新確認、catalog・署名・ZIP取得 | `crates/app/src/https_transport.rs` の `LazyHttpsTransport` / `HttpsTransport`、`crates/app/src/app_update.rs` | TCPを直接接続しrustlsでTLS検証する。プロキシ解決とCONNECTを追加する |
| 更新のURL、redirect、サイズ・時間制限、署名検証 | `crates/update/src/download.rs`、`trust.rs` | transportから独立した既存契約を維持する |
| AIランタイム取得 | `crates/ai/src/runtime_install.rs` の `CurlArchiveDownloader` | curlを各hopで起動。環境継承に任せたプロキシ選択を共通設定へ接続する |
| ChatGPT、Custom Provider、モデル取得、AI本文変換 | `crates/ai/src/runtime.rs` の `RuntimeConfig::spawn_child`、`connect.rs`、`service.rs` | pinned Codex app-serverのネットワーク。環境allowlistにproxyがあるがHane設定はない。子プロセスへ明示的に適用する |
| 更新後の子起動 | `crates/update/src/state_root.rs`、`health.rs`、`crates/app/src/update_restart.rs` | proxy環境も限定継承する。新しい認証情報を平文のtransaction記録へ保存しない |
| ブラウザによるOAuth認可、Markdownリンク、外部エディタ | `crates/ai/src/service.rs`、UIの既存OS起動経路 | 外部アプリの通信設定はOS・ブラウザが担当。Haneの設定で書き換えない |

製品コードの棚卸し対象は `crates/`。vendor、CI、開発用ダウンロード、AADW workerのネットワークは今回の製品設定の対象外。Hane側のOAuth開始・完了確認は対象、外部ブラウザ内部とlocalhostコールバックは上表の境界で扱う。

## 3. 対応範囲と利用者向け設定

macOS / Windowsに共通の「ネットワーク」設定を追加する。プロキシはアプリ全体の設定とし、work folderやAI provider単位に複製しない。

| 方式 | 動作 |
|---|---|
| 環境変数を使う（初期値） | 起動時のproxy環境から決定する。設定がなければ直接接続 |
| 直接接続 | proxy環境を無視する。子プロセスにもproxy環境を残さない |
| 手動HTTPプロキシ | `http://host:port` と除外先を設定する。環境変数より優先 |

手動はDNS名、IPv4、角括弧付きIPv6を受け入れる。port省略は80。認証は「なし / Basic」、usernameとpasswordは別入力にし、パスワードはマスクする。Basicはプロキシまで暗号化されない旨を認証方式の横で説明する。proxy URL欄のuserinfo、path（空または `/` 以外）、query、fragment、制御文字、空host、無効portは保存時に拒否する。プロキシ接続自体のHTTPS、SOCKS、PAC/WPAD、OSプロキシ設定の自動取得、NTLM/Kerberos/Digest認証は今回対象外であり、指定された場合は未対応と表示する。これらの実装を基本のHTTP CONNECT対応の開始条件にはしない。

保存は構文と秘密保存の確認のみでネットワークを発生させない。設定を開く・起動するだけで更新確認やruntime取得を始めない。各機能の既存の明示操作から接続する。失敗時に設定画面へ戻れる案内を出す。

## 4. 共通の設定・経路解決契約

### 4.1 所有と保存

新しい小さな `hane-network` crateを共通の設定型・純粋な検証/解決器・子環境構築に限定する。更新の署名・AI接続状態・GUI状態は持たせない。workspace登録と依存追加は後続実装の範囲とする。

- `ProxyMode = Environment | Direct | Manual`。
- `NetworkSettings`: schema、mode、manual endpoint、bypass、認証方式、credential参照。秘密値を含めない。
- `ProxyEnvironment`: 起動時のproxy変数だけを取り込む不変値。Debugで値を出さない。
- `ResolvedRoute = Direct | HttpProxy { endpoint, credentials }`。秘密は値を隠す型で保持し、Serializeを実装しない。
- `resolve(settings, environment, destination)` はI/Oなしで経路または構造化エラーを返す。URLを正規にparseし、文字列prefixでhost判定しない。

通常設定は `crates/session/src/store.rs` の `Settings` / `FileStateStore` の既存 `settings.conf` に `network_*` キーとして追加する。未設定はEnvironmentで既存ユーザーを移行する。既存キーとdefault folder等を保持する。未知のmode、壊れたendpoint、未知schemaは「設定エラー」として保持・表示し、黙ってDirectへ落とさない。現行のfallback parserをそのまま新しいmodeへ流用しない。

手動の認証情報はmacOS Keychain / Windows Credential Managerへ専用service名 `hane.network.proxy` と不透明な参照IDで保存する。AIのprovider keyとはnamespaceを分ける。既存 `crates/ai/src/secrets.rs` のOS backendの扱いを参考にするが、ネットワーク層からAIへ依存させない。秘密ストアへの保存成功→設定のatomic保存→旧参照削除の順にし、失敗時は旧設定を有効なまま保持する。未参照の新規秘密は回収可能にする。秘密取得不能・保存不能を認証なし接続に読み替えない。

### 4.2 環境変数の解釈

OS環境を直接読むのは入口だけとし、テストは値のmapを注入する。macOSでFinderから起動した場合、shellのexportが自動で渡るとは説明しない。手動設定を案内する。

- HTTPS宛先: `https_proxy` → `HTTPS_PROXY` → `all_proxy` → `ALL_PROXY`。
- HTTP宛先: `http_proxy` → `all_proxy` → `ALL_PROXY`。大文字のみの `HTTP_PROXY` はCGI由来の誤設定を避けるためmacOSでは採用しない。
- 同一名の大小文字が共存できるOSでは小文字を優先。Windowsは環境名が大小文字を区別しないためOSの単一値として扱い、HTTP_PROXYも同一名になる。
- 未定義または空文字は未設定。非空の優先変数が壊れていればエラーとし、下位変数や直接接続へ逃がさない。
- `no_proxy` → `NO_PROXY` は選択された除外一覧。手動modeは手動の除外一覧だけを使う。
- 環境proxyも `http://` のみ。userinfoのBasic資格情報は受け入れるがURL decodeは一回だけとし、CR/LF/NULを拒否する。値を設定ファイルへコピーしない。

環境値の読み替えをcurlやapp-serverへ二重に任せない。下記adapterはこの優先順位へ正規化する。証明書用環境変数はproxyとは別の既存契約であり、proxy切替のために一括削除しない。

### 4.3 除外先

comma区切りで前後空白と空要素を除去。`*` は全宛先をDirectにする。DNS名は大小文字を無視し末尾の `.` を正規化、`example.com` と `.example.com` は自身とドット境界のsubdomainに一致、`badexample.com` には一致しない。任意位置のwildcard、CIDR、URL形式、port付きDNSは拒否し、IPv4/IPv6リテラルは正規化後の完全一致とする。DNS解決結果を使ったbypassはしない。

`localhost`、`127.0.0.0/8`、`::1` のリテラル宛先は常にDirect（localhostのsubdomainは含めない）。これはCustom Providerの既存loopback HTTP許可とOAuth callbackを守るためであり、他の宛先をHTTPへ降格する許可ではない。無効な除外一覧は接続前エラー。除外に一致した要求は、その宛先で使わないproxy endpoint/資格情報を取得せずDirectにできる。除外一覧自体を壊して全通信Directにしない。

## 5. 更新HTTPS transport

`hane_update::download::Transport` とURL/redirect policyを維持し、`HttpsTransport` に経路解決の入力を渡す。`LazyHttpsTransport` のOnceLockはTLS設定のcacheだけとし、初回のproxy選択を永久に固定しない。

HTTPプロキシ経由のHTTPS GETは以下の順序とする。

1. 既存の宛先URL検証後、宛先ごとにrouteを解決する。
2. Directなら既存経路。HttpProxyなら**proxy hostだけ**をDNS解決しproxy portへTCP接続する。宛先hostのDNSはプロキシへ委ねる。
3. 同じdeadline付きsocketへ `CONNECT <destination-host>:443 HTTP/1.1` と対応するHostを書き込む。認証ありの場合だけCONNECTに `Proxy-Authorization: Basic ...` を付ける。
4. CONNECT responseを厳密に読み、200だけを成功とする。HTTP/1.0と1.1の3桁status、CRLF、head合計16KiB、line最大1KiB、最大4個の1xx、EOFを検証する。101、過大・不正head、想定外body framingは拒否する。1 byteずつ読むか余剰byteを保持し、トンネルのbyteを破棄しない。失敗responseの本文は読み続けない。
5. トンネル上で**宛先名**のSNI/hostnameとOS trust rootsを使ってrustls TLSを確立し、既存GETを送る。proxy hostを宛先証明書名にしない。
6. origin GETにはProxy-Authorizationを付けない。CONNECTのstatus/headerをorigin responseとして返さない。redirectは既存callerへ返し、次hopも宛先検証とroute解決をやり直す。

DNS待ち、TCP、CONNECT write/read、TLS、GET、head受信は一つの `Request.deadline_ms` の残時間を共有する。CONNECTで新しい総予算を始めない。既存のresolver in-flight制限をproxyの任意portにも適用する。bodyの時間/サイズ制限、cancel、part cleanupは既存callerのまま守る。プロキシ失敗時にDirect retryしない。

`TransportFailure` にProxyConfiguration / ProxyConnect / ProxyAuthenticationを追加し、更新側のerror変換を網羅する。407はProxyAuthentication、403/502等はProxyConnect、無応答はTimeout、宛先証明書不正はTlsとする。通常のorigin 401/403やZIP検証失敗と混同しない。表示文と構造化ログを分け、proxyのresponse本文・任意headerを表示しない。

GitHub host allowlist、最大5redirect、HTTPSのみ、catalog署名・ZIP hash/サイズ検証、更新適用/helperの権限境界は変更しない。proxy endpointは接続仲介先として別検証し、配信host allowlistへ追加しない。

## 6. AIランタイム取得とAI通信

### 6.1 curl adapter

`CurlArchiveDownloader` へ同じnetwork snapshotを注入し、各redirect hopで解決する。curlは維持し、更新用と別のHTTP clientへの大規模移行を混ぜない。

- proxy変数の大小文字を全て子環境から除去する。Directではproxy無効、HttpProxyでは共通解決済みのproxyを明示する。curl独自のNO_PROXYが再評価して経路を変えないよう `--noproxy` を空指定する。
- ユーザーのcurlrcで経路・TLS・redirectが上書きされないよう最初のoptionを `--disable` にする。
- 無認証endpointは `--proxy` で渡せる。資格情報はargvや一時ファイルへ書かず、`--config -` のstdinから適切にescapeして渡す。制御文字は事前拒否、shellを介さない。stdinは書込み後に閉じ、cancel時は子終了確認とpipe解放を守る。
- `--proto =https`、手動redirect、allowlist、`--suppress-connect-headers`、archive size/hash/inventory検証、既存connect/stall timeoutを維持する。`-k` / `--proxy-insecure` / `-L` は追加しない。
- curl終了code、CONNECT 407、origin statusを区別できるmetadataだけを収集する。stderr、header dump、config stdinをそのまま診断へ保存しない。

### 6.2 pinned app-server adapter

`RuntimeConfig`の既存env_clear/allowlistと専用CODEX_HOME/cwdを維持し、proxy変数はfiltered parentからの素通しを止め、network snapshotからのみ生成する。Directはproxy変数を除去。Environment/Manualは正規化したHTTP/HTTPSのproxy値と除外一覧を渡し、対立するALL_PROXY等を残さない。Basicの子環境値をDebug、RPC、コマンド表示へ出さない。OSの同一利用者から子環境が観測され得る点は、親環境に秘密をexportする場合と同じ境界とする。

環境を渡せたことは通信の成功証拠ではない。採用中 `assets/ai/codex-runtime-lock.json` の各OSのpinned runtimeについて、ChatGPTの認証/API・モデル取得、Custom Provider、HTTP loopback、HTTPS/WebSocket利用経路でproxyとbypassが実際に成立することを確認する。共通仕様のdomain境界、loopback、大小文字優先、Basicの挙動も実通信で一致させる。runtimeの解釈差を許容してsilent Directにしない。

pinned runtimeが必要な経路を扱えない場合は、その経路を未対応として完了扱いにせず、既存runtime設定での解決または対応版pin変更を別の明示差分として提案する。providerを勝手に変えず、ユーザー所有Codex設定も変更しない。外部ブラウザの通信設定は第2節の境界を維持する。

## 7. 設定変更、失敗表示、更新後の再起動

保存成功後にnetwork revisionを増やし、新規操作は新snapshotを使う。進行中の更新チェック/ダウンロード/runtime取得は開始時snapshotで完了またはcancelし、途中でproxyや資格情報を混ぜない。設定が変わったときは進行中操作へ「次回接続から適用」を表示する。

AI childは起動時環境が固定のため、既存のstop/reconfigureとgeneration guardへnetwork revisionを含める。進行中の本文変換・ログイン中に設定を変える場合は利用者へ再接続が必要と表示し、既存のcancel/停止完了確認後に新childを起動する。旧childの遅延応答を新revisionの成功として適用しない。設定変更だけで自動ログイン・runtime取得を開始しない。

手動設定はstate rootの設定とOS秘密ストアから再ロードし、restart ticket、journal、health引数、更新transactionの環境記録に展開しない。Environment方式でuserinfo付きproxyを継承する場合も `state_root.rs` の現行平文environment保存へそのまま流さない。資格情報部分を一時OS秘密ストア参照へ分離し、同じinstall/transactionの限定child起動時だけ復元、成功/中止後に参照を削除する。環境URL自体もログへ出さない。復元不能なら認証情報不足を表示し、Directで継続しない。既存の証明書・PATH等の限定継承とstartup healthを維持する。

表示は「プロキシ設定が不正」「プロキシへ接続できません」「プロキシ認証が必要です」「接続がタイムアウトしました」「接続先の証明書を検証できません」を分ける。既存 `crates/app/src/update_diagnostics.rs` のredactionへ新型・userinfo/password/Proxy-Authorization/curl出力を加えて回帰検証する。秘密をredactorだけに任せず、生成時点で生の値を含めない。

## 8. 変更箇所と並行作業

| 領域 | 変更責務 |
|---|---|
| 新規 `crates/network/` とworkspace依存 | 設定型、URL/除外検証、route、環境adapter、秘密参照の契約 |
| `crates/session/src/store.rs` | network設定の保存・移行・設定エラー保持 |
| `crates/app/src/https_transport.rs`、`app_update.rs`、起動配線 | snapshot注入、CONNECT、error変換、操作単位の固定 |
| `crates/update/src/download.rs` とerror呼出元 | proxy failure分類。既存download policyは維持 |
| `crates/ai/src/runtime_install.rs` | 共通routeからcurl構築・診断分類 |
| `crates/ai/src/runtime.rs`、`connect.rs`、`service.rs` | child環境、network revisionと停止/再接続 |
| `crates/ui/src/view/` の設定経路 | ネットワーク設定、検証・秘密入力・適用/失敗表示 |
| `crates/update/src/state_root.rs`、`health.rs`、appのrestart配線 | 認証proxyの安全な再起動継承 |
| 関連testsと利用者文書 | 第9節の受入証拠と設定方法 |

#567 P4 / PR #620は設定画面の機械分離を進行中。本設計の保存とnetwork coreは独立して進められる。UI実装だけは#620の受入・main反映を確認して `settings_screen` の受入済み経路へ追加し、親Viewの移動前メソッドへ機能を足さない。全体RF0や#297の完了待ちは不要。実装直前に依存のcurrent stateを再確認する。

実装順序は (1)共通設定と契約テスト、(2)更新CONNECTとcurl、(3)AI childと再起動、(4)設定UIと両OS検証。分割PRの場合は未接続の部分と残る受入条件を明示し、一部の成功で機能Issueをcloseしない。製品workerへ `.github/`、Commander Policy、AGENTS等の運用保護パスを変更させない。

## 9. 受入条件と検証対応

| ID | 完了条件 | 必須の証拠 |
|---|---|---|
| P1 | 3方式の保存/再読込、旧設定移行、不正値拒否、秘密保存失敗時の旧設定維持 | session/networkのfixture試験、macOS/Windows設定GUI |
| P2 | 環境優先順、空値、Windows大小文字、IPv6、除外domain境界、loopback、未対応schemeを仕様通り扱う | I/Oなしのtable tests。環境map注入で並行試験を汚染しない |
| P3 | 直接通信を遮断しても更新API→catalog/署名→ZIPがHTTP proxy経由で成功する | 両OSのテストproxyのCONNECT観測と署名検証済みstaging。Direct試験も残す |
| P4 | CONNECT失敗/407/過大head/分割head/EOF/無応答/不正TLS/除外を正しく扱い、Directへ逃げない | localhost proxy+TLS fixture、接続先記録、deadline境界試験。originへproxy認証が来ないことをassert |
| P5 | redirect各hopのallowlist、route再評価、HTTPS必須、hash/署名、cancelとpart cleanupを維持 | hane-update既存試験とproxy横断試験。許可外hostへCONNECTしないことを観測 |
| P6 | curlのruntime取得がproxy/Basic/除外/Directで成功し、curlrcや親環境で上書きされない | 実curl+proxy試験、argv/stdin秘匿、407分類、既存inventory検証 |
| P7 | pinned runtimeのChatGPT、Custom Provider、モデル取得、本文変換、必要なWebSocket、loopbackが各方式に従う | 両OSで実runtime+proxyの接続観測。fake childのenv試験は補助のみ。認証/API経路と外部ブラウザを区別 |
| P8 | 変更中の操作と旧AI childの応答を混ぜず、次の操作が新設定を使う | cancel/停止/遅延応答のgeneration試験、設定変更→再接続のfocused GUI |
| P9 | 更新後も設定を使え、認証proxyの秘密を通常ファイルへ保存しない | 両OSの更新/再起動fixture、OS秘密ストアと環境復元、残骸削除・復元失敗試験 |
| P10 | 本体更新metadataの追加設計による自動確認を除き、起動/設定表示/保存だけでは通信せず、失敗を分類表示し、秘密を漏らさない | 自動確認arm前/非対象機能/設定表示・保存の無通信assert、自動checkのnetwork snapshot試験、診断redaction試験、GUIエラー確認、state/ticket/log/argvのfixture秘密検索 |

実装PRでは `cargo test --workspace --all-features`、`cargo clippy --workspace --all-targets --all-features -- -D warnings` と通常Release buildをmacOS/Windowsで確認する。通信fixtureは実サービスや実ユーザーの秘密に依存させず、productionのTLS無効化flagは作らない。実ChatGPT連携等の認証が必要な検証は専用アカウントと秘匿した観測を使い、未実施は未検証と記録する。

GUIは設定入力・保存/取消・認証エラー・再接続をfocused scenarioとし、対象head/baseと結果を残す。ネットワークは専用隔離stateでDirect通信の可否を制御し、proxy logで経路を確認する。子環境に変数があるだけ、mock成功、curl単体成功だけでは全体対応の証拠にしない。

## 10. 根拠と引き継ぎ

curlの環境変数におけるHTTP_PROXYの例外とscheme別優先、明示proxy/除外、config stdinの扱いは公式資料を参照する: [curl manual](https://curl.se/docs/manpage.html)、[libcurl environment](https://curl.se/libcurl/c/libcurl-env.html)（2026-10-26確認）。本書の共通subsetと優先順を製品の契約とし、OS同梱curlとpinned runtimeの実際の対応は第9節で確認する。

更新側の保護契約は [更新実装仕様](../application-update-implementation-spec.md) と [ADR-0034](../adr/0034-zip-application-updates.md)、AI側は [AI設定/OAuth実装仕様](../ai-settings-oauth-implementation-spec.md) を参照する。設計上の未対応範囲以外で実装時の不一致が見つかった場合は、受入条件を黙って縮小せずIssueと本書を整合させる。

実装担当は本書を対象branchから読める状態で受け取り、開始時とpush直前に対象headを確認する。実装開始の正規依頼、レビュー、CI、必要なGUI、merge判断はcurrent Commander Policyに従う。本設計PRには実装workerの起動指示を混ぜない。
