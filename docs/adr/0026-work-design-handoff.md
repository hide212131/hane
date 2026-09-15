# ADR-0026: Work を上流の planner/designer とし、Claude Code への明示的な handoff を定義する

## ステータス

Superseded — [ADR-0027](0027-aadw-v2-chatgpt-commander.md) により置換。

## 注記

この ADR は AADW v1 の設計判断を記録した履歴資料である。Work を設計だけに限定する契約、`/implement` による起動、Copilot routing / final judge への handoff は現行 AADW v2 では使用しない。

現行 v2 では ChatGPT が唯一の Commander として current GitHub facts を読み、Commander Policy に従って次の一つの action を選ぶ。Claude Code が trusted same-repository PR branch の製品コード変更を担うという trust boundary は v2 にも引き継ぐ。

現行の設計は [AADW v2 設計書](../agentic-development-workflow-v2.md) と [AADW Commander Policy](../aadw-command-policy.md) を参照する。

元の全文は [`docs/history/aadw-v1/adr/0026-work-design-handoff.md`](../history/aadw-v1/adr/0026-work-design-handoff.md) に保存する。
