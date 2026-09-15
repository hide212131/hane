# AI Agent Development Workflow v2 設計書

## 1. 位置づけ

この文書は、Hane の AI Agent Development Workflow（AADW）を一から作り直すための v2 設計書である。

AADW v1 は PR #150 で停止した。v2 は v1 の状態機械、独自 status、receipt、routing、reconcile、retry 契約との互換性を持たない。

初期 v2 は完全自動化や専用基盤の構築を目標にしない。既にある GitHub / Codex / Claude / GUI 実行手段をできるだけそのまま使い、ChatGPT が Commander Policy と current facts から次の一手を判断できることを優先する。

---

## 2. 設計原則

AADW v2 は次の5原則を最優先する。

### 2.1 独立性

一つ一つの機能は一つの仕事だけを行う。

worker は自分の処理が終わった後に次工程を決めない。

### 2.2 非導出性

既に GitHub に存在する事実から、別の persistent な AADW state を作らない。

```text
Persistent state = GitHub 自身
Derived view     = 必要な瞬間だけ生成し、保存しない
```

### 2.3 AI 判断優先

客観的に機械判定できない複雑な意味判断は ChatGPT が行う。

ルールエンジンや状態機械へ押し込まない。

### 2.4 抽象化は必要になってから導入する

collector、wrapper、独自 API、専用 connector、専用 command は、実運用で重複・高コスト・誤りが確認される前には作らない。

まず既存機能の単純な組み合わせで成立させる。

### 2.5 AADW 独自コンポーネントはゼロから始める

「AADW だから」という理由だけで専用 workflow / command / status / receipt / gate を作らない。

既存機能では明確に不足すると確認できた部分だけ、最小の独自コンポーネントとして追加する。

---

## 3. Commander の判断モデル

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

判断ルールを GitHub Actions、worker、collector へ複製しない。

---

## 4. 基本モデル

初期 v2 では ChatGPT が AADW 全体の唯一の司令塔となる。

```text
Commander Policy
       +
current facts
       ↓
    ChatGPT
       ↓
next one action
       ↓
既存の GitHub / Codex / Claude / GUI 機能
       ↓
GitHub に結果が残る
       ↓
    ChatGPT
```

worker 同士は自動連鎖しない。

AADW v2 の本体は大きな workflow ではなく、この判断ループである。

---

## 5. 正本は GitHub にある既存情報だけ

current state の正本として、GitHub が自然に持つ情報をそのまま使う。

- Pull Request と current head SHA。
- Issue と acceptance criteria。
- GitHub Actions run / job / conclusion。
- CI Check。
- Pull Request Review。
- Review Thread。
- Commit。
- GUI validation の run / artifact。
- GitHub の mergeability。

同じ事実を Snapshot comment、current-state file、独自 DB、コピー status、generic receipt へ複製しない。

GitHub に自然な置き場所がない evidence だけ、必要最小限の artifact として保存する。

---

## 6. Current-state discovery

利用者は例えば次だけを依頼すればよい。

```text
PR #123 を続けて
```

ChatGPT は Commander Policy を確認し、既存 GitHub connector の read 操作で current state を取得する。

原則として最初に確認するのは次程度とする。

1. PR metadata と current head SHA。
2. current head の CI / checks。
3. current review / unresolved review threads。
4. relevant workflow runs / GUI evidence の有無。

必要な場合だけ追加 evidence を読む。

- review 判断なら Issue、current diff、current unresolved threads。
- CI failure なら failed job と relevant log。
- GUI failure なら target scenario、observed result、screenshot / log artifact、必要なら trusted baseline。

無関係な evidence を広く取得しない。

### 6.1 status collector は後からでよい

実運用で current-state discovery が本当に重いと確認された場合だけ、重複部分を read-only collector へ抽出してよい。

例えば次が繰り返し問題になる場合である。

