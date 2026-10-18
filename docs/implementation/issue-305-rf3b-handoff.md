# Issue #305 実装引き継ぎ

更新日: 2026-10-18（日本時間）
対象: hide212131/hane Issue #305 / PR #497

この更新開始時点のPR headは ed89f77fb46f7a2d55c9169d7fcf868c3c41ce5d、current mainは aab1dd5a28dc1383d34e09e0e2f01b634ad6301a。文書のみのcommitもPR headを進めるため、commit後にchecksとreviewのcurrent stateを再取得する。

## Issue と設計

Issue #305はopen。5つの受入条件は未達のまま維持し、設計PRのmergeやCI成功だけで閉じない。

設計PR [#496](https://github.com/hide212131/hane/pull/496) はmerge済み。採用仕様の正本は [短い入力欄の実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md)、設計PR最終headは 14def973e496c39b503c006db21fd187718ad683。CodeRabbit full reviewは同headで完了し、actionable commentsなし。設計を作り直さず、仕様§3・§5〜§10を実装契約とする。

## current main と並行変更

確認時点のcurrent mainは aab1dd5a28dc1383d34e09e0e2f01b634ad6301a。PR #495、#501、#503とAADW運用PR #514はすべてmainへmerge済み。#514はClaude実行ログの64 MiB bounded readと不正入力の拒否、positive modelUsage確認を維持する専用運用PRであり、製品仕様を変えない。

#495で受け入れられたOS別のSelectDocumentStart/End割当と短い欄での選択動作を維持する。仕様§6.2・§10.2の条件付きC09を現候補へ追加済み。未受入コードを別PRから取り込んでいない。

## PR #497 の候補

この文書更新開始時のPR headは ed89f77fb46f7a2d55c9169d7fcf868c3c41ce5d、baseはcurrent main、比較はahead 23 / behind 0。製品コード・テスト候補は a043ba76c9337eecbf1b78a88f8db8bac7cb502e。PR差分は11 filesで、共通型ShortTextInput、filter/rename切替、M01〜M15、C01〜C09を含む。

最新のClaude修正は古いString fixtureをShortTextInput fixtureへ直し、#495の文書端選択actionをfilter・通常rename・pending rename・本文で確認するC09を追加したもの。製品実装をこの文書更新で追加しない。文書をPRへcommitするとheadは進むため、その後の判断ではGitHubから新しいheadを取り直すこと。

## 検証・レビュー

- AADW run [39080808213](https://github.com/hide212131/hane/actions/runs/39080808213) はprepare、Claude worker、Sonnet 5.5確認、trusted finalizer、exact worker resultのworkspace testsとClippyまでsuccess。対象コード候補SHAは a043ba76c9337eecbf1b78a88f8db8bac7cb502e。
- current-head PR CI [39081791902](https://github.com/hide212131/hane/actions/runs/39081791902) はhead ed89f77で完了し、macOS / Windowsのworkspace tests、OS別チェック、Clippyがすべてsuccess。この文書更新がheadを進めた後は同じrunをcurrent-head CIとして扱わず、新しいrunを確認する。
- 以前のhead 8effd04では#495由来の古いfixtureが原因でCIが失敗した。a043ba7でfixtureを直し、C09を追加した。
- 利用者指定のCodex代替レビュー [5560782461](https://github.com/hide212131/hane/pull/497#pullrequestreview-5560782461) はhead ed89f77を対象に実施し、追加指摘なし。CodeRabbitはDraftのためreviewをskipしており、CodeRabbit full reviewではない。この文書更新後のheadは新しくreview対象を確認する。
- 仕様§10のG01〜G06はmacOS / Windowsとも未実施。Windows VM画面は取得できたが、空白領域へのクリックもComputer Use backendからnoWindowsAvailableで失敗した。入力を送れず、Windows GUI acceptanceはunknown。Mac側も候補headでのfocused G01〜G06 evidenceはない。IME、候補位置、clipboard/focus、rename待ちと完了後の遅延入力は未確認。G06の5秒遅延は隔離checkoutにだけ入れ、製品PRへ含めない。

## 次の手順

1. この文書更新後のPR current head、base、checksを再取得し、current-head macOS / Windows CIを完了させる。
2. 利用者指定のCodex代替レビューをcurrent headで行い、変更を採用済み仕様と照合する。CodeRabbit full reviewのstatusはDraftの間はskipのまま記録する。
3. 両OSで仕様§10のG01〜G06を実施し、head/build、入力方式、操作、期待/観測値、画像または動画を記録する。取得不能な結果はunknownのまま残す。
4. 旧状態/helper撤去、受入5条件、最終SHA、revert方法を照合する。merge前にexpected head、base、CI、review、必要なGUI evidence、mergeabilityを再確認し、Issue #305はすべての受入条件が満たされるまでopenのままにする。

新規実装を別に始める場合の具体的な一手は、current mainからclean checkoutを作り、仕様§10.2の不足する既存挙動テストを追加して変更前結果とSHAを記録すること。その後に限り、仕様§3・§5〜§8に沿って一つの製品PRで状態移行と旧経路撤去を行う。既存のPR #497があるため、同じ実装を重複して開始しない。
