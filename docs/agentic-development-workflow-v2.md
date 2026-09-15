# AI Agent Development Workflow v2 設計書

## 1. 位置づけ

この文書は、Hane の AI Agent Development Workflow（AADW）を一から作り直すための v2 設計書である。

AADW v1 は PR #150 で停止した。v2 は v1 の状態機械、独自 status、receipt、routing、reconcile、retry 契約との互換性を持たない。v1 の情報は履歴として参照してよいが、v2 の current state や merge 判断には利用しない。

初期 v2 は完全自動化を目標にしない。まず、ChatGPT が少ない情報取得コストで現在状況を理解し、単純な実行部品へ指示できることを優先する。

---

## 2. 設計原則

AADW v2 は次の3原則を最優先する。

### 2.1 独立性

一つ一つの機能は一つの仕事だけを行う。

各機能は、自分の処理が終わった後に次工程を決めない。

例:

- `aadw status`: GitHub 上の事実を短く表示するだけ。
- `aadw review`: current exact head を Codex にレビューさせるだけ。
- `aadw fix`: ChatGPT が指定した修正を Claude に実行させるだけ。
- `aadw gui`: 指定された GUI scenario を実行するだけ。
- `aadw gate`: merge 直前の客観条件だけを確認する。

### 2.2 非導出性

既に GitHub に存在する事実から、別の persistent な「AADW state」を作らない。

例えば CI result が GitHub Check に存在するなら `aadw-v2/ci` というコピー status を作らない。Codex review が GitHub Review に存在するなら、同じ内容の Review Receipt を必須にしない。

必要な情報は、その瞬間に正本から取得して表示する。

```text
Persistent state = GitHub 自身
Derived view     = 必要な瞬間だけ生成し、保存しない
```

### 2.3 AI 判断優先

客観的に機械判定できない複雑な判断を、ルールや状態機械としてコード化しない。

次のような判断は ChatGPT が担当する。

- review finding が blocker か。
- 複数 finding が同じ root cause か。
- follow-up に分離できるか。
- Claude にどの範囲を修正させるか。
- GUI failure が product bug か validation problem か。
- 追加検証が必要か。

機械側は、事実の取得・実行・客観条件の検査だけを担当する。

---

## 3. 基本モデル

初期 v2 では、**ChatGPT が AADW 全体の唯一の司令塔**となる。

GitHub Actions は司令塔ではない。ChatGPT に指示された単一の処理を実行する Execution Layer とする。

```text
                         ChatGPT
                     唯一の司令塔
                           │
                aadw_status(PR) を呼ぶ
                           │
                           ▼
                  Status GitHub Action
                  read-only collector
                           │
                 status.txt artifact
                           │
                           ▼
                         ChatGPT
               短い事実一覧を読む
                           │
               必要な証拠だけ追加取得
                           │
              複雑な意味判断を行う
                           │
          ┌────────────────┼────────────────┐
          ▼                ▼                ▼
       Claude            Codex         GUI Validator
          │                │                │
          └────────────────┼────────────────┘
                           ▼
                         GitHub
                     事実をそのまま保存
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

AADW の中心ループは次である。

```text
ChatGPT
  ↓
aadw_status(PR)
  ↓
GitHub Actions が status collector を実行
  ↓
status.txt artifact を ChatGPT が取得
  ↓
必要な evidence だけ読む
  ↓
判断する
  ↓
一つの worker を指示する
  ↓
worker が GitHub に結果を残す
  ↓
