# AADW Commander Policy

## 1. 目的

この文書は、Hane の AADW で ChatGPT が司令塔として Jev の判断を運用するときのルールを定める。工程管理は v2 の `Observe → Decide → Act → Observe` を保ち、意味判断には Jev を標準で使う。

AADW v3 は独立した workflow / state machine を増やさず、次の反復で運用する。

```text
Observe → Decide → Act → Observe
```

```text
Commander Policy + current facts / evidence
                  ↓
       Jev: 構造化された意味判断
                  ↓
Commander: 客観ガード確認と次の一 action
```

ChatGPT はJevと同じ意味判断を毎回やり直さない。Jev の結論を既定の判断として採用し、権限・current facts・必須証拠など機械的に確認できる条件を満たすか確認して一つの action を実行する。

current facts の取得方法や action の実行方法は固定しすぎない。まず既存の GitHub / CodeRabbit / Claude / 条件付き Codex fallback / GUI 機能を使い、重複や複雑さが実運用で確認された場合だけ専用 wrapper / collector を追加する。

---

## 2. 基本原則

### 2.1 current PR context only

PR の evidence を current と扱うときは、current PR head を確認し、その evidence の claim が target branch / base context に影響され得る場合は current base context も確認する。

head が変われば、旧 head の CI、review、GUI evidence、worker result は current state の判断材料にしない。

一方、head が同じでも target branch が進めば PR diff、merge result、CI、review、base-sensitive な GUI scenario の前提が変わり得る。そのような evidence について、どの base context に対するものか確認できない、または current base と異なる場合は、推測で current 扱いしない。必要なら current base context で検証を取り直す。

base の変更が evidence の claim に影響しないと Commander が current facts と scenario / check の性質から判断できる場合は、head に結び付く evidence を利用できる。その判断は worker に委ねず、根拠を GitHub 上に残す。

PR metadata に含まれる base SHA が常に target branch の現在の head を表すとは仮定しない。base-sensitive な判断では必要なら target branch 自体を読み、current base を確認する。

この context を AADW 独自の generation ID、snapshot、status、DB として保存しない。その時点の GitHub facts から必要な瞬間だけ判断する。

repository mutation の競合防止は別の責務である。product branch を変更する worker は、引き続き開始時と push 直前に current PR head が対象 SHA と一致することを確認する。

### 2.2 evidence first

事実が不足している場合は推測して次工程へ進まない。

必要な evidence を追加取得してから判断する。

### 2.3 fail closed

安全に判断できない場合は進めない。

`unknown` を `pass`、`continue`、`follow_up` に読み替えない。

### 2.4 Issue purpose first

review finding の件数をゼロにすること自体を目的にしない。

current Issue の目的と acceptance criteria を満たすことを優先する。

### 2.5 Issue / PR / ADR / 詳細設計の正本を分ける

設計情報を Issue、PR、ADR に重複して持たせない。情報の種類ごとに正本を決め、他の場所から参照する。

- **Issue** は仕事の契約を正とする。何を、何のために行うか、対象範囲・対象外、依存関係、acceptance criteria を置く。コメントで要求・範囲・acceptance criteria が確定または変更された場合は、後続担当がコメント履歴から推測しなくて済むよう Issue 本文へ反映する。
- **PR** は今回の変更と、その変更を受け入れてよいことを示す evidence の場とする。差分、Issue / ADR / 詳細設計との対応、review の議論と対応、current context に結び付く CI / review / GUI evidence を扱う。将来も守る設計判断や要求を PR コメントだけに残さない。
- **ADR** は複数の Issue や将来の変更にも効く設計判断を正とする。採用した方針、その理由、重要な代替案、制約・影響、適用状態を記録する。既存 ADR に従うだけの局所変更では新しい ADR を作らない。重要な設計判断を変更する場合は ADR を更新または supersede し、以前の判断を追跡可能にする。
- **詳細設計書** は、今回の実装に必要な具体的仕様が Issue 本文では長くなりすぎる場合だけ使う。変更ファイル、シンボル、処理順、不変条件、移行手順、テスト対応などを repository 内の文書として置き、Issue から参照する。小さい作業では独立した詳細設計書を必須にしない。

設計用 Draft PR を使う場合、PR 自体を設計内容の正本にはしない。repository 内の ADR または詳細設計書を差分として提示・review する場とする。設計 PR の merge は設計文書の採用・保存であり、製品実装や acceptance criteria の達成を意味しない。

