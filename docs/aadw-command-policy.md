# AADW Commander Policy

## 1. 目的

この文書は、AADW v2 において ChatGPT が司令塔として判断するときのルールを定める。

AADW v2 の判断は、原則として次の2つだけを入力とする。

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

current facts の取得方法は固定しすぎない。初期 v2 では ChatGPT が GitHub connector の少数の read 操作を固定手順で使う。実運用で重複や高コストが確認された場合だけ `aadw_status(PR)` のような read-only collector へ抽出してよい。

GitHub Actions、worker、collector は、この Policy に書かれた意味判断を代行しない。

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

一度の判断で複数 worker を自動連鎖させない。

current evidence に基づき、次に必要な一つの action を選ぶ。

---

## 3. Current state の確認

利用者から `PR #123 を続けて` のように依頼された場合、まず GitHub から current state の事実を固定手順で取得する。

初期 v2 では原則として次程度を見る。

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

current-state discovery が実運用で重いと確認された場合は、その重複部分だけを read-only collector へ抽出してよい。

---

## 4. CI

### CI success

CI が current exact head に対して成功している場合、CI を blocker としない。

次に必要な検証が何かを current evidence から判断する。

### CI failure

CI が current exact head に対して失敗している場合、そのまま merge 方向へ進めない。

失敗内容を確認し、current PR の変更で修正すべき問題なら fix を指示する。

infrastructure failure など product code の問題と判断できない場合は推測せず停止するか、必要な再実行を選ぶ。

---

## 5. Review

### 5.1 current findings をまとめて読む

current exact head の unresolved findings を一度に確認する。

review comment 1件を fix 1回に対応させない。

### 5.2 root-cause cluster

同じ原因・同じ設計面に属する finding は root-cause cluster にまとめる。

同一 cluster では、normal / error、producer / consumer、兄弟ケースなど、同じ root cause から生じる周辺ケースを fix 前に確認する。

独立した問題を同じ cluster に混ぜない。

### 5.3 classification

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

### 5.4 fix の出し方

blocker がある場合、同じ root-cause cluster のものは一回の fix にまとめる。

独立した root cause は必要に応じて別 fix とする。

fix instruction には、少なくとも次を含める。

- target PR。
- target exact head SHA。
- current Issue の目的。
- root-cause cluster。
- fix scope。
- 触らない独立問題。
- 必要な tests / validation。

---

## 6. Fix 後

Claude が push して head が変わったら、旧 head の evidence は current 判断に使わない。

新 head に対して current-state discovery から再確認する。

同じ root cause が修正後も繰り返す場合、細かい patch を無制限に続けず、設計見直し、scope 分割、追加 evidence の取得などを判断する。

この判断を固定 cycle 数だけで機械化しない。

---

## 7. GUI validation

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

---

## 8. Follow-up

current PR の目的を直接妨げない改善は follow-up に分離できる。

ただし次を follow-up に送って current PR を進めてはいけない。

- security。
- data loss / corruption。
- P0 / P1。
- acceptance criteria を満たせなくする問題。
- current normal path の明確な regression。
- evidence 不足の unknown。

---

## 9. Merge

ChatGPT の意味判断だけでは merge しない。

current Issue の目的と acceptance criteria を満たし、必要な review / GUI 判断を終えたと判断したら `aadw_gate(PR, expected_head)` を実行する。

Gate が失敗した場合は merge しない。

Gate は客観条件だけを確認し、意味判断を行わない。

---

## 10. Provider / infrastructure failure

Claude、Codex、GUI runner などが provider / infrastructure 理由で失敗した場合、その失敗を product failure とみなさない。

必要な evidence が得られなければ停止する。

自動 multi-provider fallback や複雑な retry state machine を前提にしない。

---

## 11. 判断結果

ChatGPT は current facts とこの Policy から、次に必要な一つの action を決める。

典型例:

```text
read more evidence
run review
run fix
run GUI validation
rerun failed infrastructure step
run gate
stop and ask for human decision
```

判断理由は、current facts とこの Policy に基づいて説明できること。

Policy に無い複雑な例外を新しい workflow state として作るより、必要なら Policy 自体を更新する。
