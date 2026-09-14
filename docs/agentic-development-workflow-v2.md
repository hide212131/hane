# AI Agent Development Workflow v2 設計書

## 1. 位置づけ

この文書は、Hane の AI Agent Development Workflow（AADW）を一から作り直すための v2 設計書である。

AADW v1 は PR #150 で停止した。v2 は v1 の状態機械、status、receipt、routing、GUI generation、reconcile、retry 契約との互換性を持たない。v1 の情報は履歴として参照してよいが、v2 の current state や merge 判断には利用しない。

v2 の目的は、自動化率を最大化することではない。次の3点を優先する。

1. 正常時に同じ流れで進むこと。
2. 途中で失敗したとき、どこで止まったか分かること。
3. 古い結果や別実行の結果を誤って現在の Pull Request に使わないこと。

AI サービスの利用上限や障害を完全自動で隠蔽しようとして、ワークフロー自体を複雑にしない。

---

## 2. 全体構造

AADW v2 は、意味を判断する **Commander** と、GitHub 上の進行を管理する **Orchestrator** を分離する。

```text
                      ┌─────────────────────┐
                      │      Commander      │
                      │                     │
                      │ Primary: Copilot    │
                      │ Fallback: ChatGPT   │
                      └──────────┬──────────┘
                                 │
                            方針を判断
                                 │
                                 ▼
                      ┌─────────────────────┐
                      │ AADW Orchestrator   │
                      │   GitHub Actions    │
                      └──────────┬──────────┘
                                 │
              ┌──────────────────┼───────────────────┐
              ▼                  ▼                   ▼
           Claude              Codex              GUI Validator
          implement            review               validate
              │                  │                   │
              └──────────────────┴───────────────────┘
                                 │
                                 ▼
                        Deterministic Gate
                                 │
                                 ▼
                               merge
```

GitHub Copilot を通常時の Commander とする。

GitHub Copilot が credit / quota / service availability などの理由で利用できない場合は、利用者が ChatGPT を呼び出し、同じ司令方針で Commander を代行させる。

GitHub Actions は AI の意味判断を代替しない。認証、現在状態の確認、worker 起動、結果の検証、状態遷移、merge の機械的な安全確認を担当する。

---

## 3. 役割

### 3.1 Work / ChatGPT

実装前の要求整理、調査、設計、受け入れ条件を担当する。

製品コードを変更しない。Issue が実装可能な状態になった時点で設計を終了する。

### 3.2 Claude Code

初回実装と、Commander が `fix` と判断した後の修正を担当する。

Claude Code は judge ではない。approve、merge、検証結果の偽造、merge gate の迂回を行わない。

### 3.3 Codex

Pull Request の reviewer を担当する。

レビューは current exact head 全体を対象とする。レビューコメント1件ごとに修正を起動する前提にはしない。

### 3.4 GUI Validator

Issue の受け入れ条件に対応する focused GUI scenario を実行する。

製品コードを変更しない。

### 3.5 Commander

意味を理解し、次の方針を判断する。

通常は GitHub Copilot、fallback は ChatGPT とする。

Commander が判断する例:

- review finding が current Pull Request の blocker か。
- 複数 finding が同じ root cause か。
- Claude にどの root-cause cluster をまとめて直させるか。
- GUI failure が current regression / target acceptance blocker / validation infrastructure problem / independent issue のどれか。
- current Pull Request に不要な改善を follow-up に分離できるか。
- 現在の証拠では安全に判断できず、`blocked` にすべきか。

### 3.6 AADW Orchestrator

GitHub Actions 上の trusted な通常プログラムとする。

AI は使わない。

Orchestrator は current Pull Request / current head SHA / worker receipt / Commander Decision を検証し、次に必要な worker を一つだけ起動する。

Orchestrator 自身は、review finding の意味や GUI failure の原因を判断しない。

### 3.7 Deterministic Gate

AI を使わない。

現在の exact head SHA に必要な証拠がすべて揃っている場合だけ merge を許可する。

