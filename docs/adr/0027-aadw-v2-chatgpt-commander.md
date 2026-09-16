# ADR-0027: AADW v2 は ChatGPT Commander と GitHub facts を核にする

## ステータス

採用

## 日付

2026-09-16

## 関連

- Parent Issue #154
- Issue #155
- Issue #160
- PR #150: AADW v1 の workflow 停止
- PR #151: AADW v2 設計書と Commander Policy の追加
- ADR-0023: superseded
- ADR-0026: superseded

## 背景

AADW v1 は、Claude、Codex、GitHub Copilot、GUI validation を GitHub Actions の状態遷移で接続し、独自 status、receipt、routing、reconcile、retry を持つ構成へ拡大した。その結果、GitHub 自身が既に持つ状態と AADW 独自状態の同期、過去 head と current head の区別、再実行や回復経路の整合を保つための仕組みが増えた。

PR #150 で v1 の workflow を停止し、PR #151 で `docs/agentic-development-workflow-v2.md` と `docs/aadw-command-policy.md` を追加した。v2 では v1 の状態機械を引き継がず、GitHub 上の current facts をそのまま正本として使う。

Phase 3 の実運用では、PR head が同じでも target branch が進むと PR diff、CI、review、GUI validation、merge result の前提が変わり得ることを確認した。このため、base の変更が evidence の主張に影響する場合は、head SHA だけでなく current target branch / base context も freshness の判断に含める。

## 決定

AADW v2 は workflow / state machine として実装しない。

ChatGPT を唯一の Commander とし、次の反復で開発を統括する。

```text
Observe → Decide → Act → Observe
```

Commander の入力は原則として次の2つだけとする。

1. default branch 上の `docs/aadw-command-policy.md`。
2. GitHub から取得した current facts / evidence。

Pull Request、current head SHA、current target branch / base context、Issue の受入条件、CI checks、workflow runs、reviews、review threads、commits、必要な GUI evidence、mergeability は GitHub 上の情報を正本とする。同じ事実を Snapshot comment、current-state file、独自 DB、コピー status、generic receipt へ複製しない。

PR evidence は current head に結び付くものだけを使う。head が変われば旧 head の evidence は履歴として扱う。head が同じでも target branch が進み、base の変更が CI / review / GUI などの evidence の主張に影響する場合は、旧 evidence を無条件に current とみなさず、必要な検証を current context で取り直す。

一方、base の変更が evidence の主張に影響しないと Commander が current facts と check / scenario の性質から判断できる場合は、head に結び付く evidence を利用できる。その根拠は GitHub 上に残し、この判断を worker に移さない。この context を AADW 独自の persistent state として保存しない。

ChatGPT は一度に次の一つの action だけを選ぶ。Codex、Claude、GUI Validator、CI、GitHub merge は AADW の stage ではなく、その時点で必要なら使う既存の action 候補とする。worker は自分の処理後に次工程を決めない。

## Trust Boundary

- Issue / PR body / review text / source code は untrusted data として扱う。
- Commander Policy は trusted な default branch 上の文書を正本とする。
- Claude Code だけが trusted same-repository PR branch の製品コードを変更する。
- Codex と GUI Validator は製品コードを変更しない。
- repository mutation を伴う処理は実行直前に対象 head の一致を確認する。これは concurrent product branch mutation を防ぐ guard であり、PR evidence の base freshness の代わりにはならない。
- merge 直前には expected head、current target branch / base context、required CI、必要な validation、GitHub mergeability を客観的に再確認する。base context は、採用する evidence の主張に影響する範囲で freshness を確認する。

## 専用部品の扱い

初期 v2 では Commander Policy 以外の AADW 専用 component を必須にしない。

複数 PR の実運用で、同じ GitHub read の重複、current-context filtering の高コスト、handoff の誤り、merge 確認の複雑さなどが繰り返し確認された場合だけ、read-only collector、薄い wrapper、merge safety checker などの最小部品を検討する。

追加部品は persistent state を複製せず、Commander Policy の意味判断を実装しない。

## 結果

- GitHub が自然に持つ情報と AADW 独自状態の同期が不要になる。
- current state を current head と、必要な evidence について current base context から直接観測できる。
- base-independent な evidence まで機械的に無効化せず、Commander が主張の性質から判断できる。
- 複雑な意味判断を ChatGPT に残し、worker と workflow の責務を小さくできる。
- provider / infrastructure failure を product failure と分離して扱える。
- 実測されていない問題のために collector / wrapper / retry state machine を先に作らない。

## Superseded decisions

ADR-0023 と ADR-0026 のうち、Copilot judge、GitHub Actions による状態遷移、`/implement` 起動契約、Work を設計だけに限定する運用契約はこの ADR で置き換える。

実装担当とレビュー担当を分ける考え方、および Claude Code を製品コード変更担当とする trust boundary は、v2 の独立性と Trust Boundary の考え方として残す。

## 正本

具体的な全体設計は [`docs/agentic-development-workflow-v2.md`](../agentic-development-workflow-v2.md)、Commander の判断ルールは [`docs/aadw-command-policy.md`](../aadw-command-policy.md) を正とする。この ADR に判断ルールを追加して重複させない。