利用者が ChatGPT に戻す
```

`aadw_status(PR)` は ChatGPT から見た一つの論理操作である。ChatGPT が Hane repository のローカル shell で `./scripts/aadw` を直接実行することを前提にしない。

---

## 4. 正本は GitHub にある既存情報だけ

AADW v2 は current state のコピーを維持しない。

正本として使うのは、GitHub が自然に持つ情報だけである。

- Pull Request と current head SHA。
- Issue と acceptance criteria。
- GitHub Actions run / job / conclusion。
- CI Check。
- Pull Request Review。
- Review Thread。
- Commit。
- GUI validation の run / artifact。
- GitHub の mergeability。

同じ事実を次の場所へ複製しない。

- AADW Snapshot コメント。
- current-state file。
- 独自 database。
- 同じ意味を持つ独自 commit status。
- 同じ意味を持つ receipt。

GitHub に自然な正本が存在しない情報だけ、最小限の artifact を作る。

例えば GUI validation の screenshot、操作ログ、期待値と実測値などは artifact として保持してよい。

---

## 5. AADW Status Collector

### 5.1 目的

ChatGPT が current state を知るたびに GitHub 上を探索し回らなくてよいよう、**status 取得専用の read-only GitHub Actions workflow** を用意する。

ChatGPT から見た論理操作は次とする。

```text
aadw_status(PR)
```

その実体は次である。

```text
ChatGPT
  ↓ status workflow を起動
GitHub Actions
  ↓ PR番号を入力として collector を実行
GitHub 上の正本を read-only で取得
  ↓
短い status.txt を artifact として保存
  ↓
ChatGPT が artifact を取得
```

status workflow は `workflow_dispatch` 等の明示的な起動を使う。ChatGPT 側には、workflow の起動から対象 run の特定、完了確認、`status.txt` artifact の取得までを一つの操作として扱える薄い tool / connector を用意する。

この tool は AADW の判断を行わない。単に status workflow を起動して結果を返す transport adapter である。

現在の ChatGPT GitHub connector に workflow dispatch 操作が公開されていない場合、この薄い tool / connector の提供は Phase 1 の実装要件とする。

### 5.2 Status workflow が行うこと

Status Collector が行うのは次だけである。

1. PR の current head SHA を取得する。
2. GitHub 上の関連情報を read-only で取得する。
3. current head に関係する情報だけを残す。
4. 必要な値だけを短い自然言語で `status.txt` に出力する。
5. 収集開始時と終了時の current head を比較し、途中で head が変わった場合は結果を無効として明示する。

つまり、

```text
収集
filter
format
```

だけを行う。

### 5.3 行わないこと

Status Collector は次を行わない。

- blocker / follow-up の判断。
- root-cause classification。
- 次工程の決定。
- Claude を起動すべきかの判断。
- GUI が必要かの意味判断。
- merge してよいかの判断。
- persistent snapshot の作成。
- PR comment の作成・更新。
- product branch の変更。

### 5.4 出力

ChatGPT に読ませる出力は、短く正確な自然言語を基本とする。

例:

```text
PR #123 — HEAD a1b2c3
Issue #101
CI: macOS PASS, Windows PASS
Codex review: completed
Current unresolved review threads: 3
GUI validation: none
PR: mergeable
```

ここから、

```text
Next: Claude should fix the findings.
```

のような推論結果は出さない。

ChatGPT が上記の事実を読んで、次に何を見るか・何をするか判断する。

`status.txt` は status workflow の実行結果であり、current state の正本ではない。次回は再度 workflow を実行して GitHub の正本から最新情報を取得する。

### 5.5 JSON の扱い

Collector 内部で JSON や構造体を使うことは問題ない。

ただし、独自の巨大な `AadwState` を設計して GitHub の情報を別モデルへ変換することを目的にしない。

理想は次である。

```text
GitHub API
  ↓
必要な値だけ選択
  ↓