---

## 4. 最重要原則

### 4.1 一つの head SHA を一つの世代として扱う

current PR head が `A` なら、現在の判断には `A` の証拠だけを使う。

```text
CI(A)
Review(A)
Commander(A)
GUI(A)
```

Claude が修正して head が `B` になった瞬間、`A` の証拠はすべて履歴になる。

```text
CI(B)
Review(B)
Commander(B)
GUI(B)
```

を新しく取得する。

古い SHA の結果を新しい SHA に継承する仕組みは作らない。

prior-head の複数 status を読み合わせて current state を復元する仕組みも作らない。

### 4.2 Commander と Orchestrator を混ぜない

Commander は意味判断をする。

Orchestrator は現在状態を検証し、許可された副作用を実行する。

Commander に repository write / merge 権限を持たせない。

### 4.3 AI の判断だけでは merge しない

Copilot も ChatGPT も merge を実行しない。

最終 merge は Deterministic Gate の機械的な確認を必須とする。

### 4.4 fail closed

証拠不足、head 不一致、schema 不正、AI 出力不明、validation infrastructure failure など、安全に判断できない状態を `continue` や `pass` に変換しない。

不明なら `blocked` または `error` で停止する。

### 4.5 recovery のために別の状態機械を増やさない

一つの edge case 専用 reconcile workflow を追加しない。

イベントを取りこぼした場合は、単純な Watchdog が同じ Orchestrator を再起動する。Watchdog 自身は状態遷移を判断しない。

---

## 5. Commander Policy

Copilot と ChatGPT で別々の判断ルールを持たない。

両者はこの文書の Commander Policy を正本として使う。

将来ルールが大きくなった場合は `docs/aadw-command-policy.md` へ分離してよいが、複数箇所へコピーしない。

### 5.1 判断の優先順位

Commander は、review finding の件数をゼロにすることより、current Issue の目的と受け入れ条件を満たすことを優先する。

current Pull Request の blocker とするのは、原則として次である。

- P0 / P1。
- セキュリティ問題。
- データ損失・破損。
- 権限逸脱。
- 通常経路で再現する明確な不具合。
- CI failure。
- current Issue の目的・受け入れ条件を直接満たせなくする問題。

それ以外の改善は `follow_up` に分離できる。

### 5.2 root-cause cluster

一つの review comment を一つの fix cycle としない。

Commander は current exact head の current findings を一度に確認し、同じ原因・同じ設計面に属するものを root-cause cluster にまとめる。

例:

- producer / consumer 間の evidence contract。
- pass / fail / blocked の terminal contract。
- source mapping boundary。
- state restoration。
- IME validation。

同一 cluster について、正常系/異常系、producer/consumer、pass/fail/blocked などの兄弟ケースを fix 前に横断確認する。

独立した root cause を一つの cluster に混ぜない。

### 5.3 classification

cluster の classification は次の3種類だけとする。

- `blocker`: current Pull Request で修正が必要。
- `follow_up`: current Pull Request を止めず、別 Issue 候補として残す。
- `unknown`: 証拠不足や原因不明。fail closed で current Pull Request を止める。

### 5.4 repeated blocker

同じ root-cause cluster の merge-blocking P1 が fix 後も繰り返される場合、小さな patch を積み続けない。

自動 fix は原則として同一 PR で最大2 cycle とする。

2 cycle 後も blocker が残る場合は `blocked` とし、`design-review-required` を理由に人へ引き継ぐ。

これは blocker を無視して merge するための上限ではない。

---

## 6. Commander Request

Commander は GitHub 全体を自由に推測して判断しない。

Orchestrator が current exact head に必要な情報をまとめ、Commander Request を生成する。

概念 schema:

