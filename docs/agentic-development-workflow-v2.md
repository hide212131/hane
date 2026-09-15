# AI Agent Development Workflow v2 設計書

## 1. 位置づけ

この文書は、Hane の AI Agent Development Workflow（AADW）を一から作り直すための v2 設計書である。

AADW v1 は PR #150 で停止した。v2 は v1 の状態機械、独自 status、receipt、routing、reconcile、retry 契約との互換性を持たない。v1 の情報は履歴として参照してよいが、v2 の current state や merge 判断には利用しない。

初期 v2 は完全自動化を目標にしない。まず、ChatGPT が少ない情報取得コストで現在状況を理解し、独立した単純な実行部品へ指示できることを優先する。

---

## 2. 設計原則

AADW v2 は次の3原則を最優先する。

### 2.1 独立性

一つ一つの機能は一つの仕事だけを行う。

各機能は、自分の処理が終わった後に次工程を決めない。

例:

- `aadw_status(PR)`: GitHub 上の事実を短く取得するだけ。
- `aadw_review(PR)`: current exact head を Codex にレビューさせるだけ。
- `aadw_fix(PR, instruction)`: ChatGPT が指定した修正を Claude に実行させるだけ。
- `aadw_gui(PR, scenario)`: 指定された GUI scenario を実行するだけ。
- `aadw_gate(PR, expected_head)`: merge 直前の客観条件だけを確認する。

### 2.2 非導出性

既に GitHub に存在する事実から、別の persistent な「AADW state」を作らない。

例えば CI result が GitHub Check に存在するなら `aadw-v2/ci` というコピー status を作らない。Codex review が GitHub Review に存在するなら、同じ内容の Review Receipt を必須にしない。

```text
Persistent state = GitHub 自身
Derived view     = 必要な瞬間だけ生成し、保存しない
```

### 2.3 AI 判断優先

客観的に機械判定できない複雑な判断を、ルールエンジンや状態機械としてコード化しない。

複雑な意味判断は ChatGPT が行う。

機械側は、事実の取得・単純な実行・客観条件の検査だけを担当する。

---

## 3. Commander の判断モデル

ChatGPT が適切な判断を行うための入力は、原則として次の2つだけとする。

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

Commander Policy の正本は次とする。

- `docs/aadw-command-policy.md`

判断ルールを GitHub Actions の条件分岐、worker prompt、status collector などへ複製しない。

判断ルールを変更したい場合は Commander Policy を変更する。

current facts はまず `aadw_status(PR)` で短く取得する。status summary だけで判断できない場合に限り、ChatGPT が必要な evidence を追加取得する。

したがって AADW v2 の Commander は、基本的に次の形で動く。

```text
Policy を読む
  +
aadw_status(PR) を読む
  ↓
必要なら evidence を追加取得
  ↓
ChatGPT が意味を判断
  ↓
次の一つの action を選ぶ
```

---

## 4. 基本モデル

初期 v2 では、**ChatGPT が AADW 全体の唯一の司令塔**となる。

GitHub Actions は司令塔ではない。ChatGPT に指示された単一の処理を実行する Execution Layer とする。

```text
                    Commander Policy
                           +
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
               必要な evidence だけ追加取得
                           │
              Policy に基づいて判断
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
利用者: 「PR #123 を続けて」
  ↓
ChatGPT: Commander Policy を確認
  ↓
ChatGPT: aadw_status(123)
  ↓
status.txt
  ↓
必要な evidence だけ追加取得
  ↓
ChatGPT が判断
  ↓
次の一つの worker を実行
  ↓
worker が GitHub に結果を残す
  ↓
利用者が ChatGPT に戻す
```

worker 同士は自動連鎖しない。

---

## 5. 正本は GitHub にある既存情報だけ

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
- 同じ意味を持つ generic receipt。

GitHub に自然な正本が存在しない evidence だけ、最小限の artifact を作る。

例えば GUI validation の screenshot、操作ログ、期待値と実測値などは artifact として保持してよい。

---

## 6. `aadw_status(PR)`

