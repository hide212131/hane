# AI Agent Development Workflow v2 設計書

## 1. 位置づけ

この文書は、Hane の AI Agent Development Workflow（AADW）を一から作り直すための v2 設計書である。

AADW v1 は PR #150 で停止した。v2 は v1 の状態機械、独自 status、receipt、routing、reconcile、retry 契約との互換性を持たない。

初期 v2 は完全自動化を目標にしない。まず、ChatGPT が少ない情報取得コストで current state を理解し、独立した単純な実行部品へ指示できることを優先する。

---

## 2. 設計原則

AADW v2 は次の4原則を最優先する。

### 2.1 独立性

一つ一つの機能は一つの仕事だけを行う。

各機能は、自分の処理が終わった後に次工程を決めない。

### 2.2 非導出性

既に GitHub に存在する事実から、別の persistent な AADW state を作らない。

```text
Persistent state = GitHub 自身
Derived view     = 必要な瞬間だけ生成し、保存しない
```

### 2.3 AI 判断優先

客観的に機械判定できない複雑な判断を、ルールエンジンや状態機械としてコード化しない。

複雑な意味判断は ChatGPT が行う。

### 2.4 抽象化は必要になってから導入する

重複や複雑さが実運用で確認される前に、collector、wrapper、独自 API、専用 connector を先回りして作らない。

まず既存 GitHub API / connector の単純な組み合わせで成立させる。

同じ探索や整形が何度も繰り返され、実際にコストや誤りの原因になった場合だけ、その部分を一つの小さな道具へ抽出する。

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

判断ルールを GitHub Actions、worker、collector に複製しない。

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
┌──────────┬──────────┬──────────┐
▼          ▼          ▼          ▼
CI       Codex      Claude      GUI
            GitHub / Actions
                 ↓
             current facts
                 ↓
              ChatGPT
                 ↓
          Deterministic Gate
                 ↓
               merge
