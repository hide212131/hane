# ADR-0023: AI agent 開発を実装・レビュー・進行判断に分離する

## ステータス

Superseded — [ADR-0027](0027-aadw-v2-chatgpt-commander.md) により置換。

## 注記

この ADR は AADW v1 の設計判断を記録した履歴資料である。GitHub Copilot を judge とし、GitHub Actions が状態遷移を担う契約は現行 AADW v2 では使用しない。

現行の設計は [AADW v2 設計書](../agentic-development-workflow-v2.md) と [AADW Commander Policy](../aadw-command-policy.md) を参照する。

元の全文は v2 正本化直前の commit `2ef70feda07958bbdf87b6d62c70147f4ce9ccfd` にある [履歴版](https://github.com/hide212131/hane/blob/2ef70feda07958bbdf87b6d62c70147f4ce9ccfd/docs/adr/0023-ai-agent-development-workflow.md) に保存されている。