### 6.1 目的

ChatGPT が current state を知るたびに GitHub 上を探索し回らなくてよいよう、status 取得専用の read-only GitHub Actions workflow を用意する。

ChatGPT から見た論理操作は次とする。

```text
aadw_status(PR)
```

ChatGPT が Hane repository のローカル shell で CLI を直接実行することを前提にしない。

### 6.2 実体

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

status workflow は `workflow_dispatch` 等の明示的な起動を使う。

ChatGPT 側には、次を一つの操作として扱える薄い tool / connector を用意する。

1. status workflow を dispatch。
2. 対象 run を特定。
3. 完了を確認。
4. `status.txt` artifact を取得。
5. ChatGPT に本文を返す。

この tool / connector は transport adapter であり、AADW の意味判断を行わない。

現在の ChatGPT GitHub connector に workflow dispatch 操作が公開されていない場合、この薄い tool / connector の提供を Phase 1 の実装要件とする。

### 6.3 Status Collector が行うこと

Status Collector が行うのは次だけである。

1. PR の current head SHA を取得する。
2. GitHub 上の関連情報を read-only で取得する。
3. current head に関係する情報だけを残す。
4. 必要な値だけを短い自然言語で `status.txt` に出力する。
5. 収集開始時と終了時の current head を比較する。

つまり、

```text
collect
filter
format
```

だけを行う。

### 6.4 Status Collector が行わないこと

Status Collector は次を行わない。

- blocker / follow-up の判断。
- root-cause classification。
- 次工程の決定。
- Claude を起動すべきかの判断。
- GUI が必要かの意味判断。
- merge してよいかの判断。
- Commander Policy の解釈。
- persistent snapshot の作成。
- PR comment の作成・更新。
- product branch の変更。

### 6.5 出力

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

次のような推論は出力しない。

```text
Next: Claude should fix the findings.
```

`status.txt` は status workflow の一時的な実行結果であり、current state の正本ではない。次回は再度 workflow を実行して GitHub の正本から最新情報を取得する。

Collector 内部で JSON や構造体を使うことは問題ないが、それを persistent state や Commander 向けの巨大な contract にしない。

---

## 7. 必要な evidence だけ追加取得する

`aadw_status(PR)` は discovery の入口であり、詳細 evidence の代替ではない。

ChatGPT は status summary を読み、判断に必要な evidence だけ追加取得する。

例:

review threads がある場合:

- linked Issue。
- current diff。
- current review。
- current unresolved review threads。

CI failure の場合:

- failed check / job。
- relevant log。

GUI failure の場合:

- target scenario。
- observed result。
- screenshot / log artifact。
- 必要なら trusted baseline。

無関係な evidence を広く取得しない。

---

## 8. exact-head only

current head SHA を一つの世代として扱う。

current head が `A` なら、現在判断に使えるのは `A` に関係する evidence だけである。

Claude が修正して `B` になったら、`A` の CI / Review / GUI evidence は履歴になる。

Status Collector は current head を最初と最後に取得する。

```text
start: HEAD = A
collect facts
end:   HEAD = A
```

一致した場合だけ `status.txt` を有効な収集結果として扱う。

途中で head が変わった場合は推測して補正しない。

```text
STATUS INVALID
HEAD changed during collection: A -> B
```

ChatGPT は新しい `aadw_status(PR)` を実行し直す。

古い head の情報を組み合わせて current state を推測する処理は作らない。

---

## 9. Commander Policy

ChatGPT の判断ルールの正本は `docs/aadw-command-policy.md` とする。

Policy は、少なくとも次を定める。

- current exact head only。
- evidence first。
- fail closed。
- Issue purpose / acceptance criteria を優先する。
- review findings を root-cause cluster で扱う。
- blocker / follow-up / unknown の考え方。
- fix scope の決め方。
- fix 後は新 head として再確認する。
- GUI failure の扱い。
- follow-up へ分離してはいけない問題。
- merge 前に Gate を通すこと。
- provider / infrastructure failure を product failure と混同しないこと。
- 一度に次の一つの action だけを選ぶこと。