短い自然言語 status.txt
```

テストしやすさのための一時的な内部構造は許容するが、それを persistent state や Commander 向け外部 contract にしない。

---

## 6. ChatGPT の current-state discovery

利用者は、例えば次だけを依頼すればよい。

```text
PR #123 を続けて
```

ChatGPT はまず `aadw_status(123)` を一度呼ぶ。

tool / connector は内部で status workflow を起動し、完了した run の `status.txt` を返す。

ChatGPT はその短い出力から current state を把握し、必要な証拠だけ追加取得する。

例えば review threads が3件あると分かった場合、追加取得するのは主に次だけでよい。

- linked Issue。
- current diff。
- current review。
- current unresolved review threads。

GUI logs や無関係な CI logs は読まない。

逆に GUI failure がある場合は GUI evidence だけを詳しく読む。

**ChatGPT は `aadw_status(PR)` を current-state discovery の固定入口にし、そこから必要な証拠だけへ drill-down する。**

status workflow 自体に詳細 evidence を大量に詰め込まない。

---

## 7. exact-head only

current head SHA を一つの世代として扱う。

current head が `A` なら、現在判断に使えるのは `A` に関係する証拠だけである。

Claude が修正して `B` になったら、`A` の CI / Review / GUI evidence は履歴になる。

Status Collector も current head を最初に取得し、古い head の結果を current summary に混ぜない。

さらに collector 終了時に current head を再取得する。

```text
start: HEAD = A
collect facts
end:   HEAD = A
```

一致した場合だけ `status.txt` を有効な収集結果として扱う。

途中で `A → B` に変わった場合は、推測して補正せず次のように返す。

```text
STATUS INVALID
HEAD changed during collection: A -> B
```

ChatGPT は新しい status workflow を実行し直す。

古い head の情報を組み合わせて current state を推測する処理は作らない。

---

## 8. 半自動を正式経路とする

初期 v2 では worker 完了後に次 worker を自動連鎖させない。

正式経路:

```text
worker 完了
  ↓
GitHub に結果が残る
  ↓
利用者: 「PR #123 を続けて」
  ↓
ChatGPT: aadw_status(123)
  ↓
Status GitHub Action
  ↓
status.txt
  ↓
必要な証拠だけ追加取得
  ↓
ChatGPT が判断
  ↓
次の一つの worker を実行
```

この経路が複数 PR で安定した後にのみ、自動 Commander を検討する。

---

## 9. CI

CI は current exact head を test / clippy するだけとする。

現在の minimal CI を利用する。

CI が成功したから Codex を自動起動する、という意味的な連鎖は作らない。

CI result の正本は GitHub Check とする。

別の CI Receipt や `aadw-v2/ci` status は作らない。

---

## 10. Codex Review

`aadw review <PR>` は current exact head を Codex にレビューさせるだけとする。

Codex review の正本は GitHub Review / Review Thread とする。

Review Receipt を必須にしない。

レビュー完了後に Claude を自動起動しない。

ChatGPT が current review を読み、次を判断する。

---

## 11. ChatGPT の review 判断

ChatGPT は review finding の件数をゼロにすること自体を目的にしない。

current Issue の目的と acceptance criteria を優先する。

### 11.1 classification

finding / cluster は意味判断として次のいずれかに分類できる。

- `blocker`: current PR で修正が必要。
- `follow_up`: current PR を止めず、別 Issue 候補。
- `unknown`: 証拠不足または原因不明。fail closed で停止。

### 11.2 root-cause cluster

review comment 1件を fix 1回に対応させない。

ChatGPT が current findings をまとめて読み、同じ原因に属するものを cluster 化する。

```text
8 findings
  ↓ ChatGPT
3 root causes
  ↓
2 blocker clusters
1 follow-up cluster
  ↓
