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
6. ChatGPT が現在状態を把握するために GitHub 上を毎回探索し回らなくてよいこと。

初期 v2 は、完全自動化よりも単純さ、観測可能性、低コストな current-state discovery を優先する。

---

## 2. 基本モデル

初期 v2 では、**ChatGPT が AADW 全体の唯一の司令塔**となる。

GitHub Actions は司令塔ではない。ChatGPT から依頼された処理を実行し、その結果を GitHub 上の正本として残す実行層とする。

```text
                         ChatGPT
                Commander / Coordinator
                           │
                 aadw status <PR>
                           │
          current state の短い要約を取得
                           │
              必要な証拠だけ追加取得
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
                 GitHub 上の正本へ記録
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
  ↓
aadw status <PR>
  ↓
短い current-state summary
  ↓
必要な evidence だけ追加取得
  ↓
次を判断して worker を指示
  ↓
worker が GitHub 上の正本を更新
  ↓
利用者が ChatGPT に戻す
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

- `aadw status <PR>` で current state を低コストに把握する。
- 必要な場合だけ Issue、diff、review threads、GUI evidence、logs などの詳細証拠を追加取得する。
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
- run、review、thread、artifact、status など、各 worker の結果を GitHub 上の正本として残す。
- Deterministic Gate の機械的検査を実行する。

担当しないこと:

- 次工程を意味的に決める。
- review finding の重要度を解釈する。
- root cause を推測する。
- GUI failure の責任箇所を AI 的に判断する。
- 独自に Claude fix を連鎖起動する。
- ChatGPT 用の persistent snapshot を別途維持する。

### 3.3 AADW Status Collector

ChatGPT が GitHub 上を毎回横断探索しなくてよいように、read-only の status collector を用意する。

概念コマンド:

```bash
./scripts/aadw status 123
```

または、実装上都合がよければ:

```bash
python .github/scripts/aadw_v2/status.py 123
```

Status Collector は GitHub 上に散在する正本を実行時に読み、current exact head に関係する事実だけを集め、短い current-state summary を生成する。

**Status Collector 自身は persistent state を持たない。**

PR comment、repository file、database などへ snapshot のコピーを保存しない。

### 3.4 Claude Code

初回実装と修正を担当する。

Claude は implementer であり judge ではない。

ChatGPT が指定した scope を中心に修正し、同じ root cause に属する兄弟ケースも確認する。

### 3.5 Codex

Pull Request の reviewer を担当する。

current exact head 全体を対象にレビューする。

Codex 自身が修正 worker を起動しない。

### 3.6 GUI Validator

Issue の受け入れ条件に対応する focused GUI scenario を実行する。

製品コードを変更しない。

### 3.7 Deterministic Gate

AI を使わない安全装置である。

ChatGPT が「進めてよい」と判断しても、merge 直前には current exact head の機械的条件を再確認する。

---

## 4. GitHub 上の状態管理

### 4.1 Persistent state の正本

AADW v2 では、状態を一つの snapshot へ複製しない。

GitHub がすでに持っている次の情報を正本とする。

- Pull Request metadata / current head SHA。
- linked Issue と acceptance criteria。
- GitHub Actions workflow run / job / conclusion。
- CI check / status。
- Pull Request review / review thread。
- GUI validation run / artifact / receipt。
- mergeability。

同じ事実を PR comment や別ファイルへ current state としてコピーし続けない。

### 4.2 Derived state はオンデマンドで生成する

ChatGPT が current state を知る必要があるときだけ、Status Collector が各正本を読み、derived summary を生成する。

```text
GitHub 上の正本
  ├─ PR
  ├─ Issue
  ├─ CI
  ├─ Review
  ├─ Threads
  ├─ Actions
  ├─ GUI evidence
  └─ Artifacts
        │
        ▼
   aadw status 123
        │
        ▼
短い current-state summary
        │
        ▼
      ChatGPT
```

summary はその場で生成して捨てる。

これにより、persistent snapshot と正本の不整合を作らない。

### 4.3 Status Collector の出力

ChatGPT が読む出力は、短く正確な自然言語を基本とする。

例:

```text
AADW status — PR #123
HEAD a1b2c3 current
Issue #101

CI        PASS
Review    DONE — 3 current unresolved threads
Fix       none on current head
GUI       not run
Gate      not evaluated

State:
Review results need ChatGPT judgement.

Refs:
review_run=123456
```

出力の目的は、ChatGPT が「いま何が起きているか」「次にどの詳細 evidence を読めばよいか」を数秒で把握することである。

大量の JSON、workflow log、review 本文を summary にコピーしない。

### 4.4 JSON の扱い

JSON を禁止するわけではない。

Status Collector の内部では、GitHub API の結果を正規化した構造体や JSON として扱ってよい。

```text
GitHub API
  ↓