PR の議論で結論が変わった場合は、内容に応じて Issue、ADR、詳細設計書の正本へ反映してから完了させる。要求と設計が矛盾する場合、コメントの新しさだけで優先順位を推測せず、関係する正本を整合させる。

current state と履歴を混同しない。設計時点の「未実装」「未検証」などの記録はその時点の事実として保持できるが、現在の実装・検証状態は current Issue / PR / GitHub evidence から確認する。

### 2.6 one action at a time

一度の判断で複数 action を自動連鎖させない。

current evidence に基づき、次に必要な一つの action を選ぶ。

### 2.7 existing tools first

AADW 専用 command / workflow / wrapper / Gate を前提にしない。

既存機能で十分ならそのまま使う。

同じ操作や確認が繰り返し問題になると確認された場合だけ、最小の専用部品へ抽出する。

### 2.8 base synchronization と product fix を分離する

target branch の進展を PR branch に取り込む操作は、Issue の製品コードを修正する action とは別に扱う。

- target branch が継続的に進むことを通常状態として扱う。個人開発でも、互いに競合しにくい機能・領域を選んだ複数 PR の並行開発を妨げない。base を固定するためだけに開発を直列化しない。
- Commander が並行する Issue / PR の選択に関与できる場合は、同じファイル・同じ責務・同じ UI 領域への変更が重ならない組み合わせを優先し、不要な conflict の発生確率を下げる。
- target branch が進んだだけでは自動的に base sync を行わない。まず compare と変更内容を確認し、current PR の変更領域、merge result、CI / review / GUI evidence の主張に影響するかを判断する。無関係な進展なら、必要な根拠を残して current candidate を継続できる。
- base sync が必要なのは、変更領域の重なり、merge conflict、base-sensitive な挙動・evidence への影響、または merge 前の統合条件として current target branch の取り込みが必要な場合とする。target branch の各 commit を追いかけて逐次同期しない。
- Commander は `behind > 0` だけを理由に、Claude / Codex の product-fix worker に「current main と同等のコードを書いて同期する」よう依頼しない。
- base sync が必要なら、既存 Git の merge / rebase / update-branch など、target branch の commit を PR branch の ancestry に取り込む Git 操作を使う。同期が必要と判断した場合は完了後に compare で `behind = 0` を確認する。同等のコードが存在するだけでは同期済みと判断しない。
- conflict resolution に製品コード上の判断が必要な場合は、base sync と conflict fix を区別する。current target branch の変更を基準として取り込みつつ、current Issue の acceptance criteria と PR の root-cause 修正を保持する。両立できない場合は機械的に片側を採用せず、conflict の意味を再評価する。同期のために target branch の変更を別実装として複製しない。
- product-fix worker は Issue の root-cause 修正を担当し、branch ancestry を更新する Git 操作の代替として使わない。

### 2.9 Jev を意味判断の標準担当にする

- 通常の Issue / PR 作業では、依頼の解釈、受入条件、作業範囲、原因分類、実装・検証計画、次の action、継続・停止・完了候補の判断に Jev を使う。
- この環境の既定接続は既存の local shell `~/.local/bin/jev`、provider `typesafe`、model `jev-latest` とする。設定済み認証をそのまま使い、API keyを表示・workerへ渡さない。単一判断は適切なCLI primitiveを使い、一括の候補フィルタには `filter`、複数の独立質問には `raw`、承認済みの再利用可能な質問には `run` を使う。取得したJev応答だけを判断結果として扱う。
- 現在の事実とこの Policy を必要な範囲で Jev に渡す。Choice は許可済み候補から次の action を一つ選ぶため、Noul は重要な条件を独立に判定するため、Score は具体的な順序尺度で程度を評価するために使う。既存 action 用のhandlerが利用できる場合はfunction calling形式でhandlerと閉じた引数を選ばせ、Commanderが通常の権限確認後に実行する。関係する独立質問は一回の System One 要求にまとめ、互いの回答を参照させない。
- Jev の意味判断を標準結果として使う。ChatGPT が同じ意味判断を別途再採点・再評価する工程、性能・精度・費用の追加ベンチマーク、質問別の事前閾値は要求しない。利用者は費用・性能を採用判断の条件にしないと指定している。
- Jev に渡す候補は、その時点で利用可能かつ許可された action から作る。Jev は認証情報や秘密情報を受け取らず、任意のshell・GitHub操作を直接実行しない。既存の明示的なhandlerがある場合は、閉じた選択肢と引数を選べる。handler/Commander は実行前に権限・current head/base・required CI/review/GUIを確認し、条件を満たさない action は実行しない。
- Jev の返答が得られない場合は、作り上げた返答を使わず、既存 Policy と current facts から Commander が一つの action を決める。認証・権限・環境障害を誤って Codex fallback と分類しない。
- 返答、選択肢、根拠、要求・実行モデル、取得可能な usage/latency は既存の実行記録に残せる範囲で記録する。費用や利用量は判断を止める閾値として使わない。

