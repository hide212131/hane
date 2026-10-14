# AADW v3統一の実装指示書

対象: Issue #486

## 状態

2026-10-13時点の調査・設計と実装範囲を記録する。基準mainは `af981629011f04f68b25c8b58e476eb3eeffa624`。このPRはIssue #486で明示された運用基盤専用の変更候補であり、保護パス拒否や通常workerの権限は緩めない。

初回の準備時にGitHub接続ツールへ要求した `.github/scripts/aadw_action_contract.py` の更新は、安全確認でブロックされており適用されなかった。この事実は当時の状態の記録である。利用者が本Issueの完遂を明示した後、権限を持つ担当がこの運用基盤専用PR内で変更を実装している。通常製品workerへ送ったり、保護パス拒否を迂回・解除したりしていない。

要求・範囲・完了条件の正本は [Issue #486](https://github.com/hide212131/hane/issues/486)。利用者の要求は、現行契約から旧世代の依存を完全に除くこと、Sonnet 5.5へ揃えること、Claudeへの初回実装指示で迷わないことである。初回実装の操作手順は [aadw-claude-execution.md](aadw-claude-execution.md) にまとめた。

## 1. 最初に確認するもの

current main、#486、本設計PR、`.github/workflows/claude-fix.yml`、`.github/workflows/aadw-diagnose.yml`、`.github/workflows/codex-usage-limit-fallback.yml`、`.github/scripts/aadw_action_contract.py`、関連する `.github/tests/`、AGENTS、CLAUDE、Commander Policy、ADR-0031を読む。進行中の同領域PRがないか調べる。

引用された `hane-issue-workflow` は、基準SHAの `.agents/skills/` には見つからなかった。同ディレクトリで確認できたのは `hane-gui-test` である。この実行環境では読み込まれたskillの実パスと内容を確認し、repositoryの入口を参照するよう更新した。別PCのユーザー領域まで更新済みとは判断しない。

本件は利用者が明示的に依頼した運用基盤の変更である。製品コードを変更する通常workerのfinalizerは `.github/`、AGENTS、CLAUDE、Commander Policy等を拒否する。そこへ本件の修正を依頼して失敗させたり、拒否条件を削除したりしない。通常の製品実装は引き続きClaudeへ任せる。

## 2. 実行契約を同時に変更する

以下は別々のリリースに分けず、一つの整合した候補で変更・検証する。

| ファイル | 変更 | 維持するもの |
| --- | --- | --- |
| `.github/workflows/claude-fix.yml` | 正規の実装マーカーを `AADW_COMMANDER_HANDOFF_V3` に変更。モデルを `claude-sonnet-5-5` に変更。初回実装も対象だとpromptに明記する | trusted actor、same-repository、open/paused、開始直前とpush直前のexact head、checkpointの対象一致、Read/Edit/Write/Glob/Grepだけのworker、保護パス拒否、trusted finalizer、別jobの検証 |
| `.github/scripts/aadw_action_contract.py` | `MARKERS['implement']` を正規マーカーへ変更。異なる世代の実装マーカーが混在する要求を拒否する | 各fieldの一意性、SHA検査、initial/product/testの区別、diagnoseとの混在拒否、重複実装要求の拒否、unknownを製品実装へ流さない |
| `.github/workflows/codex-usage-limit-fallback.yml` | source handoffの検査を正規マーカーに一致させる | usage_or_rate_limitだけ、元run/attempt/actor、checkpoint、同一対象head、二重writer防止、認証失敗等との区別 |
| `.github/workflows/aadw-diagnose.yml` | モデルを `claude-sonnet-5-5` に変更 | 診断専用、repository変更とpush禁止、source差分ゼロ、診断結果のschemaと対象head検査 |
| `.github/tests/test_codex_usage_limit_fallback.py` と契約の関連テスト | 正常系の期待値を正規マーカーへ更新し、旧入力・混在・重複の拒否を追加 | 既存の権限・鮮度・checkpoint・空patch・失敗分類のテスト |
| Claude Code actionとモデル検査 | Sonnet 5.5を含むClaude Code CLIを固定するaction revisionへ更新。実行結果のprimary modelとmodel usageを検査し、未確認・不一致ならpushを止める | action revisionはfull SHAで固定し、raw実行ログやusage値をjob summaryへ出さない |
| `.agents/skills/hane-issue-workflow/` | repository管理の薄いskill入口を追加し、normal workerのpatchから保護する | 実行markerや長い判断ルールはPolicyとrunbookを正とする |

実装parserで、例えば `AADW_COMMANDER_HANDOFF_` で始まる単独行が正規マーカー以外だったら拒否する。正規マーカーを一行含むだけで、旧マーカーと混在した入力を通してはいけない。現在の診断用 `AADW_DIAGNOSTIC_REQUEST_V1` は診断要求の形式番号であり、今回の実装マーカー統一とは別である。形式番号を無関係に書き換えない。

旧要求を新規受付する互換aliasは設けない。旧runの再実行で移行後の契約を回避しない。移行時に実行中のworkerや未反映checkpointがある場合は、その対象headと結果を確認し、二重writerにならないよう処理する。過去履歴を新形式で実行されたように書き換えない。

モデル名だけでなく、Anthropic actionの固定版がSonnet 5.5を実際に起動できることを確認する。旧モデルへ黙って戻す設定を加えない。認証方式、secret、利用上限fallbackの条件は変更しない。モデル不一致やprovider errorは失敗として示す。

## 3. 文書の整理範囲

基準SHAのコード検索で旧世代への参照を確認した主な文書は以下である。機械的な一括置換で過去の検証結果を改ざんせず、現行契約、履歴、AADWとは無関係なバージョン名を区別する。

- 現行の入口・正本: `AGENTS.md`、`CLAUDE.md`、`docs/aadw-command-policy.md`、`docs/agentic-development-workflow-v3.md`、`docs/aadw-v3-execution-plan.md`、`docs/adr/0031-aadw-v3-jev-bounded-execution.md`。
- 専用旧文書: `docs/agentic-development-workflow-v2.md`、`docs/adr/0027-aadw-v2-chatgpt-commander.md`。必要な現行の制約をv3側へ移したうえで、本PRで現行treeから削除した。
- 関連する設計・操作手順: `docs/adr/README.md`、ADR-0023、ADR-0024、ADR-0025、ADR-0026、ADR-0028、`docs/local-gui-validation.md`、`docs/local-gui-validate-cli.md`、`docs/refactor-plan.md`、`docs/refactor-execution-plan.md`。
- 履歴資料: `docs/final-judgement.md`、`docs/claude-retry-state.md`、`docs/agentic-implementation-recovery.md`、`docs/agentic-development-workflow.md`、`docs/aadw-v3-v3-1-validation.md`。現行版を誤認させる案内を除く。監査に必要な記録は当時のcommitへ明示的に参照させるか、旧運用の文書自体を現行treeから取り除き、削除された現行パスへの生きたリンクを残さない。

`v2`という文字列をrepository全体から無差別に削除しない。依存ライブラリ、API形式、製品機能内の識別子、実測ログの値は本件と無関係なら保持する。AADWの旧受付を拒否する否定テストは例外として区別するが、正規テンプレートや稼働コードのaliasを「テスト用」の名で残さない。

v3設計書は旧設計書を読まなくても実装・レビュー・GUI・fallback・mergeの担当と条件が分かる状態にする。Commander Policyの意味判断をworkflowやworkerに重複実装しない。

## 4. 初回PRとskillを揃える

repository内に `.agents/skills/hane-issue-workflow/SKILL.md` を置き、current default branchのCommander Policyと実行手順を読む薄い入口にする。モデル・マーカー・長い判断ルールをskillへ重複して保存しない。

AGENTSから実行手順とskillを参照する。Commanderは製品コードを書かないが、文書だけの準備commitと実装用Draft PRは作成できることを明記する。運用基盤専用PRは利用者が指定した権限の下で扱うものとして、製品実装禁止と混同しない。

CLAUDEの説明は実際のworker権限に合わせる。Actions workerにshellがないのに `gh`、git、テスト実行、commit/pushを要求しない。開始直前とpush直前のhead確認はtrusted workflow/finalizerが行う。workerは指定された作業treeの変更と未検証事項の報告を担当する。

ローカルの旧skillがある場合は、担当がその配置を確認してrepositoryの入口を読む構成へ更新する。未接続のPCまで更新済みとは言わない。設計書をURLだけで渡して、ネットワーク操作のできないworkerに読ませようとしない。

## 5. テストと受入

最低限、次の組合せを確認する。

| 対象 | 確認内容 |
| --- | --- |
| 実装要求 | 正規マーカー、initial/product/testの受付、単独旧マーカー・世代混在・marker重複・field重複の拒否 |
| 誤実装の防止 | measurement/environment/unknownの拒否、diagnoseとの混在拒否 |
| 既存のガード | stale head、fork、closed/paused、権限不足、同一要求の再送、checkpoint不一致、空patch、保護パス変更の拒否 |
| 設定の一致 | 全ての有効なClaude呼出しのmodel、実行結果のprimary model/usage、実装受付とfallbackのsource契約、文書テンプレートとparserの一致 |
| 文書 | 削除文書への生きた相対リンクなし、現行正本に旧世代を参照しないと分からない記述なし |

Pythonテストの起動方法とworkflow構文検査はcurrent `ci.yml` に合わせる。検査を省略したのに成功と書かない。製品Rustコードを変更しない本件で製品GUIを一律の完了条件にしないが、required CIとcurrent-headのCodeRabbit full reviewは既存Policyに従う。

全ての変更を同じ候補headで確認し、expected head、base context、mergeabilityを読み直してからmergeする。ここまでのmergeは設定反映であり、実際のClaude起動確認とは区別する。

## 6. main反映後の小さい実行確認

文書の限定変更を対象にした確認用Issue/実装用Draft PRを用意し、手順書どおりに準備と正規依頼を行う。Issueへのコマンド送信は使わない。既存の製品Issue #304や設計PR #476を動作確認のために書き換えない。

投稿コメント、PR、target head、受理runを結び付け、prepare成功、Claude起動モデル、patch、finalizer、変更後head、検証結果を確認する。`skipped`は未開始、モデル表示が得られなければ指定確認のみ、検証失敗なら未完了とする。設定上の文字列変更だけでSonnet 5.5の実運転成功と報告しない。

確認用PRは結果を明記し、不要ならcloseする。設定変更のPRをmergeしても、実run確認が残る場合は#486をopenに保つ。
