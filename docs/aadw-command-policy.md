# AADW Commander Policy

## 1. 目的

この文書は、Hane の AADW で ChatGPT が司令塔として Jev の判断を運用するときのルールを定める。工程管理は v2 の `Observe → Decide → Act → Observe` を保ち、意味判断には Jev を標準で使う。

AADW v2 は workflow / state machine ではなく、次の反復である。

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

current facts の取得方法や action の実行方法は固定しすぎない。まず既存の GitHub / Codex / Claude / GUI 機能を使い、重複や複雑さが実運用で確認された場合だけ専用 wrapper / collector を追加する。

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

### 2.5 one action at a time

一度の判断で複数 action を自動連鎖させない。

current evidence に基づき、次に必要な一つの action を選ぶ。

### 2.6 existing tools first

AADW 専用 command / workflow / wrapper / Gate を前提にしない。

既存機能で十分ならそのまま使う。

同じ操作や確認が繰り返し問題になると確認された場合だけ、最小の専用部品へ抽出する。

### 2.7 base synchronization と product fix を分離する

target branch の進展を PR branch に取り込む操作は、Issue の製品コードを修正する action とは別に扱う。

- target branch が継続的に進むことを通常状態として扱う。個人開発でも、互いに競合しにくい機能・領域を選んだ複数 PR の並行開発を妨げない。base を固定するためだけに開発を直列化しない。
- Commander が並行する Issue / PR の選択に関与できる場合は、同じファイル・同じ責務・同じ UI 領域への変更が重ならない組み合わせを優先し、不要な conflict の発生確率を下げる。
- target branch が進んだだけでは自動的に base sync を行わない。まず compare と変更内容を確認し、current PR の変更領域、merge result、CI / review / GUI evidence の主張に影響するかを判断する。無関係な進展なら、必要な根拠を残して current candidate を継続できる。
- base sync が必要なのは、変更領域の重なり、merge conflict、base-sensitive な挙動・evidence への影響、または merge 前の統合条件として current target branch の取り込みが必要な場合とする。target branch の各 commit を追いかけて逐次同期しない。
- Commander は `behind > 0` だけを理由に、Claude / Codex の product-fix worker に「current main と同等のコードを書いて同期する」よう依頼しない。
- base sync が必要なら、既存 Git の merge / rebase / update-branch など、target branch の commit を PR branch の ancestry に取り込む Git 操作を使う。同期が必要と判断した場合は完了後に compare で `behind = 0` を確認する。同等のコードが存在するだけでは同期済みと判断しない。
- conflict resolution に製品コード上の判断が必要な場合は、base sync と conflict fix を区別する。current target branch の変更を基準として取り込みつつ、current Issue の acceptance criteria と PR の root-cause 修正を保持する。両立できない場合は機械的に片側を採用せず、conflict の意味を再評価する。同期のために target branch の変更を別実装として複製しない。
- product-fix worker は Issue の root-cause 修正を担当し、branch ancestry を更新する Git 操作の代替として使わない。

### 2.8 Jev を意味判断の標準担当にする

- 通常の Issue / PR 作業では、依頼の解釈、受入条件、作業範囲、原因分類、実装・検証計画、次の action、継続・停止・完了候補の判断に Jev を使う。Jev は作業指示文の案も作成できる。
- 現在の事実とこの Policy を必要な範囲で Jev に渡す。Choice は許可済み候補から次の action を一つ選ぶため、Noul は重要な条件を独立に判定するため、Score は比較可能な案の評価・順位付けに使う。関係する質問は一回の System One 要求にまとめ、独立質問は互いの回答を参照させない。
- Jev の意味判断を標準結果として使う。ChatGPT が同じ意味判断を別途再採点・再評価する工程、性能・精度・費用の追加ベンチマーク、質問別の事前閾値は要求しない。利用者は費用・性能を採用判断の条件にしないと指定している。
- Jev に渡す候補は、その時点で利用可能かつ許可された action から作る。Jev は選択や提案を行うが、GitHub の書込認証、push、merge、workflow起動、秘密情報を受け取らない。実行前の権限・current head/base・required CI/review/GUIの確認は Commander が行い、条件を満たさない action は実行しない。
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

ChatGPT の意味判断だけでは merge しない。

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

---

## 5. Act

選んだ action は、まず既存機能で実行する。

Codex、Claude、CI、GUI validation、GitHub merge は AADW の stage ではなく、その時点で必要なら使う道具である。

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

Claude、Codex、GUI runner などが provider / infrastructure 理由で失敗した場合、その失敗を product failure とみなさない。

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