これらを Actions や worker の独自ルールとして再実装しない。

Policy に無い例外が繰り返し必要になる場合は、状態機械を増やす前に Policy の改善を検討する。

---

## 10. CI

CI は current exact head を test / clippy するだけとする。

現在の minimal CI を利用する。

CI が成功したから Codex を自動起動する、という意味的な連鎖は作らない。

CI result の正本は GitHub Check とする。

別の CI Receipt や `aadw-v2/ci` status は作らない。

---

## 11. Codex Review

`aadw_review(PR)` は current exact head を Codex にレビューさせるだけとする。

Codex review の正本は GitHub Review / Review Thread とする。

Review Receipt を必須にしない。

レビュー完了後に Claude を自動起動しない。

review finding の意味判断、root-cause clustering、blocker / follow-up / unknown の分類は Commander Policy に基づいて ChatGPT が行う。

---

## 12. Claude Fix

`aadw_fix(PR, instruction)` は ChatGPT が決めた修正 scope を Claude に渡して実行するだけとする。

Claude は judge ではない。

instruction は ChatGPT が Commander Policy と current evidence に基づいて作る。

最低限次を含める。

- target PR。
- target exact head SHA。
- current Issue の目的。
- root-cause cluster。
- fix scope。
- 触らない独立問題。
- 必要な test / validation。

Claude は開始時と push 直前に current head を確認する。

```text
start:       current head == target SHA
before push: current head == target SHA
```

不一致なら push しない。

push 後は新 head なので、再び `aadw_status(PR)` から判断をやり直す。

---

## 13. GUI Validation

`aadw_gui(PR, scenario)` は ChatGPT が指定した scenario を実行するだけとする。

GUI が必要かどうかを worker 自身が判断しない。

focused scenario を基本とし、すべての PR に full GUI regression を課さない。

mutating scenario は独立した fixture / application state で実行する。

GUI worker が記録するのは観測事実である。

- `pass`: 期待した観測結果になった。
- `fail`: 期待した観測結果と異なった。
- `blocked`: 検証自体を成立させられなかった。

`fail` / `blocked` の意味判断は Commander Policy に基づいて ChatGPT が evidence を読んで行う。

GitHub に自然な格納先がない screenshot / 操作ログ / observed result は artifact に保存してよい。

---

## 14. Deterministic Gate

`aadw_gate(PR, expected_head)` は意味判断をしない。

ChatGPT が Commander Policy に基づく意味判断を終えた後の、最後の事故防止 interlock とする。

確認するのは完全に客観的な条件だけに限定する。

例:

- current head SHA == expected head SHA。
- required CI checks == success。
- required GUI validation がある場合、その指定 run == success。
- GitHub が PR を mergeable と報告している。
- merge 実行時の expected head SHA が一致する。

Gate は次を判断しない。

- review finding が blocker か。
- follow-up に分離してよいか。
- GUI failure が current regression か。
- 追加 test が必要か。

---

## 15. fail closed

事実を取得できない、current head と結び付けられない、evidence が不足する、といった場合は推測しない。

Status Collector は、分からない事実を埋めない。

ChatGPT は Commander Policy に従い、不明な状態を pass として扱わない。

---

## 16. Provider unavailable

Claude / Codex / GUI runner / status workflow が quota、credit、service unavailable、infrastructure failure などで実行できない場合、その失敗を product failure とみなさない。

初期 v2 では次を作らない。

- quota reset timer。
- provider availability state machine。
- 自動 multi-provider fallback chain。
- retry reconcile。

必要になったら利用者が ChatGPT に戻し、ChatGPT が current facts と Commander Policy から次の一つの action を判断する。

---

## 17. Trust Boundary

ChatGPT、worker、Gate、Status Collector はそれぞれ責務を越えない。