normalized internal data
  ↓
policy-free formatter
  ↓
短い自然言語 summary
```

JSON はテスト、schema validation、内部実装に有用である。

ただし Commander である ChatGPT に冗長な内部 JSON をそのまま読ませることを標準経路にしない。

### 4.5 Collector は意味判断をしない

Status Collector が行うのは事実の収集、exact-head filtering、正規化、簡潔な表現だけである。

例えば次は出力してよい。

```text
Review: DONE — 3 current unresolved threads
State: Review results need ChatGPT judgement.
```

しかし次は Collector が判断してはいけない。

```text
Next: Claude should fix all 3 findings.
```

blocker / follow-up / root cause / fix scope の意味判断は ChatGPT が行う。

### 4.6 必要な詳細だけ読む

ChatGPT は `aadw status` の結果を見てから、必要な evidence だけ追加取得する。

例:

```text
State: Review results need ChatGPT judgement.
```

なら、次に読むものは主に:

- linked Issue。
- current diff。
- current review。
- current non-outdated unresolved review threads。

GUI logs や無関係な CI logs は読まない。

逆に:

```text
State: GUI failed and needs ChatGPT judgement.
```

なら、GUI receipt、artifact、baseline などだけを追加取得する。

これにより discovery cost を抑える。

---

## 5. 初期 v2 は半自動を正式経路とする

初期 v2 では、worker 完了後に次の AI worker を自動連鎖させない。

正式な経路は次である。

```text
処理完了
  ↓
GitHub 上の正本に結果が残る
  ↓
利用者が ChatGPT に依頼
  ↓
ChatGPT が aadw status <PR> を実行
  ↓
必要な詳細 evidence だけ追加取得
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

最初に `aadw status` で current state を再取得する。

この半自動方式を安定させた後でのみ、自動 Commander を検討する。

---

## 6. 最重要原則

### 6.1 exact-head only

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

Status Collector も current head を最初に取得し、その head に binding された証拠だけを current summary に含める。

### 6.2 current state は毎回オンデマンドで再計算する

ChatGPT は前回の summary を current state として保存・再利用しない。

`aadw status` を再実行し、GitHub の正本から current state を再計算する。

### 6.3 Actions に状態機械を持たせない

GitHub Actions に、

```text
CI 成功したから Codex
Codex が指摘したから Claude
Claude が push したから GUI
```

という意味的な連鎖を実装しない。

各 workflow は一つの仕事をして終了する。

### 6.4 fail closed

証拠不足、head 不一致、AI 出力不明、validation infrastructure failure など、安全に判断できない状態を `continue` にしない。

不明なら止める。

Status Collector も、現在状態を確定できない場合は推測せず、例えば次のように出す。

```text
State: UNKNOWN — current GUI result could not be resolved.
```

### 6.5 merge は AI 判断だけで行わない

ChatGPT は merge 方針を決めるが、実際の merge 前には Deterministic Gate を通す。

---

## 7. ChatGPT の判断方針

### 7.1 Issue の目的を最優先する

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

### 7.2 root-cause cluster

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

### 7.3 classification

cluster は次の3種類に分類する。

- `blocker`: current PR で修正する必要がある。
- `follow_up`: current PR を止めず、別 Issue 候補にする。
- `unknown`: 証拠不足または原因不明。current PR を止める。

### 7.4 repeated blocker

同じ root-cause cluster が複数回の fix 後も blocker として残る場合、小さな patch を積み続けない。

原則として同一 cluster の fix を2 cycle 行っても解消しない場合、設計見直しまたは scope 分割を行う。

これは blocker を無視するための上限ではない。

---

## 8. 実行部隊への指示

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

## 9. CI

CI は current exact head に対して実行する。

初期 v2 では PR #150 で残した最小 CI を使う。

最低限:

- macOS test / clippy。
- Windows test / clippy。

壊れた build を reviewer や GUI validation へ送らない。

原則として CI success 後に review へ進む。

Status Collector は CI の current exact-head 結果を短く表現する。

---

## 10. Review

Codex は current exact head 全体をレビューする。

review 完了後、ChatGPT は `aadw status` で review 完了と unresolved thread 数を把握し、必要なときだけ current review と current non-outdated unresolved threads を読む。

Codex review 完了をきっかけに Actions が自動で Claude を起動してはいけない。

### 10.1 Review Receipt

必要であれば review worker は artifact として compact receipt を残してよい。

receipt は GitHub 上の review / thread の代替正本にはしない。

Status Collector が summary を作るための補助情報として使える。

---

## 11. Claude Fix

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

