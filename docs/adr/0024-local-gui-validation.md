# ADR-0024: GUI 検証を Computer Use の承認から切り離す

## ステータス

採用。AADW orchestration に関する部分は [ADR-0027](0027-aadw-v2-chatgpt-commander.md) により改訂。

## 日付

2026-09-07（2026-09-09 hosted macOS 経路を検証、2026-09-16 AADW v2 と evidence freshness に合わせて改訂、2026-09-17 Hosted GUI Validation の comment command 入口を追加）

## 背景

Hane の GUI acceptance criteria は、コードレビューや内部テストだけでは確認できない場合がある。一方、Computer Use の承認や self-hosted runner を必須条件にすると、GUI 検証そのものと実行基盤の問題が結び付きすぎる。

AADW v1 では GitHub-hosted macOS で Hane の build / launch / window discovery / capture / cleanup を実証した。ローカル Mac 用には `scripts/gui_validate.py` も実装した。その後 PR #150 で v1 の GUI validation workflow を停止・削除したが、Phase 3 の実運用で hosted runner が必要になったため、Issue #162 で AADW v2 用の薄い `AADW Hosted GUI Validation` を再導入した。これは v1 の状態機械を戻すものではなく、current PR context に対する観測 action だけを提供する。

Phase 3 の実運用では、同じ PR head でも target branch が進むと GUI scenario の前提が変わり得ることを確認した。そのため GUI evidence も、scenario が base-sensitive な場合は current target branch / merge context と対応付けて扱う。

Hosted GUI Validation の初期入口は manual `workflow_dispatch` で、PR番号、exact head SHA、execution context、procedure を人が入力していた。PR番号、head SHA、current base、procedure は trusted GitHub facts / trusted default branch から機械的に決められるため、通常利用では人に転記させない方が誤入力を減らせる。一方、値の入力を省くことと execution-time validation を省くことは別であり、安全確認は維持する必要がある。

## 決定

### Computer Use を必須にしない

事前に決めた focused scenario に従って Hane を起動・操作し、実際の結果を確認する。Computer Use は対話的な調査に使えてもよいが、別セッションの承認再利用や安全設定の緩和を前提にしない。

### 現在存在する実行入口を使う

現行 v2 では GitHub-hosted macOS の `AADW Hosted GUI Validation` と、ローカル Mac の `scripts/gui_validate.py` を用途に応じて使う。

通常の Hosted GUI Validation は Pull Request Conversation に単独の `/gui-validate head` または `/gui-validate merge` を投稿して起動する。comment router は authorization と request construction に限定し、コメントイベントの PR 番号、GitHub API から取得した current head、current target branch、trusted default branch の procedure を使って Hosted GUI Validation を dispatch する。文章中の部分一致や未知の引数では起動しない。

comment router はコメント投稿者が repository に `write` / `maintain` / `admin` のいずれかを持つこと、PR が open かつ non-draft であること、same-repository PR であることを確認する。外部 fork のコードをこの trusted runner に流さない。

router が current head と current target branch SHA を取得しても、それを execution-time truth として固定しない。Hosted GUI Validation 側で元コメント、投稿者権限、open/non-draft、same-repository、exact head、current base、trusted procedure を再取得・再検証する。`merge` では GitHub の current PR merge ref を checkout し、第1 parent が current base、第2 parent が requested exact head であることも確認する。router の依頼後に head または base が動いた場合は fail closed とし、暗黙に最新 context へ置き換えない。

同じ GUI validation を短時間に繰り返して runner を消費しないため、router は PR、exact head、router が観測した current base、execution context、procedure を request identity として扱い、同一 identity の queued / in-progress / success run がある場合は再 dispatch しない。base を identity に含めるのは、head が同じまま target branch が進んだとき、古い base に対する成功 evidence を自動で current とみなさないためである。

受理した依頼は PR Conversation に対象 head / base、execution context、procedure、workflow run URL を通知する。ただし通知コメントは evidence の正本ではなく、workflow run、artifact、current PR facts を確認する。

障害時や管理者向けの fallback として manual `workflow_dispatch` は残す。manual fallback でも trusted actor、open/non-draft、same-repository、exact-head、current-base、merge-ref parent、procedure の検証を省略しない。

ローカル CLI は指定した commit snapshot を検証する。PR の current base や synthetic merge context を構築・記録するものではないため、base-sensitive な acceptance の current evidence として使う場合は、別の evidence で検証対象 context を証明できる必要がある。証明できない場合は `unknown` とし、head-only の `pass` を current PR context の `pass` に読み替えない。

一方、base の変更が scenario に影響しないと Commander が current facts と scenario の性質から判断できる場合は、その根拠を GitHub 上に残したうえで head snapshot の evidence を利用できる。GUI worker 自身が base-independent かどうかを決めない。

### 実行と判断を分ける

GUI validator は対象 context のアプリを検証し、観測事実と evidence を返す。product code の変更、次 action の決定、merge は行わない。

検証対象は対象 head と focused scenario を明示する。head が変わったら旧 head の結果を current evidence として使わない。base-sensitive な scenario では、実際に検証した current target branch / merge context が evidence から確認できることも必要とする。runner が `pass` / `fail` / `blocked` のような結果を出しても、その意味は ChatGPT Commander が current facts と [AADW Commander Policy](../aadw-command-policy.md) に基づいて判断する。

必要な GUI evidence を現在使える手段で取得できない場合は product failure と推測せず `unknown` とする。ただし `unknown` のまま merge 方向へ進めない。

### 権限を小さくする

comment router にだけ dispatch と PR Conversation への通知に必要な権限を与える。GUI runner 自体には product code を変更する権限を与えず、対象コードを build / execute する環境へ不要な書き込み認証情報を渡さない。画像など GitHub に自然な置き場所がない evidence だけを必要最小限の artifact として保存する。

## 結果

- GUI validation を AADW の state machine から切り離して必要なときだけ実行できる。
- Computer Use が利用できないことを、そのまま product failure と扱わずに済む。
- 通常利用では人が PR番号、head SHA、base SHA、procedure を転記せず、PR Conversation から明示的に起動できる。
- router と GUI runner の二段階で current context を確認し、head / base が動いた stale request を fail closed にできる。
- head と、必要な場合は current base / merge context に対応する focused evidence を使いながら、GUI worker の観測と Commander の意味判断を分離できる。
- workflow run / artifact / current GitHub facts を正本とし、AADW 専用の persistent state を追加しない。

## AADW v2 により置き換えた部分

v1 の GUI requirement workflow、Copilot final judge、merge gate、receipt / state transition の契約は現行では使わない。これらを含む旧全文は [`docs/history/aadw-v1/adr/0024-local-gui-validation.md`](../history/aadw-v1/adr/0024-local-gui-validation.md) に保存する。

## 現行の参照先

- [AADW v2 設計書](../agentic-development-workflow-v2.md)
- [AADW Commander Policy](../aadw-command-policy.md)
- [Local GUI validation](../local-gui-validation.md)
- [CLI 操作説明](../local-gui-validate-cli.md)