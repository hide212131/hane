# AI Agent Development Workflow v2 設計書

> [!IMPORTANT]
> この文書は v2 の設計記録である。現行の正本は [AADW v3 設計書](agentic-development-workflow-v3.md)、[ADR-0031](adr/0031-aadw-v3-jev-bounded-execution.md)、[Commander Policy](aadw-command-policy.md) とする。v3 は v2 の `Observe → Decide → Act → Observe` と GitHub facts を正本にする原則を引き継ぐ。

## 1. 位置づけ

この文書は、Hane の AI Agent Development Workflow（AADW）を一から作り直すための v2 設計書である。

AADW v1 は PR #150 で停止した。v2 は v1 の状態機械、独自 status、receipt、routing、reconcile、retry 契約との互換性を持たない。

AADW v2 は、新しい自動化基盤や大きな workflow を作ることを目的にしない。

**Commander Policy を持つ ChatGPT が、GitHub 上の現在の事実を読み、必要な既存ツールを一つ使い、その結果を再び観測する。**

これを基本とする。

---

## 2. 最重要原則

### 2.1 独立性

一つの機能は一つの仕事だけを行う。

Codex、Claude、GUI validation、CI などは、自分の処理が終わった後に次工程を決めない。

### 2.2 非導出性

GitHub に既に存在する事実から、別の persistent な AADW state を作らない。

```text
Persistent state = GitHub 自身
Derived view     = 必要な瞬間だけ生成し、保存しない
```

### 2.3 AI 判断優先

客観的に機械判定できない複雑な意味判断は ChatGPT が行う。

ルールエンジンや状態機械へ押し込まない。

### 2.4 抽象化は必要になってから導入する

collector、wrapper、専用 workflow、専用 command、専用 Gate などは、実運用で重複・高コスト・誤りが確認される前には作らない。

まず既存機能で成立させる。

### 2.5 AADW 独自コンポーネントはゼロから始める

初期 v2 で新しく必須にする AADW 固有のものは、原則として `docs/aadw-command-policy.md` だけとする。

既存機能では明確に不足すると確認できた部分だけ、後から最小の専用部品として追加する。

---

## 3. AADW v2 の本体

AADW v2 は workflow / state machine ではない。

**Observe → Decide → Act → Observe** の反復である。

```text
Observe
  GitHub から current facts / evidence を読む
        ↓
Decide
  Commander Policy + facts を ChatGPT が評価する
        ↓
Act
  次に必要な一つの action を既存ツールで実行する
        ↓
Observe
  結果を GitHub から読み直す
```

概念的には次だけである。

```text
while PR is open:
    observe current facts
    decide next one action using Commander Policy
    execute one action
```

これは実際に while loop や自動 state machine を実装するという意味ではない。

利用者が ChatGPT に、例えば次のように依頼する。

```text
PR #123 を続けて
```

ChatGPT は current facts を読み、次の一手だけを決める。

---

## 4. Commander の判断モデル

ChatGPT が判断するための主要入力は2つだけである。

1. **Commander Policy** — どう判断するか。
2. **current facts / evidence** — 今何が起きているか。

```text
Commander Policy
        +
current facts / evidence
        ↓
      ChatGPT
        ↓
   next one action
```

Commander Policy の正本は `docs/aadw-command-policy.md` とする。

判断ルールを GitHub Actions、worker、collector、wrapper に複製しない。

---

## 5. Current facts の正本

current state の正本として、GitHub が自然に持つ情報をそのまま使う。

- Pull Request と current head SHA。
- current target branch とその head / base context。
- Issue と acceptance criteria。
- CI Check。
- GitHub Actions run / job / conclusion と、取得できる場合はその run が対象にした PR base SHA。
- Pull Request Review。
- Review Thread。
- Commit。
- GUI validation の run / artifact。
- GitHub の mergeability。

同じ事実を Snapshot comment、current-state file、独自 DB、コピー status、generic receipt へ複製しない。

GitHub に自然な置き場所がない evidence だけ、必要最小限の artifact として保存する。

