# AI Agent Development Workflow v2 設計書

## 1. 位置づけ

この文書は、Hane の AI Agent Development Workflow（AADW）を一から作り直すための v2 設計書である。

AADW v1 は PR #150 で停止した。v2 は v1 の状態機械、status、receipt、routing、GUI generation、reconcile、retry 契約との互換性を持たない。v1 の情報は履歴として参照してよいが、v2 の current state や merge 判断には利用しない。

v2 では、自動化率を最大化することより、次を優先する。

1. ChatGPT が一貫して全体を統括できること。
2. GitHub Actions や各 AI worker が勝手に次工程を判断しないこと。
3. current exact head SHA 以外の証拠を現在状態として使わないこと。
4. 失敗した場合は、安全に止まり、利用者から ChatGPT に戻せること。
5. recovery のための別状態機械を増やさないこと。

初期 v2 は、完全自動化よりも単純さと観測可能性を優先する。

---

## 2. 基本モデル

初期 v2 では、**ChatGPT が AADW 全体の唯一の司令塔**となる。

GitHub Actions は司令塔ではない。ChatGPT から依頼された処理を実行し、その結果を証拠として残す実行層とする。

```text
                         ChatGPT
                Commander / Coordinator
                           │
              現在状態を読み、次を判断
                           │
          ┌────────────────┼────────────────┐
          ▼                ▼                ▼
       Claude            Codex         GUI Validator
    implementation       review          validation
          │                │                │
          └────────────────┼────────────────┘
                           ▼
                    GitHub Actions
                    Execution Layer
                           │
                    result / receipt
                           │
                           ▼
                         ChatGPT
                           │
                 次に何をするか再判断
                           │
                           ▼
                 Deterministic Gate
                           │
                           ▼
                         merge
```

AADW の中心ループは次である。

```text
ChatGPT
  ↓ 指示
実行部隊
  ↓ 証拠
GitHub
  ↓ current state を再取得
ChatGPT
```

GitHub Actions 自身は、

- 次は Review か。
- 次は Claude Fix か。
- GUI を実行すべきか。
- current failure を follow-up に分離してよいか。

といった意味判断を行わない。

---

## 3. 役割

### 3.1 ChatGPT

ChatGPT は AADW v2 の唯一の Commander / Coordinator である。

担当すること:

- Issue の目的と受け入れ条件を読む。
- Pull Request の current exact head を取得する。
- CI、Review、review threads、GUI、worker receipt を読む。
- current findings を root-cause cluster にまとめる。
- blocker / follow-up / unknown を判断する。
- Claude に何を直させるか決める。
- どの GUI validation が必要か決める。
- 次に実行する worker を決める。
- merge gate を評価させる段階まで全体を統括する。

ChatGPT 自身は、証拠なしに pass / merge を宣言しない。

### 3.2 GitHub Actions

GitHub Actions は **Execution Layer** とする。

担当すること:

- CI を実行する。
- Claude を起動する。
- Codex review を起動する。
- GUI validation を実行する。
- worker receipt や artifact を保存する。
- Deterministic Gate の機械的検査を実行する。

担当しないこと:

- 次工程を意味的に決める。
- review finding の重要度を解釈する。
- root cause を推測する。
- GUI failure の責任箇所を AI 的に判断する。
- 独自に Claude fix を連鎖起動する。

### 3.3 Claude Code

初回実装と修正を担当する。

Claude は implementer であり judge ではない。

ChatGPT が指定した scope を中心に修正し、同じ root cause に属する兄弟ケースも確認する。

### 3.4 Codex

Pull Request の reviewer を担当する。

current exact head 全体を対象にレビューする。

Codex 自身が修正 worker を起動しない。

### 3.5 GUI Validator

Issue の受け入れ条件に対応する focused GUI scenario を実行する。

製品コードを変更しない。

### 3.6 Deterministic Gate

AI を使わない安全装置である。

ChatGPT が「進めてよい」と判断しても、merge 直前には current exact head の機械的条件を再確認する。

---

## 4. 初期 v2 は半自動を正式経路とする

初期 v2 では、worker 完了後に次の AI worker を自動連鎖させない。

正式な経路は次である。

```text
処理完了
  ↓
GitHub に結果が残る
  ↓
利用者が ChatGPT に依頼
  ↓
ChatGPT が GitHub の current state を読み直す
  ↓
ChatGPT が次を判断
  ↓
次の worker を起動
```

