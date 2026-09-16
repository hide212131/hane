# ADR-0024: GUI 検証を Computer Use の承認から切り離す

## ステータス

採用。AADW orchestration に関する部分は [ADR-0027](0027-aadw-v2-chatgpt-commander.md) により改訂。

## 日付

2026-09-07（2026-09-09 hosted macOS 経路を検証、2026-09-16 AADW v2 と現行実行入口・evidence freshness に合わせて改訂）

## 背景

Hane の GUI acceptance criteria は、コードレビューや内部テストだけでは確認できない場合がある。一方、Computer Use の承認や self-hosted runner を必須条件にすると、GUI 検証そのものと実行基盤の問題が結び付きすぎる。

AADW v1 では GitHub-hosted macOS で Hane の build / launch / window discovery / capture / cleanup を実証した。ローカル Mac 用には `scripts/gui_validate.py` も実装した。その後 PR #150 で v1 の GUI validation workflow を停止・削除したため、現行 v2 で repository から直接起動できる明示的な GUI validation の入口はローカル CLI だけである。

Phase 3 の実運用では、同じ PR head でも target branch が進むと GUI scenario の前提が変わり得ることを確認した。そのため GUI evidence も、scenario が base-sensitive な場合は current target branch / merge context と対応付けて扱う。

## 決定

### Computer Use を必須にしない

事前に決めた focused scenario に従って Hane を起動・操作し、実際の結果を確認する。Computer Use は対話的な調査に使えてもよいが、別セッションの承認再利用や安全設定の緩和を前提にしない。

### 現在存在する実行入口を使う

現行 v2 では `scripts/gui_validate.py` を、ローカル Mac で launch / ready / window discovery / capture / cleanup を確認する最小の実行入口として扱う。この CLI が確認する範囲を超える acceptance criteria は、別の focused scenario と evidence が必要である。

現行 CLI は指定した commit snapshot を検証する。PR の current base や synthetic merge context を構築・記録するものではないため、base-sensitive な acceptance の current evidence として使う場合は、別の evidence で検証対象 context を証明できる必要がある。証明できない場合は `unknown` とし、head-only の `pass` を current PR context の `pass` に読み替えない。

一方、base の変更が scenario に影響しないと Commander が current facts と scenario の性質から判断できる場合は、その根拠を GitHub 上に残したうえで head snapshot の evidence を利用できる。GUI worker 自身が base-independent かどうかを決めない。

GitHub-hosted macOS の技術的な検証実績は残るが、現在は手動 dispatch 可能な hosted GUI validation workflow が存在しない。そのため hosted runner を現行の利用可能な経路としては扱わない。

複数 PR の運用で、ローカル実行だけでは必要な GUI evidence を繰り返し取得できないことが確認された場合に限り、hosted entrypoint の再導入を独立して検討する。v1 の orchestration 全体を理由なく復活させない。

### 実行と判断を分ける

GUI validator は対象 head のアプリを検証し、観測事実と evidence を返す。product code の変更、次 action の決定、merge は行わない。

検証対象は対象 head と focused scenario を明示する。head が変わったら旧 head の結果を current evidence として使わない。base-sensitive な scenario では、実際に検証した current target branch / merge context が evidence から確認できることも必要とする。runner が `pass` / `fail` / `blocked` のような結果を出しても、その意味は ChatGPT Commander が current facts と [AADW Commander Policy](../aadw-command-policy.md) に基づいて判断する。

必要な GUI evidence を現在使える手段で取得できない場合は product failure と推測せず `unknown` とする。ただし `unknown` のまま merge 方向へ進めない。

### 権限を小さくする

検証対象を build / execute する環境には不要な書き込み認証情報を渡さない。画像など GitHub に自然な置き場所がない evidence だけを必要最小限の artifact として保存する。

## 結果

- GUI validation を AADW の state machine から切り離して必要なときだけ実行できる。
- Computer Use が利用できないことを、そのまま product failure と扱わずに済む。
- 現在存在しない hosted entrypoint を利用可能だと誤認しない。
- head と、必要な場合は current base / merge context に対応する focused evidence を使いながら、GUI worker の観測と Commander の意味判断を分離できる。
- hosted entrypoint の再導入は、実運用で必要性が確認された場合だけ検討する。

## AADW v2 により置き換えた部分

v1 の GUI requirement workflow、Copilot final judge、merge gate、receipt / state transition の契約は現行では使わない。これらを含む旧全文は [`docs/history/aadw-v1/adr/0024-local-gui-validation.md`](../history/aadw-v1/adr/0024-local-gui-validation.md) に保存する。

## 現行の参照先

- [AADW v2 設計書](../agentic-development-workflow-v2.md)
- [AADW Commander Policy](../aadw-command-policy.md)
- [Local GUI validation](../local-gui-validation.md)
- [CLI 操作説明](../local-gui-validate-cli.md)