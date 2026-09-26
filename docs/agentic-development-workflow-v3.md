# AI Agent Development Workflow v3 設計書

## 1. 適用状態と改訂

初期設計はIssue [#332](https://github.com/hide212131/hane/issues/332) / PR #334。本書と[ADR-0031](adr/0031-aadw-v3-jev-bounded-execution.md)は、利用者の追加指定をIssue [#336](https://github.com/hide212131/hane/issues/336)で反映した。実装・評価・有効化は[#333](https://github.com/hide212131/hane/issues/333)で追跡する。

**ChatGPTアプリ（旧Codexアプリ）を操作と指揮の中心にする。実装はGitHub Actions経由のClaude Codeを優先し、条件付きでCodexへ切り替える。通常レビューはCodeRabbit、必要なGUIテストはv2の既存経路を使う。Jevは依頼解釈から次のactionまで通常の意味判断を担当する。このJev運用は有効であり、残るv3統合は個別に進める。**

Jev判断の運用方法は[Commander Policy](aadw-command-policy.md)と[AGENTS.md](../AGENTS.md)を正とする。工程はv2から引き継いだone-action規則を維持する。通常レビューはIssue #360の実run検証とIssue #373の切替後はCodeRabbitを使い、Codexは条件付き代替実装担当として残す。既存fallbackとGUIの権限・判断規則はそれぞれの既存契約を維持する。

## 2. 記事との関係と役割

記事からは生成・判断・検証の分離と、実装の区切りでJevを呼ぶ考え方を採る。記事のLuna/Sol優先をHaneの必須経路にはしない。「81%」「4〜6回」「confidence 0.95」、月20ドルやトークン数は記事の仮説・例であり、Haneの性能・費用保証ではない。[S1]

| 担当 | v3での役割 |
|---|---|
| ChatGPTアプリ内Commander | current factsを取得し、Jevに渡す。Jevの意味判断に従い、客観的な権限・head/base・必須証拠を確認して次の一actionを実行する。GitHubを観測し、最終受入の事実を確認する |
| アプリから使う連携手順・小さな補助ツール | Jev呼出し、回答検査、許可済みactionの実行、GitHubの結果取得をつなぐ。別の司令塔や判断規則の正本にはしない |
| GitHub Actions | 承認済みの依頼を受け、権限・対象を検査し、workerやGUIを起動する。実装指示を独自に考えない |
| Claude Code | 通常の実装担当。限定したコード変更とテストファイルの追加・修正を行う |
| Codex | 条件付きの代替実装担当。アプリ内Commanderとは別の役割・実行環境とする |
| CodeRabbit | 通常のコードレビューを担当し、指摘と対象範囲を返す。修正や次工程の指示、mergeを担当しない |
| Jev | 依頼解釈、受入条件、範囲、原因分類、作業分解、検証計画、次のaction、継続・切替・完了候補を判断する。Choice / Noul / Scoreと、利用可能な場合は既存handlerを選ぶfunction callingを使う |
| CI / GUI Validator | 実際の検査・画面操作を行い、対象contextと観測事実を残す。結果の免除やmergeを決めない |

通常は「アプリで依頼 → Jevが判断・既存handler・閉じた引数を選択 → Commanderがガードを確認して実行 → CI → CodeRabbit → 必要なGUI → 最終受入」と進める。失敗や証拠不足があれば最新事実をJevに渡し、次の一actionを選ばせる。全工程は条件なしで連鎖させない。Jevの結果を採用できる通常判断をChatGPTに毎回やり直させない。

## 3. アプリ中心の実行と継続範囲

利用者はアプリで「Issue #123を進めて」と依頼する。Commanderが承認済みGitHub接続でcurrent factsを取得し、Jevが作業分解、許可候補、既存handlerと閉じた引数を選ぶ。Commanderは客観的権限と対象を確認してhandlerを実行し、既存ツールから実装・レビュー・GUIを依頼する。アプリを使うことは、アプリ内Codexを製品コードのwriterにすることではない。[S7]

Jevは既存のTypeSafe接続とアプリ側のJev実行手段から呼び出す。Skillや設定文は使い方を示し、応答そのものの代わりにはしない。独立した常駐オーケストレーター、Hane製品へのApp Server内包、新しい操作画面は初期要件にしない。

Jev判断はアプリ側の実行セッションで行う。アプリ終了・セッション中断後もJevやCommanderが動き続けるとは仮定しない。再開時はGitHubのrun・review・head/baseを読み直し、Jevに新しいcurrent factsを渡して次の判断を得る。同じ依頼を重複発行しない。中断中の無人の工程連鎖や通知は別途の作業とする。

## 4. 依頼の入力とGitHub Actions

一件分の依頼にはrepository、Issue/PR、expected head、target branchと必要なbase、受入条件のID、不変条件、許可する責務・パス、禁止パス、検証コマンド・GUI scenario、実行設定の版と明示された停止条件を含める。費用上限は利用者から求められない限り加えない。これはHane内部の契約であり、Jev APIのフィールドではない。

現行の[Claude workflow](../.github/workflows/claude-fix.yml)はPR Conversationの`@claude`、単独行の`AADW_COMMANDER_HANDOFF_V2`、一つの`AADW_TARGET_HEAD: <40桁SHA>`を受ける。依頼者のwrite/maintain/admin権限、openのsame-repository PR、exact headを検査する。Issue本文へ書くだけでは起動しない。v3を作る間はこの既存入口を使い、v3用の新しいマーカーを存在するものとして扱わない。

現行Claude workerはRead/Edit/Write/Glob/Grepだけで編集し、シェル・テスト実行・git・pushを行わない。変更は別のfinalizerが検査してpushし、そのcurrent-head CIを最初の検証とする。v3連携でもこの隔離を緩めず、テストファイルを書いたことをテスト成功と報告しない。

アプリからのコメントがどのactorで投稿されるかと、そのactorが受理されることを実際のrunで確認する。Actionsから別Actionsへ依頼する場合、標準の`GITHUB_TOKEN`で作った`issue_comment`は後続workflowを起動しない。GitHub App等の承認済み認証か、対応する明示的dispatchを使い、受信側の権限検査も確認する。[S8] CodeRabbitという外部Appのコメント受理はこれと別に検証する。コメント投稿成功だけでは依頼受理としない。

## 5. Claude優先とCodexへの切替

| 状況 | 現行とv3での扱い |
|---|---|
| Claudeの利用上限・呼出制限 | 現行は`usage_or_rate_limit`に限り、[既存fallback](../.github/workflows/codex-usage-limit-fallback.yml)がsource run/attempt・checkpoint・依頼者・exact headを検査してCodexへ渡す |
| 同じ原因の実装が進展しない | Jevが差分・検証・review・試行内容から継続、修正、切替、停止を判断する。許可された候補にない切替や既存trust boundaryの変更は選べない |
| 認証・権限の失敗、head変更、実行環境の故障、原因不明 | Codexへ変えれば解決するとは扱わず、停止・再観測・原因確認を行う |

レビュー指摘が出たことだけではClaude失敗としない。同じroot causeの修正をまとめ、通常は同じ担当が続ける。切替後は一件の作業が終わるまでCodexを担当とし、往復切替や二重writerを避ける。新しい担当を起動する前に旧担当の終了を確認し、未反映patchは元の対象headと対応するものだけを検査して引き継ぐ。

v3の通常担当はClaudeであり、Jevが初手から自由にLuna/Solを選ぶ設計にはしない。Codex代替経路内のモデル・推論量は利用可能で承認済みの組合せだけを実指定する。要求した設定、確認できた実行設定、バージョン、利用量を記録する。候補がなければ停止し、既定モデル・別provider・別課金へ黙って切り替えない。

App Serverは代替経路の実装で必要性が確認されたときの選択肢にとどめる。採用する場合は`model/list`、`turn/start`の`model`/`effort`等を固定版で検証する。[S5] アプリ中心の運用を開始するために独自App Server実行器を先に作る必要はない。

## 6. CodeRabbitによる通常レビュー

CodeRabbit GitHub Appの導入、対象repository、契約・権限、full/incremental reviewの実際の依頼受理はV3-3 / Issue #360で確認済みである。Issue #373の切替後はCodeRabbitを標準レビューとする。CodeRabbitが応答しない、スキップした、対象が不明な状態を「指摘なし」と扱わない。

| PRコメント | 用途 |
|---|---|
| `@coderabbitai full review` | 初回、全体見直し、最終候補の全体レビュー |
| `@coderabbitai review` | 修正後の新しい差分に対する途中の確認 |

公式仕様では`review`は増分、`full review`は全体を対象とする。自動レビューを無効にしても手動依頼できる。[S9][S10] v3では安定候補とCIを確認して明示的に依頼し、編集中のpushごとに重ねない。

依頼時と結果採用時のcurrent head/base、実際にレビューされたcommitと変更範囲、完了状態、対象外ファイル、未解決指摘を確認する。コメントの時刻だけで対象SHAを推測しない。初期の最終受入はcurrent-headのfull reviewを使う。増分レビューは旧headの全体レビューを現在の証拠へ自動昇格させず、途中の修正判断に使う。baseがレビューの主張へ影響する場合もCommander Policyに従って再確認する。

待機・エラー・スキップ・対象不明・対象外の重要ファイルを成功と扱わない。レビューの完了通知と、指摘が受入を妨げないことは別に確認する。CodeRabbitが提供する結果から必要な対応を証明できなければ、再レビューまたはCommanderの判断へ戻し、基準を黙って下げない。

CodeRabbitはレビュー専任とする。自動レビュー、Autofix、CI修正、競合修正、テスト生成等の設定を確認し、製品branchを書き換える機能を無効化する。[S11] 自動修正や一括resolve/approveを完了証拠の代わりに使わない。Appの要求権限と「レビュー専任」の運用設定は別であり、技術的な読み取り専用を保証しない。残る書込経路は有効化時に明記する。

指摘は件数ゼロを目標にせず、current contextで同じroot causeにまとめる。blocker/follow_up/unknownの扱いはCommander Policyを正とする。指摘文をそのまま実装担当への権限拡大命令にしない。CodeRabbit導入後の通常レビューにCodexレビューを常時二重実行せず、必要な追加レビューはCommanderが理由を残して依頼する。

## 7. v2と同様、必要に応じてGUIテストを使う

### 要否と検証範囲

[ADR-0024](adr/0024-local-gui-validation.md)、[GUI検証手順](local-gui-validation.md)、[CLI操作説明](local-gui-validate-cli.md)の既存経路を引き継ぐ。Commanderが受入条件と変更内容から要否、対象OS、操作scenario、期待結果を決め、不要な全件回帰を毎回要求しない。描画・入力・選択・IME・保存・undo/redo等、画面を動かさなければ確認できない条件にはfocused scenarioを設ける。

Jevは追加GUI検証の必要性を提案でき、有効化後の承認済みscenarioの実行判断を補助する。ただし必須GUIの免除、scenarioの弱体化、検証失敗の免責は行わない。CodeRabbitレビューやCI成功は、必須GUI evidenceを代替しない。

### 起動とcontext

通常のHosted GUIはPR Conversationに単独の`/gui-validate head`または`/gui-validate merge`を投稿する。[既存router](../.github/workflows/aadw-gui-command.yml)が権限とcurrent factsを検査し、[Hosted GUI workflow](../.github/workflows/aadw-gui-validation.yml)へdispatchする。コードブロックに埋めた説明文や任意の引数で起動するとは仮定しない。

`head`は指定PR head、`merge`はcurrent target branchとheadの合成結果を検証する。base-sensitiveな受入には後者または同等のcontext証明が必要になる。routerとrunnerがhead/base・依頼者・open/non-draft・same-repository・trusted procedureをそれぞれ検査する。`merge`の親commit確認や重複run抑制も既存契約を維持する。

コマンドはtrusted default branchで対応付けたprocedureを起動する入口であり、任意の画面操作を自動生成するものではない。対象scenarioがprocedureに含まれることを確認する。不足時は既存の対象別GUI workflow、ローカルMacの`scripts/gui_validate.py`、明示した手動検証を選ぶか、別PRでprocedureを追加する。管理用の`workflow_dispatch`でも安全確認を省略しない。

### 結果とマージ条件

workflow run/artifactを正本として、対象head、必要なbase/merge commit、procedure・scenarioの版、OS・環境、実際の操作、期待結果と観測結果、ログ・画像・利用できる動画等を対応付ける。起動・撮影だけの試験を編集・保存・IMEの成功へ拡張しない。macOSの成功をWindowsの検証済みとせず、ローカルCLIのhead snapshotもbase-sensitiveなmerge contextの証拠へ読み替えない。

`fail` / `blocked`なら製品の回帰、受入条件の不成立、検証環境の不具合、既存の独立問題、unknownを分ける。既存問題と判断するには比較可能なbaseline evidenceを使う。必要な操作が実行できない、画像やcontextが足りない場合はunknownであり、passやfollow_upとしてmergeしない。

GUI Validatorは観測のみを担当し、コード変更・次工程・mergeは決めない。結果の最終解釈とbase-independentの判断はCommanderが行う。headが変われば取り直し、base変更はscenarioへの影響を確認する。初期v3でも必要としたGUIがcurrent contextで受け入れ可能になるまでmergeしない。Computer Useは補助手段であり必須ではなく、他セッションの承認流用や安全設定緩和を前提にしない。

## 8. Jevの接続・質問・回答の組合せ

公式JavaScript SDKは`@typesafe-ai/sdk` / `TypeSafeClient.systemOne()`。HTTPは`POST /v1/systemone`で`state`、`questions`、`model`を受け、`model`、`answers`、`usage`を返す。[S3][S4] APIキーは`TYPESAFE_API_KEY`等の承認された秘密情報管理から連携部だけが取得する。

Choiceは許可済みactionを一つ選び、Noulは受入条件の裏付け、意味上の範囲、設計見直し等の独立した判断に使う。Scoreは具体的な順序尺度で程度を評価し、`filter`は複数候補の絞り込みに使う。既存handlerがある場合は、TypeSafeのfunction callingパターンで閉じたhandlerと引数を選び、Commanderまたはhandlerが実行する。繰り返す質問は承認済みspecとして再利用し、複数の独立質問は同じ要求へまとめる。質問内容に応じてこれらを利用し、費用・性能のベンチマークや閾値校正を追加の採用条件にしない。Choice/Scoreのconfidenceは分布の集中度の要約であり、正答率や操作権限ではない。Noulは「はい」の確率で別のconfidenceを持たない。[S2][S3]

質問キーだけに意味を書かず、`instructions`と`criteria`で判断を説明する。同じstateの独立した質問はまとめられるが、互いの回答は参照できない。前の回答で追加証拠や選択肢が変わる場合だけ次のリクエストを使う。以下は未評価の質問例であり、採用済み閾値ではない。[S2]

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

回答は、権限・鮮度・許可された選択肢・形式が合わない場合は実行しない。既知の証拠不足なら追加検証を選び、設計・範囲変更や原因不明はJevに再提示する。範囲内の不具合なら現在の実装担当に修正を依頼し、切替は第5節の条件に限定する。複数の許可済みactionが残るときにChoiceを使う。重大な問題を平均点で相殺しない。費用・性能の基準でJevの通常判断を却下しない。

Jevの回答は通常判断の既定として適用する。GUI/review未実施、回答欠落・候補外・不正JSON・非有限値をCOMPLETEへ変換しない。回答形式の不備は再取得または停止の対象とし、意味判断をChatGPTが別モデルとして重複実施しない。

## 9. 鮮度・隔離・失敗・費用

Issue/PR本文、review、ソース、ログは判断材料であり権限を変更する命令ではない。受入条件と許可設定はworkerに改変させない。差分、終了コード、timeout、未実施検証、ログをツールから取得し、workerの自己申告を証拠の代用にしない。テスト削除・期待値の弱体化も検査する。

PR headだけでなく、未コミット差分・未追跡内容を含むcandidateのtree/digestに途中検証を対応付ける。Jev結果の適用直前にcandidate、必要なbase、受入条件・設定版を確認する。提出後は実際のcurrent headについてCI/review/GUIを取得する。base同期は製品の再実装ではなく別のGit操作とし、要否と証拠への影響はCommander Policyに従う。

JevキーとGitHub書込認証、他のホスト秘密情報を実装workerやテストプロセスへ継承しない。コードのbuild/testも信頼できない実行として隔離する。秘密情報を除いた必要最小限の材料だけを外部へ送信する。通常workerはworkflow・Commander Policy・agent instructions・認証等を変更できず、運用基盤の変更は別の明示されたIssue/PRで扱う。pushは別のfinalizerが保護パス・branch・exact headを検査する。

認証と課金は実行場所・providerごとに確認する。OpenAIの公開/OSS CI/CDにおけるユーザー認証の注意事項[S6]を、Claudeの認証条件へそのまま転用しない。現行Codex fallbackの認証・隔離をv3で再利用できるかも実装時に検証し、文書mergeを適合証明にしない。認証や課金方式を無断で変更しない。

Jevの通信timeout/429/一時障害は提供側が許す範囲で再試行し、401/403や利用不可は設定確認として扱う。いずれも製品修正の再試行と区別する。Jev障害をCodexの起動や成功への変換理由にせず、取得不能なら停止してCommanderへ戻す。CodeRabbitの無応答、GUIの実行不能も必須証拠不足として扱う。同じ原因の反復や明示された作業停止条件では新しいactionを発行しない。利用者が指定していない費用上限は設けない。

`COMPLETE`は実装完了候補でありmerge権限ではない。最終受入ではCommanderがcurrent head/base、CI、review、必要なGUI、mergeabilityを確認し、expected headを指定してmergeする。

実行・試行単位に担当、モデル、認証方式、request/run/attempt、質問/Policy版、入力・出力・取得できるcache利用量、時間、結果を記録する。Jevのusageは請求金額ではない。[S3] サブスクリプション利用量、実費、API参考換算額を分け、失敗・中断・旧headの費用も含める。親総量と子内訳を二重加算せず、アプリCommander・Claude・Codex・Jev・CodeRabbit・CI・GUIの計測範囲とunknownを示す。

GitHubを進行状態の正本とする。監査・費用履歴は残せるが、古い選択やCOMPLETEを再開時のcurrent factsとして使わない。専用DBやダッシュボードは初期要件ではない。

## 10. 運用範囲と残る移行作業

[実装・評価計画](aadw-v3-execution-plan.md)に従い、既存Actions、レビュー、GUIの移行を進める。Jevの意味判断は有効で、費用・性能の追加評価をその条件としない。通常コード・テストの実装担当と、運用基盤の変更担当は分ける。保護されたworkflow等まで通常workerへ一括依頼しない。

CodeRabbitの導入・actor受理・full/incremental reviewの範囲確認はV3-3で完了し、Issue #373で通常レビューへ切り替える。残る作業には、Codex代替実装経路の利用可能設定・認証、必要なGUI scenarioとOSの実行環境、停止・再開時の運用がある。未確認の実行経路や必須証拠を成功扱いしない。

<a id="sources"></a>
## 出典と確認範囲

初期確認は2026-09-25、構成改訂は2026-09-26。記事の提案、公式契約、Hane独自要件を区別する。GUI/Claudeの現行事実は上記のrepository文書・workflowを参照した。これは今回GUIを実行した証拠ではない。

- [S1] Rahul氏の記事（利用者提供本文）: https://x.com/sairahul1/article/2102694818485096803 。記事のLuna/Sol優先をClaude優先へ変更するのは利用者指定に基づくHaneの判断。
- [S2] TypeSafe公式Skill: https://github.com/typesafe-ai/skills/blob/main/skills/typesafe-ai/SKILL.md 。初期確認blob `0109513f9656917dc93cbc5ecddfca465a53ce66`。判断形式、質問の独立性、confidenceを参照。
- [S3] TypeSafe公式SDK型定義: https://github.com/typesafe-ai/typesafe-sdk-js/blob/66880ccded6cb642dc1809620c2b108c33730214/src/types.ts 。入出力、usage、再試行・ログを参照。
- [S4] 同README: https://github.com/typesafe-ai/typesafe-sdk-js/blob/66880ccded6cb642dc1809620c2b108c33730214/README.md 。
- [S5] OpenAI App Server: https://developers.openai.com/codex/app-server/ 。初期設計の調査先。独立実行器の採用を必須にしない。
- [S6] OpenAI非対話実行: https://learn.chatgpt.com/docs/non-interactive-mode 。Codexの認証注意事項を再確認。
- [S7] ChatGPT desktop app: https://learn.chatgpt.com/docs/app 。アプリ利用の公式案内。本書のAADW連携が既に備わることや無人継続を保証する出典ではない。
- [S8] GitHub workflow起動: https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow 。`GITHUB_TOKEN`と後続イベントを確認。
- [S9] CodeRabbitコマンド: https://docs.coderabbit.ai/reference/review-commands 。増分・全体レビューの区別を確認。
- [S10] CodeRabbit自動レビュー: https://docs.coderabbit.ai/configuration/auto-review 。明示的依頼との関係を確認。
- [S11] CodeRabbit設定: https://docs.coderabbit.ai/reference/configuration 。レビューと自動修正機能を区別。

TypeSafeのIntroduction/confidence本文は取得環境で直接確認できなかったため、確認できた公式Skill/SDKの範囲を採用する。CodeRabbitのfull/incremental review接続はIssue #360の実runで確認済みである。TypeSafeの追加仕様、費用・精度、個別機能のGUI実動作は各検証記録の範囲を超えて一般化しない。