1 Claude fix
```

同一 cluster の兄弟ケースを fix 前に確認し、独立した問題を同じ fix に混ぜない。

### 11.3 repeated blocker

同じ root cause が複数 fix cycle 後も残る場合、単純 patch を続けず、ChatGPT が設計見直しや scope 分割を判断する。

この判断を機械的な cycle rule として複雑に実装しない。

---

## 12. Claude Fix

`aadw fix` は ChatGPT が決めた修正 scope を Claude に渡して実行するだけとする。

指示には最低限次を含める。

- target PR。
- target exact head SHA。
- current Issue の目的。
- root-cause cluster。
- fix scope。
- 触らない独立問題。
- 必要な test。

Claude は開始時と push 直前に current head を確認する。

```text
start:       current head == target SHA
before push: current head == target SHA
```

不一致なら push しない。

push 後は新 head なので、再び `aadw_status(PR)` から判断をやり直す。

---

## 13. GUI Validation

`aadw gui` は ChatGPT が指定した scenario を実行するだけとする。

GUI が必要かどうかを `aadw gui` 自身が判断しない。

### 13.1 focused scenario

current Issue の acceptance criteria に対応する scenario を中心にする。

すべての PR に full GUI regression を課さない。

### 13.2 isolation

mutating scenario は状態を共有しない。

各 scenario は独立した fixture / application state で実行する。

### 13.3 outcome

GUI worker が記録するのは観測事実である。

- `pass`: 期待した観測結果になった。
- `fail`: 期待した観測結果と異なった。
- `blocked`: 検証自体を成立させられなかった。

`fail` の原因が product か harness かは ChatGPT が evidence を読んで判断する。

GitHub に自然な格納先がない screenshot / 操作ログ / observed result は artifact に保存してよい。

---

## 14. Deterministic Gate

Gate は意味判断をしない。

ChatGPT が必要な意味判断を終えた後の、最後の事故防止 interlock とする。

確認するのは完全に客観的な条件だけに限定する。

例:

- current head SHA == expected head SHA。
- required CI checks == success。
- required GUI validation がある場合、その指定 run == success。
- GitHub が PR を mergeable と報告している。
- merge 実行時の expected head SHA が一致する。

次は Gate が判断しない。

- review finding が blocker か。
- follow-up に分離してよいか。
- GUI failure が current regression か。
- 追加 test が必要か。

これらは ChatGPT の責務である。

---

## 15. 独自 status / receipt の原則

新しい status や receipt を「AADWだから」という理由だけで作らない。

原則:

```text
GitHub に自然な正本がある
  → それを使う

GitHub に必要な証拠の置き場所がない
  → 最小限の artifact を作る
```

したがって初期 v2 では、次を必須にしない。

- `aadw-v2/ci`
- `aadw-v2/review`
- `aadw-v2/fix`
- `aadw-v2/gui`
- generic Worker Receipt
- AADW Snapshot comment

状態を二重管理しないことを優先する。

`status.txt` artifact は status workflow の一時的な実行結果であり、これらの persistent AADW state の代替ではない。

---

## 16. fail closed

事実を取得できない、current head と結び付けられない、証拠が不足する、といった場合は推測しない。

Status Collector は、分からない事実を埋めず、例えば次のように表示する。

```text
GUI validation: unknown
```

head が収集中に変化した場合も結果を無効化する。

ChatGPT も不明な状態を pass として扱わない。

---

## 17. Provider unavailable

Claude / Codex が quota、credit、service unavailable などで実行できない場合、その処理は失敗した事実だけを残して停止する。

初期 v2 では次を作らない。

- quota reset timer。
- provider availability state machine。
- 自動 multi-provider fallback chain。
- retry reconcile。

必要になったら利用者が ChatGPT に戻し、ChatGPT が current state を確認して再実行を判断する。

---

## 18. Trust Boundary

ChatGPT、worker、Gate はそれぞれ責務を越えない。

- Issue / PR body / review text / source code は untrusted data として扱う。
- Claude だけが trusted same-repository PR branch のコードを変更できる。
- Codex と GUI Validator は product code を変更しない。
- Gate は意味判断をしない。
- Status Collector は repository mutation をしない。
- status workflow は read-only permissions を原則とする。
- ChatGPT から status workflow を起動する transport adapter は、指定 workflow の dispatch と対象 artifact 取得以外の権限を持たせない。

fork PR への自動修正は初期 v2 の対象外とする。

---

## 19. 初期コマンド構成

初期 v2 は大きな workflow を作らず、単純な道具を用意する。

ChatGPT から見た論理操作は概念的に次程度とする。

```text
aadw_status(PR)
aadw_review(PR)
aadw_fix(PR, instruction)
aadw_gui(PR, scenario)
aadw_gate(PR, expected_head)
```

実装上、これらが GitHub Actions workflow を起動する場合でも、ChatGPT 側からは一つの操作として扱える薄い tool / connector を用意する。

各操作は、

```text
一入力
一仕事
一結果
```

を原則とする。

操作同士を内部で自動連鎖させない。

---

## 20. 実装順序

### Phase 1: `aadw_status(PR)`

最初に作る。

- read-only status workflow を追加する。
- PR 番号を入力として受け取る。
- current head を取得する。
- PR / Issue / CI / Review / Thread / Actions / GUI evidence の必要な事実を取得する。
- current head に filter する。
- 開始時と終了時の head 一致を確認する。
- 短い自然言語 `status.txt` を artifact として出力する。
- persistent state を作らない。
- ChatGPT から workflow dispatch → run 特定 → artifact 取得までを一操作で行える薄い tool / connector を用意する。

Phase 1 の完成条件は、利用者が `PR #123 を続けて` と依頼したとき、ChatGPT が GitHub 上を手作業で横断探索せず `aadw_status(123)` 一回で短い status を取得できることである。