利用者から ChatGPT への依頼は、たとえば次のような短いものでよい。

```text
PR #123 を続けて
```

ChatGPT は過去の会話だけで続行してはいけない。

毎回 GitHub から current state を再取得する。

最低限確認するもの:

- Pull Request metadata。
- current exact head SHA。
- linked Issue と受け入れ条件。
- current CI。
- current review。
- current non-outdated unresolved review threads。
- current GUI evidence。
- relevant worker receipts / artifacts。
- current mergeability。

この半自動方式を安定させた後でのみ、自動 Commander を検討する。

---

## 5. 最重要原則

### 5.1 exact-head only

一つの head SHA を一つの世代として扱う。

current head が `A` なら、現在判断に使える証拠は `A` に binding されたものだけである。

```text
CI(A)
Review(A)
GUI(A)
Receipt(A)
```

Claude が修正して head が `B` になった瞬間、`A` の証拠は履歴になる。

```text
CI(B)
Review(B)
GUI(B)
Receipt(B)
```

を新しく取得する。

旧 SHA の結果を新 SHA に継承しない。

### 5.2 current state は毎回再取得する

ChatGPT は前回の判断をそのまま再利用しない。

GitHub を読み直し、current exact head と current evidence を基準に再判断する。

### 5.3 Actions に状態機械を持たせない

GitHub Actions に、

```text
CI 成功したから Codex
Codex が指摘したから Claude
Claude が push したから GUI
```

という意味的な連鎖を実装しない。

各 workflow は一つの仕事をして終了する。

### 5.4 fail closed

証拠不足、head 不一致、AI 出力不明、validation infrastructure failure など、安全に判断できない状態を `continue` にしない。

不明なら止める。

### 5.5 merge は AI 判断だけで行わない

ChatGPT は merge 方針を決めるが、実際の merge 前には Deterministic Gate を通す。

---

## 6. ChatGPT の判断方針

### 6.1 Issue の目的を最優先する

review finding の件数をゼロにすること自体を目的にしない。

current Issue の目的と受け入れ条件を満たすことを優先する。

原則として blocker にするもの:

- P0 / P1。
- security 問題。
- data loss / corruption。
- authority / permission violation。
- 通常経路で再現する明確な bug。
- CI failure。
- current Issue の受け入れ条件を直接満たせなくする問題。

それ以外は follow-up に分離できる。

### 6.2 root-cause cluster

review comment 1件を fix 1回に対応させない。

ChatGPT は current exact head の findings をまとめて読み、同じ原因・同じ設計面に属するものを cluster 化する。

例:

```text
8 findings
   ↓
3 root causes
   ↓
2 blocker clusters
1 follow-up cluster
   ↓
1 Claude fix run
```

同一 cluster では、fix 前に兄弟ケースを横断確認する。

例:

- producer / consumer。
- pass / fail / blocked。
- normal / recovery。
- insert / delete / replace。
- mouse / keyboard / IME。

独立した問題を一つの fix に混ぜない。

### 6.3 classification

cluster は次の3種類に分類する。

- `blocker`: current PR で修正する必要がある。
- `follow_up`: current PR を止めず、別 Issue 候補にする。
- `unknown`: 証拠不足または原因不明。current PR を止める。

### 6.4 repeated blocker

同じ root-cause cluster が複数回の fix 後も blocker として残る場合、小さな patch を積み続けない。

原則として同一 cluster の fix を2 cycle 行っても解消しない場合、設計見直しまたは scope 分割を行う。

これは blocker を無視するための上限ではない。

---

## 7. 実行部隊への指示

ChatGPT が worker を起動するときは、最低限次を固定する。

```text
target PR
target head SHA
purpose
scope
acceptance criteria
expected evidence
```

worker は対象 SHA が current head と一致することを開始時に確認する。

repository mutation を伴う worker は、effect boundary でも再確認する。

---

## 8. CI

CI は current exact head に対して実行する。

初期 v2 では PR #150 で残した最小 CI を使う。

最低限:

- macOS test / clippy。
- Windows test / clippy。

壊れた build を reviewer や GUI validation へ送らない。

原則として CI success 後に review へ進む。

---

## 9. Review

Codex は current exact head 全体をレビューする。

review 完了後、ChatGPT が current review と current non-outdated unresolved threads を読み、root-cause cluster を作る。

