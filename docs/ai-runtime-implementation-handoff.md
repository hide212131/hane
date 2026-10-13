# Issue #479 実装担当への引継ぎ

作成日: 2026-10-13 JST。**この文書群は設計だけであり、Hane/Codexの実装・ビルド・実機試験はまだ行っていない。**

## 1. 最初の作業

**最初は[#480](https://github.com/hide212131/hane/issues/480)だけを実装する。** 目的は、Codexを別途導入していないPCで、Haneの完成配布物からAIの接続操作を始められるようにすること。更新・Skills・汎用Pluginを同じ実装PRへ入れない。

設計ブランチは`docs/479-managed-codex-runtime-design`。親は[#479](https://github.com/hide212131/hane/issues/479)。資料は次の順に読む。

1. [ADR-0033](adr/0033-managed-codex-runtime-and-extensions.md): 方針と初版の対象範囲。
2. [配布・更新詳細](ai-runtime-distribution-implementation-spec.md): まずP0、第2〜6節とR01〜R12。
3. [Skills/Package詳細](ai-extensions-implementation-spec.md): P0では境界だけ確認し、実装しない。

調査基準mainは`af981629011f04f68b25c8b58e476eb3eeffa624`、公開版v0.26.0は`5c2e703334c66c09575b549030aa1ccb913920dd`。実装開始時にはmain、対象Issue、設計PR head/状態、関連PR、AGENTS.md、現在の実装・レビュー手順を再取得する。このSHAを未来のcurrent headとして扱わない。

設計PRが未マージの場合、その最終headの文書を参照していることを記録する。無断で製品コードを設計ブランチへ積まない。設計に重要な異論があれば該当節と影響をIssueへ一度にまとめ、設計変更と実装差分を追跡可能にする。

## 2. Issueと依存関係

| Issue | 対象 | 開始条件 | 完了する利用者操作 |
|---|---|---|---|
| [#480 / P0](https://github.com/hide212131/hane/issues/480) | 同梱・位置解決・診断 | 設計とcurrent codeを照合 | 配布物だけで専用Serverを起動し、AI接続を開始 |
| [#481 / P1](https://github.com/hide212131/hane/issues/481) | 導入・修復・独立更新 | P0のresolver/manifestが確定 | ボタンから導入/修復/検証済み版更新 |
| [#482 / P2](https://github.com/hide212131/hane/issues/482) | 文章指示SKILL.md | P0が利用可能。P1は必須依存でない | 専用フォルダー→承認→選択文AI |
| [#483 / P3](https://github.com/hide212131/hane/issues/483) | SkillsのみのローカルPackage | P2の一覧/registry/snapshotが確定 | 複数スキルの導入/更新/削除 |

個人開発ではP0→P1→P2→P3を推奨する。P1の署名カタログ公開や将来のネイティブPlugin仕様が未整備でも、P0の配布修正は止めない。

## 3. P0の具体的な実装順

### A. 実際の公式完全パッケージを確認する

詳細設計第2節に記載した2 targetの固定assetを取得し、hash、source tag/commit、entrypoint、全内容と必要ライセンスを確認する。macOS bare serverでの過去試験は今回のWindows/完全パッケージの合格にしない。

`assets/ai/codex-runtime-lock.json`、`assets/ai/inventory/<target>-0.157.1.json`、取得/検査スクリプトを作る。実ファイルの一覧と表示条件が確定してから、同梱物をコードへ組み込む。第三者表示やOS依存物が不足していれば正確に報告し、latestや他アプリ内のバイナリで穴埋めしない。

### B. locatorと診断を実装する

`main.rs`内の隣接ファイル選択を`runtime_distribution.rs`へ移し、同梱manifest/明示開発パスを型で区別する。いずれも絶対パスとし、PATH探索を作らない。relative HANE_STATE_DIRやoverrideも明確なAI設定エラーとして扱う。

missing、integrity、target、access、invalid executable、initializeの分類をUIへ渡す。未導入を「ログインしていない」と誤診しない。Windowsではchild起動時に不要なコンソールウインドウが出ないことも実画面で確認する。

### C. 完成配布物を作る

WindowsのstagingとMacの.appへ完全パッケージ/manifest/ライセンスを追加する。Haneの既存Explorer連携用ファイルは残す。完成ZIPを新しい場所へ展開し、そこから実行する試験を追加する。配布条件をCI path filterへ反映し、runtime未配置で試験がskipされたケースを合格にしない。

### D. 機能と非干渉を確認する

R01〜R12をpass/fail/not-runに分ける。`cargo test -p hane-ai --locked`、`cargo test --workspace --locked`、変更箇所のformat、`git diff --check`を実行する。全体formatの既存差分を直すためだけに無関係なファイルを変更しない。

新規スクリプトのテストはローカルfixtureを用意し、破損/不正path/サイズ超過/不足ファイルを再現する。成功コマンド・終了コードは実際に実行した値だけを記録する。

WindowsとmacOSで、通常Codex未導入の起動、実ChatGPTログイン、実Custom接続、通常Codexとの非干渉を分けて記録する。実アカウント/資格情報が与えられていなければ該当試験だけ未実施とし、実装や模擬試験の結果を捨てずに残す。ユーザーのauth.jsonをコピーして試験を通さない。

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
| Windows x64 | 完成ZIP、補助物、ファイルロック/停止/誤exe、GUI、実認証、通常Codexとの同時利用 |
| macOS arm64 | 完成.app、非書換え更新、実行属性/配布保護、GUI、実認証、通常Codexとの同時利用 |
| CI/ローカル自動試験 | manifest/展開/署名/状態遷移/競合/模擬provider/入力payload/回帰 |

一方のOSでの成功をもう一方へ転記しない。既存のテストに合格しても、追加した機能の実画面や完成配布物を未確認ならそのまま残す。

## 7. 報告・PR・完了条件

実装担当は対象Issueごとに別ブランチ/PRを作り、変更ファイル、試験IDの結果、残る阻害要因、試験したexact headと配布hashをまとめる。独立した失敗を混ぜず原因ごとに直し、意味のないコメントや同じレビュー依頼を繰り返さない。

実装PRの現在head/base、CI、current-head review、non-outdated unresolved thread、必要なGUI証拠を現在のリポジトリ手順に従って確認する。試験したcommitと後続commitを取り違えず、未実行のheadに結果を付け替えない。

設計PRは`Refs #479`とし、`Closes #479`や実装子Issueの自動close指定を付けない。P0/P1/P2/P3の各実装・検証が完了するまではIssueをopenに保つ。リリース番号変更・配布は既存の仕組みで別途行い、今回の設計作成をリリース済みと表現しない。
