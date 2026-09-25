# ADR-0031: AADW v3をChatGPTアプリ中心・Claude優先・CodeRabbitレビューで構成する

## ステータス

設計採用。実装・運用切替は未実施。初版はPR #334で採用し、本改訂はIssue #335に対応するPRのmergeで採用する。

現行運用は引き続き[ADR-0027](0027-aadw-v2-chatgpt-commander.md)と[Commander Policy](../aadw-command-policy.md)に従う。文書だけではJevへの委譲、実装担当の切替条件の拡大、CodeRabbitへのレビュー移行を有効にしない。本改訂PRのレビューも現行のCodex経路で行う。

## 日付と追跡先

2026-09-25。初版設計: [#332](https://github.com/hide212131/hane/issues/332) / PR #334。構成改訂: [#335](https://github.com/hide212131/hane/issues/335)。実装・評価・有効化: [#333](https://github.com/hide212131/hane/issues/333)。

## 背景と改訂理由

記事はCodexアプリを入口に、Luna/Solによる生成、Jevによる限定判断、テスト等による事実確認を分けている。初版はこの分離を独立したCodex App Server実行器として設計したが、利用者の求める運用との対応が不足していた。

利用者は、ChatGPTアプリ（利用者の呼称では旧Codexアプリ）を使い続け、実装は従来どおりGitHub ActionsからClaude Codeへ依頼し、継続できない条件を満たしたらCodexへ切り替え、コードレビューはCodeRabbitへ依頼する方針を明示した。本改訂はその方針を採る。アプリ内Commanderと実装workerのCodexを区別する。

記事の分業の考え方とTypeSafeの小さな意味判断は引き継ぐが、Claude優先・CodeRabbitレビューはHane独自の選択であり、記事のLuna→Solを標準経路にはしない。記事の価格・割合・confidenceの例をHaneの性能保証としない。出典と確認範囲は[v3設計書](../agentic-development-workflow-v3.md#sources)に記す。

## 決定

| 担当 | v3での役割 |
|---|---|
| ChatGPTアプリ内Commander | 利用者との対話、目的・範囲・予算・検証計画、実装指示の作成、GitHubの再観測、例外判断、最終受入・mergeを担当する |
| アプリから呼べる小さな連携機能 | Jev呼出し、回答検査、必要なGitHub情報の取得と承認済み操作への接続を行う。独立した司令塔や進行DBを作らない |
| GitHub Actions | 承認済み依頼を検証してworkerを起動し、変更を検査・反映する。CIは検証結果を残す。Actions自体に意味判断を埋め込まない |
| Claude Code / Codex | 通常はClaude Codeが実装する。許可条件を満たした場合だけCodexが引き継ぐ。同じbranchを同時に編集せず、次工程・権限を決めない |
| CodeRabbit | 指示されたPRのレビューを行う。自動修正、テスト生成、CI修正、競合解消、mergeは任せない |
| Jev | 許可範囲内の継続・修正・追加検証・担当変更候補・完了候補の意味判断を行う。コードや設計、権限、検証の事実は生成しない |
| 検証ツール / GUI Validator | 実行したテスト・build・lint・画面操作とその証拠を残す。実装担当の自己申告を事実の代用にしない |

Commander Policyを判断規則の正本とする。評価後の有効化PRで限定判断を委譲する範囲を同Policyに統合する。Jevを単なる参考情報に固定せず、評価・承認済みの通常判断はアプリ側の手順で採用できるようにする。ただし最終受入、重大問題の免除、範囲・設計変更はCommanderに残す。

## 実行方式と境界

開始点は利用者がChatGPTアプリへ作業を依頼すること。アプリのGitHub接続から承認済みPRコメントを投稿し、既存のActions経路でClaudeを動かす。結果をGitHubから読み直し、CI確認後の安定したheadへCodeRabbitレビューを依頼する。Jev呼出しはアプリから使える連携機能を通し、秘密情報はモデルの会話やworkerへ渡さない。

既存のClaude→Codex切替は`usage_or_rate_limit`に限定される。実装の行き詰まりによる切替は新しい委譲条件として別に検証・承認する。単なる認証失敗、head不一致、検証環境の故障、CodeRabbitやJevの障害でCodexへ切り替えない。

起動済みActionsとCodeRabbitの処理はGitHubで確認する。アプリ終了後も次の判断・依頼・mergeまで無人で継続すると保証しない。アプリ再開時はcurrent factsを取得し、既存jobが動いていれば重複起動しない。バックグラウンドの全工程制御は初期要件に含めない。

Codex CLI等は代替実装の実行方式であり、ChatGPTアプリを置き換えるものではない。独立したApp Server実行器、Hane製品UIへの組込み、独自常駐サービスは初期必須にしない。必要性が確認された場合だけ後続判断で追加する。

## 完了・安全・費用

Jevの`COMPLETE`は実装完了候補であってmerge権限ではない。CodeRabbitの完了や指摘ゼロも受入全体の保証ではない。current headと必要なbase、レビュー対象・完了・範囲、必須CI/GUI、未解決問題を確認してCommanderが最終受入する。confidenceは確率分布の集中度を要約するもので、正答率や権限に読み替えない。

通常workerと検証環境からGitHub書込認証・Jevキー・他のホスト秘密情報を分離する。既存の保護パス検査とexpected headの照合を維持する。CodeRabbitは運用上レビュー専任とするが、GitHub Appの権限まで読み取り専用とは仮定せず、導入時に外部送信・権限・契約を確認する。

GitHubを進行状態の正本とし、実行履歴は監査・費用にのみ使う。実費・サブスクリプション利用量・参考換算を区別し、不明値をゼロにしない。失敗、旧head、レビュー、Commanderを含む計測範囲を明示する。

## 選ばなかった案

| 案 | 採用しない理由 |
|---|---|
| Codex App Serverの独立実行器を最初に作り、Claude経路を外す | 利用者のアプリ中心・Claude優先の要求と異なり、既存Actions経路を活用しない |
| 設定欄に指示を書くだけでJev接続や自動継続が成立したことにする | 実際の呼出し、権限、結果取得を確認できない |
| CodeRabbitの指摘を無条件に実装命令とし、自動修正も任せる | 実装担当とレビュー担当が混ざり、範囲・権限や同時編集を制御できない |
| 一つのChoiceとconfidenceだけでmergeする | 検証不足、範囲逸脱、重大な問題、現在のコミットとの対応を確認できない |
| 汎用状態機械・別の進行DB・常駐司令塔を作る | アプリとGitHubとの二重管理を招く。必要な連携だけから始める |

## 実装と有効化

[実行計画](../aadw-v3-execution-plan.md)に従い、アプリ→既存Actions、CodeRabbit接続、Jev接続・比較評価、限定有効化を進める。v3自体のコードもClaudeへ実装依頼できるが、workflowやPolicy等の保護パス変更は通常workerの制限を外さず、明示した運用基盤PRで扱う。

CodeRabbit導入・契約・botからの依頼可否、アプリのJev呼出し、実推論、閾値、費用効果は未確認。実装Issue #333をopenのまま残す。有効化PRでCommander PolicyとAGENTSを同時更新するまでは、現行v2のClaude優先・利用上限時Codex・Codexレビューを継続する。