```json
{
  "schema_version": 1,
  "request_id": "aadw-v2-123-a1b2c3-review-01",
  "policy_version": 1,
  "pr_number": 123,
  "issue_number": 101,
  "base_sha": "111111...",
  "head_sha": "a1b2c3...",
  "stage": "review",
  "evidence_fingerprint": "sha256:...",
  "ci": {
    "outcome": "pass"
  },
  "review": {
    "findings": []
  },
  "gui": null,
  "convergence": {
    "fix_cycles": 1,
    "review_cycles": 2
  }
}
```

`request_id`、`head_sha`、`evidence_fingerprint`、`policy_version` を current evidence に binding する。

head または evidence が変われば、古い Request は無効になる。

---

## 7. Commander Decision

Copilot と ChatGPT は同じ schema で結果を返す。

概念 schema:

```json
{
  "schema_version": 1,
  "request_id": "aadw-v2-123-a1b2c3-review-01",
  "policy_version": 1,
  "head_sha": "a1b2c3...",
  "evidence_fingerprint": "sha256:...",
  "commander": "github-copilot",
  "decision": "fix",
  "reason": "current Issue の受け入れ条件を直接妨げる問題がある",
  "clusters": [
    {
      "id": "source-map-boundary",
      "classification": "blocker",
      "severity": "P1",
      "summary": "..."
    }
  ],
  "fix_scope": [
    "source-map-boundary"
  ],
  "follow_up": []
}
```

Commander の主要 decision は次の3種類だけとする。

### `fix`

current head に変更が必要。

Orchestrator は validated Decision に従い Claude Fix を起動する。

### `continue`

Commander の意味判断上、次工程へ進めてよい。

Orchestrator が current state を再確認し、次の validation または Gate へ進む。

### `blocked`

安全に判断できない、または人の判断が必要。

自動処理を停止する。

---

## 8. GitHub Copilot Commander

通常時は GitHub Copilot を Commander とする。

Copilot には Commander Request と Commander Policy を与える。

Copilot は structured Commander Decision を返すだけとする。

repository write、workflow write、merge 権限を与えない。

自由文だけを workflow command として扱わず、schema validation を必須とする。

---

## 9. ChatGPT Fallback Commander

### 9.1 fallback 条件

Copilot が次のような外部要因で利用できない場合、Orchestrator は無理に自動 fallback せず、安全に停止する。

- credit exhausted。
- quota exhausted。
- service unavailable。
- Commander invocation が provider 側理由で実行できない。

PR の AADW Status に、Copilot が unavailable で ChatGPT fallback を利用できることを表示する。

### 9.2 起動

ChatGPT は GitHub から常時自動起動される worker にはしない。

利用者が ChatGPT に、たとえば次のように依頼する。

```text
Hane PR #123 の AADW Commander を代行して
```

ChatGPT は過去の会話だけで判断せず、GitHub から current state を読み直す。

最低限、次を確認する。

- Pull Request metadata / diff。
- current exact head SHA。
- linked Issue と受け入れ条件。
- current Commander Request。
- current review / non-outdated unresolved review threads。
- CI evidence。
- GUI evidence。
- fix / review cycle。
- Commander Policy。

### 9.3 Copilot と同じ方針を使う

ChatGPT fallback だから品質基準を緩めない。

Copilot と同じ Request、Policy、Decision schema を使う。

違いは `commander` フィールドだけとする。

```json
{
  "commander": "chatgpt-fallback"
}
```

### 9.4 GitHub への返却

ChatGPT は判断後、Pull Request に機械可読な Commander Decision を返せるようにする。

概念例:

````markdown
<!-- aadw-v2-commander-decision -->

```json
{
  "schema_version": 1,
  "request_id": "aadw-v2-123-a1b2c3-review-01",
  "policy_version": 1,
  "head_sha": "a1b2c3...",
  "evidence_fingerprint": "sha256:...",
  "commander": "chatgpt-fallback",
  "decision": "fix",
  "reason": "...",
  "clusters": [],
  "fix_scope": []
}
```
````

Orchestrator はコメントを見つけただけでは受理しない。

少なくとも次を再確認する。

