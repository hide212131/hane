# Issue #305 実装引き継ぎ

更新日: 2026-10-17（日本時間）
対象: hide212131/hane Issue #305 / PR #497
次にGitHubへ書き込む前にもmain、Issue、PR head/checks/reviews、active AADW runを再取得してください。

## 設計とIssue

Issue #305はopenです。5つの受入条件は未達のまま残し、設計mergeだけで閉じません。設計PR [#496](https://github.com/hide212131/hane/pull/496) はmerge済み、最終head `14def973e496c39b503c006db21fd187718ad683`。正本は [実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md) で、§3・§5〜§10を実装契約とします。設計を作り直しません。

current mainは `efb38c2a0167bac8d0dfa512bacd0f85ba21817b`。設計読解基準 `2164b895e849eef4de8c80c3696c0fd8e00c496b` から7 commit進み、その範囲はAADW verifier/testsと設計・運用文書です。製品Rust source/testはmainに追加されていません。

## AADW計測の修正

前回run [39024106716](https://github.com/hide212131/hane/actions/runs/39024106716) はClaude worker成功後、52,504,659 byteの通常ログを旧16 MiB上限で拒否し、positive modelUsageを確認できずpatch/finalizer/verifyへ進みませんでした。旧runのusageは遡及証明できません。

専用運用PR [#514](https://github.com/hide212131/hane/pull/514) は64 MiB有限上限と上限+1 byteのbounded readを導入し、missing/symlink/non-regular/malformed/oversized拒否とpositive Sonnet 5.5 usage確認を維持しました。CI [39025192853](https://github.com/hide212131/hane/actions/runs/39025192853) とCodexレビュー [5557402407](https://github.com/hide212131/hane/pull/514#pullrequestreview-5557402407) の後、専用運用PRとしてmainへmerge済みです。保護パスguardは緩和していません。

## 実装PR #497

このsnapshotを作るhandoff文書commit直前のPR headは `3994e3950f1c9f2863f3c9bfdd28b40a92d885d4`。本commitはこの文書のみの更新です。PRはopen/Draftです。製品コード・exact AADW verification対象は `2c659e2723fd4f99f7c58b31227758dd6b03ebd5`。その後のPR commit `a9324af` と `3994e39` はどちらもhandoff文書のみです。current main `efb38c2` との比較はこの更新前でahead 15 / behind 3、merge base `e82b9a9fe42280bfa78497746bcd7e21c73430c5`。本commit後はahead 16 / behind 3になります。差分は10 filesのままです。

`crates/ui/src/short_text_input.rs` に共通型とM01〜M15を追加し、filter/renameの文字列・selection・marked/composition状態、編集helper、初期化/reset、描画読み出し、test fixtureを一括切替しました。既存のC01〜C08を維持し、未mergeの#495に依存するC09は加えていません。filterとrenameは別々のモデル値です。focus、入力先判定、clipboard順、通知・一覧更新、pending時の操作差、描画bounds、OS取消通知と遅延入力guardは既存所有者に残しました。#495/#501/#503の未mergeコードは取り込んでいません。

### 検証とレビュー

AADW run [39025772438](https://github.com/hide212131/hane/actions/runs/39025772438) はprepare、worker、Sonnet 5.5 positive usage確認、patch upload、trusted finalizerが成功しました。trusted verifyはexact product head `2c659e2723fd4f99f7c58b31227758dd6b03ebd5` で `cargo test --workspace --all-features --locked` と `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` を実行し、両方successしました。依存 `block v0.1.6` の将来Rust互換性warningはありましたが、tests/lintは成功です。独立した `cargo test -p hane-ui --all-features` の実行記録はありません。

current code+handoff head `3994e395` の通常PR CI [39027984552](https://github.com/hide212131/hane/actions/runs/39027984552) はmacOSとWindowsの両方でsuccessしました。workspace tests、各OSの追加チェック、Clippyを含みます。release-only jobは対象外でskipです。この文書のみの更新は新しいheadを作ります。CI successはnative GUI受け入れの代わりにはなりません。

Codex代替レビュー [5557473522](https://github.com/hide212131/hane/pull/497#pullrequestreview-5557473522) は製品差分に追加コード指摘なし。handoffの状態snapshotとMarkdown code spanに修正指摘があり、修正済みです。このhandoff更新後headに対するレビューと旧スレッドの解決が必要です。CodeRabbitのstatus successはfull reviewの証拠とは扱いません。

ローカルmacOS debug buildはsource checkout `a9324faf9aa934a52c65af9eb654c3f1fc4e1324` から作成し、binary SHA-256は `d46bbb88ef9558beec9cd501307b0894af0b8800489f75d56de461ff9f8accc3` です。これはbuild確認のみで、focused GUIシナリオの証拠ではありません。仕様§10のnative GUI G01〜G06はmacOS/Windowsとも未実施です。CUAでは候補アプリ画面へのクリックが失敗し、VMware FusionのWindows VMにも入力できませんでした。IME候補位置、取消、clipboard/focus、rename待ちと完了後の遅延入力は両OSで未確認です。画面確認できたbuild/head、操作、期待/観測値、画像または動画がそろうまでpassにしません。

## 並行PR

- [#495](https://github.com/hide212131/hane/pull/495): open/ready・未merge、current head `76d53cbf5b33a51ed91dfa259ced76403064872a`。`actions.rs` / `view.rs` / `input_routing.rs`が重なります。current-head CI [39022930215](https://github.com/hide212131/hane/actions/runs/39022930215) のmacOS/Windows workspace tests・Clippyはsuccessです。full review実施の証拠は未確認で、CodeRabbit statusはpendingです。未受入コード・条件付きC09を取り込みません。
- [#501](https://github.com/hide212131/hane/pull/501): open/ready・未merge、current head `1ba71efb6518ce475bbefc10be3629940fc5888c`。`view.rs` / `sidebar.rs`が重なります。current-head CI [39027698405](https://github.com/hide212131/hane/actions/runs/39027698405) は `action_required` です。未mergeコードを持ち込みません。
- [#503](https://github.com/hide212131/hane/pull/503): open/ready・未merge、current head `27350f887b49fa21a68da3909d01466b991ed7d1`。`view.rs`が重なります。current-head CI [39027679630](https://github.com/hide212131/hane/actions/runs/39027679630) のmacOS/Windows workspace tests・Clippyはsuccessです。current-head full reviewの状態は未確認です。未mergeコードを持ち込みません。

#495/#501/#503の受入は#305の一律先行条件にしません。mainへ入った変更が短い入力欄と実際に重なる場合だけ仕様§6.2で再評価します。

## 残作業

1. このhandoff更新後headでPR全体のCodex代替レビューを完了し、修正済みreview threadをresolveする。
2. 仕様§10のG01〜G06をmacOSとWindowsで実施し、OS/build/head、操作、期待/観測値、画像または動画を記録する。Windows shortcut確認前にKeyhac hookをOFF、終了後必ずONへ戻す。
3. 5つのIssue受入条件、旧helper/state撤去、最終SHA、結果、revert手順を確認する。Issue #305は全条件が実際に満たされるまでopenのままにする。

G06の5秒待ち差分は隔離した検証checkoutだけに置き、baselineとcandidateに同じ一時差分を適用します。製品PRへ含めません。テスト終了時は不要なHaneプロセスだけ終了し、通常/検証用として動作中のHaneを止めません。

merge前はPR #497を閉じれば製品変更はmainへ入りません。merge後に戻す場合は製品commitをrevertします。完了条件を満たす前はIssue #305をopenのままにします。