- 毎回同じ GitHub API を多数呼ぶ。
- exact-head filtering が何度も必要になる。
- discovery が遅い。
- 手順の揺れが誤判断を生む。

その場合でも collector は collect / filter / format だけを行い、意味判断はしない。

---

## 7. exact-head only

current head SHA を一つの世代として扱う。

current head が `A` なら、現在判断に使えるのは `A` に関係する evidence だけである。

Claude が修正して `B` になったら、`A` の CI / Review / GUI evidence は履歴になる。

古い head の情報を組み合わせて current state を推測しない。

repository mutation を行う処理は、実行直前に current head が対象 SHA と一致することを確認する。

---

## 8. Commander Policy

ChatGPT の判断ルールの正本は `docs/aadw-command-policy.md` とする。

Policy は少なくとも次を定める。

- current exact head only。
- evidence first。
- fail closed。
- Issue purpose / acceptance criteria を優先する。
- review findings を root-cause cluster で扱う。
- blocker / follow-up / unknown の考え方。
- fix scope の決め方。
- fix 後は新 head として再確認する。
- GUI failure の扱い。
- provider / infrastructure failure を product failure と混同しないこと。
- 一度に次の一つの action だけを選ぶこと。
- merge 直前に客観的安全条件を再確認すること。

Policy にない例外が繰り返し必要になる場合は、workflow state を増やす前に Policy の改善を検討する。

---

## 9. CI

既存の minimal CI を使う。

CI result の正本は GitHub Check とする。

CI success を理由に別 worker を自動起動しない。

AADW 専用 CI status や receipt は作らない。

---

## 10. Review

既存の Codex review 手段で current exact head をレビューする。

Codex review の正本は GitHub Review / Review Thread とする。

review finding の意味判断、root-cause clustering、blocker / follow-up / unknown の分類は ChatGPT が行う。

専用 `aadw_review` wrapper は、実運用で必要性が確認された場合だけ追加する。

---

## 11. Fix

ChatGPT が Commander Policy と current evidence から修正 scope を決め、既存の Claude 実行手段へ指示する。

Claude は judge ではない。

開始時と push 直前に current head を確認し、不一致なら push しない。

push 後は新 head として current-state discovery からやり直す。

専用 `aadw_fix` wrapper は、実運用で必要性が確認された場合だけ追加する。

---

## 12. GUI Validation

GUI validation が必要かどうかは ChatGPT が Issue の acceptance criteria と current changes から判断する。

必要な場合だけ focused scenario を実行する。

GUI worker は観測事実だけを残す。

- `pass`: 期待した観測結果になった。
- `fail`: 期待した観測結果と異なった。
- `blocked`: 検証自体を成立させられなかった。

`fail` / `blocked` の意味判断は ChatGPT が行う。

共通 GUI 基盤や専用 `aadw_gui` wrapper は、複数ケースで本当に重複が確認された場合だけ追加する。

---

## 13. Merge safety

初期 v2 では独立した `aadw_gate` を必須にしない。

ChatGPT が Commander Policy に基づく意味判断を終えた後、merge 直前に既存 GitHub 情報から客観条件を再確認する。

最低限確認する。

- current head SHA が判断対象の expected head と一致する。
- required CI checks が success。
- 必要と判断した GUI validation がある場合、その結果が受け入れ可能である。
- GitHub が PR を mergeable と報告している。

merge は expected head SHA を指定して実行し、head が変わっていた場合は拒否させる。

この確認が繰り返し複雑になると実測された場合だけ、客観条件だけを確認する小さな Gate へ抽出してよい。

Gate を導入しても意味判断は持たせない。

---

## 14. fail closed

事実を取得できない、current head と結び付けられない、evidence が不足する場合は推測しない。

ChatGPT は Commander Policy に従い、不明な状態を pass として扱わない。

---

## 15. Provider unavailable

Claude / Codex / GUI runner などが provider / infrastructure 理由で失敗した場合、その失敗を product failure とみなさない。