- `request_id` が現在要求中のもの。
- `head_sha` が current PR head と一致。
- `evidence_fingerprint` が current evidence と一致。
- `policy_version` が current version。
- schema が正しい。
- fallback が現在許可されている。
- trusted actor からの入力である。

ChatGPT の GitHub connector がどの actor として投稿するかは実装時に live test で確認する。repository owner として信頼できない actor になる場合は、owner による明示的な accept 操作を一度要求する。

### 9.5 stale 防止

ChatGPT が調査している途中で head または evidence が変わった場合、その Decision は無効とする。

Decision を受理する直前に Orchestrator が current head と evidence fingerprint を再取得する。

---

## 10. Orchestrator

Orchestrator は起動するたび、current state を GitHub から確認する。

処理順序を固定する。

```text
1. PR を取得
2. current head SHA を取得
3. trust 条件を確認
4. CI を確認
5. Review を確認
6. 必要なら Commander Request
7. Decision が fix なら Claude Fix
8. Review/Commander が通れば必要な GUI validation
9. GUI fail/blocked なら必要に応じて Commander Request
10. Deterministic Gate
```

worker 同士を直接つながない。

worker 完了後は必ず Orchestrator に戻す。

GitHub Actions の暗黙のイベント連鎖へ依存せず、`workflow_dispatch` または `repository_dispatch` の明示的な起動を使う。

---

## 11. CI

CI は current exact head に対して実行する。

v2 開発開始時点では、PR #150 で残した最小 CI を安全ベルトとして使う。

少なくとも macOS / Windows の test / clippy が必要である。

壊れた build を AI reviewer に渡さないため、原則として CI 成功後に Review へ進む。

CI failure をそのまま `continue` にしてはいけない。

---

## 12. Review

Codex は current exact head 全体をレビューする。

Review の current findings を Commander が一度に確認し、root-cause cluster にまとめる。

例:

```text
5 comments
    ↓
2 root causes
    ↓
1 Claude fix run
```

review finding の件数自体を merge condition にしない。

通常の exact-head review が current scope を十分確認できる場合、focused review を追加しない。

focused review が必要な場合は、通常 review では確認できない理由を明示する。

---

## 13. Claude Fix

Commander Decision が `fix` の場合、Orchestrator が current blocker cluster をまとめて Claude へ渡す。

review comment 1件ずつ Claude を起動しない。

Claude は `fix_scope` に示された blocker を中心に確認し、同じ root cause の兄弟ケースも横断確認する。

外部への変更を加える effect boundary では exact-head guard を必須とする。

少なくとも次の2回、current head を確認する。

```text
worker 開始前:
current head == target SHA

Claude 修正後、push 直前:
current head == target SHA
```

不一致なら push せず stale として停止する。

修正 push により新しい head SHA ができたら、古い CI / Review / Commander / GUI はすべて履歴となり、新 head で最初から検証する。

---

## 14. GUI Validation

### 14.1 focused scenario

GUI validation は current Issue の受け入れ条件に対応する focused scenario を中心にする。

すべての PR に巨大な full GUI regression suite を merge blocker として課さない。

full regression が必要なら nightly / scheduled validation として分け、unrelated failure が自動的に current PR を止めないようにする。

### 14.2 scenario isolation

各 mutating scenario は独立した fixture / application state で実行する。

```text
scenario A
Hane 起動
fixture A
test
終了

scenario B
Hane 起動
fixture B
test
終了
```

前の scenario の編集結果や失敗を次へ持ち越さない。

### 14.3 outcome

GUI outcome は次の3種類とする。

- `pass`: 対象 scenario の期待を満たした。
- `fail`: Hane の観測結果が期待と異なった。
- `blocked`: 検証装置・環境・証拠不足などで製品の成否を判定できなかった。

OCR で座標を特定できない、runner が起動できない、fixture を既知状態へ戻せない、証拠が不足する、といった問題を製品 `fail` と混同しない。

### 14.4 Commander への接続

`pass` の場合、意味判断が不要なら次へ進む。

