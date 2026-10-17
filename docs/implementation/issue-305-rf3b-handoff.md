# Issue #305 実装引き継ぎ

更新日: 2026-10-17（日本時間）
対象: hide212131/hane Issue #305 / PR #497
本書は実装・検証の現状記録です。次の書込み前にmain、Issue、PR head/checks/reviews、active AADW runを再取得してください。

## 設計とIssue

Issue #305はopenです。5つの受入条件は未達のまま残し、設計mergeだけで閉じません。設計PR [#496](https://github.com/hide212131/hane/pull/496) はmerge済み、最終head `14def973e496c39b503c006db21fd187718ad683`。正本は [`docs/refactor-rf3b-short-text-input-implementation-spec.md`](../refactor-rf3b-short-text-input-implementation-spec.md) で、§3・§5〜§10を実装契約とします。設計を作り直しません。

current mainは `efb38c2a0167bac8d0dfa512bacd0f85ba21817b`。設計読解基準 `2164b895e849eef4de8c80c3696c0fd8e00c496b` から7 commit進み、その範囲はAADW verifier/testsと設計・運用文書です。製品Rust source/testはmainに追加されていません。

## AADW計測の修正

前回run [39024106716](https://github.com/hide212131/hane/actions/runs/39024106716) はClaude worker成功後、52,504,659 byteの通常ログを旧16 MiB上限で拒否し、positive modelUsageを確認できずpatch/finalizer/verifyへ進みませんでした。旧runのusageは遡及証明できません。

専用運用PR [#514](https://github.com/hide212131/hane/pull/514) は64 MiB有限上限と上限+1 byteのbounded readを導入し、missing/symlink/non-regular/malformed/oversized拒否とpositive Sonnet 5.5 usage確認を維持しました。CI [39025192853](https://github.com/hide212131/hane/actions/runs/39025192853) とCodexレビュー [5557402407](https://github.com/hide212131/hane/pull/514#pullrequestreview-5557402407) の後、専用運用PRとしてmainへmerge済みです。保護パスguardは緩和していません。

## 実装PR #497

既存の [PR #497](https://github.com/hide212131/hane/pull/497) を再利用しています。製品変更・検証対象headは `2c659e2723fd4f99f7c58b31227758dd6b03ebd5`。mainとの比較はahead 13 / behind 3、merge base `e82b9a9fe42280bfa78497746bcd7e21c73430c5`。PRはopen/Draftです。本書更新が作るcommitは文書のみで、製品Rust差分を変更しません。

10 filesを変更しました。`crates/ui/src/short_text_input.rs` に共通型とM01〜M15を追加し、filter/renameの文字列・selection・marked/composition状態、編集helper、初期化/reset、描画読み出し、test fixtureを一括切替しました。既存のC01〜C08を維持し、未mergeの#495に依存するC09は加えていません。filterとrenameは別々のモデル値です。focus、入力先判定、clipboard順、通知・一覧更新、pending時の操作差、描画bounds、OS取消通知と遅延入力guardを既存所有者に残しました。#495/#501/#503の未mergeコードは取り込んでいません。

### 検証とレビュー

AADW run [39025772438](https://github.com/hide212131/hane/actions/runs/39025772438) はprepare、worker、Sonnet 5.5 positive usage確認、patch upload、trusted finalizerが成功しました。trusted verifyはexact product head `2c659e2723fd4f99f7c58b31227758dd6b03ebd5` で `cargo test --workspace --all-features --locked` と `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` を実行し、両方successしました。依存 `block v0.1.6` の将来Rust互換性warningはありましたが、tests/lintは成功です。`cargo test -p hane-ui --all-features` の独立実行記録はありません。

通常PR CI [39026233867](https://github.com/hide212131/hane/actions/runs/39026233867) は `action_required`、job未開始です。CI successとは扱いません。CodeRabbit status successもcurrent-head full reviewの証拠とは扱いません。

Codex代替レビュー [5557473522](https://github.com/hide212131/hane/pull/497#pullrequestreview-5557473522) は製品コードに追加指摘なし。handoffの実装前snapshotとcode spanの誤りを指摘したため、本書を更新中です。更新後headで文書を再確認してください。

仕様§10のmacOS/Windows native GUI G01〜G06は未実施です。IME候補位置、取消、clipboard/focus、rename待ちと完了後の遅延入力を実画面で確認します。exact verificationはmacOS runnerであり、Windows CI・GUIの証拠にはなりません。

## 並行PR

- [#495](https://github.com/hide212131/hane/pull/495): open/ready・未merge、head `76d53cbf5b33a51ed91dfa259ced76403064872a`。`actions.rs` / `view.rs` / `input_routing.rs`が重なります。CI [39022930215](https://github.com/hide212131/hane/actions/runs/39022930215) のmacOS/Windows workspace tests・Clippyはsuccessですが、current-head full review済みとは扱いません。未受入コード・条件付きC09を取り込みません。
- [#501](https://github.com/hide212131/hane/pull/501): open/ready・未merge、最新確認head `0d5fae73a8a0cfe7c65c2724b6b981ae4601f99b`。`view.rs` / `sidebar.rs`が重なります。別のSave As/trash修正が進行中。CI [39026141859](https://github.com/hide212131/hane/actions/runs/39026141859) は `action_required`。未mergeコードを持ち込みません。
- [#503](https://github.com/hide212131/hane/pull/503): open/ready・未merge、最新確認head `aaab7f01c815a58653686c374044d361079466fa`。`view.rs`が重なります。current-head reviewは未確認、CI [39026094149](https://github.com/hide212131/hane/actions/runs/39026094149) は実行中です。未mergeコードを持ち込みません。

#495/#501/#503の受入は#305の一律先行条件にしません。mainへ入った変更が短い入力欄と実際に重なる場合だけ仕様§6.2で再評価します。

## 残作業

1. Issue/PRのstatusをcurrent head、exact verification、CI/reviewへ同期する。
2. current product headのCodex代替レビューを完了し、文書指摘を解消する。
3. `action_required`でjob未開始の通常CIを実行し、macOS/Windows workspace tests・Clippyを確認する。
4. 仕様§10のG01〜G06をmacOSとWindowsで実施し、OS/build/head、操作、期待/観測値、画像または動画を記録する。Windows shortcut確認前にKeyhac hookをOFF、終了後必ずONへ戻す。
5. 5つのIssue受入条件、旧helper/state撤去、最終SHA、結果、revert手順を確認する。

G06の5秒待ち差分は隔離した検証checkoutだけに置き、製品PRへ含めません。テスト終了時は不要なHaneプロセスだけ終了し、通常/検証用として動作中のHaneを止めません。

merge前はPR #497を閉じれば製品変更はmainへ入りません。merge後に戻す場合は製品commitをrevertします。完了条件を満たす前はIssue #305をopenのままにします。