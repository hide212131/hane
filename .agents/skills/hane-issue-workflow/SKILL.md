---
name: hane-issue-workflow
description: HaneのIssue実現や複数PRの進行調整に使う。Commanderの初回PR準備、Claudeへの正規依頼、実行結果の確認と共有GUI環境の利用調整を行う。
---

# Hane Issue workflow

Hane repositoryでIssue実現、Claude実装、複数PRの進行調整を依頼されたときに使う。着手時にcurrent main、対象Issue、関連PR、同じ対象の実行中runを確認し、`AGENTS.md` と [Commander Policy](../../../docs/aadw-command-policy.md) を読む。

## 実装依頼と実行結果の確認

実装依頼の正本は [Claude実行手順](../../../docs/aadw-claude-execution.md)。Issueへ `/implement` を投稿する経路は使わない。

- 同じIssue・フェーズの実装PRがあればcurrent headを取り直して再利用する。
- 実装PRがなければ、Commanderが文書だけの準備commitを置いた実装用Draft PRを作成し、製品コードとテストはClaudeに依頼する。準備PRの作成に、製品コードをCodexが直接実装する許可を求めない。
- 設計用PRを無断で実装用に転用しない。Claudeには設計のURLだけでなく、対象branchから読めるパスと必要な受入条件を渡す。
- `@claude` と `AADW_COMMANDER_HANDOFF_V3` を含む依頼は、実装用PRのConversationへ一度だけ投稿する。投稿前にcurrent exact head、same-repository/open状態、依頼者のwrite権限を確認する。
- コメント投稿やrun生成だけで受理済みとしない。`prepare`成功を受理の証拠にし、`skipped`や失敗は未開始として原因を確認する。Claude起動、Sonnet 5.5使用、patch/finalizer、current-head検証はそれぞれ別に確認する。
- 原因診断は実装依頼と分ける。`usage_or_rate_limit`以外の失敗をCodexの実装fallbackへ送らず、通常workerの保護パス制限を緩めない。CodeRabbit最終reviewの未完了時に限るCodexのreview fallbackは、[Commander Policy](../../../docs/aadw-command-policy.md#coderabbit最終レビューのcodexフォールバック)に従い、実装fallbackと混同しない。

Issue/PRの状態は証拠を添えて日本語で区別する。設計文書やDraft PRを作っただけで実装完了と報告しない。

## 複数PRの進行調整

依存関係・base同期・修正の集約と検証順序の判断は、Commander Policyの[§2.8](../../../docs/aadw-command-policy.md#28-base-synchronization-と-product-fix-を分離する)と[§4.6](../../../docs/aadw-command-policy.md#46-修正の収束と高コスト検証)に従う。ここでは既存担当と共有環境の使い方を調整する。

- 既存の担当チャットと実行中runを照合し、PRごとの担当・次の作業・残条件を把握する。優先PRに依存しないPRは、その完了待ちにせず各担当が進める。優先度は主に共有資源の利用順に反映し、変更が重なるPRは上記Policyに従って順序を調整する。
- 実装・ビルド・画面を使わないテスト・レビューはPR別worktreeで並行して進め、同じPRのwriterを重ねない。共有マシンのCPU・メモリに余力がなければ重い処理の同時数を調整する。worktreeはソースの分離であり、アプリ設定・入力・クリップボードの分離ではない。
- GUI操作担当と利用順は、OS名だけでなく実際に操作するデスクトップごとに調整する。同じデスクトップの操作は一件ずつとし、MacとWindowsが独立して直接操作できる場合はそれぞれ並行できる。担当チャットへの送信は利用者の許可範囲で行い、受理済みの依頼を重複送信しない。
- GUI担当へ渡す前に、対象headと必要なbase context、候補バイナリ、隔離state・テスト文書、focused scenario、既存evidenceと未確認項目を揃える。起動時の識別、Windows側への直接依頼、既存の排他、後片付けは[GUIテストの診断・復旧手順](../../../docs/gui-test-recovery.md)に従う。対象プロセスの終了と排他解放を確認して次のPRへ渡す。
- GUI待ちの間も、そのPRと他PRで画面に触れず進められる作業を選ぶ。CIの未起動・承認待ち・失敗は通常の実行待ちと区別し、原因診断や必要な復旧を次の作業として扱う。
- 既存チャットの進捗報告には、PRごとの残条件（修正待ち・CI待ち・レビュー待ち・GUI待ちなど）と次の担当、デスクトップごとのGUI利用順を簡潔に示す。これは報告時点の整理であり、新しい永続状態・専用キュー・Gateを作らない。次のactionではGitHubと担当の実行状況を再観測し、受入・mergeはCommander Policyで判断する。