次回の `aadw status` は新 head を基準にゼロから summary を再構成する。

---

## 12. GUI Validation

### 12.1 focused validation

current Issue の受け入れ条件に対応する scenario を実行する。

すべての PR で full GUI regression を merge blocker にしない。

full regression が必要なら nightly / scheduled validation として分ける。

### 12.2 scenario isolation

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

### 12.3 outcome

GUI outcome は次の3種類とする。

- `pass`: product behavior が期待を満たした。
- `fail`: product behavior が期待と異なった。
- `blocked`: 検証装置、環境、証拠不足などで product の成否を判定できなかった。

OCR failure、runner failure、fixture restoration failure などを product failure と混同しない。

### 12.4 GUI failure の判断

GUI が fail / blocked の場合、Status Collector は current GUI outcome と参照先だけを短く示す。

ChatGPT は必要な GUI evidence だけを追加で読む。

区別する候補:

- current regression。
- target acceptance blocker。
- proven pre-existing independent issue。
- validation infrastructure problem。
- unknown。

`unknown` は fail closed とする。

pre-existing independent と判断する場合、比較可能な trusted baseline など、AI 推測以外の証拠を要求する。

---

## 13. Worker Evidence

各 worker の証拠は、可能な限り GitHub が元々持つ表現を正本として利用する。

例:

- CI: workflow run / check。
- Review: Pull Request review / review thread。
- GUI: workflow run / artifact / compact receipt。
- Fix: commit / workflow run。

独自 receipt は、GitHub の正本だけでは表せない情報を補う場合に限定する。

AI worker について取得可能なら token 数や推定費用を compact receipt に含めてよい。

費用は merge 判断には使わない。

---

## 14. PR コメント方針

AADW の current state を保持するための snapshot comment は作らない。

PR comment は、人間への説明、review discussion、必要な AI decision の記録など、本来コメントとして意味がある場合だけ使う。

current state discovery のためだけに lifecycle comment を増やさない。

ChatGPT が current state を知るための入口は `aadw status <PR>` とする。

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
- persistent current-state snapshot updater。

必要な処理が止まった場合、利用者が ChatGPT に current PR の確認を依頼する。

ChatGPT は `aadw status` で current state を再計算し、必要なら worker を再実行する。

---

## 17. AI provider unavailable

Claude / Codex が quota、credit、service unavailable などで実行できない場合、その worker は `error` / `blocked` で終了する。

自動で別 provider へ多段 fallback しない。

利用可能になった後、利用者が ChatGPT に PR 継続を依頼する。

ChatGPT は `aadw status` で current state を確認して再開方法を決める。

---

## 18. Trust Boundary

Status Collector、Deterministic Gate、effect guard は trusted code とする。

Pull Request body、Issue body、review text、source code、Markdown、GUI fixture などは untrusted data として扱う。

それらに書かれた命令が AADW の権限や安全規則を上書きしてはいけない。

Status Collector は read-only を基本とする。

Claude Fix だけが trusted same-repository PR branch へ変更を push できる。

public fork PR へ自動 fix を行わない。

---

## 19. 初期 workflow / script 構成

workflow 数を少なく保つ。

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

初期 v2 では `aadw-v2-watchdog.yml` も作らない。

通常のロジックは script へ寄せる。

```text
.github/scripts/aadw_v2/
  status.py
  evidence.py
  gate.py
  guards.py
```

`status.py` / `evidence.py` は read-only な current-state discovery を担当する。

workflow YAML は trigger、permissions、runner、worker invocation を中心にする。

---

## 20. 正常経路

```text
Issue
 ↓
ChatGPT が設計
 ↓
Claude implementation
 ↓
Pull Request
 ↓
CI
 ↓
利用者: 「PR #123 を続けて」
 ↓
ChatGPT: aadw status 123
 ↓
Codex review を指示
 ↓
Review 完了
 ↓
利用者: 「PR #123 を続けて」
 ↓
ChatGPT: aadw status 123
 ↓
必要な review evidence だけ読む
 ↓
root-cause cluster / blocker 判断
 ├─ fix
 │    ↓
 │ Claude fix
 │    ↓
 │ new HEAD
 │    ↓
 │ CI から再検証
 │
 ├─ blocked / unknown
 │    ↓
 │ human / design review
 │
 └─ continue
      ↓
必要なら focused GUI
      ↓
利用者: 「PR #123 を続けて」
      ↓
ChatGPT: aadw status 123
      ↓
必要な GUI evidence だけ読む
      ↓
Deterministic Gate
      ↓
merge
```

---

## 21. GitHub Copilot Commander は将来拡張

初期 v2 の Commander は ChatGPT のみとする。

ChatGPT 主導のループが複数 PR で安定するまで、GitHub Copilot Commander を実装しない。

