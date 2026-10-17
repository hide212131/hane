# Hane専用Codexの配布・導入・更新 詳細設計

状態: **設計。2026-10-13時点の調査記録と、段階ごとの実装契約。** 作成日: 2026-10-13 JST。

親: [#479](https://github.com/hide212131/hane/issues/479)。P0: [#480](https://github.com/hide212131/hane/issues/480)。P1: [#481](https://github.com/hide212131/hane/issues/481)。判断理由: [ADR-0033](adr/0033-managed-codex-runtime-and-extensions.md)。調査基準main: `af981629011f04f68b25c8b58e476eb3eeffa624`。

2026-10-17追記: P0は[PR #485](https://github.com/hide212131/hane/pull/485)で実装され、#480は完了した。P0の受入条件は同Issueの確定契約に合わせて第2節・第10節へ反映した。P1は#481で未完了のまま追跡する。固定archiveの取得情報と限定smokeは、設計時点の確認記録として保持する。

## 1. 実装前提と対象

調査基準mainの`main.rs::start_ai_service`は実行ファイルの位置を決めて`AiService`へ渡す。`AiService`が設定・認証・モデル・接続確認・選択文処理を管理し、`AiRuntime`が子プロセスの開始/終了を直列化する。これらを置換する第二のAIサービスは作らない。[C1][C2]

P0では、AI設定画面の明示操作を入口に、固定版の完全App Server packageを取得・検査し、Hane専用領域へ初回導入する。初回のAI利用にはインターネット接続を必須とする。Hane ZIP / .appへruntime実行ファイルは同梱しない。P1では専用領域の修復、署名済みカタログからの更新、旧版復旧を加える。対象はWindows x64、macOS arm64。Linux/Windows arm64/macOS Intelの追加、文書モデル、汎用Pluginは対象外。

初回導入をアプリ起動時に自動実行しない。利用者がAI設定の「インストール」を押したときだけネットワークへ接続する。AI用プログラムの異常はAI機能だけの異常とし、エディタ起動、ファイル操作、保存を妨げない。

## 2. 固定する初期配布物

### 2.1 取得情報

初回導入は公式`rust-v0.157.1`の**完全App Serverパッケージ**をHaneからオンデマンド取得する。通常CLI用`codex-package-*`やbare standalone archiveとは別物である。2026-10-13にGitHub Releaseの資産メタデータを確認し、固定バイト列を取得してarchive全体のSHA-256とファイルinventoryが一致することも確認した。[O1]

| 項目 | Windows x64 | macOS arm64 |
|---|---|---|
| target | `x86_64-pc-windows-msvc` | `aarch64-apple-darwin` |
| asset | `codex-app-server-package-x86_64-pc-windows-msvc.tar.gz` | `codex-app-server-package-aarch64-apple-darwin.tar.gz` |
| asset ID | `589596907` | `589606956` |
| 圧縮サイズ bytes | `122840599` | `98491706` |
| SHA-256 | `7ca926f4fc443c0ead83109314e9a97bf43d2da276be147f1919a4b5e62dfa8b` | `9a6033c9fe30260e71d784d003ad1e2de2a55c93e24a24856e8433029a3f811a` |
| payload内entrypoint | `bin/codex-app-server.exe` | `bin/codex-app-server` |

取得URLは`https://github.com/openai/codex/releases/download/rust-v0.157.1/`にasset名を連結したもの。GitHub redirect先は実測で`release-assets.githubusercontent.com`だった。実装ではHTTPSを維持し、元URLとredirect先の各hostを許可リストで検証する。entrypointは同tagの`.github/dotslash-config.json`で確認した。[O2]

同tagのsource commitは`36650394c5b38c2990ccf2a3457165ca3e9d9726`。現在のruntime packageはmacOSで展開し、Codex/Node.jsのない一時環境からApp Server `--version`、JSON-RPC initialize、stdin closeによる正常停止まで成功した。これはHaneの導入画面、完成したHane配布物、Windows実機の受入試験を代替しない。

### 2.2 配布lockと内容一覧

P0では`assets/ai/codex-runtime-lock.json`にschema、version、source commit、target、asset URL/ID/size/SHA、entrypoint、runtime/config/state profileを固定する。ビルド中にlatestへ置換しない。

最初の取得で、全アーカイブ項目の相対パス・種別・サイズ・ファイルSHA-256を`assets/ai/inventory/<target>-0.157.1.json`へ記録してレビューする。実際に含まれる補助プログラムを一覧化し、毎回の配布で項目の欠落/増加/変化を検査する。bare serverだけの試験結果をコピーしない。

P0ではHaneによるruntimeの同梱・再配布を行わず、公式GitHub Releaseから利用者の端末へ直接取得する。#480の確定契約に従い、第三者ライセンス監査、LICENSE/NOTICEの収集・同梱、license一覧・資料の検査はP0の受入条件に含めない。これはIssueの対象範囲の記録であり、ライセンス上の義務の有無を判断する記述ではない。

P1のライセンス検証は#481の契約を維持する。採用タグと配布依存物に対応するLICENSE/NOTICEを保存し、各表示の取得元・適用対象・内容SHAを一覧へ記録する。必要な表示の所在が不明なら、その点をP1の阻害要因として報告し、確認済みと扱わない。Hane本体のライセンスがあってもCodexの表示の代わりにはしない。[O3]

## 3. 配置と解決順序

### 3.1 配置

HaneのZIP / .appにはruntimeの実行ファイルを置かず、P0のruntime向け資産は小さな固定lockとinventoryとする。利用者の明示的な操作で取得したpayloadとmanifestはHane app-dataへ置く。P1のライセンス資料は第2.2節と#481の契約に従う。

```text
<Hane app data>/ai/
  codex-chatgpt/                      # 既存。移動しない
  codex-custom/                       # 既存。移動しない
  runtime-install.lock
  active-runtime.json
  runtime-activation.json             # 切替途中だけ存在するjournal
  runtimes/<target>/<bundle-id>/<installation-id>/
    manifest.json
    licenses/...                      # P1の検証資料。P0では要求しない
    payload/...
  runtime-staging/<operation-id>/
  runtime-health/<operation-id>/
```

`bundle-id`は`codex-<version>-<archive-sha先頭12文字>`、`installation-id`と`operation-id`はHaneが生成するランダムIDとする。同一版の修復もP1で新しいinstallation-idへ展開し、使用中のファイルを上書きしない。IDはASCII英数字とハイフンのみ、最大96文字。entrypointを含む相対パスはmanifestから解決し、正規化後に所有root内であることを確認する。

app-dataは既存の`FileStateStore::from_environment()`を使う。Windows標準は`%LOCALAPPDATA%\\Hane`、macOS標準は`~/Library/Application Support/Hane`。`HANE_STATE_DIR`を尊重し、AIで使うrootが相対パスなら明確に設定エラーを返す。他アプリのCODEX_HOMEを代わりに使わない。[C3]

### 3.2 優先順位

1. 明示的な`HANE_CODEX_APP_SERVER`。絶対パスのstandalone executableのみ。表示は「開発用指定・Hane管理対象外」。異常なら原因を表示し、管理版へ無言fallbackしない。P1の修復/更新を無効にする。
2. `active-runtime.json`が選んだ、Haneが検査済みの専用インストール版。
3. active指定がない場合は`Missing`として「インストール」ボタンを表示する。

隣接する任意の旧`codex-app-server.exe`、PATH上の`codex`、Codex Desktop/IDEの内部実行ファイルは探索しない。Hane配布物や通常Codexのインストール先を変更しない。active指定の破損は見つからなかったことにして無視しない。第8節の復旧契約に合致するときだけ旧成功版へ戻す。

### 3.3 manifestとポインター

固定lockは`assets/ai/codex-runtime-lock.json`に置き、schema、version、source commit、target、asset URL/ID/size/SHA、entrypoint、runtime/config/state profileを固定する。実行時にlatestを解決しない。`manifest.json`は`schema_version=1`、`bundle_id`、`target`、`codex_version`、`source_commit`、`entrypoint`、`archive_sha256`、`files`、`runtime_profile`、`config_profile`、`state_profile`を持つ。未知schemaは拒否する。各`files` entryは相対path/type/size/sha256/実行属性を持ち、全payloadファイルを覆う。

P0ではlicense sourceや`licenses`を必須にしない。P1では#481のライセンス検証のため、固定lockに全license source、manifestに`licenses`を保持する。

`active-runtime.json`は`schema_version=1`、単調増加`revision`、`current`、`previous_successful`を持つ。各参照はtarget/bundle-id/installation-idだけを持ち、任意の絶対パス、URL、キー、CODEX_HOMEを保存しない。P0初回導入時はcurrentのみを設定し、既存の`ai-settings.json`と`user-prompts.json`は移行しない。

ポインターとjournalには既存のatomic fileユーティリティを使う。rename後の耐久性未確認を「保存前失敗」と同じに扱わない。journalに認証データを含めない。

## 4. コード境界と診断

### 4.1 最小構成

| 場所 | 責務 |
|---|---|
| 新規`crates/ai/src/runtime_distribution.rs` | manifest、locator、完全性検査、診断型。P0から使う |
| 新規`crates/ai/src/runtime_install.rs` | 固定版取得、検証、展開、health、cancel、P0初回導入。P1で修復/更新を追加 |
| `paths.rs` | Hane専用runtime root、staging/health、限定的な掃除 |
| `service.rs` | インストール状態、操作受付、既存所有ロック/AI操作との調停 |
| `runtime.rs` / `connect.rs` | 既存の唯一の起動/停止経路、選択版の再設定。第二の通常runtimeを作らない |
| `main.rs` | exeとapp-dataをresolverへ渡す。固定binary_pathの組み立てを除く |
| AI設定画面 | 未導入状態・進捗・検査結果・診断と明示的なボタン |

`AiServiceConfig`はresolverに必要なapp-dataと既存credential/browser依存を受け取る。試験ではdownload source、候補情報、clockを注入できるようにする。開発用明示パスもresolverの一経路にし、テスト用に実グローバル環境を変更しない。

UI向けの`RuntimeInstallationSnapshot`は`source`、`installed_version`、`available_version`、`status`、`diagnostic`、`operation_id`、`downloaded_bytes/total_bytes`を持つ。秘密を含む引数・環境変数・HTTP header・生RPC・認証ファイルは含めない。

### 4.2 操作と診断コード

P0操作は`InspectRuntime`と`InstallRecommended`。P1で`RepairRuntime`、`CheckRuntimeUpdate`、`InstallRuntimeUpdate`、`RestorePreviousRuntime`を追加する。cancelは既存OperationIdの流儀に合わせる。すべての通常runtime起動/停止は既存coordinatorへ委譲する。

| stable code | 意味と画面 |
|---|---|
| `runtime_missing` | 実行ファイルまたは必須付属物がない。「AI用プログラムが見つかりません」 |
| `runtime_network_failed` | 取得失敗/timeout/オフライン。固定版を未取得として表示 |
| `runtime_download_untrusted` | 不許可host、redirect、archive size/hash、manifestの検証不一致 |
| `runtime_integrity_mismatch` | 導入後の内容が承認manifestと不一致。「AI用プログラムが破損しています」 |
| `runtime_target_unsupported` | OS/CPU対象外。対応targetを案内 |
| `runtime_access_denied` | 読取/実行/保存の権限不足 |
| `runtime_invalid_executable` | 不正な実行形式。通常CLIや誤CPU版も区別できる範囲で表示 |
| `runtime_initialize_failed` | 内容検査後の初期化が失敗。欠損とは呼ばない |
| `runtime_update_untrusted` | P1カタログ署名/取得物検査の不一致 |
| `runtime_update_incompatible` | P1でHane/profile/状態互換の対象外 |
| `runtime_install_io` | ディスク不足/保存失敗。OSコードは秘密を除いたローカル診断に限る |

既存の`owned_elsewhere`、busy、timeout、restart-blockedの分類は維持する。`RuntimeError::Spawn`のio::ErrorKindとWindows OSエラーを分類し、文言の部分一致だけに依存しない。詳しいパスは利用者のローカル画面で確認できるが、通常ログ・公開証拠にユーザー名を含むpathを載せない。

## 5. 設定画面

AI設定の先頭に「AI用プログラム」を追加する。現在の版、取得元、導入検査状態を表示する。「Hane専用として追加します。他のCodexは変更しません」を導入前に表示する。導入前に推論しない。

| 状態 | 主な操作 |
|---|---|
| 未導入 | 固定版・概算サイズ・初回ネット接続が必要と示し「インストール」 |
| ダウンロード/検証中 | 進捗/取消。文書編集と既存AI処理は継続 |
| 正常な専用版 | 「導入済み」表示。更新確認はP1 |
| 破損 | 修復はP1。P0では診断と失敗内容を表示 |
| AI使用中で候補準備済み | P1で「AI処理が終わってから適用できます」。自動で使用中処理を中断しない |
| 開発用指定 | pathと管理対象外を表示。install/repair/updateを無効化 |

未導入時の「インストール」一操作で固定版の取得から初回導入まで進める。AI設定の「接続確認」は導入/initializeと区別する。導入成功後に自動で推論せず、既存ブラウザログインは利用者が必要に応じて操作する。

## 6. P0: 初回ダウンロード

Haneの製品配布工程にCodex実行ファイルを追加しない。P0のruntime向け資産は小さな固定lockとinventoryとし、ライセンス監査・表示資料の収集や同梱は第2.2節のとおり受入条件に含めない。利用者のPCでは実行時にPython/Node.js/npmを要求せず、Rust側のruntime installerが固定HTTPS URLから取得する。Haneのビルド時にlatestへ置換しない。

完成したWindows ZIPおよびmacOS .appから起動し、Codex/Node/PATH上のCodex/開発用overrideを見せない試験環境で、設定画面の「インストール」操作を行う。実際の固定archiveを取得し、archive hash、inventory、展開先、initialize/stopを確認する。Hane配布物を展開し直して試験し、開発ツリーのfixtureのみで受入済みにしない。初回取得にはインターネット接続が必要であり、ネットワークなしではAIのみ利用不可、文書編集は継続できる。

P0のCI path filterはruntime code、固定lock/inventory、対応するtestsを対象にする。runtime binaryを各OS release assetへ同梱するためのworkflow変更は行わない。固定inventoryに不足があれば、installerと検査処理はfail closedにする。テストのskipは成功扱いにしない。

## 7. 取得・展開と段階ごとの導入手順

### 7.1 P0: 固定lock

P0はHane本体に含むレビュー済みlockだけからCodex 0.157.1を取得する。ユーザー指定URL、latest、無署名catalog、通常Codexのコピーは使わない。公式GitHub release assetのURLはIssue #480と本節2.1で固定する。HTTPSを必須にし、redirectは実測・レビュー済みのGitHub資産配信hostだけを許可する。取得元URL、各redirect host、status、サイズを秘密のqueryを除いて記録する。

P1bの署名カタログは第7.4節を正とする。

### 7.2 P0/P1共通: 展開と有限処理

ダウンロード上限512 MiB、総展開上限2 GiB、最大10,000項目、manifestのファイル数/サイズ以内とする。HTTP接続20秒、無進捗60秒、全体15分を上限とし、有限の失敗をUIへ返す。これは内部timeoutであり利用者への完了予測ではない。ネットワーク再試行は利用者操作にし、永久retryをしない。

展開は新しい同一ボリュームのstagingだけへ行う。絶対パス、`..`、drive prefix、Windows ADS、予約名、末尾dot/space、正規化/case-insensitive衝突、特殊デバイス項目を拒否する。既存root/親に予期しないsymlink/reparseがあれば停止する。

通常はregular file/directoryのみを許可する。上流の承認inventoryに内部相対symlinkが実在する場合だけ、その正確なlink項目を許可できる。リンク先が同一payload内の検査済み通常ファイルであること、循環/連鎖がないこと、展開中にリンクを辿らないことを検査し、通常ファイル展開後に作成する。Windowsではreparseを作る必要のない公式Windows内容を前提に試験し、未知のlinkを追加承認しない。任意Packageにこの例外を流用しない。

cancelではダウンロードと新規ファイル作成を止め、自分のstagingだけを消す。検査済みcandidateは保存しても自動適用しない。解凍器が指定外の場所へ書かないことを攻撃fixtureで確認する。

### 7.3 P0: healthと初回確定

1. stagingへ取得し、圧縮hash、内容一覧、target/entrypointを検査する。
2. 専用health CODEX_HOMEと空cwdで候補の`--version`、initialize/initialized/停止を確認する。既存runtimeの起動/停止機構を再利用し、同時に2つのサーバーを起動しない。通常の認証HOMEへ触れない。
3. 検査中はOPENAI_API_KEY/既存認証/host session変数を渡さず、model/turn/account-loginを呼ばない。実AI接続確認はこのhealth checkに含めない。
4. 全検査に成功したpayloadだけを不変installationとして保存し、`active-runtime.json`をatomic更新する。既存版がない初回導入でpointer更新が失敗したら、AI runtimeは未導入のままとしcandidateだけを掃除する。
5. `runtime-install.lock`はstaging/final公開を守る。設定/owner lockと同時保持しない。通常AIがbusy、別Hane所有、終了未確認ならpointerを変えない。
6. 適用直前にpointer revision、settings generation、candidate digestを再検査する。callbackはoperation IDとruntime generationが一致するものだけ反映する。

### 7.4 P1b: 署名カタログによる独立更新

P1bの独立更新はGitHub Releases上の固定入口`ai-runtime-catalog-v1/catalog.json`と`catalog.sig`を使う。このtag/資産は**新規実装する公開物で、現在存在するとは扱わない**。

カタログは最大256 KiB。Ed25519のdetached signatureを**取得したJSONの生bytes**に対して検証後にparseする。JSONの再整形や独自canonicalizationを署名入力にしない。署名はbase64の64 bytes、key IDを持ち、公開鍵はHane本体に埋め込む。暗号実装を自作せず既存ライブラリを使う。

カタログにはschema、channel=stable、単調増加sequence、issued_at、expires_at、entriesを置く。entryはtarget、Hane version range、bundle/manifest URL・サイズ・hash、runtime/config/state profile、許可する更新元bundle IDとrollback先ID、検証証拠URLを持つ。署名済みでもHaneが知らないschema/profileや不明な更新元には適用しない。

最高受理sequenceより古いもの、期限切れ、未来時刻が5分を超えるもの、未知key ID、署名不一致は更新不可にする。カタログには最大30日の有効期限を設け、時計異常なら説明付きで停止する。これは更新の制限であり既存版での編集/AI起動を止めるものではない。

署名鍵はmaintainerが生成して管理する。秘密鍵はリポジトリへ置かず、保護されたrelease環境だけで使い、カタログ検証/対象OS試験後に明示公開する。鍵未設定中はP1bを利用不可と表示するがP0/P1aは動く。新たな常駐サービスや複数チャンネルは初期に作らない。

元URLはHTTPSかつ`github.com/openai/codex/releases/download/`または`github.com/hide212131/hane/releases/download/`の承認資産に限定する。GitHubの資産配信先へのHTTPS redirectは既知のGitHub資産ホストだけ許し、最大5回、認証header/cookieは送らない。signed URLのqueryをログに残さない。loopback/private IPへのredirectや任意hostは拒否する。

### 7.5 P1: healthと修復・更新の適用

P1aの同版修復はP0と同じ固定manifest・公式URL・hashを使い、P1bの更新は第7.4節の署名カタログを使う。ライセンス検証を含むP1の受入条件は#481に従う。取得・展開の共通条件は第7.2節を参照する。

1. stagingへ取得し、圧縮hash、内容一覧、ライセンス、target/entrypointを検査する。
2. 通常AIがidleで自分がruntimeを所有できるときに、専用health CODEX_HOMEと空cwdで候補の`--version`およびinitialize/initialized/停止を検査する。候補検査も既存runtimeの起動/停止機構を再利用し、同時に2つのサーバーを起動しない。通常の認証HOMEへ触れない。
3. 検査中はOPENAI_API_KEY/既存認証/host session変数を渡さず、model/turn/account-loginを呼ばない。ネットワークなしのfixtureで成立することを試験する。実AI接続確認はこの検査に含めない。
4. 内容検査とhealthに合格したら不変installationとして公開する。`runtime-install.lock`はstaging/final公開を守り、**既存の設定/所有ロックと同時保持しない**。
5. サービスの既存AI排他受付を取得し、owner lockとsettings lockは`connect.rs`の既存の取得順序/共有guardを使う。既に自分が持つowner lockを再取得しない。別Hane所有やbusyなら候補を残して戻る。
6. 現在のpointer revision・settings generation・candidate digestを再検査する。旧runtimeを停止し、終了確認後にjournalを書き、activeポインターをatomic更新する。現在の接続のCODEX_HOMEと生成設定で新runtimeを初期化する。
7. 成功を記録しjournalを終了する。失敗時は第8節に従う。P0と同じ初期化結果の時点で導入成功とし、認証や推論まで成功したとは表示しない。

手順2と6の間に通常AI状態が変われば、手順5からの再検査で止める。ネットワーク/解凍/長いhash中にsettings/owner lockを保持しない。callbackはoperation IDとruntime generationが一致するものだけを反映する。

## 8. 復旧と状態互換

### 8.1 journal

`runtime-activation.json`はoperation ID、expected pointer revision、旧参照、新参照、phase=`prepared|pointer_written|started`のみを保存する。資格情報、設定ファイル本文、OAuth tokenを含めない。処理中は既存busy理由に`RuntimeActivation`を加え、ログイン・設定保存・選択文処理との同時実行を認めない。

起動時にjournalが残れば、所有権を取得し、旧/新childの終了確認後にjournalとポインターの組合せを判定する。`prepared`でポインターが旧版なら新候補を未適用として扱う。`pointer_written`で成功記録がなければ、新版のローカル初期化を一度だけ試し、許可された復旧先へ戻すかAIだけを停止する。`started`は成功ポインターとmanifestを確認してjournalを削除する。破損journalを推測で補完しない。

### 8.2 復旧を許可する範囲

同じversion文字列や同じstate_profile名だけでは互換性の証明にならない。候補公開時に、許可する旧版→新版→旧版でアカウント/設定/実接続を確認し、具体的なbundle対をcatalogへ列挙する。この試験がない更新は「Hane本体の更新が必要」と表示し、独立更新しない。

新runtimeが本番CODEX_HOMEへ触れた後は、許可済みのrollback先だけを使う。コピーしておいたauth.json/資格情報を戻して整合を取らない。OS資格情報の保存形式も試験対象に含める。

初期化エラー/不正実行形式は復旧候補だが、接続先の認証失敗・ネットワーク・料金/利用枠・モデル不存在では自動で版を戻さない。停止未確認なら既存restart-blockedと同様にAIの新規起動を止め、文書編集を続ける。

初版は成功済みruntimeの自動世代削除を作らない。掃除は自分の失敗staging/healthのみ。明示的な削除を後で追加する際もactive/previous/journal/candidate参照がある版は対象外とする。

## 9. 環境分離の不変条件

実行用rootをCODEX_HOMEにしない。ChatGPTとCustomは従来の別HOMEを維持する。Hane更新・ログアウトが通常Codexの状態へ影響しないかは2 OSで実証する。単にCODEX_HOMEを別にしたことだけを証拠にしない。

既存の環境許可リスト、明示CODEX_HOME、空cwd、`project_doc_max_bytes=0`、`project_root_markers=[]`、`skip_host_skill_discovery=true`を維持する。[C2][C6]

実HOMEとPATHをOS互換のため引き継ぐことと、ホストCodexを探すことは別である。採用profileで周囲のAGENTS/config/Skillsが入力へ混ざらないことを模擬先と読み取りtripwireで確認する。完全なOS sandboxを新たに保証したとは表現しない。

Windows補助プログラムのサービス/pipe/一時ファイル等に共有資源があれば、他Codexとの同時利用試験で確認する。管理者権限やサーバー常駐登録を単に同梱するために要求しない。必要になる場合は未確認事項として報告し、暗黙のsystem変更で通さない。

## 10. 試験IDと完了条件

### P0

| ID | 必須試験 |
|---|---|
| R01 | 2 targetの公式archive size/hash/source commit/content inventory/entrypoint一致 |
| R02 | Hane製品配布物にruntime binaryを含めず、runtime向け資産は固定lock/inventoryとする。ライセンス監査・表示資料の収集と同梱はP0の受入条件に含めない |
| R03 | 完成ZIP/.app、Codex/Node/PATH上Codexなし、オンライン環境から「インストール」を操作し、固定版を取得・検証・導入してinitialize/停止 |
| R04 | 明示path、相対path拒否、無言fallbackなし、allowlist外redirect拒否 |
| R05 | 欠損・破損・target相違・network失敗・hash不一致・アクセス拒否・不正exe・初期化失敗を分類 |
| R06 | AI異常でも編集/保存/設定画面が動く。オフラインではAIだけ未導入と表示 |
| R07 | Custom模擬先へ固定input/model/key、選択文AI回帰。秘密は記録しない |
| R08 | 周辺AGENTS/config/Skills/他アプリ環境変数の不混入 |
| R09 | Haneと通常Codexの同時ログイン/片方のログアウト/再起動の非干渉 |
| R10 | Windows/macOSで実ChatGPTログインと固定接続確認。模擬と別記録 |
| R11 | Windows/macOSで実Custom接続確認。利用者がキーを画面へ直接入力 |
| R12 | Windows no-console、pipe transport、停止を完成Hane配布物から確認 |
| R13 | exact-head CI/レビュー、2 OSの実画面確認、完成Hane配布物の再検証。実画面は利用者の完了報告で確認し、画像添付は不要 |

### P1

| ID | 必須試験 |
|---|---|
| U01 | 同版修復が新installationに入り、実行中exeを上書きしない |
| U02 | 正常署名catalog、改ざん/未知鍵/schema/profile/期限/sequence拒否 |
| U03 | 不適合Hane version/target/許可されていない更新対を拒否 |
| U04 | HTTP断/timeout/不足容量/hash不一致/展開上限/cancel |
| U05 | traversal/link/reparse/drive/ADS/case collisionの攻撃fixture |
| U06 | AI中・ログイン中・別Hane所有時の適用拒否。文書編集は継続 |
| U07 | pointer更新前/後、初期化後の各crashをjournalから復旧 |
| U08 | Windows file lock、stop timeout、遅延callbackで二重起動なし |
| U09 | 検証済み旧→新→旧でChatGPT/Custom状態を保つ |
| U10 | 認証/通信/利用枠エラーでは自動downgradeしない |
| U11 | .app/システムPATH/他Codex/Skills/プロンプトを変更しない |
| U12 | P0/P1修復は署名鍵なしで使え、独立catalog更新だけ鍵なしで無効 |
| U13 | 2 OS実画面、exact-head CI/レビュー、公開catalogと取得物の一致 |

各IDはpass/fail/not-runを区別し、模擬試験と実アカウント試験の結果を分けて記録する。

P0では#480の確定契約に従い、実ChatGPT/Custom接続が実行環境上不可能な場合は、可能な範囲の確認と未確認の境界を記録する。Windows/macOSの実画面確認は利用者の完了報告を採用し、スクリーンショット添付は受入条件にしない。P0の最終的な実装・検証記録は[#480](https://github.com/hide212131/hane/issues/480)と[PR #485](https://github.com/hide212131/hane/pull/485)を参照する。

P1は#481の受入条件を維持する。実アカウント試験に必要なログイン・keyがなければ、その試験を未実施と明記し、未確認の受入項目があるまま#481を閉じない。模擬試験までの実装やPRを放棄する理由にはしない。

P1の証拠は`docs/ai-runtime-evidence/<phase>-<head>/README.md`へ、OS/CPU、Hane exact head、固定archive hash、Codex version、操作/終了結果、対象/対象外を保存する。本文・key・token・生認証headerは含めない。

## 11. 実装順序と完了の境界

P0は(a)固定lock/inventory、(b)resolver/診断、(c)明示的な固定版download/安全な初回導入、(d)完成Hane配布物からの2 OS試験の順。P0の実接続・実画面の記録方法は第10節に従う。P1は(a)専用保存/repair fixture、(b)署名catalog検証・更新とrollback、(c)画面、(d)競合/復旧の順。

設計PRには製品コード、runtime binary、秘密鍵、CI成功の捏造、release操作を含めない。実装PRが開発運用policyの変更を必要とすると判断した場合は、配布修正へ混ぜず理由を別途提示する。

## 12. 参照

- [C1 起動入口](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/crates/app/src/main.rs)
- [C2 runtime](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/crates/ai/src/runtime.rs)
- [C3 app-data](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/crates/session/src/store.rs)
- [C4 macOS bundle](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/scripts/bundle-macos.sh)
- [C5 既存検証の範囲](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/docs/issue-379-t00-evidence.md)
- [C6 生成設定](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/crates/ai/src/provider.rs)
- [O1 上流release](https://github.com/openai/codex/releases/tag/rust-v0.157.1)
- [O2 entrypoint](https://github.com/openai/codex/blob/rust-v0.157.1/.github/dotslash-config.json)
- [O3 Apache License](https://github.com/openai/codex/blob/rust-v0.157.1/LICENSE)
