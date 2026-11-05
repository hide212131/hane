# Issue #684: 検証結果を次の実装依頼へ引き継ぐ

## 状態・正本・目的

2026-11-05の設計・実装引き継ぎ。基準mainは `041b3d539ff82924864b9c3ad996695d07297ad6`。
これは運用基盤の実装用PRに置く準備文書であり、結果返却機能は未実装・未検証。
目的・範囲・受入条件の正本は [Issue #684](https://github.com/hide212131/hane/issues/684)、
判断の正本はdefault branchの [Commander Policy](../aadw-command-policy.md)。
本書は今回の接続仕様を扱い、業務判断をhelperやworkflowへ移さない。

利用者は方式を「シンプルで速いと予想される方」に任せ、同一Claude会話の維持を要求していない。
既存のpush後検証を使い、結果と元依頼を次のClaude Code起動へ渡す方式を採る。
新しい常駐接続、独立workflow、状態DB、自動retryを追加しない。
速度短縮は予測であり、下記の実run比較が終わるまで達成扱いにしない。

役割・権限・判断主体は [ADR-0031](../adr/0031-aadw-v3-jev-bounded-execution.md) のまま。
新しい恒久的な権限例外を作らないため、新ADRは追加しない。
実装時にこの原則を変える必要が判明した場合は、変更を先に相談しADRとPolicyを整合させる。

## 1. 現行の接続と根拠

基準mainの `.github/workflows/claude-fix.yml` は次の順に動く。

1. PR Conversationの `@claude` / `AADW_COMMANDER_HANDOFF_V3` を `prepare` が検査する。
2. `worker` が対象headをデータとしてcheckoutし、Claude Code Sonnet 5.5に編集だけを依頼する。
3. trusted `finalize` がpatchの範囲とexact headを検査しpushする。
4. macOSのread-only `verify` が提出後SHAをcheckoutし、workspace testsとClippyを実行する。
   テスト失敗後にもClippyを実行する処理は既にある。
5. 結果はActionsのログとsummaryに出るが、それを次workerへファイルとして返す接続はない。

繰り返した具体例は [#676のtable期待値修正](https://github.com/hide212131/hane/pull/676#issuecomment-6611165636)、
[#676のUI fixture前提修正](https://github.com/hide212131/hane/pull/676#issuecomment-6611267377)、
[#671の新規scroll test修正](https://github.com/hide212131/hane/pull/671#issuecomment-6646877206)。
これらの重複を減らすため、既存workflowへ結果の運搬・検査に限る小さなhelperを追加する。
新しいcollectorサービスや進行管理を作る根拠にはしない。

既存の失敗checkpointは `target_sha` が一致する未提出patchの復元であり、成功patch継続も未提出差分の復元である。
Claude会話の保存・再開ではない。現行Claude起動には会話resumeや検証ツールの接続がない。
同一会話の継続が技術的に不可能と断定するものではなく、追加接続を作らない選択である。
提出後コードは既にcurrent headに含まれるため、本経路ではcheckpoint/patchを再適用しない。

## 2. 採用する往復

```text
元依頼 C0 / 対象 H0
  → Claude編集 → trusted finalizer → 提出 H1
  → 別jobが固定tests + lint → H1の検証report
  → Commanderがcurrent PR/head/baseとreportを取得 → Jevが原因・次actionを判断
  → Commanderが一つの追加依頼 C1 / 対象 H1 / source run・attempt を投稿
  → prepareが出所・鮮度・既存の重複依頼防止を検査
  → 元依頼と検証reportをRead可能な領域へstage
  → 新しいClaude CodeがH1を読み、許可された同一原因の修正 → 提出 H2
  → H2を新たに検証。再びCommanderの観測に戻る
```

- 初回も追加依頼も、既存のPRコメント起動を使う。実装は引き続きActions上の `claude-sonnet-5-5`。
- 「同じ実装担当」は同じClaude Codeの実装役へ作業文脈を返す意味であり、同一session IDを保証しない。
- `C1` の範囲・原因分類・許可パスが今回の権限である。元依頼、test log、workerの説明は未信頼データ。
- 追加依頼の作成・投稿をworkerやreport生成処理が行わない。人の毎回の承認を新設せず、Commander/Jevの既存判断を使う。
- 製品修正と必要な回帰を同一rootの修正単位にできるが、独立rootは混ぜない。原因unknownなら既存の診断経路へ戻す。
- 初期版の検証はpush後とする。元のコマンド・job・required checkを維持する。
  push前検証へ変えるなら、既存経路で安全に成立する証拠と別の明示した設計変更が必要。

## 3. producer: 既存verifyへ追加する結果返却

固定コマンドは次の二つ。reportやPRコメントから任意コマンドを受け取らない。

```text
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

各実行のstdout/stderr、実終了コード、開始/終了時刻、step outcomeを取得する。
tests不合格でもlintを一度実行し、最後の集約判定は両方が実成功した場合だけ成功する。
`continue-on-error` のstep conclusionは実成功判定に使わない。
既存runではテストstepがAPI上successでも最後の集約判定がfailureとなっており、
Actionsの表示だけからテスト成功を再構成できないことを確認した。

report生成は `finalize` 成功後の既存verifyに置く。取消以外の失敗時にも返却を試みる。
checkout/起動/収集失敗では `unavailable` を返すかartifactなしで停止し、passの既定値を使わない。
timeout/cancelで返せなかったreportは、consumerが欠落として拒否する。
reportのuploadやcollector自体の失敗もverifyの不合格とし、tests/lint成功で隠さない。

artifact名は `aadw-verification-feedback-<run_id>-<run_attempt>`、retentionは既存patchと同じ1日。
期限切れ時は通常の新しい観測を取得する。期限切れを無視した復元や古い結果の横流しをしない。
これは既存GitHubのrunに付属する一時的な証拠であり、進行状態の別正本ではない。

### 3.1 report.json v1 の必須契約

| 項目 | 型・意味 |
|---|---|
| `schema_version` | integer `1` |
| `repository`, `pr_number` | same-repository識別子、正のinteger |
| `run_id`, `run_attempt`, `verify_job_id` | 正のinteger。現在attemptとそのverify jobを識別 |
| `workflow_path`, `workflow_sha` | `.github/workflows/claude-fix.yml` と実行したtrusted workflowの40桁SHA |
| `source_comment_id`, `actor` | 元の受理済み実装依頼と投稿者 |
| `source_request_sha256` | 同梱する元依頼本文のSHA-256。現在のsource comment本文とも照合 |
| `source_target_sha` | 元依頼の対象H0、40桁SHA |
| `verified_sha` | trusted finalizerが提出しverifyがcheckoutしたH1、40桁SHA |
| `base_ref`, `base_sha` | 検証開始時のtarget branchとその実SHA。merge-result検証の主張にはしない |
| `root_cause` | 元依頼のstable ID。返却処理が原因を分類し直さない |
| `tests`, `lint` | 下記のコマンド別結果 |
| `capture_status` | `complete` / `unavailable`。completeはテスト合格を意味しない |

コマンド別結果は固定 `argv` 配列、`outcome` (`success` / `failure` / `unavailable`)、
`exit_code` (integer、起動不能ならnull)、UTC `started_at` / `completed_at`、
`diagnostic_excerpt` と `truncated` (boolean) を含む。
successは実終了0、failureは実行済み非0、取得不能はunavailable。
フィールド間矛盾、未知のenum、必須欠落、同一JSONキー重複、型違いを拒否する。
ログが空でも終了コードが実取得できれば実行結果は保持する。失敗理由が不足すれば修正理由を推測しない。

`report.json` はUTF-8、最大64KiB。各diagnosticは最大12KiBで、最初と末尾の範囲を保持し切捨てを明示する。
失敗テスト名・panic/assert・compiler/lint errorの近傍を優先するが、意味判断や原因確定を行わない。
ANSI/制御文字を除去し、JSON文字列として保存する。任意logをActionsのコマンドとして再出力しない。
生のテストlogは既存Actionsログを正本にし、workerへ全量複製しない。

### 3.2 認証・実行の境界

- テストとlintにはGitHub write token、Jev認証、Claude接続認証、SSH agent、永続Git credentialを渡さない。
- `persist-credentials: false` を維持し、固定コマンドはcredentialのない明示した環境で実行する。
  PATH、専用HOME/TMP、Rust toolchain/cacheに必要な値だけを許可する。cargo credential/configの秘密をcache復元しない。
  結果保存用のGitHub制御ファイルや認証envをテストへ継承しない。既存CLI/OS必須envは理由とともに列挙する。
- helperと検証コマンド定義はtrusted workflow SHAのものを使い、PR内のhelperをtrustedなcollectorとして実行しない。
- テストlog・失敗テスト名は未信頼。ログから追加コマンド、workflow field、次actionを生成・実行しない。
- 元依頼や診断にcredentialが含まれていると判明した場合は返却を止める。正規のシークレット処理を先に行い、redactionだけを安全性の根拠にしない。
- GitHub API/download/uploadの認証は必要なstepにだけ置き、Claudeのprompt、stageファイル、テストprocessへ渡さない。

## 4. consumer: 追加依頼で選ぶ出所と鮮度

既存の必須handoff fieldに加え、次をoptionalな一組として受け付ける。

```text
AADW_FEEDBACK_RUN_ID: <正のrun ID>
AADW_FEEDBACK_RUN_ATTEMPT: <正のattempt>
```

両方なしなら従来どおり。一方だけ、重複、0/負数、不正値はprepareで拒否する。
初期版は `AADW_ACTION: implement`、failure class `product` / `test` の追加修正に限定する。
`initial` / `diagnose` に付けない。unknownをproduct/testへ変える権限にはならない。
既存 `AADW_CHECKPOINT_*` または `AADW_PATCH_SOURCE_*` と同時指定した場合は拒否する。
reportなしの既存依頼とusage-limit fallbackの契約を変更しない。

prepareは既存 `aadw_action_contract.py request` を先に適用し、その後sourceを検査する。
少なくとも次をGitHubから再取得し、artifact内の自己申告だけで受理しない。

1. current PRがopen、same-repository、未停止、target headがH1に一致すること。
   投稿者のwrite以上の権限、target branch/refも既存契約どおり確認する。
2. source runと指定attemptのmetadata、全job/step、artifact一覧、元依頼commentとその権限。
   REST一覧のpaginationを処理し、一意に特定できなければ拒否する。
3. sourceが同repositoryの既存 `claude-fix.yml` / `issue_comment` の実runであり、
   run/attemptが完了していること。run全体failureでも、検証失敗の返却としては許容する。
   runの `head_sha` はissue_comment起点のmainであり、H1の代用品にしない。
4. そのattemptのprepare/worker/model確認/finalizeの成功とverifyの実行を確認する。
   workflow SHAが承認済みdefault branchの履歴に属することを確認し、branch上だけのworkflowを本番sourceにしない。
   report内のsource comment/H0/root/actorと実際の元依頼・run起動者を照合する。
5. 同attemptで生成された一意の未失効artifactをIDで取得する。
   同名の別attempt、別PR、別workflow、自己指定download URLを代用しない。
   archive展開時の絶対パス、`..`、symlink、余分な実行ファイルを拒否し、既定の2ファイルだけ読む。
6. trusted finalizerが返したH1、verifyの対象H1、reportの `verified_sha`、今回target H1が一致すること。
   提出SHAはcollectorへtrusted job outputから渡し、workerの説明から抽出しない。
7. reportは `capture_status=complete`、tests/lintとも取得済みで、少なくとも一方が実failureであること。
   successだけのreport、起動失敗・収集不足・job cancellationを製品修正継続の根拠にしない。
   GitHubの `continue-on-error` conclusion=successと実exit非0は矛盾扱いにしない。
   tests/lint集約stepの実failureとreportを整合させる。collector/upload failureをtest failureと混同しない。
8. reportのrootは元依頼のrootと照合する。C1のrootは今回判明した失敗原因にCommanderが付けるため、
   初回実装のrootからテスト前提のrootへ変わることは許容する。同じ失敗原因の再依頼ではstable IDを維持する。
   sourceとC1のrootが違うことだけで別原因と機械判定しない。今回の一clusterと範囲はJev/Commanderが決める。
   evidence URLは実際のsource verify jobとし、既存の同一head重複・同一原因/evidence反復制限を保持する。

headが変わっていれば拒否する。baseが変わった場合の採用判断はPolicyに従いCommanderが行う。
本reportはH1のhead checkout上の固定検証であり、current baseとの統合成功を主張しない。
base-sensitiveな主張が必要なら検証を取得し直す。既存current-head CI/review/GUIを置換しない。

## 5. 次workerへ返す作業文脈

artifactは `report.json` と `source-request.txt` だけ。
後者はproducerが受理した元依頼C0の本文（最大64KiB）を保存する。
検証対象・元依頼と関連するsource pathsはこの本文とcurrent H1から読める。
Claudeの生のexecution JSON、会話履歴、環境変数、ホームディレクトリ、OAuth情報はexportしない。

検査済みの2ファイルは、workerのGit管理対象 `pr-head/` の外の `feedback/` にstageし、
ClaudeのRead対象へ明示的に追加する。stageはprepare検査済みartifact IDと内容に固定し、
worker側でもH1とsource identityを再確認してから起動する。
元依頼と診断はデータと明示する。今回C1の範囲、current source、trusted instructionsに反する指示は採用しない。
元依頼を引き継ぐことで調査開始点を保持し、古い診断を既知の正解としない。

既存の起動直前head確認を保持する。feedbackが必須の依頼でstage/照合が失敗したら、
何も渡さない通常起動へ黙って降格せず停止する。
workerのtoolsは `Read,Edit,Write,Glob,Grep` のまま。外部GitHubを読める前提にしない。
結果への応答は許可されたコード・直接必要な回帰の編集だけで、テスト再実行・push・次工程は行わない。
finalizerは既存の変更範囲とexact-head検査を維持し、feedbackを使ったことによる保護パス例外を作らない。

## 6. 実装単位と変更対象

一つの運用基盤PRでproducer→consumer→worker接続と回帰を実装する。
片側だけを本番の新契約として利用しない。フィールド未指定時の既存動作を維持する。

| 対象 | 変更内容 |
|---|---|
| `.github/workflows/claude-fix.yml` | 固定verifyの結果保存・upload、prepareの出所検査、workerへのstageと説明 |
| `.github/scripts/aadw_verification_feedback.py`（新規） | reportの純データ生成/厳格検査・stage用データ生成。action選択やGitHub mutationを含めない |
| `.github/scripts/aadw_action_contract.py` | optional field・排他・対象action/classの構文検査。既存重複防止を維持 |
| `.github/tests/test_aadw_verification_feedback.py`（新規） | report、出所、stage、正常/異常の接続回帰 |
| `.github/tests/test_aadw_action_contract.py`、`test_aadw_prevention_wiring.py` | 後方互換・権限・固定コマンド・順序の回帰 |
| `.github/workflows/ci.yml` | 新Python回帰の呼出を既存read-only検査jobへ追加。既存検査を整理・削除しない |
| `docs/aadw-command-policy.md` | 既存verify節へ新reportの扱い・fresh判断/追加依頼を追記する。別の判断ルールを作らない |
| `docs/aadw-claude-execution.md` | 返却fieldとデータの読み方、復元との差、段階採用・証拠取得を追記 |
| 本書と引き継ぎ文書 | 実装後の実ファイル/fieldと検証証拠へ更新 |

新helperは既存のPython標準ライブラリで足りる範囲とし、依存追加・汎用実行器を作らない。
GitHub取得には既存gh/API処理を利用する。意味判断は追加しない。
`.agents/skills/`、AGENTS、CLAUDE、他workflow、Rust製品コード、vendor、release、モデル切替は今回変更しない。

## 7. #684そのものの実装経路

改善後の通常製品実装は `@claude` のままだが、このPR自身は `.github/` とPolicyを変更する運用基盤専用PRである。
基準mainの通常finalizerはこれらを拒否する。PR #526だけのU2例外は転用しない。
通常workerに本PRの実装を投稿して拒否させる試行や、self-modifyingな例外追加をしない。

PR担当は権限を持つ運用基盤担当として着手する。Claudeを編集担当に使う場合は、
専用のローカルClaude Code、モデル `claude-sonnet-5-5`、編集用toolsだけで対象allowlistを渡す。
GitHub/Jev認証と提出権限は渡さず、Git管理情報のない対象treeを編集データとして使う。
実行・差分検査・commit/pushは運用基盤担当が開始時/提出直前のexact headを確認して行う。
ローカルClaude CLI 2.1.296の存在は確認済みだが、認証とSonnet 5.5の実接続は未確認。
着手時に既存認証で確認し、利用不能ならその事実を報告する。別モデル・Codex実装fallbackへ自動変更しない。
Commanderによる製品コード変更は本PRに含めない。

## 8. 検証・採用・完了の区別

### 8.1 merge前のコード・接続検証

| ケース | 必須の結果 |
|---|---|
| tests失敗/lint成功、tests成功/lint失敗、両方失敗、両方成功 | 実exitとoutcomeが正確でtests失敗後もlintを一度実行。集約結果を維持 |
| `continue-on-error` conclusion成功/exit非0 | successへ誤変換しない |
| checkout/job/collector/upload失敗、取消、欠落/不正/巨大/duplicate-key JSON | unavailableまたは拒否。worker起動・pushの新しいactionなし |
| stale H1、別PR/run/attempt/workflow/actor、失効/重複artifact | prepareまたは起動直前に拒否 |
| feedback fieldの片側・重複・不正値、checkpoint/patchとの同時指定 | 拒否。旧依頼は従来どおり |
| 同一headの同じ依頼、同一原因/evidence反復、paused/closed/fork | 既存拒否を維持。ID/URL付替えで回避しない |
| shell指示を含む診断、ANSI、symlink/path traversal、ログ上限超過 | データとしてのみ扱う。展開/実行を拒否し、切捨てを明示 |
| 環境の偽credential sentinel | 子process・report・stageに流れない。実認証情報をテストfixtureにしない |
| feedbackを使った保護パス変更 | 既存finalizerが拒否しpushしない |
| local producer→artifact→consumer→Read可能なstage | 実固定runnerを模した成功/失敗processとAPI応答fixtureで接続。単なる文字列assertだけにしない |

必須の実行コマンドは引き継ぎ文書に記載する。新コードについてはcurrent-head CI、
current-head最終review、Policy上必要な検証を取得する。
本PRはエディタを変えないため製品native GUIの新scenarioは予定しないが、製品PRでのGUI義務は維持する。
`.github/workflows/ci.yml` を変更するので、現行path filterでは両OS Rust CIも実行対象になる。
これを今回の都合でskipへ変えない。

### 8.2 default branch採用後の実経路確認

`issue_comment` はdefault branchのworkflowを使う。branchだけに新fieldを置いても本番の接続確認にならない。
そのためmerge前は上記の隔離接続検証、採用後は次の限定rolloutを行う。
mergeは機能の実装採用であり、Issue #684の全条件達成や親 #683の完了とはしない。

1. 運用基盤PRを採用後、専用fixture PRを用意する。通常文書・ユーザーデータ・製品仕様を変えない。
   削除可能な小さいRust回帰fixtureを明示し、初回だけ期待値を意図的に誤らせる。
   内容・変更可能パス・正しい期待値をCommanderが固定し、テスト弱化を正解にしない。
2. 既存入口から通常Claude Codeに限定変更を依頼し、H1がpushされverifyが意図したtestで実失敗、lintも返ることを確認する。
3. CommanderがH1/current base・report・run/attemptを読み、Jev判断後に新field付きC1を一度投稿する。
   新workerが元依頼と診断を実際に読み、指定testを正しい期待値に直したH2を提出する。
4. H2のverify tests/lint成功、必要な通常CI成功を確認する。fixture PRは製品へmergeせず閉じる。
   実run/attempt/head・model・元依頼/再依頼・report artifact・結果を運用基盤PRへ記録する。
5. stale head、出力欠落、job障害、禁止パスの実接続拒否を隔離fixtureで確認する。
   意図した拒否を、worker失敗や診断完了と混同しない。token漏えいを実認証で注入しない。
6. 代表的な同一rootの実修正でも比較し、下記の測定を残す。

新契約の本番利用はこのPRの採用後とし、既存の通常依頼を利用できる状態を保つ。
rolloutで問題が出たらfeedback fieldの使用を止めて従来依頼へ戻し、原因と修正をCommander判断へ返す。
新markerで旧guardを迂回したり、unknownをpassに変えたりしない。
コード差分のrevert単位は本PR全体。別の進行DBや常駐processの撤去は不要。

### 8.3 効果測定

基準例は [#671 run 41945402340 attempt 1](https://github.com/hide212131/hane/actions/runs/41945402340) と
[再依頼6646877206](https://github.com/hide212131/hane/pull/671#issuecomment-6646877206)。
verify開始 2026-11-05 01:01:36 UTC、終了01:11:27 UTC（591秒）。
test stepは437秒、lint stepは132秒。終了から再依頼01:15:11 UTCまで224秒。
test stepのAPI conclusionはsuccessだが集約stepはfailureであり、実テスト失敗は当該再依頼と実ログで確認する。
この224秒すべてを自動的に削減可能とは主張しない。

従来/変更後それぞれ同一rootの `元依頼→H1→検証終了→追加依頼→H2→検証成功` を記録する。
OS/profile/cache、run/attempt、対象head/base、再依頼数、push数、tests/lint時間、
検証終了から追加依頼、queue/起動、追加実装、往復全体の時間を分ける。
再起動とpush後CIの待ちは残る。初期版でpush数・再依頼数が減らなくても正直に記録し、
診断の取得・組立・読み直しと往復時間のどこが減ったかを説明する。
合成fixtureだけの高速化を実修正の効果と称さず、短縮を示せなければ効果未達のままIssueを閉じない。

## 9. 依存と実装担当への完了判定

#333の既存接続を再利用し、#311のCI全体整理、#685/#686のGUI、#687のreview改善の完了は待たない。
同じ `claude-fix.yml` / `ci.yml` / Policyを変更する他PRが現れた場合だけ、current factsを読んで責務を調整する。
current mainの製品側 #671/#675/#678/#679へ本機能のコードを混ぜない。

Issue #684の5受入条件は、実fixtureの往復、異常拒否、credential分離、実修正の比較、
CI/review/GUI・非自動mergeの維持について、それぞれ対応する実証が揃ってから完了にする。
設計準備、コード採用、fixture成功、実修正の効果確認を別々に報告する。
親 #683の2機能PR・releaseまでの効果確認は、引き続き親Issueで追跡する。
