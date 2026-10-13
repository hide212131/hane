# Hane専用Codexの配布・導入・更新 詳細設計

状態: **設計。製品コード・配布物・実機検証は未変更/未実施。** 作成日: 2026-10-13 JST。

親: [#479](https://github.com/hide212131/hane/issues/479)。P0: [#480](https://github.com/hide212131/hane/issues/480)。P1: [#481](https://github.com/hide212131/hane/issues/481)。判断理由: [ADR-0033](adr/0033-managed-codex-runtime-and-extensions.md)。調査基準main: `af981629011f04f68b25c8b58e476eb3eeffa624`。

## 1. 実装前提と対象

現在の`main.rs::start_ai_service`は実行ファイルの位置を決めて`AiService`へ渡す。`AiService`が設定・認証・モデル・接続確認・選択文処理を管理し、`AiRuntime`が子プロセスの開始/終了を直列化する。これらを置換する第二のAIサービスは作らない。[C1][C2]

P0では同梱・位置解決・診断・配布物試験を実装する。P1では専用領域への導入/修復/更新を加える。対象はWindows x64、macOS arm64。Linux/Windows arm64/macOS Intelの追加、文書モデル、汎用Pluginは対象外。

AI用プログラムの異常はAI機能だけの異常とし、エディタ起動、ファイル操作、保存を妨げない。起動直後にネットワークへ更新確認を行わない。

## 2. 固定する初期配布物

### 2.1 取得情報

初期候補は公式`rust-v0.157.1`の**完全App Serverパッケージ**。通常CLI用`codex-package-*`やbare standaloneのアーカイブとは別物である。2026-10-13にGitHub Releaseの資産メタデータを確認した。[O1]

| 項目 | Windows x64 | macOS arm64 |
|---|---|---|
| target | `x86_64-pc-windows-msvc` | `aarch64-apple-darwin` |
| asset | `codex-app-server-package-x86_64-pc-windows-msvc.tar.gz` | `codex-app-server-package-aarch64-apple-darwin.tar.gz` |
| asset ID | `589596907` | `589606956` |
| 圧縮サイズ bytes | `122840599` | `98491706` |
| SHA-256 | `7ca926f4fc443c0ead83109314e9a97bf43d2da276be147f1919a4b5e62dfa8b` | `9a6033c9fe30260e71d784d003ad1e2de2a55c93e24a24856e8433029a3f811a` |
| payload内entrypoint | `bin/codex-app-server.exe` | `bin/codex-app-server` |

取得URLは`https://github.com/openai/codex/releases/download/rust-v0.157.1/`にasset名を連結したもの。entrypointは同タグの`.github/dotslash-config.json`で確認した。[O2]

同タグのsource commitは既存検証記録が`36650394c5b38c2990ccf2a3457165ca3e9d9726`を記録している。P0取得処理でtagの解決結果を再確認し、一致しない場合は停止する。[C5]

**今回アーカイブ本体のダウンロード・展開・実行、全付属ライセンスの監査は行っていない。** ここに示したハッシュは上流メタデータとの照合値であり、実装担当が取得バイト列を検査して初めて一致を証明できる。

### 2.2 配布lockと内容一覧

実装時に`assets/ai/codex-runtime-lock.json`を追加し、schema、version、source commit、target、asset URL/ID/size/SHA、entrypoint、runtime/config/state profile、ライセンス一覧を固定する。ビルド中にlatestへ置換しない。

最初の取得で、全アーカイブ項目の相対パス・種別・サイズ・ファイルSHA-256を`assets/ai/inventory/<target>-0.157.1.json`へ記録してレビューする。実際に含まれる補助プログラムを一覧化し、毎回の配布で項目の欠落/増加/変化を検査する。bare serverだけの試験結果をコピーしない。

LICENSE/NOTICEは採用タグと配布依存物に対応するものを保存する。各表示の取得元・適用対象・内容SHAを一覧へ記録する。必要な表示の所在が不明な場合はその点をP0の阻害要因として報告し、配布を成功にしない。Hane本体のライセンスがあってもCodexの表示の代わりにはしない。[O3]

## 3. 配置と解決順序

### 3.1 配置

```text
Windowsの配布フォルダー/
  hane.exe
  runtime/codex/
    manifest.json
    licenses/...
    payload/bin/codex-app-server.exe
    payload/...                     # 上流完全パッケージの全内容

Hane.app/Contents/
  MacOS/hane
  Resources/codex/
    manifest.json
    licenses/...
    payload/bin/codex-app-server
    payload/...

<Hane app data>/ai/
  codex-chatgpt/                      # 既存。移動しない
  codex-custom/                       # 既存。移動しない
  runtime-install.lock
  active-runtime.json
  runtime-activation.json             # 切替途中だけ存在するjournal
  runtimes/<target>/<bundle-id>/<installation-id>/
    manifest.json
    licenses/...
    payload/...
  runtime-staging/<operation-id>/
  runtime-health/<operation-id>/
```

`bundle-id`は`codex-<version>-<archive-sha先頭12文字>`、`installation-id`と`operation-id`はHaneが生成するランダムIDとする。同一版の修復も新しいinstallation-idへ展開し、使用中のファイルを上書きしない。IDはASCII英数字とハイフンのみ、最大96文字。entrypointを含む相対パスはmanifestから解決し、正規化後に所有root内であることを確認する。

app dataは既存の`FileStateStore::from_environment()`を使う。Windows標準は`%LOCALAPPDATA%\Hane`、macOS標準は`~/Library/Application Support/Hane`。`HANE_STATE_DIR`を尊重し、AIで使うrootが相対パスなら明確に設定エラーを返す。他アプリのCODEX_HOMEを代わりに使わない。[C3]

### 3.2 優先順位

1. 明示的な`HANE_CODEX_APP_SERVER`。絶対パスのstandalone executableのみ。表示は「開発用指定・Hane管理対象外」。異常ならその原因を表示し、別候補へ無言で切り替えない。P1の導入/更新/修復を無効にする。
2. `active-runtime.json`が選んだ、Hane検証済みの管理パッケージ。署名済み/埋込manifestに含まれるIDと実体を照合する。
3. active指定がない場合の同梱パッケージ。

隣接する任意の旧`codex-app-server.exe`、PATH上の`codex`、Codex Desktop/IDEの内部実行ファイルは探索しない。開発で外部standaloneを使う必要がある場合は明示変数に統一する。

active指定の破損は見つからなかったことにして無視しない。第8節の復旧契約に合致する場合だけ旧成功版/同梱版へ戻し、画面にも復旧を表示する。許可されていないdowngradeを自動で行わない。

### 3.3 manifestとポインター

`manifest.json`の必須項目は`schema_version=1`、`bundle_id`、`target`、`codex_version`、`source_commit`、`entrypoint`、`archive_sha256`、`files`、`licenses`、`runtime_profile`、`config_profile`、`state_profile`。未知schemaは拒否する。`files`はpath/type/size/sha256/実行属性を持ち、全実行依存物を覆う。署名済みカタログはmanifest自体のSHAも固定する。

`active-runtime.json`は`schema_version=1`、単調増加`revision`、`current`、`previous_successful`。各参照は`source=bundled|managed`、target/bundle-id/installation-idのみを持つ。同梱参照はinstallation-idなし。任意の絶対パス、URL、キー、CODEX_HOMEは保存しない。既存`ai-settings.json`と`user-prompts.json`は移行しない。

ポインターとjournalには既存のatomic fileユーティリティを使う。rename後の耐久性未確認を「保存前失敗」と同じに扱わない。journalに認証データを含めない。

## 4. コード境界と診断

### 4.1 最小構成

| 場所 | 責務 |
|---|---|
| 新規`crates/ai/src/runtime_distribution.rs` | manifest、locator、完全性検査、診断型。P0から使う |
| 新規`crates/ai/src/runtime_install.rs` | 取得、検証、展開、health、cancel。P1で追加 |
| `paths.rs` | 上記専用パスと限定的な掃除 |
| `service.rs` | インストール状態、UI操作受付、既存所有ロック/AI操作との調停 |
| `runtime.rs` / `connect.rs` | 既存の唯一の起動/停止経路、選択版の再設定。第二の通常ランタイムを作らない |
| `main.rs` | exeとapp-dataをresolverへ渡す。固定binary_pathの組み立てを除く |
| AI設定画面 | 検査結果表示とボタン、モデル/認証/接続確認との状態連動 |

`AiServiceConfig`はresolverに必要な配布root・app-dataと既存credential/browser依存を受け取る。試験では候補情報を注入できるようにする。開発用明示パスもresolverの一経路にし、テスト用に実グローバル環境を変更しない。

UI向けに`RuntimeInstallationSnapshot`を追加する。`source`、`installed_version`、`available_version`、`status`、`diagnostic`、`operation_id`、`downloaded_bytes/total_bytes`を持つ。秘密を含むコマンド引数・環境変数・HTTP header・生RPC・認証ファイルを含めない。

`status`は`CheckingLocal / Missing / Available / Downloading / Verifying / ReadyToActivate / Activating / Failed`。認証済み状態とインストール済み状態を同一視しない。Availableはローカルプログラムが使用可能という意味で、実AI応答確認の成功ではない。

### 4.2 操作と診断コード

新規操作は`InspectRuntime`、`InstallRecommended`、`RepairRuntime`、`CheckRuntimeUpdate`、`InstallRuntimeUpdate`、`RestoreBundledRuntime`。cancelは既存OperationIdの流儀に合わせる。すべての通常runtime起動/停止は既存coordinatorへ委譲する。

| stable code | 意味と画面 |
|---|---|
| `runtime_missing` | 実行ファイルまたは必須付属物がない。「AI用プログラムが見つかりません」 |
| `runtime_integrity_mismatch` | 内容が承認manifestと不一致。「AI用プログラムが破損しています」 |
| `runtime_target_unsupported` | OS/CPU対象外。適切な配布物を案内 |
| `runtime_access_denied` | 読み取り/実行/保存の権限不足。Windows保護機能等は原因の断定をしない |
| `runtime_invalid_executable` | 不正な実行形式。通常CLIや誤ったCPU版も区別できる範囲で表示 |
| `runtime_initialize_failed` | 存在・内容検査後に初期化が失敗。欠損と呼ばない |
| `runtime_update_untrusted` | カタログ署名/取得物検査が不一致 |
| `runtime_update_incompatible` | Hane/profile/状態互換の対象外 |
| `runtime_install_io` | ディスク不足/保存失敗。OSコードは秘密を除いたローカル診断に限る |

既存の`owned_elsewhere`、busy、timeout、restart-blockedの分類は維持する。`RuntimeError::Spawn`のio::ErrorKindとWindows OSエラーを分類し、文言の部分一致だけに依存しない。詳しいパスは利用者のローカル画面で確認できるが、通常ログ・公開証拠にはユーザー名を含むパスを載せない。

## 5. 設定画面

AI設定の先頭に「AI用プログラム」を追加する。使用中の版、同梱/専用インストール/開発用指定、検査状態を表示する。導入前に「Hane専用として追加します。他のCodexは変更しません」を表示する。

| 状態 | 主な操作 |
|---|---|
| 正常な同梱/管理版 | 更新を確認。更新があれば版と変更内容を表示して「更新する」 |
| 未導入 | 「インストール」。固定recommendedを使う |
| 破損 | 「修復」。新しいinstallationへ再導入する |
| ダウンロード中 | 進捗/取消。文書編集と既存AI処理は継続できる |
| AI使用中で候補準備済み | 「AI処理が終わってから適用できます」。自動で使用中処理を中断しない |
| 開発用指定 | パスと管理対象外を表示。インストール/修復/更新は無効 |

「インストール/更新する」の一操作で取得から適用まで進む。ただし適用時に他のAI操作が動いていれば候補を保持して明示的な「適用」を待ち、後から無断で再起動しない。先に利用者の文書を保存させる必要はない。

Programの初期化確認と、課金や契約枠を使う可能性のある「接続確認」を別ボタン/状態として維持する。導入成功後に自動で推論しない。既存のブラウザログインは必要に応じて利用者が操作する。

## 6. P0: 配布パイプライン

`assets/ai/codex-runtime-lock.json`を読む共通の取得/検査スクリプトを`scripts/prepare-codex-runtime.py`として追加する。Pythonは**ビルド用**であり利用者のWindowsへ要求しない。引数は`--target <triple> --output <root> --lock <path>`。ネットワーク処理は固定HTTPS URLだけを使い、取得物と展開物の検査に失敗したら非0で終了する。

Windowsは完成stagingへ`runtime/codex`一式をコピーしてからZIP化する。macOSは`bundle-macos.sh`で`Contents/Resources/codex`を配置してから最終検査する。調査時の同スクリプトにはコード署名工程はない。署名/公証を追加する場合は全実行物を配置し終えてから適切に署名し、実行時の.app書換えでは修復しない。[C4]

完成ZIPを別ディレクトリに展開してP0試験を行う。開発用target内のbare executableを試験するだけでは不足する。テスト用に通常Codex・Node・開発用環境変数を見せず、必要なOS変数は維持する。

変更対象のCI path filterへ`crates/ai/**`、配布lock/inventory、取得スクリプト、mac bundle、release workflowを含める。資産がないために試験をskipした場合は合格としない。P0の機能PRでCargo版を勝手に上げず、既存のリリース手順で配布する。

## 7. P1: 取得・展開・適用

### 7.1 取得元の信頼

P1aはレビュー済みの埋込lockだけからrecommendedを取得する。P1bの独立更新はGitHub Releases上の固定入口`ai-runtime-catalog-v1/catalog.json`と`catalog.sig`を使う。このtag/資産は**新規実装する公開物で、現在存在するとは扱わない**。

カタログは最大256 KiB。Ed25519のdetached signatureを**取得したJSONの生bytes**に対して検証後にparseする。JSONの再整形や独自canonicalizationを署名入力にしない。署名はbase64の64 bytes、key IDを持ち、公開鍵はHane本体に埋め込む。暗号実装を自作せず既存ライブラリを使う。

カタログにはschema、channel=stable、単調増加sequence、issued_at、expires_at、entriesを置く。entryはtarget、Hane version range、bundle/manifest URL・サイズ・hash、runtime/config/state profile、許可する更新元bundle IDとrollback先ID、検証証拠URLを持つ。署名済みでもHaneが知らないschema/profileや不明な更新元には適用しない。

最高受理sequenceより古いもの、期限切れ、未来時刻が5分を超えるもの、未知key ID、署名不一致は更新不可にする。カタログには最大30日の有効期限を設け、時計異常なら説明付きで停止する。これは更新の制限であり既存版での編集/AI起動を止めるものではない。

署名鍵はmaintainerが生成して管理する。秘密鍵はリポジトリへ置かず、保護されたrelease環境だけで使い、カタログ検証/対象OS試験後に明示公開する。鍵未設定中はP1bを利用不可と表示するがP0/P1aは動く。新たな常駐サービスや複数チャンネルは初期に作らない。

元URLはHTTPSかつ`github.com/openai/codex/releases/download/`または`github.com/hide212131/hane/releases/download/`の承認資産に限定する。GitHubの資産配信先へのHTTPS redirectは既知のGitHub資産ホストだけ許し、最大5回、認証header/cookieは送らない。signed URLのqueryをログに残さない。loopback/private IPへのredirectや任意hostは拒否する。

### 7.2 展開と有限処理

ダウンロード上限512 MiB、総展開上限2 GiB、最大10,000項目、manifestのファイル数/サイズ以内とする。HTTP接続20秒、無進捗60秒、全体15分を上限とし、有限の失敗をUIへ返す。これは内部timeoutであり利用者への完了予測ではない。ネットワーク再試行は利用者操作にし、永久retryをしない。

展開は新しい同一ボリュームのstagingだけへ行う。絶対パス、`..`、drive prefix、Windows ADS、予約名、末尾dot/space、正規化/case-insensitive衝突、特殊デバイス項目を拒否する。既存root/親に予期しないsymlink/reparseがあれば停止する。

通常はregular file/directoryのみを許可する。上流の承認inventoryに内部相対symlinkが実在する場合だけ、その正確なlink項目を許可できる。リンク先が同一payload内の検査済み通常ファイルであること、循環/連鎖がないこと、展開中にリンクを辿らないことを検査し、通常ファイル展開後に作成する。Windowsではreparseを作る必要のない公式Windows内容を前提に試験し、未知のlinkを追加承認しない。任意Packageにこの例外を流用しない。

cancelではダウンロードと新規ファイル作成を止め、自分のstagingだけを消す。検査済みcandidateは保存しても自動適用しない。解凍器が指定外の場所へ書かないことを攻撃fixtureで確認する。

### 7.3 healthと適用の順序

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
| R01 | 2 targetの圧縮hash/source commit/content inventory/entrypoint一致 |
| R02 | 全補助物と必要LICENSE/NOTICEを完成配布物から検査。欠落で失敗 |
| R03 | Codex/Node/PATH上書きなしで完成ZIP/.appからinitialize/停止 |
| R04 | 環境変数の明示パス、相対パス拒否、無言fallbackなし |
| R05 | 欠損・破損・target相違・アクセス拒否・不正exe・初期化失敗を分類 |
| R06 | AI異常でも編集/保存/設定画面が動く |
| R07 | Custom模擬先へ固定input/model/key、選択文AI回帰。秘密は記録しない |
| R08 | 周辺AGENTS/config/Skills/他アプリ環境変数の不混入 |
| R09 | Haneと通常Codexの同時ログイン/片方のログアウト/再起動の非干渉 |
| R10 | Windows/macOSで実ChatGPTログインと固定接続確認。模擬と別記録 |
| R11 | Windows/macOSで実Custom接続確認。利用者がキーを画面へ直接入力 |
| R12 | exact-head CI/レビュー、2 OS GUI、完成配布物の再検証 |

### P1

| ID | 必須試験 |
|---|---|
| U01 | 未導入→固定recommended導入→エディタ再起動なしの接続開始 |
| U02 | 同版修復が新installationに入り、実行中exeを上書きしない |
| U03 | 正常署名カタログ、改ざん/未知鍵/schema/profile/期限/sequence拒否 |
| U04 | 不適合Hane version/target/許可されていない更新対を拒否 |
| U05 | HTTP断・timeout・不足容量・hash不一致・展開上限・cancel |
| U06 | traversal/link/reparse/drive/ADS/case collisionの攻撃fixture |
| U07 | AI中・ログイン中・別Hane所有時の適用拒否。文書編集は継続 |
| U08 | pointer更新前/後、初期化後の各crashをjournalから復旧 |
| U09 | Windows file lock、stop timeout、遅延callbackで二重起動なし |
| U10 | 検証済み旧→新→旧でChatGPT/Custom状態を保つ |
| U11 | 認証/通信/利用枠エラーでは自動downgradeしない |
| U12 | .app/システムPATH/他Codex/Skills/プロンプトを変更しない |
| U13 | 署名鍵なしでもP0/P1a可、P1bのみ不可。権限外URLへ接続しない |
| U14 | 2 OS実画面、exact-head CI/レビュー、公開catalogと取得物の一致 |

各IDはpass/fail/not-runを区別する。CIの模擬試験は実アカウント試験の代替にしない。実アカウント試験に必要なログイン・キーがなければ、そのIDを未実施と明記しIssueを閉じない。模擬試験までの実装やPRを放棄する理由にはしない。

証拠は`docs/ai-runtime-evidence/<phase>-<head>/README.md`へ、OS/CPU、Hane exact head、配布hash、Codex versionとhash、実行コマンド/終了コード、対象/対象外、画面操作結果を保存する。本文・キー・token・生認証headerは含めない。

## 11. 実装順序と完了の境界

P0は(a)固定lock/inventory/license、(b)resolver/診断、(c)配布処理、(d)完成物試験の順。P1aは(a)専用保存/fixture、(b)取得・health・切替、(c)画面、(d)競合/復旧。P1bは(a)catalog検証・署名補助、(b)公開手順、(c)互換対試験、(d)実画面と配信検証の順。

設計PRには製品コード、バイナリ、秘密鍵、CI成功の捏造、リリース操作を含めない。実装PRが開発運用ポリシーの変更を必要とすると判断した場合は、配布修正へ混ぜず理由を別途提示する。

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
