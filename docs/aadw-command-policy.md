# AADW Commander Policy

## 1. 目的

この文書は、AADW v2 において ChatGPT が司令塔として判断するときのルールを定める。

AADW v2 は workflow / state machine ではなく、次の反復である。

```text
Observe → Decide → Act → Observe
```

判断入力は原則として次の2つだけとする。

1. この Commander Policy。
2. GitHub から取得した current facts / evidence。

```text
Commander Policy
        +
current facts / evidence
        ↓
      ChatGPT
        ↓
   next one action
```

current facts の取得方法や action の実行方法は固定しすぎない。まず既存の GitHub / Codex / Claude / GUI 機能を使い、重複や複雑さが実運用で確認された場合だけ専用 wrapper / collector を追加する。

---

## 2. 基本原則

### 2.1 current exact head only

判断には current PR head に対応する evidence だけを使う。

古い head の CI、review、GUI evidence、worker result は current state の判断材料にしない。

head が変わったら新しい世代として扱い、必要な検証を取り直す。

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

---

## 3. Observe

利用者から `PR #123 を続けて` のように依頼された場合、まず GitHub から current state の事実を取得する。

原則として次程度を見る。

- PR metadata / current head SHA。
- current head の CI / checks。
- current review / unresolved review threads。
- relevant workflow runs / GUI evidence の有無。

これだけで判断できない場合に限り、必要な evidence を追加取得する。

例:

- review 判断なら Issue、current diff、current review、current unresolved review threads。
- GUI 判断なら scenario、observed result、artifact、baseline。
- CI failure なら failed check / job / log。

無関係な evidence を広く取得しない。

---

## 4. Decide

ChatGPT は current facts とこの Policy から、次に必要な一つの action を決める。

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

CI が current exact head に対して成功している場合、CI を blocker としない。

CI が失敗している場合、そのまま merge 方向へ進めない。

失敗内容を確認し、current PR の変更で修正すべき問題なら fix を選ぶ。

infrastructure failure など product code の問題と判断できない場合は推測せず停止するか、必要な再実行を選ぶ。

### 4.2 Review

current exact head の unresolved findings を一度に確認する。

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
- required CI checks が success。
- 必要と判断した GUI validation がある場合、その結果が受け入れ可能である。
- GitHub が PR を mergeable と報告している。

merge は expected head SHA を指定して行う。

専用 Gate は必須ではない。確認が実運用で繰り返し複雑になる場合だけ、客観条件だけを確認する小さな Gate へ抽出してよい。

---

## 5. Act

選んだ action は、まず既存機能で実行する。

Codex、Claude、CI、GUI validation、GitHub merge は AADW の stage ではなく、その時点で必要なら使う道具である。

worker は意味判断をしない。

repository mutation を伴う action は、実行直前に current head が対象 SHA と一致することを確認する。

Claude が push して head が変わったら、旧 head の evidence は current 判断に使わない。

---

## 6. Observe again

action の結果が GitHub に残ったら、過去の判断をそのまま継続せず、current facts を読み直す。

新しい head なら新しい世代として扱う。

同じ root cause が修正後も繰り返す場合、細かい patch を無制限に続けず、設計見直し、scope 分割、追加 evidence の取得などを判断する。

---

## 7. Provider / infrastructure failure

Claude、Codex、GUI runner などが provider / infrastructure 理由で失敗した場合、その失敗を product failure とみなさない。

必要な evidence が得られなければ停止する。

自動 multi-provider fallback や複雑な retry state machine を前提にしない。

---

## 8. 最終原則

Commander は、

```text
Observe → Decide → Act → Observe
```

を繰り返す。

新しい workflow state を増やすより、必要なら Policy 自体を改善する。

専用の collector / wrapper / Gate は、実運用で必要性が証明された場合だけ追加する。