- Issue / PR body / review text / source code は untrusted data として扱う。
- Commander Policy は trusted な default branch 上の文書を正本とする。
- Claude だけが trusted same-repository PR branch のコードを変更できる。
- Codex と GUI Validator は product code を変更しない。
- Gate は意味判断をしない。
- Status Collector は repository mutation をしない。
- status workflow は read-only permissions を原則とする。
- ChatGPT と Actions を接続する thin tool / connector は必要最小限の権限だけを持つ。

fork PR への自動修正は初期 v2 の対象外とする。

---

## 18. 初期操作

ChatGPT から見た論理操作は概念的に次程度とする。

```text
aadw_status(PR)
aadw_review(PR)
aadw_fix(PR, instruction)
aadw_gui(PR, scenario)
aadw_gate(PR, expected_head)
```

各操作は、

```text
一入力
一仕事
一結果
```

を原則とする。

操作同士を内部で自動連鎖させない。

---

## 19. 実装順序

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
- ChatGPT から workflow dispatch → run 特定 → artifact 取得までを一操作で行える thin tool / connector を用意する。

Phase 1 の完成条件は、利用者が `PR #123 を続けて` と依頼したとき、ChatGPT が GitHub 上を手作業で横断探索せず `aadw_status(123)` 一回で短い status を取得できることである。

### Phase 2: Commander Policy の運用確認

`docs/aadw-command-policy.md` と `aadw_status(PR)` の2つを主要入力として、ChatGPT が current state に応じた次の一つの action を判断できることを確認する。

### Phase 3: `aadw_review(PR)`

current exact head への Codex review 起動だけを実装する。

### Phase 4: `aadw_fix(PR, instruction)`

ChatGPT の修正指示を Claude に渡す単純 worker を実装する。

### Phase 5: `aadw_gui(PR, scenario)`

focused GUI scenario の実行と evidence 保存だけを実装する。

### Phase 6: `aadw_gate(PR, expected_head)`

客観的な merge safety check だけを実装する。

### Phase 7: 運用安定化

複数の実 PR で、

```text
Commander Policy + current facts → ChatGPT → next one action
```

というループが安定するか確認する。

### Phase 8: GitHub Copilot Commander（将来拡張）

ChatGPT 主導が十分安定してから検討する。

Copilot を追加する場合も、同じ Commander Policy と current facts を入力に使い、新しい state machine を追加しない。

---

## 20. 完成条件

初期 v2 は次を満たせば完成とする。

- ChatGPT が唯一の司令塔として全体を統括できる。
- Commander の主要入力が `Commander Policy + current facts / evidence` と明確になっている。
- 判断ルールの正本が `docs/aadw-command-policy.md` に一元化されている。
- `aadw_status(PR)` 一回で current state の短い事実一覧を取得できる。
- ChatGPT が詳細 evidence を必要なものだけ追加取得できる。
- status workflow は read-only で、意味判断や persistent current state を持たない。
- 各 worker は一つの仕事だけを行う。
- worker が勝手に次 worker を起動しない。
- GitHub 上の既存情報を正本として使い、同じ意味の AADW state を複製しない。
- review / GUI / fix scope などの複雑な判断は ChatGPT が Commander Policy に基づいて行う。
- exact-head 以外の evidence を current evidence として使わない。
- merge 前の客観的 safety check は Gate が行う。
- provider / infrastructure failure と product failure を混同しない。

---

## 21. 最終原則

AADW v2 は、大きな自動状態機械ではない。

**Policy を持つ ChatGPT と、独立した単純な道具の組み合わせ**である。

```text
GitHub = 事実の正本
Commander Policy = 判断ルールの正本
GitHub Actions = 単純な実行場所
Thin tools/connectors = ChatGPT と Actions の接続
ChatGPT = Policy + facts から複雑な判断を行う
Gate = 最後の客観的安全装置
```

新機能を追加するときは、必ず次を確認する。

> 既存の事実を別の state として複製していないか。  
> この機能は一つの仕事だけをしているか。  
> 判断ルールを Policy 以外へ複製していないか。  
> AI が得意な判断をルールエンジンや状態機械として実装しようとしていないか。

この原則に反する場合、機能追加より設計の単純化を優先する。