---

## 6. Observe: current-state discovery

ChatGPT は最初から repository 全体を探索しない。

まず既存 GitHub connector の少数の read 操作で、current state の概要を確認する。

原則として最初に見るのは次程度とする。

1. PR metadata と current head SHA。
2. current target branch head / base context。
3. current context の CI / checks。
4. current review / unresolved review threads。
5. relevant workflow runs / GUI evidence の有無。

これだけで判断できない場合に限り、必要な evidence を追加取得する。

例:

- review 判断なら Issue、current diff、current unresolved threads、review 後に target branch が進んでいないか。
- CI failure なら failed job、relevant log、run が対象にした PR base SHA。
- GUI failure なら scenario、observed result、artifact、必要なら trusted baseline、対象 head / base context。

無関係な evidence は広く取得しない。

### 6.1 Status Collector は最適化であり前提ではない

実運用で次のような問題が繰り返し確認された場合だけ、current-state discovery の重複部分を read-only collector へ抽出してよい。

- 毎回同じ GitHub API を多数呼ぶ。
- current context の filtering が繰り返される。
- discovery が遅い。
- 手順の揺れが誤判断を生む。

必要性が確認できなければ作らない。

---

## 7. current PR context: head + 必要な base context

product branch の mutation 世代は current head SHA で扱う。

current head が `A` のとき、`A` 以外の head に対する CI / Review / GUI evidence は current 判断に使わない。Claude が修正して `B` になったら、`A` の evidence は履歴になる。

ただし PR の evidence freshness は常に head SHA だけで決まるわけではない。head が `A` のままでも target branch が進めば、PR diff、merge result、base-sensitive な CI、review、GUI scenario の前提が変わり得る。

その evidence の主張が target branch / merge context に影響される場合は、current head に加えて current target branch / base context と整合しているかを確認する。evidence がどの base context に対するものか確認できない、または current base と異なる場合は、同じ head だからという理由だけで current 扱いしない。必要なら current base context で取り直す。

一方、base の変更が evidence の主張に影響しないと Commander が current facts と check / scenario の性質から判断できる場合は、head に結び付く evidence を利用できる。その判断は worker に委ねず、根拠を GitHub 上に残す。

PR metadata の base SHA が常に target branch の現在の head を表すとは仮定しない。base-sensitive な判断では必要なら target branch 自体を読み、current base を確認する。

この head / base context を AADW 独自の persistent generation として保存しない。GitHub 上の事実から、その時点だけ導出する。

repository mutation を行う処理は別の競合防止責務として、実行開始時と push 直前に current head が対象 SHA と一致することを確認する。

---

## 8. Decide: Commander Policy

ChatGPT の判断ルールの正本は `docs/aadw-command-policy.md` とする。

Policy は少なくとも次を定める。

- current head と、base-sensitive な evidence に必要な current base context の freshness。
- base-independent evidence の扱いを Commander が判断すること。
- repository mutation の exact-head guard。
- evidence first。
- fail closed。
- Issue purpose / acceptance criteria を優先する。
- review findings を root-cause cluster で扱う。
- blocker / follow-up / unknown の考え方。
- fix scope の決め方。
- fix 後は新 head として再確認する。
- base が進んだ場合の evidence 再評価。
- GUI failure の扱い。
- provider / infrastructure failure を product failure と混同しないこと。
- 一度に次の一つの action だけを選ぶこと。
- merge 直前に客観的安全条件を再確認すること。

Policy にない例外が繰り返し必要になる場合は、workflow state を増やす前に Policy の改善を検討する。

---

## 9. Act: 既存ツールを使う

Codex、Claude、CI、GUI validation、GitHub merge は、AADW 固有の stage ではない。

ChatGPT が current facts と Commander Policy から選ぶ **action の候補**である。

### Review

必要なら既存の Codex review 手段で current PR head をレビューする。

