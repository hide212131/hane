# Issue #305 実装引き継ぎ

更新日: 2026-10-18（日本時間）
対象: hide212131/hane Issue #305 / PR #497

## Issue と設計

Issue #305 は open。5つの受入条件は未達のまま維持し、設計PRのmergeやCI成功だけで閉じない。

設計PR [#496](https://github.com/hide212131/hane/pull/496) はmerge済み。採用仕様の正本は [短い入力欄の実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md)（設計PR最終head `14def973e496c39b503c006db21fd187718ad683`）。設計を作り直さず、仕様§3・§5〜§10を実装契約とする。

## current main と並行変更

確認時点のcurrent mainは `aab1dd5a28dc1383d34e09e0e2f01b634ad6301a`。PR #495、#501、#503とAADW運用PR #514はすべてmainへmerge済み。#514はClaude実行ログの64 MiB bounded readと不正入力の拒否、positive modelUsage確認を維持する専用運用PRであり、製品仕様を変えない。

#495で受け入れられたOS別のSelectDocumentStart/End割当と短い欄での選択動作を維持する。仕様§6.2・§10.2の条件付きC09を現候補へ追加済み。未受入コードを別PRから取り込んでいない。

## PR #497 の候補

この文書更新前の製品コード・テスト候補は `a043ba76c9337eecbf1b78a88f8db8bac7cb502e`。PRはopen / Draft、baseはcurrent main `aab1dd5a28dc1383d34e09e0e2f01b634ad6301a`、比較はahead 22 / behind 0、merge baseはcurrent main。PR差分は11 filesで、共通型 ShortTextInput、filter/rename切替、M01〜M15、C01〜C09を含む。

最新のClaude修正は古いString fixtureをShortTextInput fixtureへ直し、#495の文書端選択actionをfilter・通常rename・pending rename・本文で確認するC09を追加したもの。製品実装をこの文書更新で追加しない。文書をPRへcommitするとheadは進むため、その後の判断ではGitHubから新しいheadを取り直すこと。

## 検証・レビュー

- AADW run [39080808213](https://github.com/hide212131/hane/actions/runs/39080808213) はprepare、Claude worker、Sonnet 5.5確認、trusted finalizer、exact worker result のworkspace testsとClippyまでsuccess。製品候補SHAは `a043ba76c9337eecbf1b78a88f8db8bac7cb502e`。
- 通常PR CI [39080917931](https://github.com/hide212131/hane/actions/runs/39080917931) はattempt 2で再実行中。対象headは同じ `a043ba76c9337eecbf1b78a88f8db8bac7cb502e`。確認時点ではmacOS / Windowsのworkspace tests、OS別チェック、Clippyが進行中で、完了扱いにしない。以前のhead `8effd04` では#495由来の古いfixtureが原因で失敗した。今回の修正はそのfixtureを直す。
- 直前のCodex代替レビュー [5557600998](https://github.com/hide212131/hane/pull/497#pullrequestreview-5557600998) はhead `53baadbb96d5a1eb5b5927322b5e01a5bbd18922` が対象で、今回のC09追加前。current候補の正式レビューとはみなさない。
- CodeRabbitはDraftのためreviewをskipしている。current-head CI成功後にPRをnon-draftへ進め、current mainのCommander Policyに従ってCodeRabbit full reviewを確認する。Codex fallbackはPolicyの条件を満たした場合だけ使う。

## GUI と残課題

仕様§10のG01〜G06はmacOS / Windowsとも未実施。CI、AADWのworkspace tests、画面captureを実GUI操作の代わりにしない。

Windows VM画面は取得できるが、空白領域へのクリックもComputer Use backendから `noWindowsAvailable` で失敗した。したがって入力を送れず、Windows GUI acceptanceはunknown。Mac側も候補headでのfocused G01〜G06 evidenceはない。IME、候補位置、clipboard/focus、rename待ちと完了後の遅延入力は未確認。G06の5秒遅延は隔離checkoutにだけ入れ、製品PRへ含めない。

## 次の手順

1. この文書更新後のPR current head、base、checksを再取得し、current-head macOS / Windows CIを完了させる。
2. CodeRabbitのcurrent-head full reviewを取得し、指摘を仕様と照合する。CodeRabbitがPolicy上のfallback条件を満たさない限り、Codex reviewへ切り替えない。
3. 両OSで仕様§10のG01〜G06を実行し、head/build、入力方式、操作、期待/観測値、画像または動画を記録する。取得不能な結果はunknownのまま残す。
4. 旧状態/helper撤去、受入5条件、最終SHA、revert方法を照合する。merge前にexpected head、base、CI、review、必要なGUI evidence、mergeabilityを再確認し、Issue #305はすべての受入条件が満たされるまでopenのままにする。

新規実装を別に始める場合の具体的な一手は、current mainからclean checkoutを作り、仕様§10.2の不足する既存挙動テストを追加して変更前結果とSHAを記録すること。その後に限り、仕様§3・§5〜§8に沿って一つの製品PRで状態移行と旧経路撤去を行う。既存のPR #497があるため、同じ実装を重複して開始しない。