---

## 3. Observe

利用者から `PR #123 を続けて` のように依頼された場合、まず GitHub から current state の事実を取得する。

原則として次程度を見る。

- PR metadata / current head SHA。
- current target branch head / base context。
- current context の CI / checks。
- current review / unresolved review threads。
- relevant workflow runs / GUI evidence の有無。

これだけで判断できない場合に限り、必要な evidence を追加取得する。

例:

- review 判断なら Issue、current diff、current review、current unresolved review threads、review 後に base が進んでいないか。
- GUI 判断なら scenario、observed result、artifact、baseline、対象 head / base context。
- CI failure なら failed check / job / log と workflow run が対象にした PR base SHA。

無関係な evidence は広く取得しない。

---

## 4. Decide

Jev は current facts とこの Policy から次に必要な一つの action を選ぶ。ChatGPT は候補が現在許可されていること、必要な事実が新しいこと、必須の客観条件を満たすことを確認して実行する。これらの確認は Jev の意味判断を二重評価する工程ではない。

典型例:

```text
read more evidence
run review
run fix
run GUI validation
rerun failed infrastructure step
merge after objective safety checks
stop and ask for human decision
```

判断理由は current facts とこの Policy に基づいて説明できること。

### 4.1 CI

CI evidence は current PR head に対応していることを必須とする。

その CI の主張が target branch / merge context に影響される場合は、run の base context が current target branch と整合していることも確認する。同じ head の成功 run でも、base-sensitive な CI が古い base を対象にした場合は current CI とみなさない。GitHub が run の pull request base SHA などを持つ場合はそれを使い、必要なら current base で CI を取り直す。

一方、変更内容と check の性質から base の変更がその CI の主張に影響しないと Commander が判断できる場合は、head に結び付く成功 evidence を利用できる。その根拠を GitHub 上に残す。

CI が失敗している場合、そのまま merge 方向へ進めない。

失敗内容を確認し、current PR の変更で修正すべき問題なら fix を選ぶ。

infrastructure failure など product code の問題と判断できない場合は推測せず停止するか、必要な再実行を選ぶ。

### 4.2 Review

通常レビューはCodeRabbitを使う。`.coderabbit.yaml` でnon-draft PRのAutomatic Reviewとpush後のAutomatic Incremental Reviewを有効にし、`auto_pause_after_reviewed_commits: 2` で過剰な再レビューを抑える。自動レビューはCommanderが発行するactionではなく、GitHubに現れたreview evidenceとして観測する。最終候補ではcurrent-head CI成功後に `@coderabbitai full review` を明示的に実行してPR全体を取り直す。途中で自動pauseした場合や追加確認が必要な場合だけ `@coderabbitai review` / `resume` を使う。対象head・reviewed range・完了状態をGitHub上の実結果で確認し、Codex Reviewは通常経路として要求せず、外部設定で投稿されてもCodeRabbit reviewの代替証拠にはしない。

current PR head の unresolved findings を一度に確認する。

review の主張が PR diff / merge context に影響される場合は、review 後に target branch が進んでいないかを確認する。base が変わり、review が対象にした context を current と証明できない場合は無条件に再利用せず、必要なら current base context で review を取り直す。

一方、特定の head-local な指摘など、base の変更が review の主張に影響しないと Commander が current facts から判断できる場合は、その review evidence を利用できる。その根拠を GitHub 上に残す。

review comment 1件を fix 1回に対応させない。

同じ原因・同じ設計面に属する finding は root-cause cluster にまとめる。

独立した問題を同じ cluster に混ぜない。

各 finding / cluster を次のいずれかとして判断する。

- `blocker`: current PR で修正する必要がある。
- `follow_up`: current PR の acceptance を妨げず、別 Issue 候補にできる。
- `unknown`: evidence 不足または原因不明。進めない。