```

worker 同士は自動連鎖しない。

---

## 5. 正本は GitHub にある既存情報だけ

AADW v2 は current state のコピーを維持しない。

正本として使うのは GitHub が自然に持つ情報である。

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

GitHub に自然な置き場所がない evidence だけ最小限の artifact として保存する。

---

## 6. Current-state discovery

利用者は例えば次だけを依頼すればよい。

```text
PR #123 を続けて
```

ChatGPT は Commander Policy を確認し、current state を取得する。

### 6.1 初期方式

初期 v2 では、専用 Status Collector を前提にしない。

ChatGPT は GitHub connector の既存 read 操作を固定手順で使う。

原則として最初に取得するのは次程度とする。

1. PR metadata と current head SHA。
2. current head の CI / checks。
3. current review / unresolved review threads。
4. relevant workflow runs / GUI evidence の有無。

その結果から短い事実一覧を内部的に組み立てる。

例:

```text
PR #123 — HEAD a1b2c3
CI: macOS PASS, Windows PASS
Review: completed
Current unresolved review threads: 3
GUI validation: none
PR: mergeable
```

これは保存しない。

### 6.2 必要な evidence だけ追加取得する

短い事実一覧だけで判断できない場合、必要な evidence だけ追加取得する。

review 判断なら Issue、current diff、current unresolved threads。

CI failure なら failed job と relevant log。

GUI failure なら target scenario、observed result、screenshot / log artifact、必要なら trusted baseline。

無関係な evidence は広く取得しない。

### 6.3 `aadw_status(PR)` は最適化として後から導入できる

実運用で次のような問題が確認された場合のみ、固定読み取りを `aadw_status(PR)` という一つの read-only 操作へ抽出してよい。

- 毎回同じ GitHub API を多数呼ぶ。
- exact-head filtering が繰り返し実装される。
- current-state discovery が遅い。
- discovery 手順の揺れが誤判断の原因になる。

その場合でも `aadw_status(PR)` は collect / filter / format だけを行い、意味判断は行わない。

実装方式は最小のものを選ぶ。GitHub Actions workflow + artifact + thin connector を採用するか、既存 connector の単純な wrapper にするかは、実測した複雑さに基づいて決める。

初期設計の段階で特定方式へ固定しない。

---

## 7. exact-head only

current head SHA を一つの世代として扱う。

current head が `A` なら、現在判断に使えるのは `A` に関係する evidence だけである。

Claude が修正して `B` になったら、`A` の CI / Review / GUI evidence は履歴になる。

古い head の情報を組み合わせて current state を推測しない。

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
- merge 前に Gate を通すこと。
- provider / infrastructure failure を product failure と混同しないこと。
- 一度に次の一つの action だけを選ぶこと。

Policy にない例外が繰り返し必要になる場合は、状態機械を増やす前に Policy の改善を検討する。

---

## 9. CI

CI は current exact head を test / clippy するだけとする。

CI result の正本は GitHub Check とする。

CI success を理由に次 worker を自動起動しない。

---

## 10. Codex Review

`aadw_review(PR)` は current exact head を Codex にレビューさせるだけとする。

Codex review の正本は GitHub Review / Review Thread とする。

review finding の意味判断、root-cause clustering、blocker / follow-up / unknown の分類は ChatGPT が行う。

---

## 11. Claude Fix

`aadw_fix(PR, instruction)` は ChatGPT が決めた修正 scope を Claude に渡して実行するだけとする。

Claude は judge ではない。

開始時と push 直前に current head を確認し、不一致なら push しない。

push 後は新 head として current-state discovery からやり直す。

---

## 12. GUI Validation

`aadw_gui(PR, scenario)` は ChatGPT が指定した focused scenario を実行するだけとする。

GUI worker は観測事実だけを記録する。

- `pass`: 期待した観測結果になった。
- `fail`: 期待した観測結果と異なった。
- `blocked`: 検証自体を成立させられなかった。

`fail` / `blocked` の意味判断は ChatGPT が行う。

---

## 13. Deterministic Gate

`aadw_gate(PR, expected_head)` は意味判断をしない。

ChatGPT が必要な意味判断を終えた後の事故防止 interlock とする。

確認するのは完全に客観的な条件だけに限定する。

例:

- current head SHA == expected head SHA。
- required CI checks == success。
- required GUI validation がある場合、その指定 run == success。
- GitHub が PR を mergeable と報告している。

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
- Gate は意味判断をしない。
- optional な Status Collector を導入する場合も repository mutation をしない。

---

## 17. 初期操作

初期 v2 で必要な実行操作は概念的に次程度とする。

```text
aadw_review(PR)
aadw_fix(PR, instruction)
aadw_gui(PR, scenario)
aadw_gate(PR, expected_head)
```

current-state discovery はまず既存 GitHub read 操作で行う。

`aadw_status(PR)` は必要性が実測された場合だけ追加する。

各操作は一入力・一仕事・一結果を原則とし、内部で次操作を自動起動しない。

---

## 18. 実装順序

### Phase 1: Commander Policy + 固定 current-state discovery

最初に実装する。

- Commander Policy を正本として使う。
- ChatGPT が GitHub connector の固定された少数の read 操作で current state を取得する。
- exact-head only を守る。
- 必要 evidence だけへ drill-down する。
- 何回の API 呼び出しが必要か、どこに重複や揺れがあるかを観測する。

### Phase 2: `aadw_review(PR)`

current exact head への Codex review 起動だけを実装する。

### Phase 3: `aadw_fix(PR, instruction)`

ChatGPT の修正指示を Claude に渡す単純 worker を実装する。

### Phase 4: `aadw_gui(PR, scenario)`

focused GUI scenario の実行と evidence 保存だけを実装する。

### Phase 5: `aadw_gate(PR, expected_head)`

客観的な merge safety check だけを実装する。

### Phase 6: current-state discovery の評価

複数 PR で運用し、current-state discovery が十分軽いか確認する。

既存 GitHub read 操作で十分なら、`aadw_status(PR)` は作らない。

実際に重複・高コスト・誤りが確認された場合だけ、最小の `aadw_status(PR)` を抽出する。

### Phase 7: 運用安定化

複数の実 PR で `Commander Policy + current facts → ChatGPT → next one action` のループを安定させる。

### Phase 8: GitHub Copilot Commander（将来拡張）

ChatGPT 主導が十分安定してから検討する。

---

## 19. 完成条件

初期 v2 は次を満たせば完成とする。

- ChatGPT が唯一の司令塔として全体を統括できる。
- Commander の主要入力が `Commander Policy + current facts / evidence` と明確である。
- 判断ルールの正本が `docs/aadw-command-policy.md` に一元化されている。
- ChatGPT が固定された少数の GitHub read 操作で current state を把握できる。
- 必要な evidence だけ追加取得できる。
- 各 worker は一つの仕事だけを行う。
- worker が勝手に次 worker を起動しない。
- GitHub 上の既存情報を正本として使い、同じ意味の AADW state を複製しない。
- exact-head 以外の evidence を current evidence として使わない。
- merge 前の客観的 safety check は Gate が行う。
- 不要な抽象化を先に作らない。

---

## 20. 最終原則

AADW v2 は、大きな自動状態機械ではない。

**Policy を持つ ChatGPT と、独立した単純な道具の組み合わせ**である。

```text
GitHub = 事実の正本
Commander Policy = 判断ルールの正本
ChatGPT = Policy + facts から複雑な判断を行う
Workers = 一つの仕事だけを行う
Gate = 最後の客観的安全装置
Optional abstraction = 実際に必要になってから追加する
```

新機能を追加するときは必ず次を確認する。

> 既存の事実を別の state として複製していないか。  
> この機能は一つの仕事だけをしているか。  
> 判断ルールを Policy 以外へ複製していないか。  
> AI が得意な判断を状態機械へ押し込んでいないか。  
> まだ実際には困っていない問題を先回りして抽象化していないか。

この原則に反する場合、機能追加より設計の単純化を優先する。