`fail` / `blocked` の場合は Commander に判断を求められる。

Commander は evidence に基づき、たとえば次を区別する。

- current regression。
- target acceptance blocker。
- proven pre-existing independent issue。
- validation infrastructure problem。
- unknown。

`unknown` は fail closed で `blocked` とする。

pre-existing independent と判断する場合は、current base に anchor された比較可能な trusted baseline など、機械検証できる証拠を要求する。AI の推測だけで current blocker を waive しない。

---

## 15. Worker Receipt

各 worker は共通 schema の receipt を残す。

概念 schema:

```json
{
  "schema_version": 1,
  "step": "review",
  "pr_number": 123,
  "head_sha": "a1b2c3...",
  "run_id": 123456789,
  "outcome": "pass",
  "reason_code": "review_clean",
  "summary": "...",
  "data": {},
  "usage": {}
}
```

AI worker について取得可能な場合は `usage` に token 数や推定費用を記録してよい。

費用情報は merge 判断には使わない。

receipt は current exact head に binding する。

---

## 16. 状態表現

v2 の current status は少数に絞る。

例:

```text
aadw-v2/ci
aadw-v2/review
aadw-v2/commander
aadw-v2/fix
aadw-v2/gui
aadw-v2/gate
```

status の意味は通常の意味に揃える。

- `pending`: 実行待ち / 実行中。
- `success`: その step が正常に完了。
- `failure`: 対象に実際の問題がある。
- `error`: 実行環境、AI provider、証拠不足などで判定不能。

`failure = GUI が必要` のような意味の反転は禁止する。

v1 の `hane/*` status は v2 では読まない。

---

## 17. PR 上の状態表示

大量の lifecycle comment を作らない。

PR には Orchestrator が管理する AADW Status コメントを一つだけ置く。

例:

```text
## AADW v2

Head: a1b2c3

CI          ✅ pass
Review      ✅ completed
Commander   ⚠️ GitHub Copilot unavailable
Fallback    ▶ ChatGPT available
GUI         — waiting
Gate        — waiting

Commander Request:
aadw-v2-123-a1b2c3-review-01
```

ChatGPT fallback 後は、たとえば次のように更新する。

```text
Commander   ✅ ChatGPT fallback: fix
Fix         ▶ Claude running
```

このコメントは人間向け表示であり、merge 判断の正本にはしない。

---

## 18. Watchdog

イベント取りこぼしによる永久停止を防ぐため、小さな Watchdog を一つだけ置く。

Watchdog は一定間隔で AADW v2 管理対象の open PR を探し、Orchestrator を再起動するだけとする。

Watchdog は次の判断をしない。

- CI が必要か。
- Review が必要か。
- Fix が必要か。
- GUI が必要か。
- merge 可能か。

これらはすべて Orchestrator が current state から決める。

recovery logic を Watchdog と Orchestrator に二重実装しない。

---

## 19. Concurrency と重複防止

同じ PR を複数 Orchestrator が同時に処理しない。

PR 単位の concurrency group を使う。

概念例:

```text
aadw-v2-pr-123
```

worker も `(PR, head SHA, step)` 単位で重複を防ぐ。

概念例:

```text
aadw-v2-worker-123-a1b2c3-review
```

worker は開始時に current head と既存 terminal receipt を確認し、同じ current evidence に対して不要な AI / GUI 呼び出しを重複実行しない。

---

## 20. Deterministic Gate

Commander が `continue` を返しただけでは merge しない。

Gate が current exact head に対して機械的に条件を確認する。

最低限、次を確認する。

- current head SHA が Gate 評価開始時と変わっていない。
- 必須 CI が成功。
- required review が current exact head に対して完了。
- current Commander semantic blocker がない。
- GUI required の場合、current GUI receipt が `pass`。
- current non-outdated unresolved merge-blocking review thread がない。
- active changes-requested review がない。
- GitHub が PR を mergeable と報告している。
- auto merge が明示的に opt-in されている。
- AADW 制御 workflow 自身の変更など、owner review が必要な変更ではない。