review の主張が PR diff / merge context に影響する場合は、head が同じでも review 後に target branch が進んだかを確認し、必要に応じて current base context でレビューを取り直す。base の変更が review の主張に影響しないと Commander が判断できる場合は、その根拠を GitHub 上に残して head に結び付く review evidence を利用できる。

Review の正本は GitHub Review / Review Thread とする。

### Fix

必要なら ChatGPT が修正 scope を決め、既存の Claude 実行手段へ指示する。Claude の実行が trusted な失敗分類器によって `usage_or_rate_limit` と判定された場合だけ、同じ exact head に対する限定的な Codex CLI fallback を利用できる。これは provider の利用上限からの実装継続であり、Codex が次の工程を判断する仕組みではない。

Claude と Codex の実装 worker は judge ではない。Codex fallback は self-hosted Mac の専用 runner でのみ実行し、trusted Commander handoff、same-repository PR、current head、push 直前の exact-head guard を再確認する。Codex のローカル実行には GitHub の認証情報を渡さない。

実装 worker は開始時と push 直前に current head を確認し、不一致なら push しない。`authentication`、`max_turns`、`model_or_provider`、`unknown`、診断不能な失敗では fallback を起動しない。

### GUI validation

Issue の acceptance criteria と変更内容から必要だと ChatGPT が判断した場合だけ実行する。

focused scenario を基本とする。

GUI evidence は対象 head と、その scenario に base / merge context が影響する場合は実際に検証した context を対応付ける。head が同じでも base の product code が scenario に影響する形で変われば、必要に応じて validation を取り直す。base-independent と判断できる場合は、その根拠を GitHub 上に残して head-only evidence を利用できる。

GUI worker は観測事実だけを残し、`fail` / `blocked` の意味判断は ChatGPT が行う。

### Merge

ChatGPT の意味判断だけでは merge しない。

merge 直前に GitHub 上の客観条件を再確認する。

最低限確認する。

- current head SHA が判断対象の expected head と一致する。
- current target branch / base context が、採用する base-sensitive な CI / review / GUI evidence の前提と整合している。
- required CI checks が current context で success。
- 必要と判断した GUI validation がある場合、その結果が current context で受け入れ可能である。
- GitHub が PR を mergeable と報告している。

base-independent と判断した evidence は、その根拠が GitHub 上に残っていることを確認する。

merge は expected head SHA を指定して行う。expected head は concurrent product branch mutation を防ぐ guard であり、base freshness の代わりにはならない。

専用 Gate は初期必須ではない。この確認が実運用で繰り返し複雑になる場合だけ、小さな客観チェックへ抽出する。

---

## 10. Review / Fix の判断

review finding の件数をゼロにすること自体を目的にしない。

current Issue の目的と acceptance criteria を優先する。

current findings はまとめて読み、同じ原因・同じ設計面に属するものを root-cause cluster として扱う。

cluster は意味判断として次のいずれかに分類する。

- `blocker`: current PR で修正が必要。
- `follow_up`: current PR の acceptance を妨げず、別 Issue 候補にできる。
- `unknown`: evidence 不足または原因不明。進めない。

同じ root-cause cluster の blocker は、一回の fix にまとめる。

独立した root cause を一つの fix に混ぜない。

---

## 11. fail closed

事実を取得できない、evidence を current head と、その主張に必要な context に結び付けられない、または evidence が不足する場合は推測しない。

ChatGPT は Commander Policy に従い、不明な状態を pass として扱わない。

---

## 12. Provider / infrastructure failure

Claude / Codex / GUI runner などが provider / infrastructure 理由で失敗した場合、その失敗を product failure とみなさない。

通常は利用者が ChatGPT に戻し、current facts と Commander Policy から次の一つの action を判断する。ただし Claude の失敗分類が `usage_or_rate_limit` の場合だけ、既存の GitHub Actions と self-hosted `hane-codex` runner による限定的な Codex CLI fallback を許可する。これは一般的な provider routing、quota timer、retry state machine ではない。

fallback は checkpoint artifact の分類結果を読み、分類以外の意味判断を行わない。artifact がない、分類が不明、actor / repository / head の guard が満たせない場合は fail closed で停止する。