原則として次は blocker とする。

- P0 / P1。
- security 問題。
- data loss / corruption。
- permission / authority violation。
- 通常経路で再現する明確な bug。
- current Issue の acceptance criteria を直接満たせなくする問題。

blocker がある場合、同じ root-cause cluster のものは一回の fix にまとめる。

### 4.3 GUI validation

GUI validation を行うかどうかは current Issue の acceptance criteria と変更内容から判断する。

不要な full regression を毎回要求しない。

focused scenario を優先する。

GUI evidence は対象 head に加えて、その scenario の結果に current base / merge context が影響する場合は、その context と対応していることを確認する。head が同じでも base の product code が scenario に影響する形で変わった場合、旧 GUI evidence を無条件に current とみなさない。base-independent と判断できる場合は、その根拠を GitHub 上に残して head-only evidence を利用できる。

GUI result が `fail` / `blocked` の場合は evidence を読み、少なくとも次を区別する。

- current product regression。
- target acceptance blocker。
- validation infrastructure / harness problem。
- pre-existing independent issue。
- unknown。

`unknown` は進めない。

pre-existing independent と判断する場合は、trusted baseline など比較可能な evidence を要求し、推測だけで current blocker を waive しない。

### 4.4 Follow-up

current PR の目的を直接妨げない改善は follow-up に分離できる。

ただし次を follow-up に送って current PR を進めてはいけない。

- security。
- data loss / corruption。
- P0 / P1。
- acceptance criteria を満たせなくする問題。
- current normal path の明確な regression。
- evidence 不足の unknown。

### 4.5 Merge

ChatGPT の意味判断だけで merge しない。

current Issue の目的と acceptance criteria を満たし、必要な review / GUI 判断を終えたら、merge 直前に GitHub 上の客観条件を再確認する。

最低限確認する。

- current head SHA が判断対象の expected head と一致する。
- current target branch / base context が、採用する base-sensitive な CI / review / GUI evidence の前提と整合している。
- required CI checks が current context で success。
- 必要と判断した GUI validation がある場合、その結果が current context で受け入れ可能である。
- GitHub が PR を mergeable と報告している。

base-independent と判断した evidence は、Commander がその根拠を GitHub 上に残していることを確認する。

merge は expected head SHA を指定して行う。expected head は concurrent product branch mutation を防ぐための guard であり、base freshness の代わりにはならない。

専用 Gate は必須ではない。確認が実運用で繰り返し複雑になる場合だけ、客観条件だけを確認する小さな Gate へ抽出してよい。

### 4.6 修正の収束と高コスト検証

この節は新しい workflow state、generation、receipt、Gate を定義するものではない。PR の修正と検証を開始する順序を、Commander が current facts から判断するための運用ルールである。

複数の finding、失敗、境界条件がある場合、最初に次を一つの作業単位として整理する。

1. current head と、主張に必要な current base context。
2. Issue の acceptance criteria と、変更が守るべき不変条件。
3. 同じ原因・設計面に属する finding の root-cause cluster。
4. 不変条件が変化する境界のテスト表。少なくとも、通常経路と境界値、初期化・再読込、incremental / background 処理、cache / measured value / virtualization、入力・selection・scroll が関係する場合の各状態を含める。

その上で、次を守る。