一つでも不明なら merge しない。

実際の merge request には expected head SHA を指定し、head が変わっていた場合は GitHub 側でも拒否させる。

---

## 21. AI provider unavailable の扱い

AI の quota / credit / service availability を複雑な内部状態機械へ変換しない。

Claude / Codex / Copilot が provider 側理由で利用できない場合、その step は明確な `error` / `blocked` として停止する。

初期 v2 では次を実装しない。

- quota reset 時刻の解析。
- quota lease。
- provider availability 専用 retry dispatcher。
- 自動 fallback reviewer の多段連鎖。

Copilot Commander については、利用者が ChatGPT fallback を明示的に呼び出せることを正式な回復経路とする。

---

## 22. Trust Boundary

Orchestrator / Gate / schema validator は default branch 上の trusted code を使う。

Pull Request body、Issue body、review text、source code、Markdown、GUI fixture などは untrusted data として扱う。

それらに記載された命令が、workflow の権限や安全規則を上書きしてはいけない。

Claude Fix だけが、trusted same-repository PR branch へ変更を push できる。

public fork PR へ自動 fix を行わない。

AI credential と通常の GitHub orchestration credential を分ける。

---

## 23. Workflow 構成

初期 v2 は workflow 数を少なく保つ。

目標例:

```text
.github/workflows/
  ci.yml
  aadw-v2-implement.yml
  aadw-v2-orchestrator.yml
  aadw-v2-review.yml
  aadw-v2-fix.yml
  aadw-v2-gui.yml
  aadw-v2-watchdog.yml
```

制御ロジックを巨大な YAML に書かない。

通常のプログラムへ寄せる。

```text
.github/scripts/aadw_v2/
  orchestrator.py
  commander.py
  policy.py
  receipt.py
  github.py
```

workflow YAML は主に trigger、permissions、runner、script invocation を担当する。

---

## 24. 正常経路

```text
Issue
 ↓
Work / ChatGPT 設計
 ↓
Claude implementation
 ↓
Pull Request
 ↓
CI
 ↓
Codex review
 ↓
必要なら Commander
 ├─ GitHub Copilot
 │
 │ unavailable
 │     ↓
 │ ChatGPT fallback
 │
 ├─ fix
 │    ↓
 │ Claude fix
 │    ↓
 │ new SHA
 │    ↓
 │ CI からやり直す
 │
 ├─ blocked
 │    ↓
 │ human
 │
 └─ continue
      ↓
必要なら focused GUI
      ↓
必要なら Commander
      ↓
Deterministic Gate
      ↓
merge
```

---

## 25. Copilot credit 枯渇時の経路

```text
Copilot Commander Request
        ↓
Copilot credit / quota unavailable
        ↓
aadw-v2/commander = error
        ↓
PR Status:
"ChatGPT fallback available"
        ↓
利用者:
「PR #123 の AADW Commander を代行して」
        ↓
ChatGPT:
current exact state を GitHub から取得
        ↓
Commander Policy を読む
        ↓
同じ Commander Decision schema を生成
        ↓
GitHub へ Decision を返す
        ↓
Orchestrator が current head / fingerprint / schema / actor を再検証
        ↓
通常 workflow に復帰
```

fallback のためだけに別の開発フローを作らない。

---

## 26. 設計上の禁止事項

AADW v2 では、原則として次を追加しない。

- Copilot 専用の状態機械。
- ChatGPT 専用の状態機械。
- 一つの edge case 専用 reconcile workflow。
- 過去 SHA の複数 status を組み合わせた current state 推定。
- PR comment 件数からの状態推定。
- review comment 1件ごとの Claude fix。
- AI の自由文を schema validation せず workflow command として使用。
- Commander の `continue` だけを根拠に merge。
- unrelated GUI failure を証拠なしに current PR の blocker または waive 対象とする処理。
- quota reset 時刻を解釈する複雑な retry controller。
- Commander Policy の複製。

