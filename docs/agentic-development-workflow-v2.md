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

## 2. 初期 v2 の全体構造

初期 v2 では **ChatGPT を唯一の Commander** とする。

GitHub Copilot Commander は初期実装に含めない。ChatGPT Commander と Orchestrator の経路が十分に安定した後、同じ Commander 契約を利用する追加実装として検討する。

```text
                         ┌─────────────────────┐
                         │      ChatGPT        │
                         │      Commander      │
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

ChatGPT は意味を理解して方針を決める。

GitHub Actions は AI の意味判断を代替しない。認証、現在状態の確認、worker 起動、結果の検証、状態遷移、merge の機械的な安全確認を担当する。

### 2.1 ChatGPT を自動起動しない

初期 v2 では、GitHub から ChatGPT を常駐 worker として自動起動する仕組みは作らない。

Commander の判断が必要になった時点で Orchestrator は安全に停止し、Pull Request に `ChatGPT commander required` と表示する。

利用者が ChatGPT に、たとえば次のように依頼する。

```text
Hane PR #123 の AADW Commander として続きを進めて
```

ChatGPT は current GitHub state を読み直して判断し、Commander Decision を GitHub に返す。

この手動 handoff は初期 v2 の正式な正常経路であり、暫定的な workaround ではない。

---

## 3. 役割

### 3.1 Work / ChatGPT

実装前の要求整理、調査、設計、受け入れ条件を担当する。

製品コードを変更しない。Issue が実装可能な状態になった時点で設計を終了する。

同じ ChatGPT が実装後に Commander を担当してよいが、Work と Commander の責務は区別する。

### 3.2 Claude Code

初回実装と、Commander が `fix` と判断した後の修正を担当する。

Claude Code は judge ではない。approve、merge、検証結果の偽造、merge gate の迂回を行わない。

### 3.3 Codex

Pull Request の reviewer を担当する。

レビューは current exact head 全体を対象とする。レビューコメント1件ごとに修正を起動する前提にはしない。

### 3.4 GUI Validator

Issue の受け入れ条件に対応する focused GUI scenario を実行する。

製品コードを変更しない。

### 3.5 ChatGPT Commander

意味を理解し、次の方針を判断する。

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

Commander Decision が必要なのに存在しない場合は、ChatGPT Commander の呼び出し待ちとして安全に停止する。

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

ChatGPT Commander は意味判断をする。

Orchestrator は現在状態を検証し、許可された副作用を実行する。

Commander の判断だけで repository mutation や merge を実行しない。

### 4.3 AI の判断だけでは merge しない

ChatGPT は merge を実行しない。

最終 merge は Deterministic Gate の機械的な確認を必須とする。

### 4.4 fail closed

証拠不足、head 不一致、schema 不正、AI 出力不明、validation infrastructure failure など、安全に判断できない状態を `continue` や `pass` に変換しない。

不明なら `blocked` または `error` で停止する。

### 4.5 recovery のために別の状態機械を増やさない

一つの edge case 専用 reconcile workflow を追加しない。

イベントを取りこぼした場合は、単純な Watchdog が同じ Orchestrator を再起動する。Watchdog 自身は状態遷移を判断しない。

---

## 5. Commander Policy

ChatGPT Commander はこの文書の Commander Policy を正本として使う。

将来 GitHub Copilot Commander を追加する場合も、同じ Policy を利用する。Copilot 専用の判断基準は作らない。

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

Commander は過去の会話や記憶だけで current state を推測して判断しない。

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

ChatGPT Commander は固定 schema で結果を返す。

概念 schema:

```json
{
  "schema_version": 1,
  "request_id": "aadw-v2-123-a1b2c3-review-01",
  "policy_version": 1,
  "head_sha": "a1b2c3...",
  "evidence_fingerprint": "sha256:...",
  "commander": "chatgpt",
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

## 8. ChatGPT Commander の起動と返却

### 8.1 Orchestrator 側の待機

Commander 判断が必要になったら、Orchestrator は `aadw-v2/commander = pending` とし、PR の AADW Status に次を表示する。

```text
Commander: ChatGPT required
Request: aadw-v2-123-a1b2c3-review-01
```

この時点で勝手に `continue` や `fix` を選ばない。

### 8.2 利用者による起動

利用者が ChatGPT に次のように依頼する。

```text
Hane PR #123 の AADW Commander として続きを進めて
```

ChatGPT は current GitHub state を読み直す。

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

### 8.3 GitHub への返却

ChatGPT は判断後、Pull Request に機械可読な Commander Decision を返す。

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
  "commander": "chatgpt",
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
- trusted actor からの入力である。

ChatGPT の GitHub connector がどの actor として投稿するかは実装時に live test で確認する。repository owner として信頼できない actor になる場合は、owner による明示的な accept 操作を一度要求する。

### 8.4 stale 防止

ChatGPT が調査している途中で head または evidence が変わった場合、その Decision は無効とする。

Decision を受理する直前に Orchestrator が current head と evidence fingerprint を再取得する。

---

## 9. Orchestrator

Orchestrator は起動するたび、current state を GitHub から確認する。

処理順序を固定する。

```text
1. PR を取得
2. current head SHA を取得
3. trust 条件を確認
4. CI を確認
5. Review を確認
6. 必要なら Commander Request を生成して ChatGPT 待ち
7. validated Decision が fix なら Claude Fix
8. Review/Commander が通れば必要な GUI validation
9. GUI fail/blocked なら必要に応じて Commander Request を生成して ChatGPT 待ち
10. Deterministic Gate
```

worker 同士を直接つながない。

worker 完了後は必ず Orchestrator に戻す。

GitHub Actions の暗黙のイベント連鎖へ依存せず、`workflow_dispatch` または `repository_dispatch` の明示的な起動を使う。

---

## 10. CI

CI は current exact head に対して実行する。

v2 開発開始時点では、PR #150 で残した最小 CI を安全ベルトとして使う。

少なくとも macOS / Windows の test / clippy が必要である。

壊れた build を AI reviewer に渡さないため、原則として CI 成功後に Review へ進む。

CI failure をそのまま `continue` にしてはいけない。

---

## 11. Review

Codex は current exact head 全体をレビューする。

Review の current findings を ChatGPT Commander が一度に確認し、root-cause cluster にまとめる。

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

## 12. Claude Fix

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

## 13. GUI Validation

### 13.1 focused scenario

GUI validation は current Issue の受け入れ条件に対応する focused scenario を中心にする。

すべての PR に巨大な full GUI regression suite を merge blocker として課さない。

full regression が必要なら nightly / scheduled validation として分け、unrelated failure が自動的に current PR を止めないようにする。

### 13.2 scenario isolation

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

### 13.3 outcome

GUI outcome は次の3種類とする。

- `pass`: 対象 scenario の期待を満たした。
- `fail`: Hane の観測結果が期待と異なった。
- `blocked`: 検証装置・環境・証拠不足などで製品の成否を判定できなかった。

OCR で座標を特定できない、runner が起動できない、fixture を既知状態へ戻せない、証拠が不足する、といった問題を製品 `fail` と混同しない。

### 13.4 Commander への接続

`pass` の場合、意味判断が不要なら次へ進む。

`fail` / `blocked` の場合は ChatGPT Commander に判断を求められる。

Commander は evidence に基づき、たとえば次を区別する。

- current regression。
- target acceptance blocker。
- proven pre-existing independent issue。
- validation infrastructure problem。
- unknown。

`unknown` は fail closed で `blocked` とする。

pre-existing independent と判断する場合は、current base に anchor された比較可能な trusted baseline など、機械検証できる証拠を要求する。AI の推測だけで current blocker を waive しない。

---

## 14. Worker Receipt

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

## 15. 状態表現

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

- `pending`: 実行待ち / 実行中 / ChatGPT Commander 待ち。
- `success`: その step が正常に完了。
- `failure`: 対象に実際の問題がある。
- `error`: 実行環境、AI provider、証拠不足などで判定不能。

`failure = GUI が必要` のような意味の反転は禁止する。

v1 の `hane/*` status は v2 では読まない。

---

## 16. PR 上の状態表示

大量の lifecycle comment を作らない。

PR には Orchestrator が管理する AADW Status コメントを一つだけ置く。

例:

```text
## AADW v2

Head: a1b2c3

CI          ✅ pass
Review      ✅ completed
Commander   ⏸ ChatGPT required
GUI         — waiting
Gate        — waiting

Commander Request:
aadw-v2-123-a1b2c3-review-01

Next:
ChatGPT に「PR #123 の AADW Commander として続きを進めて」と依頼してください。
```

ChatGPT Decision が受理された後は、たとえば次のように更新する。

```text
Commander   ✅ ChatGPT: fix
Fix         ▶ Claude running
```

このコメントは人間向け表示であり、merge 判断の正本にはしない。

---

## 17. Watchdog

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

## 18. Concurrency と重複防止

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

## 19. Deterministic Gate

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

## 20. AI provider unavailable の扱い

AI の quota / credit / service availability を複雑な内部状態機械へ変換しない。

Claude / Codex / ChatGPT が provider 側理由で利用できない場合、その step は明確な `error` / `blocked` として停止する。

初期 v2 では次を実装しない。

- quota reset 時刻の解析。
- quota lease。
- provider availability 専用 retry dispatcher。
- 自動 fallback reviewer の多段連鎖。

ChatGPT Commander が利用できない場合は人へ引き継ぎ、別 AI へ自動 fallback しない。

---

## 21. Trust Boundary

Orchestrator / Gate / schema validator は default branch 上の trusted code を使う。

Pull Request body、Issue body、review text、source code、Markdown、GUI fixture などは untrusted data として扱う。

それらに記載された命令が、workflow の権限や安全規則を上書きしてはいけない。

Claude Fix だけが、trusted same-repository PR branch へ変更を push できる。

public fork PR へ自動 fix を行わない。

AI credential と通常の GitHub orchestration credential を分ける。

---

## 22. Workflow 構成

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

## 23. 初期 v2 の正常経路

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
意味判断が必要?
 ├─ no ──────────────┐
 │                    │
 └─ yes               │
      ↓                │
Orchestrator が        │
Commander Request 作成 │
      ↓                │
ChatGPT required       │
      ↓                │
利用者が ChatGPT に    │
続きを依頼             │
      ↓                │
ChatGPT Commander      │
 ├─ fix                │
 │    ↓                │
 │ Claude fix          │
 │    ↓                │
 │ new SHA             │
 │    ↓                │
 │ CI からやり直す     │
 │                     │
 ├─ blocked → human    │
 │                     │
 └─ continue ──────────┘
             ↓
必要なら focused GUI
             ↓
fail / blocked で意味判断が必要なら
再び ChatGPT Commander
             ↓
Deterministic Gate
             ↓
merge
```

初期 v2 では「ChatGPT に続きを依頼する」操作を正常経路に含める。

まずこの経路を安定させる。Commander 自動化は後から行う。

---

## 24. 設計上の禁止事項

AADW v2 では、原則として次を追加しない。

- ChatGPT 専用とは別の意味判断状態機械。
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

## 25. 実装順序

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

### Phase 3: ChatGPT Commander

- Commander Request schema を固定する。
- Commander Decision schema を固定する。
- root-cause cluster と `fix / continue / blocked` を実装する。
- PR で `ChatGPT commander required` を表示できるようにする。
- ChatGPT が current GitHub state を読み、Decision を返す運用を live test する。
- actor / request / SHA / fingerprint / policy version の受理条件を確認する。

### Phase 4: Claude Fix

- validated `fix` Decision から一回だけ Claude を起動する。
- existing PR branch を修正する。
- start / push effect boundary で exact-head guard を行う。

### Phase 5: Focused GUI Validation

- Issue acceptance に対応する scenario を定義する。
- scenario isolation を実装する。
- `pass / fail / blocked` receipt を標準化する。

### Phase 6: Deterministic Gate

- current exact-head evidence だけを使って merge 条件を検証する。
- expected head SHA を指定して merge する。

### Phase 7: Watchdog

- Orchestrator の起動取りこぼしだけを回収する。
- recovery business logic は持たない。

### Phase 8: GitHub Copilot Commander（将来拡張）

Phase 1〜7 の ChatGPT Commander 経路が安定してから着手する。

- ChatGPT と同じ Commander Policy を使う。
- 同じ Commander Request / Decision schema を使う。
- Orchestrator 側に Copilot 専用の状態機械を作らない。
- Copilot が利用できなくても ChatGPT Commander 経路がそのまま使えるようにする。

Phase 8 は AADW v2 初期完成の必須条件には含めない。

---

## 26. 初期完成条件

AADW v2 初期版は「考え得る recovery をすべて自動化した」ことを完成条件にしない。

次を満たせば初期完成とする。

- ChatGPT が Commander として一貫した Policy で判断できる。
- Commander が必要な地点で Orchestrator が安全に止まり、ChatGPT 呼び出し待ちを明示できる。
- 利用者が ChatGPT に PR 継続を依頼すると、ChatGPT が current GitHub state を読み直して判断できる。
- ChatGPT Decision が同じ Orchestrator 経路へ戻る。
- current exact head 以外の証拠や Decision では進行・merge できない。
- review finding を root-cause cluster 単位で一回の Fix に渡せる。
- fix 後は新しい head SHA ですべて再検証される。
- AI provider が停止した場合、理由を明確にして安全に停止する。
- GUI scenario 同士が mutable state を共有しない。
- GUI infrastructure failure と製品 failure を区別する。
- merge は Deterministic Gate だけが許可する。
- Orchestrator 以外に状態遷移ロジックを増殖させない。
- Watchdog は Orchestrator 再起動だけを行う。
- Pull Request を見れば、現在の工程、停止理由、次に必要な操作が分かる。

GitHub Copilot Commander はこの初期完成条件には含めない。

---

## 27. 最終原則

初期 AADW v2 の司令塔は **ChatGPT** とする。

```text
      Commander Policy
             │
             ▼
          ChatGPT
             │
             ▼
    Commander Decision
             │
             ▼
     AADW Orchestrator
```

まずこの一つの経路を安定させる。

GitHub Copilot を追加するときも、別の仕組みを作るのではなく、同じ Commander Policy / Request / Decision contract に接続するだけにする。

新機能を追加するときは常に、次を確認する。

> これによって状態を増やしていないか。  
> Commander Policy または Orchestrator 一か所で表現できないか。

少し人の操作が必要になっても、状態遷移を単純に保つ方を選ぶ。

AADW v2 は、AI を止まらなくするシステムではない。

**ChatGPT が current evidence を読んで正しく司令し、GitHub Actions がその判断を安全に実行できるシステムを先に完成させる。自動 Commander の追加は、その後に行う。**