### Phase 2: `aadw_review(PR)`

current exact head への Codex review 起動だけを実装する。

### Phase 3: ChatGPT Commander Loop

`PR #xxx を続けて` → `aadw_status(PR)` → 必要 evidence → 判断、の運用を成立させる。

### Phase 4: `aadw_fix(PR, instruction)`

ChatGPT の修正指示を Claude に渡す単純 worker を実装する。

### Phase 5: `aadw_gui(PR, scenario)`

focused GUI scenario の実行と証拠保存だけを実装する。

### Phase 6: `aadw_gate(PR, expected_head)`

客観的な merge safety check だけを実装する。

### Phase 7: 運用安定化

複数の実 PR で、ChatGPT 主導ループが安定するか確認する。

### Phase 8: GitHub Copilot Commander（将来拡張）

ChatGPT 主導が十分安定してから検討する。

Copilot を追加しても既存 worker や新しい state machine を作り直さない。

---

## 21. 完成条件

初期 v2 は次を満たせば完成とする。

- ChatGPT が唯一の司令塔として全体を統括できる。
- `aadw_status(PR)` 一回で status GitHub Action を起動し、current state の短い事実一覧を取得できる。
- ChatGPT が Hane repository のローカル shell を直接操作することを前提にしない。
- ChatGPT が詳細 evidence を必要なものだけ追加取得できる。
- status workflow は read-only で、意味判断や persistent current state を持たない。
- 各 worker は一つの仕事だけを行う。
- worker が勝手に次 worker を起動しない。
- GitHub 上の既存情報を正本として使い、同じ意味の AADW state を複製しない。
- review の複雑な判断は ChatGPT が行う。
- GUI failure の意味判断は ChatGPT が行う。
- exact-head 以外の証拠を current evidence として使わない。
- merge 前の客観的 safety check は Gate が行う。
- provider failure 時は分かりやすく停止できる。

---

## 22. 最終原則

AADW v2 は、大きな自動状態機械ではない。

**ChatGPT と、独立した単純な道具の組み合わせ**である。

```text
GitHub = 事実の正本
GitHub Actions = 単純な実行場所
Thin tools/connectors = ChatGPT と Actions の接続
ChatGPT = 複雑な判断
Gate = 最後の客観的安全装置
```

`aadw_status(PR)` のポイントは、状態を新しく保存することではない。

**ChatGPT が必要な瞬間に GitHub Actions へ収集を依頼し、その時点の正本から作った短い実行結果だけを受け取ること**である。

新機能を追加するときは、必ず次を確認する。

> 既存の事実を別の state として複製していないか。  
> この機能は一つの仕事だけをしているか。  
> AI が得意な判断をルールや状態機械として実装しようとしていないか。

この3点に反する場合、機能追加より設計の単純化を優先する。