新しい機能が必要な場合は、まず「既存 Orchestrator の一つの規則、または Commander Policy の一つの規則として表現できないか」を検討する。

---

## 27. 実装順序

v2 を一度に作らない。

PR #150 merge 後の minimal CI だけの状態から、次の順で導入する。

### Phase 1: Orchestrator の骨格

- PR と current head SHA を取得する。
- v2 管理対象かを判断する。
- AADW Status を一つ表示する。
- AI や repository mutation はまだ行わない。

### Phase 2: Codex Review

- CI success 後に exact-head review を起動する。
- Review Receipt を標準化する。
- current exact head 以外の review を current state に使わない。

### Phase 3: GitHub Copilot Commander

- Commander Request を生成する。
- Commander Decision schema を固定する。
- root-cause cluster と `fix / continue / blocked` を実装する。

### Phase 4: Claude Fix

- validated `fix` Decision から一回だけ Claude を起動する。
- existing PR branch を修正する。
- start / push effect boundary で exact-head guard を行う。

### Phase 5: ChatGPT Commander Fallback

- Copilot unavailable を明確に表示する。
- ChatGPT が current GitHub state を読み、同じ Decision schema を返せるようにする。
- actor / request / SHA / fingerprint / policy version の受理条件を live test する。

### Phase 6: Focused GUI Validation

- Issue acceptance に対応する scenario を定義する。
- scenario isolation を実装する。
- `pass / fail / blocked` receipt を標準化する。

### Phase 7: Deterministic Gate

- current exact-head evidence だけを使って merge 条件を検証する。
- expected head SHA を指定して merge する。

### Phase 8: Watchdog

- Orchestrator の起動取りこぼしだけを回収する。
- recovery business logic は持たない。

---

## 28. 完成条件

AADW v2 は「考え得る recovery をすべて自動化した」ことを完成条件にしない。

次を満たせば完成とする。

- 通常時は GitHub Copilot が Commander として一貫して動く。
- Copilot が使えない場合、ChatGPT が同じ Commander Policy / Request / Decision schema で代行できる。
- ChatGPT fallback 後も同じ Orchestrator 経路へ戻れる。
- current exact head 以外の証拠や Decision では進行・merge できない。
- review finding を root-cause cluster 単位で一回の Fix に渡せる。
- fix 後は新しい head SHA ですべて再検証される。
- AI provider が停止した場合、理由を明確にして安全に停止する。
- GUI scenario 同士が mutable state を共有しない。
- GUI infrastructure failure と製品 failure を区別する。
- merge は Deterministic Gate だけが許可する。
- Orchestrator 以外に状態遷移ロジックを増殖させない。
- Watchdog は Orchestrator 再起動だけを行う。
- Pull Request を見れば、現在の工程、Commander、停止理由、次に必要な操作が分かる。

---

## 29. 最終原則

AADW v2 の司令塔は GitHub Copilot とする。

ただし AADW v2 を GitHub Copilot そのものには依存させない。

依存関係は次の形にする。

```text
          Commander Policy
                │
         ┌──────┴──────┐
         ▼             ▼
 GitHub Copilot     ChatGPT
         │             │
         └──────┬──────┘
                ▼
        Commander Decision
                │
                ▼
         AADW Orchestrator
```

Copilot が止まっても、司令方針と current evidence は失われない。

ChatGPT は非常用の別ワークフローではなく、**同じ司令席に一時的に座る代替 Commander** として扱う。

新機能を追加するときは常に、次を確認する。

> これによって状態を増やしていないか。  
> Commander Policy または Orchestrator 一か所で表現できないか。

少し人の操作が必要になっても、状態遷移を単純に保つ方を選ぶ。

AADW v2 は、AI を止まらなくするシステムではない。

**AI が成功すれば確実に次へ進み、失敗すれば分かりやすく安全に止まり、必要なら別 Commander が同じ方針で引き継げるシステム**とする。
