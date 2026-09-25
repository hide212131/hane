# AI Agent Development Workflow v3 設計書

## 1. 適用状態と読み方

初版の設計Issue [#332](https://github.com/hide212131/hane/issues/332) / PR #334を、利用者の追加方針に従い[#335](https://github.com/hide212131/hane/issues/335)で改訂する。[ADR-0031](adr/0031-aadw-v3-jev-bounded-execution.md)を設計判断、[実行計画](aadw-v3-execution-plan.md)を実装・評価手順とする。実装・評価・有効化は[#333](https://github.com/hide212131/hane/issues/333)で追跡する。

**文書のmergeではv3を有効にしない。現行は[v2](agentic-development-workflow-v2.md)と[Commander Policy](aadw-command-policy.md)であり、Claude優先・利用上限時だけのCodex実装・Codexレビューを維持する。** 以下は将来の設計要件。有効化PRで委譲範囲をCommander Policyへ統合し、AGENTSと同時に更新する。今回の文書PRのレビューも現行経路で行う。

本書のChatGPTアプリは、利用者が「旧Codexアプリ」と呼ぶ操作窓口を指す。アプリ内のAIをCommanderと呼び、代替実装workerであるCodex CLI等とは区別する。特定のアプリ版・プランの機能を名称だけで利用可能と仮定しない。

## 2. 記事から採る考え方とHaneの選択

記事はCodexアプリの設定を入口に、生成をLuna/Sol、限定判断をJev、事実確認をテスト等に分離する。[S1] Haneでは分離を採り、利用者の方針に従って生成担当をClaude優先・条件付きCodexに、通常レビューをCodeRabbitに変更する。これは記事の実装そのものではなくHane独自の設計である。

初版の「独立したCodex App Server実行器から始める」と「Luna/Solを通常の実装経路にする」は撤回する。アプリから使う既存GitHub接続と、Jevを呼ぶ小さな連携機能を出発点にする。独自画面、Hane製品への組込み、常駐司令塔、進行状態DBは初期必須にしない。

記事の81%、1タスク4〜6回、confidence 0.95、月20ドルは仮説・例でありHaneの保証値ではない。モデルID、可用性、料金、推論量を記事から固定しない。テスト成功は実行したテストの事実であり、要求全体の正しさの証明ではない。

## 3. アプリを中心にした一件の流れ

```text
利用者 → ChatGPTアプリ内Commander
             ├─ 目的・範囲・受入条件・予算・検証を確定
             ├─ GitHubのcurrent factsを取得
             ├─ アプリから使える連携機能 → Jevの限定判断
             │
             ├─ 承認済み実装指示 → GitHub Actions → Claude Code
             │                                    └─ 許可条件時だけCodex
             │                         ↓ 変更を検査・反映
             │                         CI / 必要なGUI検証
             ├─ 安定したheadへレビュー指示 → CodeRabbit
             │
             └─ GitHubの結果を再観測 → Jevの限定判断を採用
                       ├─ 同じ原因をまとめて現在の実装担当へ修正指示
                       ├─ 追加証拠・検証を取得
                       ├─ 範囲・設計変更、不明な障害はCommanderで判断
                       └─ 完了候補 → Commanderが最終受入・merge
```

矢印は役割間の受け渡しを示し、現行workflowが全工程を自動連鎖することを意味しない。一つのactionを実行したら結果を観測する。有効化後は、承認済み通常判断をJevへ委譲し、アプリ側の手順が許可されたactionへ結び付ける。毎回Commanderが同じ意味判断をやり直す構成にはしないが、例外・最終受入は委譲しない。

Actionsは承認済み依頼の実行場所であり、指示を考える主体ではない。CodeRabbitの指摘文をそのまま実行命令にしない。Jevはコードや修正指示の文章を生成せず、具体的な実装指示はCommanderが目的・制約・現在の証拠から作る。

## 4. アプリからの指示、接続、待機と再開

アプリ側の手順は、default branchのCommander Policyを読み、GitHub接続でIssue/PR、current head、target branch、CI、レビュー、run/artifactを必要な範囲で取得する。PRがない作業では先に承認された変更範囲を整理し、ブランチと差分を持つPRを用意する。現行Claude workflowはIssue単体のコメントでは起動しない。

現行の[Claude workflow](../.github/workflows/claude-fix.yml)は、PRの`issue_comment`にある`@claude`、独立した`AADW_COMMANDER_HANDOFF_V2`行、一つの`AADW_TARGET_HEAD: <40桁SHA>`行を検査する。write以上の依頼者、openなsame-repository PR、expected headの一致が必要。この既存契約を使う間は現在の形式を守り、v3という文書名だけで新しい起動markerを発行しない。

アプリ側からのコメント投稿を初期の標準入口にする。Actionsから別Actionsを起動する必要が生じた場合、`GITHUB_TOKEN`で投稿したコメントの`issue_comment`は新しいworkflow runを作らない。[S9] 承認済みGitHub App認証、または明示的なdispatch等を採用する際は、起動先の対応、actor検査、expected head、権限を検証する。現行workflowがdispatchを受け付けるとは仮定しない。外部CodeRabbit Appへのコメント配送とは分けて試験する。

再利用する手順はアプリのskill等に置けるが、skillは接続や権限そのものではない。[S5] Jevについては「アプリ→利用可能なツール/小さなhelper→Jev→型付き回答」という実呼出しを確認する。型・候補・鮮度を検査する部分だけをコードにし、巨大な制御サービスにしない。実装方式はアプリの対応機能と既存構成を調べて選ぶ。設定欄に「Jevを使う」と書いただけでは接続試験を通過しない。

起動済みActionsの実行・既存の限定fallbackと、次の依頼を発行するアプリ側の処理は別。アプリ終了後に全工程が無人で進むとは保証しない。会話が終わる場合は対象PRとrun、待っている結果を残す。再開時はGitHubから取り直し、実行中jobや未反映patchを確認してから次の一手を選ぶ。アプリを閉じることはリモートjobの取消でもない。バックグラウンド継続を追加する場合は、別途対応機能・認可・重複防止を確認する。

## 5. 実行入力とClaude→Codexの切替

Commanderが承認する入力は、repository、Issue/PR、expected head、必要なbase、受入条件ID、不変条件、変更可能/禁止パス、必須検証、実装担当と許可モデル・認証方式、Policy/質問版、総試行・時間・通信再試行・利用量の上限を含む。これらはHaneの契約であってJev APIのフィールドではない。

通常はClaude Codeへ実装指示を出す。現在のworkerはRead/Edit/Write/Glob/Grepでコードとテストファイルを編集し、shell、テスト実行、git、pushは行わない。別の信頼された反映処理が変更を検査・pushし、current-head CIを最初の検証とする。v3のためにこの隔離を緩めず、「Claudeがテストファイルを書いた」と「テストが実行成功した」を区別する。

| 状況 | 切替の扱い |
|---|---|
| Claudeの`usage_or_rate_limit` | [既存fallback](../.github/workflows/codex-usage-limit-fallback.yml)のsource run/attempt、checkpoint、依頼者、head等を検証した場合だけCodexが引き継ぐ |
| Claudeが実装したが同じ原因で進展しない | 新しい委譲条件。差分・失敗・修正履歴・予算から初期はCommanderが判断する。評価・有効化後だけJevの候補を採用できる |
| 認証失敗、権限不足、head不一致、モデル利用不可 | 設定・権限・鮮度を確認して停止または再観測。無断の別認証・別課金への切替はしない |
| 検証環境の故障、Jev/CodeRabbit障害、原因不明 | 製品実装の失敗と分け、追加証拠かCommander判断へ戻す。Codex切替で隠さない |

実装が行き詰まったことを`usage_or_rate_limit`へ偽装しない。新しい切替経路は別の明示した条件・起動契約として実装する。レビュー指摘があるだけではClaude失敗ではない。まず同じ原因をまとめて現在の担当に修正させる。

切替前に旧workerの終了と未反映変更を確認し、checkpointの出所・対象head・内容を検証する。push後なら新しいcurrent headを対象にする。同じbranchは一人のwriterとし、Codexへ切り替えた作業は原則としてそのまま完了まで担当させる。Claudeへ自動的に戻して往復させない。他のPRの並行開発は妨げない。

Codexは代替実装担当であり、アプリ内Commanderとは実行場所・権限を分ける。採用するCLI等の版、実際のモデル指定と取得可能なusageを記録し、promptのモデル名だけを実指定の証拠にしない。Luna→Solの段階は標準条件にしない。App Serverを使う場合も代替経路の選択肢として別途検証する。

## 6. CodeRabbitへのレビュー指示と受け入れ

v3の通常レビュー担当はCodeRabbitとする。GitHub AppのHaneへの導入、契約・利用上限、対象repo、依頼actor、外部送信の許可は未確認であり、実装段階の前提確認とする。設定・接続が完了していなければレビュー未実施であり、無断で省略や他サービスへの切替をしない。

CIを確認した安定候補に、アプリからGitHub接続を使ってPRコメントを投稿する。Actionsに配送を任せる方式は追加選択肢であり、レビュー処理自体はCodeRabbit側が行う。初回は`@coderabbitai full review`、修正後の新しい差分は`@coderabbitai review`を使う。[S7] これらのコマンド自体にexpected headを固定する機能があるとは仮定せず、依頼前後と結果採用時に対象を照合する。

設定は、自動レビューとpushごとの自動差分レビューを止め、明示的依頼を標準にする。labelや本文keyword等の別の自動起動条件、対象除外も確認する。[S10] 日本語レビュー、状態・検査範囲の表示、エラーの識別を設定時に確認する。自動修正・docstring/テスト生成・CI修正・競合解消などの変更機能を無効化し、自動承認を最終受入に使わない。[S8] 今回は`.coderabbit.yaml`やApp設定を変更しない。

| 確認対象 | 受け入れるための条件 |
|---|---|
| 出所と対象 | 信頼するCodeRabbit Appの結果で、現在のheadとの対応が取れる。reviewのcommit IDやcheckのhead等、実際に取得できる情報を使う。依頼コメント内のSHAだけでは証明にならない |
| 実行完了 | 待機・実行中・error・skipped・契約/上限による未実施を区別する。コメントがないことや緑の状態だけを「問題なし」にしない |
| 検査範囲 | 変更ファイルの除外・省略・打切りを確認する。incremental reviewは新規差分の検査であり、それだけをPR全体の検査と扱わない |
| 指摘 | 現在の未解決指摘を同じ原因ごとに整理し、blocker/follow-up/unknownをPolicyに沿って扱う。件数ゼロを目的にしない |

incremental後に現在のPR全体の確認を主張するには、最新結果からその範囲を裏付けられる必要がある。過去headのpassを自動的に継承せず、対応や範囲を証明できなければcurrent headのfull reviewを依頼する。重要なbase変更も同様に再評価する。識別方法やレスポンス形式はHaneの実PRで確認するまで未達とする。

指摘の修正は現在の実装担当へ戻し、CodeRabbitには作らせない。CodeRabbitが終わったという事実と、受入条件を満たしたという判断は別である。通常のCodexレビューを外すのはCodeRabbit経路を実証した有効化PRからとし、本改訂では既存レビューを残す。

## 7. Jevの接続契約と小さな判断

公式JavaScript SDKは`@typesafe-ai/sdk`、`TypeSafeClient.systemOne()`を提供し、`state`、`questions`、`model`を扱う。HTTPは`POST /v1/systemone`、結果は`model`、`answers`、`usage`を持つ。[S3][S4] APIキーはhelper等の秘密情報として扱い、アプリの会話・設定文・AGENTS・workerへ出さない。アプリの環境変数が必ず使えるとは仮定せず、採用するツールの安全な設定方法で確認する。

| 形式 | 意味 | 初期用途 |
|---|---|---|
| Choice | 選択肢から一つ、各確率、confidence | 許可済みの継続・修正・追加検証・担当変更候補 |
| Noul | 「はい」の確率。別のconfidenceはない | 受入条件ごとの裏付け、意味上の範囲、設計判断の必要性 |
| Score | 定義した段階の評価値と分布 | 初期は使わない |

confidenceは分布の集中度の要約で、全工程の正しさ・正答率・権限ではない。計算式は確認範囲にない。[S2] Noulの低値は証拠不足も含み、モデルを強くする根拠に直結しない。重大な問題を他の良い評価との平均で相殺しない。

質問キーはモデルへの説明ではない。内容は`instructions`、基準は`criteria`に書く。同じstateの独立質問はまとめられるが、同じリクエスト内の他の回答は参照できない。先の回答で証拠や候補を作り直す場合だけ次の呼出しを行う。[S2]

次はHane向けの質問例であり、実測済みprompt・閾値ではない。

```json
{
  "type": "noul",
  "instructions": "state.acceptance_criterion が state.diff と state.verification によって十分に裏付けられているかを判断する。",
  "criteria": {
    "true": "差分と検証結果が受入条件を十分に裏付ける。",
    "false": "反する証拠がある、または十分な裏付けがない。"
  }
}
```

アプリ側はCodeRabbit結果も出所・対象・指摘ID付きの判断材料として渡す。file readのたびにJevを呼ばず、実装・検証・レビューの区切りで、機械的確認の後に残る意味判断へ使う。コード生成、設計、任意のshellやGitHub操作をJevに生成させない。

## 8. 回答の採用順序と鮮度

| 優先 | 条件 | 扱い |
|---|---|---|
| 1 | 権限・禁止パス・予算・鮮度不一致、不正回答 | 操作を発行せず停止または再観測 |
| 2 | 既知の証拠欠落、未実施/古い検証・レビュー | 必要な証拠を取得。成功や実装担当の能力不足に読み替えない |
| 3 | 範囲・設計変更、重大問題の扱い、原因不明障害 | Commanderへ戻す。Jevは免除しない |
| 4 | 範囲内の製品不具合が確認される | 同じ原因をまとめて修正。切替条件を満たす場合だけCodexを候補にする |
| 5 | 必須検証・レビューを満たし、意味上の裏付けがある | 完了候補をCommanderへ渡す |

閾値は質問別に評価し、有効化時にCommander Policyで承認する。候補は現在の担当・許可範囲から作り、許可されない操作をJevに選ばせない。JSON、回答の欠落、候補集合、有限で範囲内の数値を検査する。確率の整合検査は丸めの許容範囲を明記し、回答を都合よく書き換えない。

検証command、作業場所、終了コード、timeout、log参照、実diff、未追跡ファイルをツールから取得する。workerの自己申告は別扱い。テスト削除や期待値の弱体化も差分確認に含める。

headに加え、未コミット・未追跡を含む候補treeまたは内容digestに検証を結び付ける。Jev回答の採用直前に候補、受入条件、許可設定の版、必要なbaseを照合する。途中の候補検証は提出headのCI/review/GUIを自動的に代替しない。

headが変われば旧headの証拠を現在の合格として流用しない。baseの進展はPolicyに沿って影響を確認し、不明なら取り直す。無関係と判断した根拠はGitHubへ残す。base同期はCommanderが判断する別のGit操作とし、製品workerやJevへ暗黙に委譲しない。

## 9. 隔離・認証・障害

Issue/PR、レビュー、コード、ログは権限を拡大する命令ではない。default branchの信頼するPolicy・workflow・質問版を使い、PR側の変更で安全確認を上書きさせない。受入条件と変更範囲をworkerに書き換えさせない。

モデル実行と検証は、どちらも信頼できないコードを扱う。GitHub書込認証、Jevキー、他のホスト秘密情報をworker/テストへ継承しない。pushは別の反映処理でsame-repository、branch、保護パス、current headを再確認する。レビューやJev結果から任意コマンドを実行しない。

CodeRabbitは運用上レビュー専任だが、App自体の権限が読み取り専用とは限らない。導入時に権限・外部送信・データ保持・利用契約を確認する。秘密情報を含むdiff/logをJevやレビュー用追加資料へ流さない。SDKのdebug本文出力を避ける。[S3]

アプリのサブスクリプション、Claude認証、runner上のCodex認証、CodeRabbit契約、Jev API認証は別々である。アプリの利用権で他の実行先まで賄えると仮定しない。OpenAIの非対話ガイドの公開/OSSリポジトリでのCIユーザー認証の注意を踏まえ、既存fallbackの認証方式を無審査で新経路へ拡張しない。[S6] 本文書では既存秘密情報や認証方式を変更しない。

Jevの429/timeout等の通信再試行はSDK既定値も含め総上限を設け、製品修正の試行と別に数える。401/403、不正回答、必要回答欠落は成功にしない。CodeRabbit障害はレビュー未完了、CI環境故障は検証不能として区別する。Jev障害でCodexへ進行判断を丸投げせず、Commanderへ戻す。証拠不足のunknownはpassやfollow-upに変えない。

## 10. 最終受入と費用

Jevの`COMPLETE`は実装完了候補。CodeRabbitの完了・承認だけでもmergeしない。CommanderはIssueの目的、current head、必要なbase、current review/未解決指摘、required CI、必要なGUI、mergeabilityを確認し、expected headを指定してmergeする。保存・undo/redo・入力等の品質基準を下げない。必須検証をconfidenceで免除しない。

呼出し/run単位で、担当・モデル・認証方式・質問/Policy版・取得可能な入出力/cache利用量・時間・結果を記録する。Jevのusageは請求金額ではない。[S3] 実費、サブスクリプション利用量、API単価の参考換算を分け、不明は`unknown`とする。失敗・中断・再試行・旧headを除外せず、親合計と子内訳を二重加算しない。

Commander、Claude、Codex、Jev、CodeRabbit、CI/GUIをどこまで計測できたか明示する。指標は受入完了あたりの費用・時間・再試行と誤完了。artifactと完了時の集計から始め、独自ダッシュボードや進行DBは作らない。

## 11. 実装・評価・移行

[実行計画](aadw-v3-execution-plan.md)をアプリ→既存Actions、CodeRabbit接続、Jevの接続・評価、限定有効化に改める。v3自体の実装もClaudeへ依頼できるが、workflow、agent instructions、Commander Policy等は通常workerの保護対象のまま、明示した運用基盤PRで扱う。

アプリからのJev実呼出し、CodeRabbit導入/契約/actor対応/対象head識別、実推論の評価、閾値と予算、認証・隔離は未確認。文書mergeで実装済みと報告しない。#333はopenのまま残し、必要な証拠とPolicy/AGENTS更新がそろうまで現行v2を維持する。

<a id="sources"></a>
## 出典と確認範囲

2026-09-25確認。記事、公式の契約、利用者の方針に基づくHaneの改訂を区別する。アプリ中心・Claude優先・CodeRabbitという組合せの動作は未検証である。

| ID | 出典・確認範囲 |
|---|---|
| S1 | [Rahul氏の記事](https://x.com/sairahul1/article/2102694818485096803)。利用者提供本文。アプリ設定、分業、例・仮説を参照し、全文や性能保証は転載しない |
| S2 | [TypeSafe公式Skill](https://github.com/typesafe-ai/skills/blob/main/skills/typesafe-ai/SKILL.md)。初版確認blob `0109513f9656917dc93cbc5ecddfca465a53ce66`。型付き判断、質問の独立性、confidenceの説明を継承 |
| S3 | [公式JavaScript SDK型定義](https://github.com/typesafe-ai/typesafe-sdk-js/blob/66880ccded6cb642dc1809620c2b108c33730214/src/types.ts)。入出力、usage、再試行・ログの契約 |
| S4 | [同SDK README](https://github.com/typesafe-ai/typesafe-sdk-js/blob/66880ccded6cb642dc1809620c2b108c33730214/README.md)。パッケージと呼出し方法 |
| S5 | [OpenAI: Plugins in ChatGPT and Codex](https://help.openai.com/en/articles/20001256-plugins-in-chatgpt-and-codex)。skillと接続・権限の区別、利用条件。アプリの無人継続の保証には使わない |
| S6 | [OpenAI: Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)。CI等の認証の注意。アプリ中心の構成とworker認証を分ける |
| S7 | [CodeRabbit: Review commands](https://docs.coderabbit.ai/reference/review-commands)。manual full/incremental review。コマンド自体にexact-head指定があるとは主張しない |
| S8 | [CodeRabbit: Configuration](https://docs.coderabbit.ai/reference/configuration)。状態表示、検査範囲、変更機能・自動承認の設定。導入時は実際の版と設定で再確認 |
| S9 | [GitHub: Triggering a workflow](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow)。GITHUB_TOKEN由来のissue_commentと別workflow起動の制約 |
| S10 | [CodeRabbit: Auto review](https://docs.coderabbit.ai/configuration/auto-review)。自動起動・差分レビュー・除外と手動依頼の関係 |

TypeSafe Introductionとconfidenceページの直接取得は本環境ではできなかった。初版で確認した公式Skill/SDKの契約を継承し、実API確認は未実施として扱う。製品の名称や機能の利用可否を記事の日付表示だけで判定しない。