Codex review 完了をきっかけに Actions が自動で Claude を起動してはいけない。

### 9.1 Review Receipt

可能なら review 結果を worker receipt として残す。

```json
{
  "schema_version": 1,
  "step": "review",
  "pr_number": 123,
  "head_sha": "a1b2c3...",
  "run_id": 123456789,
  "outcome": "completed",
  "summary": "...",
  "data": {},
  "usage": {}
}
```

receipt は review 内容そのものの代替ではない。

ChatGPT は必要に応じて GitHub review threads を直接読む。

---

## 10. Claude Fix

ChatGPT が blocker clusters を決めた後、一回の Claude fix にまとめて渡す。

Claude への指示には次を含める。

- target PR。
- target exact head SHA。
- root-cause clusters。
- current Issue の目的。
- fix scope。
- 触らない独立問題。
- 必要な test。

Claude は開始時に current head を確認する。

push 直前にも current head を再取得する。

```text
start:
current head == target SHA

before push:
current head == target SHA
```

不一致なら push しない。

push 後は新 head となるため、古い CI / Review / GUI は current evidence として使わない。

---

## 11. GUI Validation

### 11.1 focused validation

current Issue の受け入れ条件に対応する scenario を実行する。

すべての PR で full GUI regression を merge blocker にしない。

full regression が必要なら nightly / scheduled validation として分ける。

### 11.2 scenario isolation

mutating GUI scenario は状態を共有しない。

```text
scenario A
  Hane 起動
  fixture A
  操作
  検証
  終了

scenario B
  Hane 起動
  fixture B
  操作
  検証
  終了
```

前 scenario の編集結果や failure が次の scenario を汚染してはいけない。

### 11.3 outcome

GUI outcome は次の3種類とする。

- `pass`: product behavior が期待を満たした。
- `fail`: product behavior が期待と異なった。
- `blocked`: 検証装置、環境、証拠不足などで product の成否を判定できなかった。

OCR failure、runner failure、fixture restoration failure などを product failure と混同しない。

### 11.4 GUI failure の判断

GUI が fail / blocked の場合、ChatGPT が evidence を読む。

区別する候補:

- current regression。
- target acceptance blocker。
- proven pre-existing independent issue。
- validation infrastructure problem。
- unknown。

`unknown` は fail closed とする。

pre-existing independent と判断する場合、比較可能な trusted baseline など、AI 推測以外の証拠を要求する。

---

## 12. Worker Receipt

各 worker は可能な範囲で共通 receipt を残す。

```json
{
  "schema_version": 1,
  "step": "gui",
  "pr_number": 123,
  "head_sha": "a1b2c3...",
  "run_id": 123456789,
  "outcome": "pass",
  "reason_code": "focused_gui_passed",
  "summary": "...",
  "data": {},
  "usage": {}
}
```

AI worker について取得可能なら `usage` に token 数や推定費用を記録してよい。

費用は merge 判断には使わない。

receipt は必ず exact head に binding する。

---

## 13. 状態表示

v2 の current status は少数にする。

例:

```text
aadw-v2/ci
aadw-v2/review
aadw-v2/fix
aadw-v2/gui
aadw-v2/gate
```

status の意味を通常の意味に揃える。

- `pending`: 実行待ち / 実行中。
- `success`: step が正常に完了。
- `failure`: 対象に実際の問題がある。
- `error`: infrastructure / provider / evidence 問題で判定不能。

`failure = GUI required` のような意味の反転を禁止する。

v1 の `hane/*` status は読まない。

---

## 14. PR 上の人向け表示

初期 v2 では、複雑な自動 status comment updater を必須にしない。

必要なら人向けに一つの状態コメントを持つ。

例:

```text
## AADW v2

Head: a1b2c3

CI       ✅ pass
Review   ✅ completed
ChatGPT  ⏸ action required
GUI      — not started
Gate     — not evaluated

Next:
ChatGPT に「PR #123 を続けて」と依頼してください。
```

このコメントは人向け表示であり、merge 判断の正本にはしない。

---

## 15. Deterministic Gate

merge 直前だけは、機械的な safety gate を置く。

最低限確認する。

- current head SHA が評価開始時から変わっていない。
- required CI が success。
- required review が current exact head に対して完了。
- current merge-blocking review thread が残っていない。
- active changes-requested review がない。
- GUI required の場合、current exact-head GUI evidence が pass。
- PR が mergeable。
- auto merge / merge 実行が明示的に許可されている。
- AADW の制御コード自身を変更する PR など、owner review を必須にすべき変更ではない。