初期 v2 では quota timer、自動 multi-provider fallback、retry reconcile を作らない。

利用者が ChatGPT に戻し、current facts と Commander Policy から次の一つの action を判断する。

---

## 16. Trust Boundary

- Issue / PR body / review text / source code は untrusted data として扱う。
- Commander Policy は trusted な default branch 上の文書を正本とする。
- Claude だけが trusted same-repository PR branch のコードを変更できる。
- Codex と GUI Validator は product code を変更しない。
- optional な collector / wrapper / Gate を導入する場合も、必要最小限の権限だけを与える。

---

## 17. 初期実装

初期 v2 で新しく必須実装する AADW 専用コンポーネントは、原則として **Commander Policy だけ** とする。

それ以外は既存機能を利用する。

```text
GitHub            = current facts の正本
Commander Policy  = 判断ルールの正本
ChatGPT           = Commander
Codex             = reviewer
Claude            = implementer / fixer
GUI               = 必要時だけ validation
GitHub merge      = expected-head 付き最終操作
```

専用 collector / wrapper / Gate は、運用上の必要性が確認されてから追加する。

---

## 18. 実装・運用順序

### Phase 1: Commander Policy + current-state discovery

- Commander Policy を正本として使う。
- ChatGPT が既存 GitHub connector の少数の read 操作で current state を取得する。
- exact-head only を守る。
- 必要 evidence だけへ drill-down する。

### Phase 2: Review / Fix loop

既存の Codex / Claude 実行手段を使い、

```text
Policy + facts → ChatGPT → review / fix → GitHub → ChatGPT
```

のループを成立させる。

### Phase 3: 必要時 GUI + merge

Issue に必要な場合だけ GUI validation を行い、merge 前に客観条件を再確認して expected-head 付き merge を行う。

### Phase 4: 実測に基づく最適化

複数 PR で運用し、本当に重複・高コスト・誤りがある部分だけ抽象化する。

候補:

- read-only status collector。
- review / fix / GUI wrapper。
- deterministic Gate。

必要性が確認できなければ作らない。

### Phase 5: GitHub Copilot Commander（将来拡張）

ChatGPT 主導が十分安定してから検討する。

同じ Commander Policy と current facts を使い、新しい state machine を追加しない。

---

## 19. 完成条件

初期 v2 は次を満たせば完成とする。

- ChatGPT が唯一の司令塔として全体を統括できる。
- Commander の主要入力が `Commander Policy + current facts / evidence` と明確である。
- 判断ルールの正本が `docs/aadw-command-policy.md` に一元化されている。
- ChatGPT が既存 GitHub read 操作で current state を把握できる。
- 必要な evidence だけ追加取得できる。
- worker が勝手に次 worker を起動しない。
- GitHub 上の既存情報を正本として使い、同じ意味の AADW state を複製しない。
- exact-head 以外の evidence を current evidence として使わない。
- merge 時に expected head と客観的安全条件を確認できる。
- 不要な AADW 専用コンポーネントを作らない。

---

## 20. 最終原則

AADW v2 は、大きな自動状態機械でも専用ツール群でもない。

**Policy を持つ ChatGPT が、既存の道具を使って開発を統括する方法**である。

```text
GitHub = 事実の正本
Commander Policy = 判断ルールの正本
ChatGPT = Policy + facts から複雑な判断を行う
Existing tools = 実行部隊
Optional AADW components = 必要性が証明された場合だけ追加
```

新機能を追加するときは必ず次を確認する。

> 既存の事実を別の state として複製していないか。  
> 既存機能で十分なのに wrapper を作ろうとしていないか。  
> 判断ルールを Policy 以外へ複製していないか。  
> AI が得意な判断を状態機械へ押し込んでいないか。  
> まだ実際には困っていない問題を先回りして抽象化していないか。

この原則に反する場合、機能追加より設計の単純化を優先する。