- 一つの cluster に対して一つの coherent な fix と回帰テストをまとめる。レビューコメントやテストケースごとに条件分岐・commit・push を分割しない。
- push 前に、変更範囲に対応する最小の local test / lint と差分検査を実行できる実装経路では、それを完了する。関連する修正をまとめて確認できるまで、次の head を作らない。
- trusted worker が sandbox のため test / build / lint を実行できず、trusted finalizer だけが push する既存経路では、sandbox を緩めてこの責務を worker に移さない。その場合は current-head CI を push 後の最初の検証とし、CI が成功するまで local validation 済み、review / GUI validation 済み、または次の修正へ進める状態とは扱わない。CI failure は current head の evidence として読み、必要ならその root-cause cluster を修正する。
- 高コスト検証を始める前に current target branch の進展を観測し、candidate への影響を判断する。変更領域が重なる、merge result が変わる、または evidence が base-sensitive なら統合ポイントとして base sync を行う。無関係な進展なら candidate を維持し、base-independent と判断した根拠を残す。
- base sync が必要な場合は product-fix worker による再実装ではなく、target branch commit を ancestry に取り込む Git 操作として行う。同期後は compare で `behind = 0` を確認し、古い base-sensitive evidence を再利用しない。
- CI、review、GUI validation など所要時間の大きい action は、実行経路に応じた最初の検証（local validation または trusted worker 経路の current-head CI）を通過した安定候補 head に対して選ぶ。検証中に target branch が進んでも、それだけで candidate や evidence を破棄しない。進展内容と evidence の base sensitivity を再評価し、影響がある場合だけ sync / 再検証する。
- 並行開発そのものを抑止しない。Commander が作業順を選べる場合は conflict の可能性が低い Issue を並行させ、同じファイル・同じ責務を大きく変更する PR 同士は可能な範囲で同時進行を避ける。
- 同じ cluster が一度の fix 後も再現する場合、局所的な条件追加を続けず、presentation と index、計算値と実測値、同期処理と background 処理など共有される不変条件を再設計し、境界をまたぐ回帰テストを追加する。

この手順の目的は push 数を機械的に制限することではない。current head に結び付かない CI / review / GUI evidence の再利用と、安定していない head に対する高コストな検証の繰り返しを避けることである。

### 4.7 原因未確定の修正反復を防ぐ実行契約（Issue #408）

修正前に、Jev / Commander は観測と原因の推定を分け、`product`（製品）、`test`（テスト）、`measurement`（測定器）、`environment`（実行環境）、`unknown`（未確定）を分類する。新規実装は `initial` とする。分類を選択しただけでは原因の証明にならない。根拠となる current code と観測を確認し、変更でどの観測が改善するはずかを説明できる場合にだけ製品修正を依頼する。

既存の修正依頼には、以下の必須行を含める。SHA・原因ID・evidence URLは実際の値へ置き換える。各行を一つずつ記載する。

```text
@claude
AADW_COMMANDER_HANDOFF_V2
AADW_ACTION: implement
AADW_TARGET_HEAD: <40桁のcurrent head>
AADW_FAILURE_CLASS: product
AADW_ROOT_CAUSE: <同じ原因に同じIDを使う>
AADW_EVIDENCE: https://github.com/<owner>/<repo>/actions/runs/<run-id>
```

`measurement` / `environment` / `unknown` を製品修正経路へ流さない。原因を区別できる診断は別の明示的な依頼とする。

```text
@claude
AADW_DIAGNOSTIC_REQUEST_V1
AADW_ACTION: diagnose
AADW_TARGET_HEAD: <40桁のcurrent head>
AADW_FAILURE_CLASS: unknown
AADW_ROOT_CAUSE: <原因候補の安定ID>
AADW_EVIDENCE: https://github.com/<owner>/<repo>/actions/runs/<run-id>
```

診断用 `AADW Diagnose` は既存と同じClaude接続を使用するが、製品変更・push・次工程の起動を行わない。共有のconcurrency groupで製品修正と同時実行しない。現在のheadのコードとCommanderが提示した観測を調べ、`diagnosis.json` に日本語の結論・根拠・次に必要な観測を返す。取得できないartifactや実機を確認したと主張しない。`unknown` の報告も診断の正常な返却であり、製品の受入成功ではない。診断のsource patchが非空なら失敗とする。実装経路で空patchを成功にする変更は行わない。

`aadw_action_contract.py` は意味判断をしない。同じ権限確認済みCommanderの過去コメントをGitHubから読み、同一head・原因・evidenceの実装再送、および同じ原因とevidenceを2回使った後の追加実装要求を拒否する。これは実行失敗件数を数える機能ではなく、要求の重複を保守的に防ぐ。新しいpersistent counterやworkflow stateは作らない。Commanderは原因IDやリンクを付け替えてこの制限を避けず、識別できる新しい観測を得る。旧workflowの再実行で新しい契約を迂回しない。

同じ失敗が修正後も続き、原因を区別する情報が増えていなければ、次のpatch・full review・同条件GUIの再送を止める。診断では「入力配送」「アプリの入力受信」「描画」「画面取得」「判定」のどこを比較するかを決める。人の判断や実環境が必要なら、その不足を具体的に報告する。停止を未解決のまま隠したり、判定不能を合格に変えたりしない。