---

## 13. Trust Boundary

- Issue / PR body / review text / source code は untrusted data として扱う。
- Commander Policy は trusted な default branch 上の文書を正本とする。
- Claude が通常の trusted same-repository PR branch のコードを変更する。Claude の `usage_or_rate_limit` fallback に限り、Codex CLI も専用 self-hosted runner から同じ trusted branch を変更できる。
- Codex review と GUI Validator は product code を変更しない。Codex fallback の repository mutation は、trusted actor、same-repository、exact head、保護パス拒否、push 直前の再確認を満たす場合だけ許可する。
- optional な collector / wrapper / Gate を導入する場合も、必要最小限の権限だけを与える。

---

## 14. 初期 v2 で実装するもの

原則として **Commander Policy だけ** とする。

それ以外は既存機能を使う。

```text
GitHub            = current facts の正本
Commander Policy  = 判断ルールの正本
ChatGPT           = Commander
Existing tools    = Codex / Claude / CI / GUI / GitHub merge
```

AADW 専用の collector / wrapper / status / receipt / Gate は、必要性が実運用で確認された場合だけ追加する。Issue #241 の fallback は、既存の Claude failure checkpoint と GitHub `workflow_run` を使う限定的な provider failure 継続経路であり、別の検索 index や汎用 routing state を追加しない。

---

## 15. 運用開始順序

### Step 1: Observe → Decide → Act をそのまま運用する

- Commander Policy を使う。
- ChatGPT が既存 GitHub connector で current facts を読む。
- 必要 evidence だけ追加取得する。
- 次の一つの action を既存ツールで実行する。
- 結果を再び GitHub から読む。

### Step 2: 複数 PR で運用する

実際にどこが遅いか、重複するか、誤りやすいかを観測する。

### Step 3: 実測された重複だけ抽象化する

候補:

- read-only status collector。
- review / fix / GUI wrapper。
- merge safety checker。

必要性が確認できなければ作らない。

### Step 4: GitHub Copilot Commander は将来検討する

ChatGPT 主導が十分安定してから検討する。

同じ Commander Policy と current facts を使い、新しい state machine を追加しない。

---

## 16. 完成条件

初期 v2 は次を満たせば完成とする。

- `Observe → Decide → Act → Observe` のループで開発を統括できる。
- ChatGPT が唯一の Commander として動く。
- Commander の入力が `Commander Policy + current facts / evidence` に整理されている。
- 判断ルールが `docs/aadw-command-policy.md` に一元化されている。
- GitHub 上の既存情報を正本として使う。
- current head と、その主張に必要な base / merge context に整合しない evidence を current evidence として使わない。
- base-independent evidence は Commander が根拠を残して利用できる。
- repository mutation は expected head guard で concurrent change を上書きしない。
- worker が勝手に次 action を決めない。
- merge 時に expected head、必要な current base context、客観的安全条件を確認する。
- 不要な AADW 専用コンポーネントを作らない。

---

## 17. 最終原則

AADW v2 は、大きな workflow でも state machine でも専用ツール群でもない。

**Policy を持つ ChatGPT が、GitHub 上の現在の事実を観測し、既存の道具から次の一手を選び、結果を再観測する方法**である。

```text
Observe → Decide → Act → Observe
```

残すべき最小核は次の3つである。

```text
Commander Policy
GitHub facts
ChatGPT
```

Codex / Claude / CI / GUI / merge は AADW の state や stage ではなく、必要に応じて使う既存ツールである。

新しい仕組みを追加するときは必ず次を確認する。

> 既存の事実を別の state として複製していないか。  
> 既存機能で十分なのに wrapper を作ろうとしていないか。  
> 判断ルールを Policy 以外へ複製していないか。  
> AI が得意な判断を状態機械へ押し込んでいないか。  
> まだ実際には困っていない問題を先回りして抽象化していないか。

この原則に反する場合、機能追加より設計の単純化を優先する。
