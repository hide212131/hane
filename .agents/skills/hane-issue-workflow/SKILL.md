---
name: hane-issue-workflow
description: HaneのIssueを進めるときに、Commanderの初回PR準備、Claudeへの正規依頼、実行結果の確認を行う。
---

# Hane Issue workflow

Hane repositoryでIssue実現やClaude実装を依頼されたときに使う。着手時にcurrent main、対象Issue、関連PR、同じ対象の実行中runを確認し、`AGENTS.md` と [Commander Policy](../../../docs/aadw-command-policy.md) を読む。

実装依頼の正本は [Claude実行手順](../../../docs/aadw-claude-execution.md)。Issueへ `/implement` を投稿する経路は使わない。

- 同じIssue・フェーズの実装PRがあればcurrent headを取り直して再利用する。
- 実装PRがなければ、Commanderが文書だけの準備commitを置いた実装用Draft PRを作成し、製品コードとテストはClaudeに依頼する。準備PRの作成に、製品コードをCodexが直接実装する許可を求めない。
- 設計用PRを無断で実装用に転用しない。Claudeには設計のURLだけでなく、対象branchから読めるパスと必要な受入条件を渡す。
- `@claude` と `AADW_COMMANDER_HANDOFF_V3` を含む依頼は、実装用PRのConversationへ一度だけ投稿する。投稿前にcurrent exact head、same-repository/open状態、依頼者のwrite権限を確認する。
- コメント投稿やrun生成だけで受理済みとしない。`prepare`成功を受理の証拠にし、`skipped`や失敗は未開始として原因を確認する。Claude起動、Sonnet 5.5使用、patch/finalizer、current-head検証はそれぞれ別に確認する。
- 原因診断は実装依頼と分ける。`usage_or_rate_limit`以外の失敗をCodex fallbackへ送らず、通常workerの保護パス制限を緩めない。

Issue/PRの状態は証拠を添えて日本語で区別する。設計文書やDraft PRを作っただけで実装完了と報告しない。
