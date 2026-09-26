# ADR-0031: アプリ中心のAADW v3で実装・レビュー・GUI検証・意味判断を分離する

## ステータス

設計採用。初版はPR #334で採用。本改訂はIssue [#336](https://github.com/hide212131/hane/issues/336)の文書PRで承認済み。Jevの意味判断はCommander Policyへ反映し、その他の実装・運用切替はIssue #333で追跡する。

このADRはv3導入時の設計判断を記録する。Jevの意味判断は、利用者指定による運用更新として[Commander Policy](../aadw-command-policy.md)と[AGENTS.md](../../AGENTS.md)に従い現在の通常作業で有効である。その他のv3移行状態は個別のIssue/PRで確認する。

## 日付と追跡先

2026-09-25初版、2026-09-26改訂。初期設計: [#332](https://github.com/hide212131/hane/issues/332)。改訂: #336。実装・評価・有効化: [#333](https://github.com/hide212131/hane/issues/333)。

## 背景と変更理由

記事はLuna/Solによる生成、Jevの限定判断、ツールによる検証を分ける。初版はその実装担当と独立したCodex App Server実行器を優先したが、その後、利用者からアプリ中心、Actions経由のClaude優先・条件付きCodex、CodeRabbitレビュー、v2同様の必要時GUIテストが指定された。

記事の「生成・判断・検証の分離」を引き継ぎ、担当製品と実行経路はHaneの指定に合わせる。記事をそのまま再現する設計ではない。費用削減率・confidence閾値・モデル利用量は未実測であり、保証値にしない。仕様の出典と確認範囲は[v3設計書](../agentic-development-workflow-v3.md#sources)にまとめる。

## 決定

| 役割 | 採用する構成 |
|---|---|
| 操作と指揮 | ChatGPTアプリ（旧Codexアプリ）内のCommander。Jevへcurrent factsを渡し、Jevの意味判断に沿って客観的な権限・head/base・必須証拠を確認し、一つのactionを実行する |
| 実装 | GitHub ActionsからClaude Codeへ依頼する。利用上限等の確認済み条件でCodexへ切り替える |
| レビュー | CodeRabbitを通常担当とし、依頼と完了・対象head・範囲を確認する。自動修正やmergeは担当させない |
| 意味判断 | JevのChoice/Noul/Score、利用可能な場合はTypeSafe function callingを使い、依頼解釈、範囲、作業分解、次のaction、継続・修正・検証・切替候補を扱う。具体的な運用範囲はCommander Policyが正本 |
| 事実確認 | CIと、必要に応じたGUI Validator。実行・観測に限定し、必須検証の免除や次工程は決めない |
| 連携 | 既存GitHub接続と最小限のアプリ用手順・補助ツール。別の常駐司令塔や状態DBを前提にしない |

Commanderの文章生成・設計と、Jevの狭い判断を分ける。独立質問は一括問い合わせできるが、同一リクエスト内で他の回答を参照させない。規則・権限・終了コード・SHA一致など、コードで確認できることはJevに聞かない。質問・補助ツール・workflowに別々の業務判断の正本を作らず、有効化時にCommander Policyへ集約する。

## 実装経路と切替の境界

既存のClaude手渡し・隔離・finalizerを再利用する。通常workerはコードとテストファイルを編集するだけで、シェル、テスト実行、pushの権限を追加しない。v3自体の通常コードもv2のClaudeに作らせられるが、workflow・認証・Policy等の保護パスは別の明示された運用基盤PRで扱う。

利用上限の既存fallbackと、実装停滞による新しい切替を区別する。後者はまずCommanderが試行と証拠を確認し、評価・条件承認後に限ってJevへ委譲する。認証・権限・head変更・原因不明をモデル切替で隠さない。切替前に旧writerを止め、同じ作業でClaude/Codexを往復・同時実行しない。

アプリの実行中セッションから依頼・結果確認を行う構成を初期範囲とする。閉じた後の無人継続を保証しない。既に起動したActions等の完了を再開時に確認し、重複発行しない。設定文だけでJev接続やモデル実指定が実現したとは報告しない。

## レビュー・GUI・完了

CodeRabbitの導入と権限、依頼受理、current-headのレビュー完了・範囲を有効化前に検証する。増分レビューと最終候補の全体レビューを分ける。指摘がないことだけで実行成功とせず、実装担当へ戻す修正と追加検証を区別する。通常のCodexレビューは切替完了までは維持し、移行後の二重レビューは必要時だけ行う。

GUIは[ADR-0024](0024-local-gui-validation.md)の既存Hosted/ローカル経路を継続する。Commanderが変更内容と受入条件に応じてfocused scenarioとOSを指定する。`/gui-validate head` / `/gui-validate merge`の起動、実行時のhead/base検査、procedureの対応範囲、run/artifactを確認する。起動・撮影だけで機能全体を検証済みとせず、macOSだけの成功をWindowsへ一般化しない。

GUIのfail/blocked/unknownは成功へ変換しない。既存不具合とするにはbaseline等の証拠を求める。CodeRabbit、CI、Jevのconfidenceで必須GUIを省略しない。GUIは観測専任とし、最終解釈・base-independent判断はCommanderが行う。

`COMPLETE`は実装完了候補でありmerge許可ではない。current headと必要なbase、CI/review/GUI、mergeabilityを最終確認し、expected headを指定してmergeする。

## 選ばなかった案

| 案 | 採用しない理由 |
|---|---|
| 独立App Server実行器を最初に作る | アプリ中心・Claude優先という利用者指定と導入順序が合わない。App Serverは代替経路で必要なら検討する |
| 記事どおりLuna/Solを通常実装担当に固定する | 記事の分離原則と、Haneの実装担当の選択は別の判断である |
| CodeRabbitの自動修正・一括解決で完了させる | 実装とレビューが混ざり、対象headと受入証拠の確認を省く原因になる |
| Jevの一つのChoiceとconfidenceだけでmergeする | 必須GUI・review・CI、権限、証拠不足の確認を代替できない |
| 汎用状態機械や無人常駐司令塔を追加する | GitHubとの二重管理や不要な基盤を増やす。初期のアプリ内手順と既存Actionsで検証する |

## 影響・移行

GitHubを進行状態の正本とし、監査・費用履歴と現在状態を区別する。アプリ・Claude・Codex・CodeRabbit・Jev・CI・GUIの費用とunknownを分け、失敗や中断も含めて評価する。

[実装・評価計画](../aadw-v3-execution-plan.md)はアプリから既存Actionsへの依頼を出発点に改訂する。今回の文書PRはコード・workflow・秘密情報・現行Policy・既存fallbackを変更せず、#333を閉じない。実サービス試験、質問と切替条件の評価、GUIの対象範囲確認、停止・再開試験を経て、別PRでPolicyとAGENTSを更新して限定有効化する。
