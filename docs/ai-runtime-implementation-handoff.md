# Issue #479 実装担当への引継ぎ

作成日: 2026-10-13 JST。**この文書群は設計だけであり、Hane/Codexの実装・ビルド・実機試験はまだ行っていない。**

## 1. 最初の作業

**最初は[#480](https://github.com/hide212131/hane/issues/480)だけを実装する。** 目的は、Codexを別途導入していないWindows/macOSで、設定画面のボタンから固定版をダウンロードし、Hane専用領域へ安全に初回導入できるようにすること。初回AI利用はネット接続必須とする。Haneの配布物にはruntime実行ファイルを入れず、修復・署名カタログ更新・Skills・汎用Pluginを同じ実装PRへ入れない。

設計ブランチは`docs/479-managed-codex-runtime-design`。親は[#479](https://github.com/hide212131/hane/issues/479)。資料は次の順に読む。

1. [ADR-0033](adr/0033-managed-codex-runtime-and-extensions.md): 方針と初版の対象範囲。
2. [配布・導入詳細](ai-runtime-distribution-implementation-spec.md): まずP0、第1〜7節・第9〜11節とR01〜R13。
3. [Skills/Package詳細](ai-extensions-implementation-spec.md): P0では境界だけ確認し、実装しない。

調査基準mainは`af981629011f04f68b25c8b58e476eb3eeffa624`、公開版v0.26.0は`5c2e703334c66c09575b549030aa1ccb913920dd`。実装開始時にはmain、対象Issue、設計PR head/状態、関連PR、AGENTS.md、現在の実装・レビュー手順を再取得する。このSHAを未来のcurrent headとして扱わない。

設計PRが未マージの場合、その最終headの文書を参照していることを記録する。無断で製品コードを設計ブランチへ積まない。設計に重要な異論があれば該当節と影響をIssueへ一度にまとめ、設計変更と実装差分を追跡可能にする。

## 2. Issueと依存関係

| Issue | 対象 | 開始条件 | 完了する利用者操作 |
|---|---|---|---|
| [#480 / P0](https://github.com/hide212131/hane/issues/480) | 固定版の初回ダウンロード・導入・診断 | 設計とcurrent codeを照合 | 設定画面から取得・検証し、Hane専用領域へ導入してAI接続を開始 |
| [#481 / P1](https://github.com/hide212131/hane/issues/481) | 専用版の修復・復旧・署名カタログ更新 | P0が利用可能 | ボタンから破損修復・既知正常版への復旧・検証済み版更新 |
| [#482 / P2](https://github.com/hide212131/hane/issues/482) | 文章指示SKILL.md | P0が利用可能。P1は必須依存でない | 専用フォルダー→承認→選択文AI |
| [#483 / P3](https://github.com/hide212131/hane/issues/483) | SkillsのみのローカルPackage | P2の一覧/registry/snapshotが確定 | 複数スキルの導入/更新/削除 |

個人開発ではP0→P1→P2→P3を推奨する。P1署名鍵の準備や将来のネイティブPlugin仕様が未整備でも、P0の初回導入は止めない。
## 3. P0の具体的な実装順

### A. 固定した公式完全パッケージと再配布条件を確認する

詳細設計第2節の2 targetについて固定assetを取得し、source tag/commit、archive size/SHA-256、内容inventory、entrypoint、必要なライセンス表示を確認する。macOS bare serverでの過去試験は完全パッケージやWindowsの証拠にしない。ライセンス監査で未確認の第三者物が残る場合、解決まで配布物の受入を止め、確認済みのように扱わない。

`assets/ai/codex-runtime-lock.json`、`assets/ai/inventory/<target>-0.157.1.json`、レビュー済みLICENSE/NOTICE資料を作る。これらは小さな固定メタデータと表示資料であり、runtime executableをHaneの配布物へ追加しない。実際の上流内容とライセンス範囲が確定する前に、想定の補助プログラム名や文書をでっち上げない。

### B. 初回ダウンロード・導入・診断を実装する

起動時の自動通信やPATH探索を追加せず、AI設定画面の「インストール」操作を入口にする。lockに固定したGitHub Release assetだけをHTTPS取得し、source/redirect host、サイズ、SHA-256、展開inventory、target、entrypointを検査する。失敗はnetwork/hash/size/content/license/target/access/initializeなど原因別に表示する。

payloadはHane app-data内の新しいstagingへ展開し、絶対パス・traversal・symlink/reparse・衝突・容量超過を拒否する。専用の一時CODEX_HOMEと空cwdでversion/initialize/停止を確認し、認証情報や通常HOMEを渡さない。成功後のみ不変のinstallationとして公開してactive pointerを更新する。処理中キャンセル、途中失敗、次回起動時の中断状態もIssue #480の契約に合わせる。

`main.rs`の隣接server選択をHane管理版のresolverへ集約する。明示的な絶対開発pathは型で区別し、異常時に管理版へ無言fallbackしない。相対`HANE_STATE_DIR`、未導入、破損、target不一致、権限不足、実行形式不正、initialize失敗を異なる診断にする。Windows childにはconsole windowを作らない起動フラグを適用し、完成配布物から確認する。

### C. Hane配布物をruntimeなしで検証する

Windows ZIPとmacOS `.app`は小さなlock/inventoryと必要な表示資料だけを含み、runtime実行ファイルを含まないことを確認する。完成配布物を新しい場所へ展開し、Codex/Node/PATH上のCodexがないオンライン環境から「インストール」を操作して導入・初期化できることを2 OSそれぞれで記録する。Haneの既存Explorer連携用ファイルは維持する。

配布工程の変更は必要最小限にし、`.github/**`またはworkflow保護ルールを変える必要がある場合は、Haneの専用ops手順に従う。権限やレビュー条件を弱めて通さない。runtime未配置をskip扱いにするCIは合格根拠にしない。

### D. 機能・非干渉を確認する

R01〜R13をpass/fail/not-runに分ける。`cargo test -p hane-ai --locked`、`cargo test --workspace --locked`、変更箇所のformat、`git diff --check`を実行する。全体formatの既存差分を直すためだけに無関係なファイルを変更しない。

fixtureで破損archive、hash/size不一致、悪意あるpath、リンク/reparse、容量超過、redirect拒否、ネットワーク失敗、キャンセル、activation中断を再現する。成功コマンド・終了コードは実際に実行した値だけを記録する。

WindowsとmacOSで、完成配布物からの初回導入、実ChatGPTログイン、実Custom接続、通常Codexとの非干渉、no-consoleを分けて記録する。実アカウント/資格情報が与えられていなければ該当試験だけ未実施とし、実装や模擬試験結果を捨てない。ユーザーのauth.jsonをコピーして試験を通さない。
## 4. P1実装時の補足契約

候補healthで既存coordinatorを再利用するときも、通常runtimeを起動したまま候補をspawnしない。AIがidleであることを確認し、既存の所有guard・設定ロック順序を維持して通常childを停止する。health用CODEX_HOMEを使った検査・停止後、適用しない場合/失敗した場合は同じguardのもとで旧configへ戻す。restoreに失敗したら失敗を可視化する。ネットワークダウンロード中はこの所有権の切替を行わない。

health/activationの排他処理は同じサービス内へ集約し、owner lockを再取得する補助関数を増やさない。`RuntimeConfig`だけを書き換え、`configured_settings_generation`の整合確認を省略しない。候補検査中/適用中の新規AI操作はbusyを返すが、文書編集は可能にする。

catalog署名ファイルの形式は`{"schema_version":1,"key_id":"…","algorithm":"ed25519","signature_base64":"…"}`とする。Haneはkey IDに対応する埋込公開鍵で、取得したcatalog.jsonの生bytesを検証する。test鍵はfixtureだけ、production秘密鍵は保護release環境だけで扱う。test鍵や空鍵をproductionの代替にしない。

カタログ検査や候補導入が失敗しても、使用中の正常runtimeのAvailable/Readyは失わせない。既存`AiSnapshot.runtime_state`と更新操作の失敗状態を分け、更新失敗を理由に正常なAI接続を無効化しない。未導入/不正な現行版の場合だけ接続操作を止める。

## 5. Skills/Packageで判断を戻さない点

最初のSkillsは**Haneが文章指示としてSKILL.md本文を添える限定対応**。現行固定版で未確認のネイティブ読み込みAPIは使わない。ネイティブ`type:skill`へpathを送るだけ、`skip_host_skill_discovery=false`へ変更するだけの実装は採用しない。

初期PackageはSkillsのみのローカル集合で、Codex/ChatGPT汎用Pluginではない。scripts等を勝手に無視して「導入成功」にしない。READMEやUIにもこの対象範囲を書く。

選択文AIの既存の任意ツール使用を一律禁止する変更も行わない。モデルが提供済みツールを使うことと、Packageに含まれるスクリプトをHaneが自動実行することを区別する。固定接続確認へSkillsや選択文を送らない。

## 6. 検証の環境分担

| 環境 | 主に実施すること |
|---|---|
| Windows x64 | runtimeなしの完成ZIP、設定画面からの導入、展開/アクセス/停止/誤exe、no-console、GUI、実認証、通常Codexとの同時利用 |
| macOS arm64 | runtimeなしの完成.app、設定画面からの導入、実行属性/配布保護、GUI、実認証、通常Codexとの同時利用 |
| CI/ローカル自動試験 | manifest/展開/署名/状態遷移/競合/模擬provider/入力payload/回帰 |

一方のOSでの成功をもう一方へ転記しない。既存のテストに合格しても、追加した機能の実画面や完成配布物を未確認ならそのまま残す。

## 7. 報告・PR・完了条件

実装担当は対象Issueごとに別ブランチ/PRを作り、変更ファイル、試験IDの結果、残る阻害要因、試験したexact headと配布hashをまとめる。独立した失敗を混ぜず原因ごとに直し、意味のないコメントや同じレビュー依頼を繰り返さない。

実装PRの現在head/base、CI、current-head review、non-outdated unresolved thread、必要なGUI証拠を現在のリポジトリ手順に従って確認する。試験したcommitと後続commitを取り違えず、未実行のheadに結果を付け替えない。

設計PRは`Refs #479`とし、`Closes #479`や実装子Issueの自動close指定を付けない。P0/P1/P2/P3の各実装・検証が完了するまではIssueをopenに保つ。リリース番号変更・配布は既存の仕組みで別途行い、今回の設計作成をリリース済みと表現しない。