実装workerのshell・Git credential禁止を維持する。trusted finalizerによるpush後、別のread-only jobがそのexact headのworkspace testsとClippyを実行する。テスト不合格でもlintを実行し、両方の結果を一度に返す。これが成功するまで、worker run全体を検証済みの実装結果として扱わない。この検証はmacOS / Windowsのrequired CIやcurrent-head reviewを置き換えず、mergeを自動起動しない。

GUIでは元の `result.json` の観測と、trustedな保存処理が付ける `observation_quality` を併せて読む。元の結果は `raw-result.json` に残す。期限内の画像がない・時計が不正・入力経路が不明・OCRや反転入力が慣性窓を越える場合は測定不能であり、製品原因を確定しない。期限内の画像で動きが見えなかった場合も、アプリの入力受信と描画が確認できなければ原因は `unknown` とする。80ms・55ms・135ms等の既存閾値はこの分類のために緩めない。元のfailはpassに変えず、成立しない観測に基づくpassはblockedにする。

測定器の採用・変更時は、既知の正常・既知の異常・測定不能の回帰ケースを確認する。その上で実際の実行環境で入力配送・時計・画面取得の成立を確認する。過去artifactの再評価や数値fixtureのテストは分類器の検証であり、現在の実機で測定が成立した証拠ではない。成立しない場合は測定不能を記録し、受け入れ条件を満たす別の確認方法をCommanderが明示的に選ぶ。GUI不要への読み替えや閾値の緩和は行わない。

停止中のPRは、Closedまたは `aadw:paused` ラベル／本文 `<!-- AADW_PAUSED -->` を確認して、通常の実装を拒否する。既存GUI経路を確実に止めるにはDraftまたはClosedを併用する。Draftだけでは既存の実装経路を止めないことに注意する。停止前後のheadを保存し、ブランチは削除しない。他の独立したPRを一律停止しない。

Issue #408に伴うPR #395の停止解除は、防止策のコード・経路接続テスト・current CI・レビュー・変更なし診断の実行確認、および実環境の観測可否の記録を揃えてから行う。文書の追記だけ、または分類器の単体テスト成功だけで再開しない。再開時はcurrent head/baseを読み直し、新しい診断経路で残る失敗を先に分類する。再開できることと、Issue #389の製品が合格したことは別である。

---

## 5. Act

選んだ action は、まず既存機能で実行する。

CodeRabbit、Claude、条件付きCodex fallback、CI、GUI validation、GitHub merge は AADW の stage ではなく、その時点で必要なら使う道具である。

worker は意味判断をしない。

repository mutation を伴う action は、実行直前に current head が対象 SHA と一致することを確認する。

Claude が push して head が変わったら、旧 head の evidence は current 判断に使わない。

target branch が動いて evidence の base context が変わった場合も、head が同じだからという理由だけで base-sensitive な旧 CI / review / GUI evidence を current 扱いしない。

---

## 6. Observe again

action の結果が GitHub に残ったら、過去の判断をそのまま継続せず、current facts を読み直す。

新しい head なら新しい product branch 世代として扱う。head が同じでも target branch が進んだ場合は、base-sensitive な evidence の context を再評価する。

同じ root cause が修正後も繰り返す場合、細かい patch を無制限に続けず、設計見直し、scope 分割、追加 evidence の取得などを判断する。

---

## 7. Provider / infrastructure failure

Claude、Codex、CodeRabbit、GUI runner などが provider / infrastructure 理由で失敗した場合、その失敗を product failure とみなさない。

必要な evidence が得られなければ停止する。

Claude の failure diagnostic が `usage_or_rate_limit` の場合だけ、既存の Claude checkpoint artifact と trusted `workflow_run` を使った Codex CLI fallback を許可する。この fallback は利用上限からの実装継続に限定し、一般的な provider routing や retry state machine にはしない。

fallback worker を起動するには、分類結果が current source run に結び付いていること、trusted handoff actor が repository write 権限を持つこと、対象 PR が open same-repository で current head が一致することを確認する。self-hosted runner では GitHub 認証情報を Codex に渡さず、保護パスの変更を拒否し、push 直前に exact-head を再確認する。条件を一つでも確認できない場合は fail closed とする。

---

## 8. 最終原則

Commander は、

```text
Observe → Decide → Act → Observe
```

を繰り返す。

新しい workflow state を増やすより、必要なら Policy 自体を改善する。

専用の collector / wrapper / Gate は、実運用で必要性が証明された場合だけ追加する。