実際の merge request には expected head SHA を指定する。

Gate は root cause や follow-up の意味判断をしない。

意味判断は ChatGPT が先に終えている前提とする。

---

## 16. 自動 recovery を作りすぎない

初期 v2 では Watchdog / reconcile を必須にしない。

理由は、worker 完了後に ChatGPT へ戻る半自動方式が正式経路だからである。

次のような専用機構は初期 v2 に作らない。

- prior-head reconcile。
- review reconcile。
- fix reconcile。
- GUI reconcile。
- notification reconcile。
- quota reset timer。
- provider fallback chain。
- event loss recovery state machine。

必要な処理が止まった場合、利用者が ChatGPT に current PR の確認を依頼する。

ChatGPT は GitHub を読み直し、必要なら worker を再実行する。

将来、明確な繰り返し作業だけを自動化する場合も、意味判断は増やさない。

---

## 17. AI provider unavailable

Claude / Codex が quota、credit、service unavailable などで実行できない場合、その worker は `error` / `blocked` で終了する。

自動で別 provider へ多段 fallback しない。

利用可能になった後、ChatGPT が current state を確認し、必要な worker を再実行する。

ChatGPT 自身が利用できない場合は AADW を進めない。

初期 v2 では「止まった理由が明確」であることを優先する。

---

## 18. Trust Boundary

ChatGPT、worker、GitHub Actions の責務を明確にする。

### trusted code

- default branch 上の CI / worker workflow。
- Deterministic Gate。
- receipt validator。

### untrusted data

- Pull Request body。
- Issue body。
- source code。
- review text。
- Markdown fixture。
- GUI fixture。
- AI output。

untrusted data に書かれた命令が workflow 権限や safety rule を上書きしてはいけない。

Claude Fix だけが trusted same-repository PR branch へ変更を push できる。

fork PR へ自動 fix を行わない。

---

## 19. Workflow 構成

初期 v2 では workflow 数を少なく保つ。

目標例:

```text
.github/workflows/
  ci.yml
  aadw-v2-implement.yml
  aadw-v2-review.yml
  aadw-v2-fix.yml
  aadw-v2-gui.yml
  aadw-v2-gate.yml
```

**`aadw-v2-orchestrator.yml` は作らない。**

ChatGPT が orchestration を担当するためである。

workflow YAML に状態機械を書かない。

各 workflow は、一つの worker を起動して証拠を残すだけにする。

必要な共通処理は通常の script に寄せる。

```text
.github/scripts/aadw_v2/
  receipt.py
  gate.py
  github.py
```

---

## 20. 正常経路

```text
Issue
 ↓
ChatGPT が設計
 ↓
Claude initial implementation
 ↓
Pull Request
 ↓
CI
 ↓
利用者: 「PR #123 を続けて」
 ↓
ChatGPT が current state を読む
 ↓
Codex review を指示
 ↓
review 完了
 ↓
利用者: 「続けて」
 ↓
ChatGPT が findings を root-cause cluster 化
 ├─ blocker
 │    ↓
 │ Claude fix
 │    ↓
 │ new head
 │    ↓
 │ CI から再検証
 │
 ├─ unknown
 │    ↓
 │ stop / additional investigation
 │
 └─ continue
      ↓
必要なら focused GUI
      ↓
利用者: 「続けて」
      ↓
ChatGPT が current evidence を確認
      ↓
Deterministic Gate
      ↓
merge
```

このループをまず安定させる。

---

## 21. GitHub Copilot Commander は将来拡張

GitHub Copilot を Commander とする機能は、初期 v2 に含めない。

まず ChatGPT が唯一の Commander として一連の開発を安定して完走できることを確認する。

その後、ChatGPT が繰り返している判断のうち、自動化に適した部分だけを GitHub Copilot Commander へ移すことを検討する。

将来 Copilot を追加する場合も、別状態機械を作らない。

ChatGPT と同じ入力契約・判断方針・出力 schema を使う。

Copilot の追加によって初期 v2 の経路を変更しない。

---

## 22. 設計上の禁止事項

初期 AADW v2 では、原則として次を作らない。

