# Issue #305 実装引き継ぎ

更新日: 2026-10-17（日本時間）
対象: hide212131/hane Issue #305
本書は下記時点のGitHub状態を記録します。実装依頼を出す直前に、main、Issue、PR、head、checks、active AADW runを必ず再取得してください。

## 設計とIssueの状態

Issue #305はopenです。5つの受入条件はすべて未達です。設計PR [#496](https://github.com/hide212131/hane/pull/496) はmerge済み、最終headは `14def973e496c39b503c006db21fd187718ad683`。正本は `docs/refactor-rf3b-short-text-input-implementation-spec.md`。設計を作り直さず、特に§3、§5〜§10を実装契約として使います。

current mainは `efb38c2a0167bac8d0dfa512bacd0f85ba21817b`。設計読解基準 `2164b895e849eef4de8c80c3696c0fd8e00c496b` から7 commit進みましたが、変更はAADW用Python verifier/testsと設計・運用文書で、製品Rust source/testの変更はありません。

## 計測修正と前回の失敗

前回の実装run [39024106716](https://github.com/hide212131/hane/actions/runs/39024106716) はClaude workerが成功した後、trusted model verifierが52,504,659 byteの通常ファイルを旧16 MiB上限で拒否しました。modelUsageの正のSonnet 5.5 usageは確認できず、patch/finalizer/verifyは実行されていません。これは製品実装の失敗ではなく計測上限の問題です。旧runのusageは現在も遡及証明できません。

専用運用PR [#514](https://github.com/hide212131/hane/pull/514) は64 MiBの有限上限と実読込上限+1 byteを追加し、symlink/non-regular/missing/malformed/oversized拒否と正のusage確認を維持しました。既存テストを含むcurrent-head CI [39025192853](https://github.com/hide212131/hane/actions/runs/39025192853) と利用者指定Codexレビュー [5557402407](https://github.com/hide212131/hane/pull/514#pullrequestreview-5557402407) の成功後、merge済みです。workerの保護パスguardは変更していません。

## 実装PR #497

実装先は既存の [PR #497](https://github.com/hide212131/hane/pull/497) を再利用し、新しい製品PRや設計PRを作らないでください。更新前のheadは `6e18caa2eeccb2d69fa2d3c128cbfe5569df053d`、current main比較はahead 11 / behind 3、merge baseは `e82b9a9fe42280bfa78497746bcd7e21c73430c5`。差分は `crates/ui/src/view/input_routing.rs` のC01〜C08基準テストと本書の2 filesだけで、製品実装はありません。handoff更新後のheadは、この値と区別してGitHubから取得してください。

head `6e18caa` の通常PR CI [39023602717](https://github.com/hide212131/hane/actions/runs/39023602717) は `action_required` でsuccessではありません。C01〜C08のテストは以前のexact-head AADW検証でsuccessしましたが、現在のhead・新しいmainを検証するものではありません。前述のfailed runも製品patchの検証にはなりません。

前headの利用者指定Codex review [5557349961](https://github.com/hide212131/hane/pull/497#pullrequestreview-5557349961) はC01〜C08を仕様§6.2・§10.2と照合し、コード指摘なしでした。CodeRabbit full reviewはrate limitで未実施です。今回のhandoff更新後は新headの状態を記録し、実装後の最終候補でcurrent-head reviewを行います。

## 並行PRの扱い

- [PR #495](https://github.com/hide212131/hane/pull/495) はopen/ready・未merge、head `76d53cbf5b33a51ed91dfa259ced76403064872a`。current main比較はahead 18 / behind 7、merge base `2164b895e849eef4de8c80c3696c0fd8e00c496b`、13 files。変更は `actions.rs`、`view.rs`、`input_routing.rs` 等で重なります。CI [39022930215](https://github.com/hide212131/hane/actions/runs/39022930215) はmacOS/Windows workspace testsとClippyがsuccess。既知のCodeRabbit review threadsはresolvedですが、latest full reviewは現headより古く、current-head review済みとは扱いません。未受入コードを取り込まず、C09を追加せず、#495のmergeを一律の前提にしません。着手直前にも状態を読み直します。受け入れが先行した場合だけ文書端選択のOS別挙動を仕様§6.2で再確認します。
- [PR #503](https://github.com/hide212131/hane/pull/503) はopen/ready。worker/finalizer後のhead `a3b793f049372c3cc1ec02534e29f520935da86e` のverificationとCIが進行中です。前head `38d6373` に対するCodex reviewには未解決findingがありました。新headでのreview結果を確認し、未mergeコードを#305へ持ち込みません。
- [PR #501](https://github.com/hide212131/hane/pull/501) はopen/ready。worker/finalizer後のhead `dc577aa9fb569197e86a3a269ab4eda981061c9f` のverificationとCIが進行中です。 `view.rs` / `sidebar.rs` 等が重なるため、未mergeコードを持ち込みません。

#495、#501、#503が未mergeのままなら、#305はcurrent mainを基準に実装します。共通化の対象と実際に重なるaction/state/callbackだけを再確認し、無関係なPRを待ったりその変更を取り込んだりしません。

## 実装の依頼と境界

現在のworker/finalizerが終了したこととactive writerがないことを、依頼投稿の直前に確認してください。PR #497のcurrent 40桁headを取得し、同じheadに対する重複runがない場合に限り、ConversationへAADW v3の実装依頼を一度投稿します。markerは全て必要です: `@claude`、`AADW_COMMANDER_HANDOFF_V3`、`AADW_ACTION: implement`、current `AADW_TARGET_HEAD`、`AADW_FAILURE_CLASS: initial`、安定したroot cause `issue-305-rf3b-short-text-input-cutover`、原因を示すGitHub evidence。分類を診断目的で流用したり、再送を通すためにroot cause/evidenceを付け替えたりしません。

採用仕様に従い、C01〜C08を維持し、M01〜M15と不足するC回帰を追加しながら一括切替を行います。同期用の新旧状態は作りません。新しい `crates/ui/src/short_text_input.rs` と§8.1の追従範囲を確認し、旧helper/状態の二重管理を撤去します。focus、入力先判定、rename pending・ファイル操作、clipboard、通知、一覧更新、描画boundsは既存所有者に残します。OS取消通知と空文字置換、filter blur、pending中操作、遅延入力guardを一律化しません。永続形式を変更せず、必要性を説明せずに `actions.rs`、`input.rs`、`capture.rs`、session、vendor、Cargo、workflowへ範囲を広げません。

workerはClaude Sonnet 5.5を使い、Read/Edit/Write/Glob/Grepのみで差分を作ります。workerにshell、git、test/build/lint、commit、pushをさせません。Commanderはprepare、Sonnet 5.5とpositive usage確認、patch、trusted finalizer、exact result検証を別々に確認します。失敗・skipped・unknownを成功扱いしません。

## 検証と完了

現headの実装で仕様§10のM01〜M15、C01〜C08（#495受入が先なら条件付きC09）、macOS/WindowsのG01〜G06を確認します。最終候補では `cargo test -p hane-ui --all-features`、`cargo test --workspace --all-features)、`cargo clippy --workspace --all-targets --all-features -- -D warnings)、通常feature/実画面用build、current-head CI/reviewを区別して記録します。G06用の一時差分は隔離検証checkoutだけに置きます。native GUIはIME、候補位置、selection/clipboard、取消、focus、rename待ち中/完了後の入力を両OSで記録し、片OSやunit testを代用にしません。Windowsでshortcutを試す前にKeyhac hookをOFFにし、終わったらONへ戻します。終了時は不要なHaneプロセスだけを閉じます。

Issue #305は、5つの受入条件すべてとrevert手順を確認するまでopenのままです。設計merge、文書更新、AADWの成功だけでIssueを閉じません。
