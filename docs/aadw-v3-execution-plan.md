# AADW v3 実装・評価・移行計画

運用更新: Jevの意味判断は [Commander Policy](aadw-command-policy.md) に従って通常作業へ導入済み。利用者指定により、費用・性能の追加比較や閾値校正は有効化条件にしない。以下の計画はJev判断以外の移行作業と過去の受入条件を記録する。

## 適用範囲と改訂

設計Issue [#332](https://github.com/hide212131/hane/issues/332) / PR #334を、利用者指定により[#336](https://github.com/hide212131/hane/issues/336)で改訂する。実装は[#333](https://github.com/hide212131/hane/issues/333)。[ADR-0031](adr/0031-aadw-v3-jev-bounded-execution.md)と[v3設計書](agentic-development-workflow-v3.md)を参照する。

**入口はChatGPTアプリ、実装はActions経由のClaude優先・条件付きCodex、通常レビューはCodeRabbit、GUIはv2と同じく必要時に実行する。** 独立したCodex App Server実行器を作ることからは始めない。

文書のmergeでは稼働を切り替えない。[Commander Policy](aadw-command-policy.md)が現行運用の正本である。以下の段階は実装計画であって独自のworkflow stateではない。進捗はGitHubのIssue/PRと証拠で確認する。

## 1. V3-1: アプリから既存の実装経路を使う

アプリ内Commanderがcurrent main/PR/Issue/Policyを読み、現在のClaude用PRコメントを投稿してActionsの受理・実行・結果を確認できることを検証する。Issueだけの場合は作業branch/PRと対象headを用意してから手渡す。投稿actorの権限と同一対象の重複起動抑制を確認する。

Claude workerのRead/Edit/Write/Glob/Grep限定と別finalizerを維持する。コード・テストファイルの編集、patchと対象headの対応、push後のcurrent-head CIを確認し、workerがテスト実行できるという前提を置かない。取得可能な担当・モデル設定・バージョン・run/attempt・利用量と未計測項目を残す。

既存の利用上限fallbackはsource run/attempt・checkpoint・依頼者・headを検証する。Codexの固定版・利用可能モデル/推論量・認証/課金方式を確認する。promptにモデル名を書いただけで実指定済みにしない。実装停滞での切替は新規条件として、初期はCommanderが判断する。認証失敗・環境故障・head変更で無条件fallbackしない。

受入証拠はアプリからの依頼、対応するActions run、実際の差分、CI、担当終了と切替の記録である。通常コード・テストの実装は現行v2のClaudeへ依頼し、保護されたworkflow・Policy・認証の変更を同じ依頼へ混ぜない。

## 2. V3-2: アプリからJevを呼び、限定した判断を検査する

既存接続に不足する最小の補助ツールを用意し、アプリの実行セッションから実際に呼べることを確認する。設定文やSkillの配置だけで完了しない。実装言語・SDK固定版・配置は既存構成を読んで選び、別の常駐司令塔を作らない。

同一stateの独立したChoice/Noulを使い、回答同士を参照させない。schema、必要な回答、候補集合、有限で範囲内の数値、入力の鮮度を検査する。分布の検査をする場合は公式契約と丸め許容を明示し、独自補正しない。fixtureに加えて許可された実APIを確認する。

キーを実装worker・テスト・ログに渡さず、入力の必要最小化と秘密情報除去を検証する。通信再試行の総上限にはSDK既定値も含め、製品修正とは別に数える。Jev障害や情報不足をCodex起動・COMPLETEの理由にしない。

## 3. V3-3: CodeRabbitと必要なGUI検証を接続する

### CodeRabbit

App導入、対象repository、契約、投稿actorの受理、レビュー専任設定を確認する。安定候補とCIの確認後に`@coderabbitai full review`を依頼し、修正途中は`@coderabbitai review`を利用できる。増分を最終候補全体のレビューへ読み替えない。初期は最終current headへfull reviewを行う。

依頼・完了・対象SHAとbaseに影響される範囲・対象外ファイル・未解決指摘を証明できることを受入条件にする。対象不明や無応答を合格にしない。自動修正、CI修正、競合修正、テスト生成等を無効化し、残るApp権限を明記する。CodeRabbitを標準にする有効化前はv2のCodexレビューを維持する。

### GUI

[ADR-0024](adr/0024-local-gui-validation.md)と[既存GUI手順](local-gui-validation.md)を再利用する。アプリからPRに単独の`/gui-validate head`または`/gui-validate merge`を投稿し、対応するHosted runとartifactを取得できることを確認する。必要なら管理用dispatch、対象別workflow、ローカルCLI、明示した手動検証を選ぶ。

受入条件から必要なscenario/OS/期待結果を決め、trusted procedureがその操作を実際に含むか確認する。含まれなければprocedureを別PRで追加するか、適切な別経路を使う。単なる起動・撮影では不足する条件、macOSだけでは証明できないWindows固有動作を含めて検証する。

head/baseの実行時再検査、merge parents、duplicate run抑制、scenario・context・観測結果・画像/ログの対応を確認する。fail/blocked/unknown、古いhead、古いbase、artifact欠落、baselineのない既存問題扱いでmergeを止める。Jev/CodeRabbit/CI成功で必須GUIを免除しない。

Actions内から次のActionsへ送る場合は`GITHUB_TOKEN`のイベント制約と受信側actor検査を試験する。外部CodeRabbitの受理と内部Claude/GUI workflowの起動を混同しない。

## 4. V3-4: 観測評価とJev判断の運用

観測比較はIssue #365で完了した。以降、Jevの意味判断をCommander Policyに従って通常作業へ適用し、同じ判断をChatGPTが毎回やり直す工程は設けない。費用・性能の追加比較は利用者指定で不要。停止・再開や残るv3移行作業は、それぞれの受入条件に必要な範囲で進める。

同じ課題・初期commit・検証条件・予算をそろえ、前の試行のpatchを別条件へ持ち込まない。成功例に加えて証拠不足、範囲拡大、古い差分、誘導命令、同じ失敗の反復を含める。誤完了、無駄な切替、回数、時間、費用、未計測項目を記録し、少数例を一般的な精度保証にしない。

運用Policyは対象作業、許可action、既存切替条件、認証、必要なreview/GUI、停止・復帰方法を明記する。質問別閾値・費用/性能条件によってJev判断を無効化しない。PolicyとAGENTSを一緒に更新し、通常のJev判断とCommanderの客観ガードを一つの正本にする。

最初の有効化はアプリ内セッションからの承認済み作業と最終受入への提出まで。アプリ終了後の新規action発行、自動merge、権限拡大、設計変更、検証免除は含めない。保存・undo/redo・入力等の厳しい受入基準は維持する。

## 5. 試験表

| ID | ケース | 合格時に確認すること |
|---|---|---|
| T01 | アプリからClaude用PRコメントを投稿 | actor・対象head・受理run・結果が対応し、Issue本文だけで起動済みとしない |
| T02 | Claudeの編集とfinalizer | workerはテスト実行/pushせず、patch検査・current-head CIで検証する |
| T03 | 利用上限によるCodex切替 | source run/attempt・checkpoint・権限・headが一致し、旧writerが終了している |
| T04 | 実装停滞/認証失敗/環境故障 | 停滞は承認条件で判断し、認証等を無条件fallbackへ変えない |
| T05 | モデル・推論量が利用不可 | 無断の既定値・provider・課金切替を行わない |
| T06 | アプリからJevを実際に呼ぶ | 設定文だけでなくrequest/response・版・usageを取得できる |
| T07 | Jev不正応答、回答欠落、候補外、非有限値 | 採用せず、unknownをCOMPLETEにしない |
| T08 | Jevの429/一時障害/認証失敗 | 上限内の通信再試行と停止を区別し、実装試行に混ぜない |
| T09 | head/base/candidate/受入条件/設定版の変更 | 応答待ち中の変化を含め、古い判断と証拠を適用しない |
| T10 | 禁止パス、ログ中の誘導、テスト弱体化 | 許可範囲と秘密情報の分離を維持し、弱い検証で成功を作らない |
| T11 | CodeRabbitのfull/incremental | 実際の対象head・範囲・完了を確認し、増分を全体と偽らない |
| T12 | CodeRabbit未導入/無応答/除外/エラー | 指摘ゼロと区別する。書込機能無効化と残存権限も確認する |
| T13 | GUI head/mergeの起動 | 権限・current context・merge parents・procedure・run/artifactを確認する |
| T14 | GUI scenario未対応/OS違い/起動撮影だけ | 未検証の操作・OSを合格へ拡張せず、適切な別経路を選ぶ |
| T15 | GUI fail/blocked/unknown/古い証拠/画像欠落 | 製品・環境・証拠不足を分け、必要な証拠がなければmergeしない |
| T16 | CI成功だが受入条件やGUIの裏付け不足 | 追加検証へ進めるか停止し、COMPLETEや検証免除にしない |
| T17 | ActionsからClaude/GUI・外部CodeRabbitへ依頼 | 実認証・actor・イベントの受理を確認し、コメント成功だけで実行済みとしない |
| T18 | 同じ原因の反復、明示された停止条件または提供側の上限到達 | 新しいactionを止め、未解決事項と全試行の利用量を返す。利用者が指定していない費用上限は設けない |
| T19 | アプリ中断後の再開/v2への復帰 | 外部runの継続・終了とcurrent factsを確認し、二重writer/重複依頼を防ぐ |
| T20 | COMPLETEだがreview/GUI/CIが不足 | 完了候補にとどめ、mergeしない |
| T21 | 費用不明、途中失敗、親子usage | unknownをゼロにせず、失敗を除外せず、二重加算しない |

fixture、実API/外部App、実GUIは証拠の種類を分ける。すべてを実サービスで実施する必要はないが、接続と実行の確認をfixtureだけで完了したことにはしない。

## 6. 停止・再開・v2への復帰

停止時は新しいactionを発行せず、既に起動したworker/GUI/reviewの状態を確認する。旧workerをキャンセルする場合は実際の終了を確認してから次のwriterへ渡す。未反映patch、candidate、run/attempt、未解決検証と費用履歴を残し、秘密情報は含めない。

再開時はGitHubのcurrent head/base、実行済み操作、稼働中runを読み直し、古いJev回答・COMPLETEやアプリの会話履歴だけで進めない。v2復帰は新たなv3依頼を止め、担当と未完了差分をCommanderへ渡す。既存Issue/PRや失敗証拠を消さない。

## 7. PR分割と今回の非対象

アプリ連携、Jev、CodeRabbit設定、GUIの不足procedure、評価・有効化は必要な範囲でPRを分ける。既存v2で通常コードをClaudeに実装させ、workflow/Policy/認証等の変更は明示した運用基盤PRにする。全域リファクタリング#297を待つ前提は置かず、変更する経路の進行中PRとの重なりだけ確認する。

今回の#336はMarkdownとIssue情報だけを変更し、コード・workflow・秘密情報・依存関係・バージョン・現行Policy・既存fallbackを変えない。今回のPR自体のGUI実行は不要。文書検査、current-headの現行Codexレビュー、該当CIを確認する。#333はopenを維持し、文書mergeを実装・実API・GUI・v3有効化の完了と扱わない。