将来追加する場合も、Copilot 専用の状態機械を作らない。

同じ GitHub 上の正本と、同じ `aadw status` / evidence discovery contract を利用する。

---

## 22. 設計上の禁止事項

初期 v2 では原則として次を追加しない。

- Actions による意味的な自動状態遷移。
- current state を複製する PR snapshot comment。
- current state を保存する repository file / database。
- 一つの edge case 専用 reconcile workflow。
- 過去 SHA の status を current state に継承する処理。
- PR comment 件数からの状態推定。
- review comment 1件ごとの Claude fix。
- AI の自由文だけを根拠に merge。
- unrelated GUI failure を証拠なしに waive / blocker 判定する処理。
- quota reset 時刻を解釈する複雑な retry controller。
- ChatGPT に巨大な raw JSON や全ログを毎回読ませる current-state discovery。

---

## 23. 実装順序

### Phase 1: AADW Status Collector

- PR / current head を取得する。
- CI、Review、current threads、GUI、Gate の有無を read-only で収集する。
- exact-head filtering を行う。
- 短い自然言語 summary を出力する。
- Collector が意味判断をしないことをテストする。

### Phase 2: Codex Review Worker

- ChatGPT から exact-head review を明示的に起動できるようにする。
- review evidence が GitHub 上に明確に残ることを確認する。
- `aadw status` が review 完了と unresolved thread 数を短く示せるようにする。

### Phase 3: ChatGPT Commander Loop

- `PR #xxx を続けて` から `aadw status` を最初に読む運用を固定する。
- summary から必要 evidence だけを選択的に読む。
- root-cause cluster / blocker / follow-up / unknown を判断する。

### Phase 4: Claude Fix Worker

- ChatGPT が決めた fix scope から一回だけ Claude を起動する。
- exact-head guard を start / push boundary で行う。

### Phase 5: Focused GUI Validation

- acceptance に対応した scenario を実行する。
- `pass / fail / blocked` と evidence refs を `aadw status` から把握できるようにする。

### Phase 6: Deterministic Gate

- current exact-head evidence を機械的に再確認する。
- expected head SHA 付き merge を行う。

### Phase 7: 運用安定化

- 複数 PR で ChatGPT 主導ループを運用する。
- `aadw status` の出力が不足している場合のみ collector を改善する。
- 同じ GitHub API を何度も追加取得している箇所を観測し、summary に必要最小限の refs / counts を追加する。
- current state の persistent copy は追加しない。

### Phase 8: GitHub Copilot Commander（将来）

ChatGPT 主導が安定した後にのみ検討する。

---

## 24. 完成条件

初期 AADW v2 は次を満たせば完成とする。

- ChatGPT が唯一の司令塔として一貫して判断する。
- `PR #xxx を続けて` に対して、最初に一つの `aadw status` 相当処理で current state を把握できる。
- ChatGPT が current-state discovery のために GitHub 上を毎回あちこち探索しなくてよい。
- Status Collector の出力が短い自然言語で、current head、主要 step、現在位置、必要な refs を正確に示す。
- GitHub 上の元情報だけが persistent state の正本であり、current snapshot のコピーを持たない。
- summary と正本の不整合という新しい failure modeを作らない。
- ChatGPT は必要な詳細 evidence だけを追加取得する。
- current exact head 以外の証拠で判断・merge できない。
- review finding を root-cause cluster 単位で修正できる。
- fix 後は新 head で再検証される。
- GUI infrastructure failure と product failure を区別する。
- merge は Deterministic Gate を通す。
- Actions に意味的な自動状態機械がない。
- Watchdog / reconcile 群が増殖していない。

---

## 25. 最終原則

AADW v2 の persistent state は **GitHub 自身が持つ元情報だけ**とする。

ChatGPT のために current state のコピーを保存しない。

代わりに、必要な瞬間だけ `aadw status` が current exact-head evidence を集め、短い自然言語へ変換する。

```text
Persistent truth
    = GitHub native evidence

Derived current state
    = aadw status 実行時だけ生成

Semantic decision
    = ChatGPT

Execution
    = GitHub Actions / workers

Safety
    = Deterministic Gate
```

この分離を崩さない。

新機能を追加するときは常に次を確認する。

> その情報はすでに GitHub のどこかに正本として存在しないか。  
> 永続コピーを増やさず、`aadw status` でオンデマンドに導出できないか。  
> ChatGPT に必要なのは raw data 全部ではなく、短い current-state summary と必要な evidence への入口だけではないか。

AADW v2 は、**状態を複製して管理するシステムではなく、GitHub 上の事実を必要なときだけ安く読み解き、ChatGPT が司令するシステム**とする。