- 独立した Orchestrator state machine。
- Copilot 専用 state machine。
- ChatGPT 専用の GitHub 側 state machine。
- worker から worker への自動意味判断付き連鎖。
- 一つの edge case 専用 reconcile workflow。
- 過去 SHA の status を組み合わせた current state 推定。
- PR comment 数からの状態推定。
- review comment 1件ごとの Claude fix。
- AI 自由文を validation せず workflow command として使用。
- AI 判断だけによる merge。
- unrelated GUI failure の根拠なし waiver。
- quota reset 時刻を解析する複雑な retry controller。

新機能を追加する前に、

> ChatGPT が current GitHub state を読み直して判断すれば済まないか。

を確認する。

---

## 23. 実装順序

初期 v2 を一度に作らない。

### Phase 1: Execution Layer の最小骨格

- current PR / target SHA を指定して worker を実行できる。
- worker が exact-head guard を行う。
- receipt を残せる。
- AI による次工程判断はまだない。

### Phase 2: Codex Review

- ChatGPT から current PR の review を起動できる。
- current exact head に review を binding する。
- review evidence を ChatGPT が読み直せる。

### Phase 3: ChatGPT Commander Loop

- 「PR #xxx を続けて」で current state を取得する。
- Issue / CI / review / threads を読む。
- root-cause cluster を作る。
- `fix / continue / blocked` 相当の方針を一貫して出せる。

### Phase 4: Claude Fix

- ChatGPT が決めた blocker cluster を Claude に渡せる。
- start / push 前の exact-head guard を実装する。
- new head 後に旧 evidence を使わない。

### Phase 5: Focused GUI Validation

- Issue acceptance に対応する focused scenario を実行できる。
- scenario isolation を実装する。
- `pass / fail / blocked` evidence を残す。

### Phase 6: Deterministic Gate

- current exact-head evidence だけを使って merge 条件を検証する。
- expected head SHA を指定して merge する。

### Phase 7: 運用安定化

- 複数 PR で ChatGPT 主導ループを実際に運用する。
- 不要な状態、重複処理、分かりにくい停止箇所を削る。
- 自動化すべき反復作業と、人が ChatGPT を呼ぶべき判断点を見極める。

### Phase 8: GitHub Copilot Commander（将来拡張）

- ChatGPT 経路が安定してから検討する。
- 同じ policy / evidence / decision contract を使う。
- 新しい state machine は作らない。

---

## 24. 初期 v2 の完成条件

初期 v2 の完成条件に GitHub Copilot Commander は含めない。

次を満たせば完成とする。

- ChatGPT が唯一の司令塔として Issue から merge まで統括できる。
- GitHub Actions は実行部隊としてのみ動作する。
- Actions 自身が意味的な次工程を判断しない。
- 利用者が「PR #xxx を続けて」と依頼すれば、ChatGPT が current GitHub state を再取得して続行できる。
- current exact head 以外の証拠では進行・merge できない。
- review findings を root-cause cluster 単位で Claude に渡せる。
- fix 後は new head で再検証される。
- GUI product failure と validation infrastructure failure を区別できる。
- merge は Deterministic Gate を通る。
- provider failure 時は安全に止まり、停止理由が分かる。
- v1 の reconcile / routing / prior-head recovery を必要としない。

---

## 25. 最終原則

AADW v2 の初期形は次である。

```text
                    ChatGPT
             唯一の Commander
                    │
          ┌─────────┼─────────┐
          ▼         ▼         ▼
       Claude     Codex      GUI
          │         │         │
          └─────────┼─────────┘
                    ▼
             GitHub Actions
              Execution Layer
                    │
                 Evidence
                    │
                    ▼
                  ChatGPT
                    │
                    ▼
          Deterministic Gate
                    │
                    ▼
                  merge
```

ChatGPT が全体を統括する。

GitHub Actions は命令された処理を実行する。

worker は自分の仕事だけを行う。

意味判断を GitHub 側へ複製しない。

完全自動化を急がず、まずこの単純なループを安定させる。

新しい自動化を追加するときは常に次を確認する。

> それは ChatGPT が current state を読んで判断するだけでは不足なのか。  
> GitHub 側へ新しい状態機械を追加する価値が本当にあるのか。

少し人の操作が必要でも、全体を理解できる単純さを優先する。

**初期 AADW v2 は、ChatGPT が司令し、実行部隊が動き、結果を ChatGPT が再び読む、という一つのループで構成する。**