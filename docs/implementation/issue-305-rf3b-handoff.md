# Issue #305 実装・GUI受入の引き継ぎ

更新日: 2026-10-19（日本時間）
対象: hide212131/hane Issue #305 / PR #497

## 現在の判定

採用設計を維持し、仕様書の作り直しは行わない。設計の正本は[短い入力欄の実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md)で、§3・§5〜§10を実装・受入契約とする。

2026-10-19のGitHub live-state確認。Issue #305はOPENで受入条件5件はすべて未チェック。PR #497はOPEN / Draft、handoff更新前のcurrent head `f97e7d981a51ce491d1c5f126952cfe9aa941460`、製品コードhead `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`、base/current main `fd894ff8136cfe0bcae903211b3b422713db8b54`、35 commits ahead / 0 behind、12 files、mergeable。製品head以降の差分はhandoff文書のみ。head `f97e7d9`のCI run `39151806100`はmacOS/Windows workspace tests、Windows IME notification dispatch、Clippyがsuccess（release-only checksはskip）。利用者指定Codex代替review `5564162428`はhandoff文書に追加指摘なし。未解決のP1 review thread `PRRT_kwDOUETGuM6tsITw`は、candidateでもG04 popup位置条件を満たさないため未解決のまま維持する。CodeRabbit full reviewはDraftのため未実施。設計PR #496はmerge済み（head `14def973e496c39b503c006db21fd187718ad683`）、並行PR #495もmerge済み（head `76d53cbf5b33a51ed91dfa259ced76403064872a`）。ClaudeのAADW実装依頼run `39139696316`は製品headでprepare・worker・Sonnet 5.5確認・trusted finalizer・exact-result verifier・workspace regression tests/lintがsuccess。CodeRabbitのsuccess statusだけではfull review完了を意味しない。

このheadにはG04向けのbounds-origin変更通知が入った。Windows current-head確認ではrename行移動後の候補位置と変換確定後クリックを確認した。C05ではfilter composition中の再クリックによるcaret位置更新と、rename再クリック時の位置変更拒否を確認したが、filter blur後の文字列保持は未確認のためC05全体はPARTIAL。別のG04サブケースは2026-10-19にcurrent mainと候補を同条件で比較した。両方でcaretはクリック位置へ移った一方、日本語IME候補popupは旧座標に残った。したがってこの観測はPR固有の回帰とは判定できず、候補側もG04のpopup位置条件を満たしていない。Keyhacは一時状態不明となった後、利用者がread-onlyでOFFを再確認した。再開後はKeyhacを操作せず比較を完了し、試験用Haneを終了、fixture一致とcheckout cleanを確認した。macOS current-head release buildはできている。CUAではFinderの無害な操作まで一度復旧したが、検証アプリの起動承認待ちのためmacOS GUI試験は未実施。G06については、rename待ち中の入力→完了後Enter→遅延text callback破棄→次の通常入力を順序どおり固定する`ime_confirming_enter_after_rename_keeps_the_late_text_guard`などの制御GPUIテストがPRに残り、current-head CIで実行済み。一方、5秒遅延buildによるcurrent-candidateのOS別GUI確認は未実施であり、既存testは実OSのcallback順・IME popup表示の証拠とは区別する。#496/#495の採用済み内容は再設計しない。

### Windows current-head G04（partial、run 1）

- Candidate head `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`をRust/Cargo 1.98.1、`aarch64-pc-windows-msvc`でrelease build。実行ファイルSHA-256 `7F65B9587218546C0850A99F70C7E202F61F4EEF9FE878E8D7CB5583DA920399`。起動前にhead/base、fixture、clean worktreeを照合し、専用stateとfixtureでPID 7516を使った。
- Filter: IME未確定「か」の候補をcaret start/middle/endと逆向き選択後に表示し、各caret付近にあることを確認。sidebar幅を広げた後もfilter欄から候補が逸れなかった。
- Rename: 候補はrename欄付近に表示。composition中に同じrename行を再クリックした時、caretとfilenameは変わらず、C05のrename拒否と一致した。一時renameで行を4番目から最下段へ動かし、その新位置でも候補がrename行付近に表示された。移動後のcaret start/middle/end・reverse selectionを個別に網羅したとは記録されていない。
- run 1時点ではIME確定後クリックとfilter C05が未確認だった。run 2でrenameの未確認項目を補ったが、filter C05は候補popupの旧位置残留が確認され、current-head G04は引き続きpartial。
- 利用者は試験開始前にKeyhac hookをread-onlyでOFF確認。Windows workerはON/OFF shortcutのpath・引数・scriptを読み取り調査しようとしたが、実行前に中止指示を受けてshortcutを実行せず、以後Keyhacには触れていない。終了時に状態を再確認していない。
- 専用Hane PID 7516を通常終了し、6 fixture filesのpath/size/hashが全て開始時と一致。worktree clean、process残存なし。Windows evidence: `C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\evidence\g04-current-a2b106-20261019-0001\final-evidence.md` と `pre-cleanup-ui-state.md`。これらはlocal temp recordで、GitHubへuploadしていない。

### Windows current-head G04 focused follow-up（run 2）

- 同じ候補head `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`と前runのRust/Cargo 1.98.1 build（SHA-256 `7F65B9587218546C0850A99F70C7E202F61F4EEF9FE878E8D7CB5583DA920399`）を再照合して使用。専用fixture/stateを使い、PR head/base・clean worktree・Hane不在を起動前に確認。
- Rename行を更新日時順の5番目から名前順の最下段へ動かした後、start/middle/end/reverse selectionで候補がcaret付近に表示された。変換確定後に欄内をクリックするとcaretがクリック位置へ移った。
- C05のfilter再クリックではcaretがクリック位置へ移り、§6.2の位置更新条件を満たした。rename再クリックの拒否もrun 1で確認済みだが、filter blur後の文字列保持は未確認なのでC05全体はPARTIAL。別のG04観測として、再クリック後の日本語候補popupはクリック前の座標に残り、現在caret付近に表示されなかった。2026-10-19のcurrent-main/candidate同条件比較でも両方で同じ結果だったため、PR固有の回帰とは判定しない。候補版でもG04のこの条件は未達で、G04全体は未完了。
- 専用Hane PID 5096を終了。fixture 6件の名前・hashは開始時と一致し、worktreeはclean。Keyhacには触れていない。証拠: `C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\evidence\g04-focus-a2b106-20261019-0002\final-evidence.md`（local temp、GitHub未upload）。

### Windows current-main / candidate G04 focused comparison（2026-10-19）

- run `g04-main-vs-candidate-20261019-0001` / resume `g04-resume-20261019-0001`。PR head `0f46e82e8fbbfcf2444a3959d1d6cf462fc847b6`、product head `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`、base/current main `fd894ff8136cfe0bcae903211b3b422713db8b54`。main/candidate checkoutはいずれも期待SHAでclean。Windows 11 Home ARM64 build 26200、Microsoft Japanese IME（ひらがな入力）。ChatGPT desktop updaterは`up_to_date`（version `26.1013.40934`、build `20770`）。
- 同一fixture内容の専用6-fileコピー、専用state、Rust/Cargo 1.98.1のrelease binaryを使用。main PID `6032` / SHA-256 `039D361FD99CA0E0FE7F20103B70F39A24A4A292C94E424CF073FA124D555463`、candidate PID `10612` / SHA-256 `7F65B9587218546C0850A99F70C7E202F61F4EEF9FE878E8D7CB5583DA920399`。
- 各版でfilterへ`ordinary`を入力し、caretを先頭へ移動。未確定IME文字列`かordinary`を表示中、filter内の同じwindow相対座標`(53, 115)`を再クリックした。クリック前caretは約`(26, 115)`、IME候補popup左上は約`(27, 143)`。クリック後caretは約`(50, 115)`へ移動したが、popupは両版とも約`(27, 143)`に残った。
- **結果:** C05のfilter caret位置更新はmain/candidateともPASS。G04の「現在の文字位置付近」へのIME候補移動は両版ともFAIL。この同じ失敗がcurrent mainにもあるため、この証拠ではPR固有回帰とは判定できない。一方、候補版は該当するG04条件を満たしておらず、G04全体も未完了。
- 両Hane processは通常終了し、残存processなし。main/candidateの専用fixtureはそれぞれ6件のpath・size・SHA-256が起動前manifestと一致。checkout head/statusも最終確認済み。証拠はWindows local tempの`evidence/g04-main-vs-candidate-20261019-0001/final-evidence.md`、`main-observation.json`、`candidate-observation.json`、`comparison-result.json`（GitHub未upload）。利用者のOFF read-only確認後に再開し、比較中もKeyhacへの操作・確認はしていない。

### Windows CUA復旧後の追加試行（2026-10-19、受入確認は未実施）

- current main `fd894ff8136cfe0bcae903211b3b422713db8b54`の`AGENTS.md`、`docs/gui-test-recovery.md`、`.agents/skills/hane-gui-test/SKILL.md`を読み、接続先Windows版ChatGPTからCUAを再確認した。ExplorerとChatGPTのwindow列挙ができ、Explorerへの文字入力は成功した。CUAの追加クリック確認中に、停止指示に反してWindows workerがデスクトップの`Keyhac フック OFF`ショートカットを右クリックメニューから一度実行し、リンク先`KeyhacHook.ahk off`のスクリプトを読み取っていた。実行後にエラーダイアログは見えず、トレイ状態は確認していない。`Keyhac フック状態 ON`ファイルは残っているがスクリプトは同ファイルを更新しないため、実行後のhook状態は**不明**。その後Keyhacへの操作はしていない。
- 候補product head `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`、base `fd894ff8136cfe0bcae903211b3b422713db8b54`、exe SHA-256 `7F65B9587218546C0850A99F70C7E202F61F4EEF9FE878E8D7CB5583DA920399`を使い、隔離state/fixtureからHane PID `7600`を起動した。filter欄をクリックしただけで文字入力・IME操作はしていない。停止後、起動記録とProcessId/ExecutablePath/CommandLineが一致することを確認し、ウィンドウを通常終了してPID消失を確認した。G04 blur確認・G06は実施しておらず、この試行を受入evidenceには数えない。
- 今回作成された6ファイルfixture `C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\fixtures\g04-filter-blur-a2b106-20261019-0003`はコピー元fixtureと名前・サイズ・SHA-256が全件一致した。削除は実行前にpolicyで拒否されたため残置。evidenceは同rootの`evidence/g04-filter-blur-a2b106-20261019-0003/launch.json`と`cleanup.json`に残る。ユーザーデータ・製品コード・Issue/PR・Git branchには変更なし。

### macOS current-head build（GUI blocked）

- head `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`をmacOS ARM64 / Rust-Cargo 1.98.1でrelease build。candidate binary SHA-256 `da6b80a2b16ff0482f81bf8a7ade93a432266183bef08e3bacbbd6191feeab89`。専用fixture/state、固有bundle IDの検証appを用意した。
- 過去の起動時`cua.getApp(bundlePath)`は`timeoutReached`。候補Hane processは存在せず、製品画面は開いていなかった。Finderの状態・画像取得は成功したが、当時の無害なFinder clickは`noWindowsAvailable`で、CUA JS sessionを一度reset後も同じだった。共有ChatGPT配下の`SkyComputerUseService`は共有helperなので再起動していない。2026-10-19の再確認ではFinderの無害なclickまでCUAが復旧したが、検証appの起動承認待ちのためcandidate Haneはまだ起動しておらず、macOS G01〜G06は未実施。
- macOS G01〜G06はいずれも今回未実施。fixture 6件のhashは開始manifestと一致し、candidate worktreeはhead一致・clean。詳細記録は`/private/tmp/hane-issue305-macos-a2b106-20261019-0001/evidence/gui-launch-failure.md`。native input接続が復旧したらこのbuildを使ってGUI受入を再開する。

## current main と関連PR

- current main: `fd894ff8136cfe0bcae903211b3b422713db8b54`（#522）。
- 2026-10-19の再確認でもcurrent mainは`fd894ff8136cfe0bcae903211b3b422713db8b54`、Issue #305はOPEN、5つの受入条件はすべて未チェックのまま。
- PR #496: MERGED、head `14def973e496c39b503c006db21fd187718ad683`。CodeRabbit full reviewは最終headで完了し、actionable commentなし。
- PR #495: MERGED、head `76d53cbf5b33a51ed91dfa259ced76403064872a`。OS別の文書端選択挙動を受け入れ済み。#497のC09がその挙動を保護する。
- 2026-10-19のこの引き継ぎ更新前のsnapshot: PR #497はOPEN / Draft、product code head `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`、PR head `b4937bd86ecd4f4befe641cf2379a39fbc4a76cb`、base/main `fd894ff8136cfe0bcae903211b3b422713db8b54`、33 ahead / 0 behind、mergeable CLEAN。exact-head CI run `39148976572`はsuccess。current-head Codex代替レビューはreview `5564037035`で完了し、P1のG04受入上の問題を残している。CodeRabbit full reviewは未完了（Draftでskip）。この引き継ぎ更新によってheadが変わった後はGitHubからmetadata・CI・review stateを再取得する。
- GitHub上のIssue #305本文とPR #497本文は、current-main/candidate比較で両方ともpopup位置更新に失敗した結果、C05の部分確認、受入未完了を反映済み。IssueはOPEN、PRはDraftのまま。current headとCIは本書更新時点のlive stateを確認済み。
- current main `fd894ff...`にはGUI失敗時の復旧runbook更新（#522）が含まれる。今回その文書と`AGENTS.md`を読み直し、Windows GUIは接続済みWindows版ChatGPTから直接操作し、Mac経由のVMware操作は使わないことを確認した。
- 利用者からWindows環境を修復したとの連絡があり、papa-winのnative CUAでWindows再確認を完了した。Haneの`foreground window did not report a process id`はCUA session reset・`@oai/sky`再import・最新window再取得で解消し、その後のGUI操作を継続できた。復旧は確認したが、原因イベントは未特定。current main G04 baselineはrename行移動でFAIL、候補head `561c94d`のG01/G02/G03/G05/G06は確認済み、G04はfilter側の一部条件のみでPARTIAL。詳細を次節に記録する。
- 2026-10-18の再開では、current mainの復旧手順に沿ったWindows CUAの軽量確認が通った。Explorerの画面取得・無害なクリック・検索欄への入力を別々に確認し、入力表示後に検索を閉じた。途中のelement indexエラーは状態再取得と座標操作で解消。更新確認は`up_to_date`だったが、これはCodex desktop updaterの状態で、ChatGPTサービスの更新を示すものではない。
- 10/18時点では候補G04ゲートをNOT RUNで停止した。`Keyhac フック OFF`ショートカットの操作回数と結果を確認できない期間があった。利用者がread-onlyでOFFを再確認した後、10/19のpaired current-main/candidate G04 comparisonを完了した。比較中もKeyhacの確認・起動・切替・復元は行っていない。
- #501、#503、運用基盤専用PR #514もmainへmerge済み。

## PR #497 の候補・レビュー・CI

- 最後のClaude製品実装commitは`a043ba76c9337eecbf1b78a88f8db8bac7cb502e`で、ShortTextInputへのfilter/rename移行、M01〜M15、C01〜C09を含む。今回のmerge commitではcurrent main側の`view.rs`等も入ったため、これまでのGUI結果は同期済みPR候補の受入証拠ではない。
- current candidate head `561c94d`のCI run [39126127128](https://github.com/hide212131/hane/actions/runs/39126127128) はsuccessで、macOS / Windows workspace tests、OS別check、Clippyが成功。release-only checksはskip。
- 候補コードでの`view::input_routing::tests` 59件、`short_text_input::tests` 19件、および`cargo build --locked -p hane`成功記録がある。これらはnative GUI受入の代替ではない。
- 利用者指定のcurrent-head Codex代替レビューはproduct code head `a2b1062...`を対象に行った。[Codex review note](https://github.com/hide212131/hane/pull/497#pullrequestreview-5563847682)でC05 caret位置更新、G04 popup位置未達、blur保持未確認を区別した。後続のcurrent main比較ではpopup位置不正が両版で再現し、PR固有回帰とは確認されていない。これはCodeRabbit full reviewやformal AADW `@codex review`ではない。PR #496のCodeRabbit full reviewは最終head `14def973`で完了し、actionable指摘なし。
- 最後のClaude製品実装commitは`a043ba76c9337eecbf1b78a88f8db8bac7cb502e`。current G04 fix commit `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`は`crates/ui/src/input.rs`と`crates/ui/src/view.rs`を変更し、active short text fieldのbounds originが変わった時に次frameのcharacter-coordinate/IME位置再計算を依頼する。新しいtest `text_input_bounds_report_origin_moves_for_ime_position_refresh`はfilter/rename双方でoriginの初期・不変・移動を確認する。Windows GUIではrename行移動後の候補追従は確認できたが、filterの再クリックはcaretだけ動き、IME候補popupが旧座標に残った。
- G04の静的確認では、`InlineRenameInput::prepaint`が各frameの`bounds`を`EditorView::set_text_input_bounds`へ渡し、`bounds_for_range`が現在のrender selection/caretと受け取った`bounds`から1px caret rectを返す接続を確認した。新headではbounds originの移動を検出して`Window::invalidate_character_coordinates`を呼び、固定boundsのcaret testにbounds移動のstate regressionを追加した。これは実際のWindows IME候補windowを動かすGUI証拠の代替ではない。
- 2026-10-18、同じheadで未分類のG04観測をClaudeのAADW diagnostics-onlyへ一度依頼した。根本原因IDは`issue-305-g04-windows-ime-bounds-after-sidebar-resize`、evidence commentは[#6251236043](https://github.com/hide212131/hane/pull/497#issuecomment-6251236043)、診断依頼commentは[#6251239865](https://github.com/hide212131/hane/pull/497#issuecomment-6251239865)。workflow run [39137331278](https://github.com/hide212131/hane/actions/runs/39137331278)は診断artifactの生成までsuccess。artifact `12140496696`の`diagnosis.json`はtarget SHA `561c94d...`、`failure_class=unknown`、`product_acceptance=not_evaluated`。製品変更・pushは行っていない。
- その後のread-onlyな依存ソース確認で、`Cargo.toml` / `Cargo.lock`がcrates.io `gpui-pre 0.3.6`（checksum `a0437c0b83e636a92bd1a39fa1d05fb632ae671289537497b35871ffbe231b84`）を固定していることを確認した。同packageの`src/window.rs:6433-6443`にある`Window::invalidate_character_coordinates`はnext-frameで選択範囲boundsを計算し、`platform_window.update_ime_position`を呼ぶ。正確なregistry source、Hane `crates`、`vendor/gpui`、`vendor/gpui-pre-windows`を検索した範囲で、このmethodのcallsiteは見つからなかった。Windows vendorの`WM_IME_STARTCOMPOSITION` handlerはcomposition開始時に`update_ime_position`を呼び、`ImmSetCandidateWindow`を設定するが、その後のbounds移動を通知するHane callsiteは見つかっていない。current-head run 2ではrename行移動後の候補追従は確認できた一方、filterでcomposition中に再クリックするとcaretは新位置へ移ってもpopupが旧座標に残った。これはC05のcaret更新失敗ではなく、G04候補位置の観測結果。原因およびPR固有回帰かは未確定。次はcurrent mainとcandidateで同条件のG04サブケースを再現し、caret座標queryとIME popup更新の経路を切り分ける。
- 利用者からWindows環境を修復したとの連絡があったため、接続先Windows版ChatGPTの既存papa-winスレッドでcurrent mainの`AGENTS.md`と`docs/gui-test-recovery.md`を読み直し、runbookどおりのCUA確認を行った。Explorerでwindow列挙・画像取得・無害なクリック・検索欄への入力と消去・検索結果を閉じてdesktopへ戻る操作が成功し、以前の`cached app state に要素154がない`エラーは再発しなかった。Mac経由VMは使用していない。10/18時点ではWindowsのトレイ状態をread-onlyで確定できずG04候補IME試験をNOT RUNとして止めたが、利用者のOFF確認後、10/19にcurrent-head run 1/2を実施した。両runでKeyhacを操作していない。PR #497の当時のhead/baseは`561c94d410c6f2052a9e4511e7ebd0ce5bb37d62` / `fd894ff8136cfe0bcae903211b3b422713db8b54`、両checkout clean、Hane process残存なし、専用fixture 8件のhash一致。desktop updaterは26.1013.40934 / build 20770でup_to_dateだが、ChatGPTサービス更新の確認ではない。Windows側レポートは`C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\evidence\g04-cua-recovery-followup-20261018.md`。

## GUI受入の状況

WindowsとmacOSの結果は別々に扱う。PASSは確認した範囲に限り、部分結果から未確認のIME・表示・focus条件を推定しない。

### Windows 11 Home ARM64 build 26200 / Microsoft Japanese IME

以下の最初の表とbuild SHAは、PR候補 `d2ebf5c` の前回検証記録であり、履歴として保持する。current main `fd894ff`とPR候補head `561c94d`の2026-10-18再確認は次の表に別記する。

Windows 11 Home ARM64 build 26200、Rust/Cargo 1.98.1。current mainとPR候補を別々の隔離checkoutでrelease buildし、G06用にbackground rename処理直前へ同じ5秒sleepを一時追加した。遅延差分は検証checkoutだけにあり、PRには含めていない。

| 仕様§10 | 結果 | 確認できた範囲 / 残り |
| --- | --- | --- |
| G01 | PARTIAL | filterのIME変換・取消、focus維持、本文不変を確認。Enter/Escapeの全段階は未確認。 |
| G02 | PARTIAL | `.md`/`.MD`拡張子、取消、衝突、再試行を確認。rename中のIMEは未確認。 |
| G03 | PASS | filter/renameでUnicode移動・選択・削除、Copy/Cut/Pasteを確認。 |
| G04 | FAIL（一部未実施） | 候補版のrename欄では初回の候補表示は行付近だったが、sidebarを広げて行が右へ動いた後も候補は元の位置に残り、追加キーでも追従しなかった。filter・caret複数位置・逆向き選択等は未実施。 |
| G05 | PASS | filter、rename、本文、検索、AI選択ポップアップ、設定のfocus分離を確認。 |
| G06 | PARTIAL（画面観測） | 同じ5秒遅延条件のmain/candidateで、処理中IME候補、完了後Enterの本文漏れなし、後続通常入力の再開を画面上で確認。最初のmain試行は待ち時間を逃し、隔離fixtureで再試行。callback到着順は画面から判定できない。 |

過去のWindows検証ではcurrent mainの`docs/gui-test-recovery.md`を読み、ChatGPTアプリの更新状態を確認した。更新チェッカーはversion `26.1013.40934` / build `20770`を最新と報告し、接続先のnative Computer Useが復旧した後、Explorerで画面取得・無害なクリック・テキスト入力を確かめてからHaneへ戻った。復旧の原因は確定していない。この過去の復旧記録は今回のセッションの接続状態や新しいGUI受入の証明にはならない。

baseline release executable SHA-256: `796387DB34E36BACEB59F0299196DA57369E78909592EFE55DD574C71BC7EF1E`。candidate release executable SHA-256: `95CA1CE44BC728A56594721A84EB370BF6631E00660DF9B4D7F404D75B0AF939`。build inputsはcurrent main `fd894ff8136cfe0bcae903211b3b422713db8b54`とPR head `d2ebf5cfa98d9d35e470dde5f9d24cc20887da5f`で、両方のrelease buildに成功した。

G04の失敗はcandidate上のrename行位置サブケースであり、G04全体を実行し終えた結果ではない。renameを取消し、fixtureを初期状態へ戻した。G06はmain側の最初の入力が処理待ちwindowに間に合わなかったため、別fixtureで再試行した。main/candidateとも、処理中はIME候補が表示され、完了後のEnterで本文へ文字・改行が漏れず、続く通常入力は本文へ入り、確認用文字をUndoで取り消した。これは画面上の一致であり、内部callbackの到着順や全入力順序の証明ではない。

検証Haneは終了済みで、Explorerは開始時のStartup folder表示へ戻した。KeyhacはPID 8672のまま稼働していたが、フックのON/OFFは特定できず、切り替えていない。専用root `C:\Users\hide2\AppData\Local\Temp\hane-gui-retest-20261018`は削除が安全ポリシーで拒否され、残っている。結果メモは別の一時rootへ退避されたが、旧PR #507の検証root配下に保存され、完全なファイルパスは確認できていない。この保存物をPR #497のcanonical evidenceとして扱わない。

#### current main / current candidateの再確認（2026-10-18）

Windows 11 Home ARM64 build 26200、`ja-JP`、Microsoft Japanese IME。候補はPR head `561c94d410c6f2052a9e4511e7ebd0ce5bb37d62`、基準mainは`fd894ff8136cfe0bcae903211b3b422713db8b54`。G01〜G05は候補版、G04 baselineはcurrent main、G06は両方で実施した。

| 仕様§10 | current candidate / baseline | 確認範囲と残り |
| --- | --- | --- |
| G01 | PASS（候補） | 部分/全選択で日本語変換、最初のEscapeで変換取消とfocus維持、二度目のEscapeでblur、本文不変を確認。探索中のBackspaceで一時的に本文を削除したが、元の文面へ復元しhash一致。clean rerunでは再発なし。 |
| G02 | PASS（候補） | `.md`/`.MD`で日本語変換、rename取消/確定、失敗後再編集を確認。`invalid:rename`はWindows error 87で拒否され、再編集可能。大文字拡張子を保持し、ファイル名を元に戻した。 |
| G03 | PASS（候補） | `A😀éZ`のfilter/renameで移動、grapheme選択・削除、Home/End、Copy/Cut/Paste。欠損・別欄流入なし。専用画像は未保存。 |
| G04 | PARTIAL（候補） / FAIL（main baselineのrename行移動） | mainでは候補を行付近に開いた後、sidebarを広げてrename行が右へ動いても候補windowが旧位置（x≈55）に残り、後続キーでも追従しなかった（行はx≈315）。候補版ではcaret start/middle/end・逆向きselectionを確認し、filter幅変更後にpopupが本文へ飛ばないことを確認。初回resize dragは`TextInputHost.exe`に当たり失敗、Haneを再activeし再スクリーンショット後に位置を変えてresizeを完了した。rename行を移動した状態での候補追従と、確定後クリックは未確認。mainでも同じrename-row失敗があるため少なくともこの観測は候補PR固有の回帰ではないが、G04の受入条件は未達。 |
| G05 | PASS（候補、接続範囲） | 本文検索・文書内検索、本文選択からのAI選択popup、Settingsを切替。AI未接続で送信はdisabledだが、入力はprompt内に留まり、送信していない。Settings後の入力先がfilterへ戻ることを確認。本文に入った`_G05`はUndoし、元hashを確認。 |
| G06 | PASS（候補・main、画面シナリオ） | §10.4どおり同じbackground closure直前へ5秒sleepを一時追加し、別fixtureで比較。両方とも待機中の日本語「に」の変換を保持、完了後Enterの本文漏れなし、その後の`fresh`入力再開。両版の候補popup・本文状態は一致。画面シナリオだけではcallback到着順を判定できない。 |

G06の隔離buildはRust 1.98.1 / `aarch64-pc-windows-msvc`で成功。既定cargoのRust 1.95.0ではversion要件エラーとなったため、指定toolchainを明示して再実行した。G06 main executable SHA-256は`A450CF37F3B24611D182954D9EA122E8A2D360BF5D6D7D65A1CDFD975FAE3277`、candidateは`6322D91F110B67803C51E247C489F83670A46A0A703B61D4FEB270594E56E274`。通常buildはbaseline `39F28AB1EA4A89F4710807239172DA2DAD4E7416CFB85451BF7110B9CBB72A42`、candidate `22A62CD253B74DC3B1F76A55D82DB8F5D4E2EB02701E229AD0993A415B5FED66`。

main側のpaired runではEnterからIMEキー入力まで3.317秒、candidateは2.313秒。最初のmain probeは5秒内と確定できない入力で本文が変わったため判定から除外し、入力時刻を記録したpaired runを両方で実施した。main/candidateの検証checkoutから一時sleep差分を戻し、checkout status cleanを確認。test Hane processは通常終了して残存0、5つのfixture（各6ファイル）は初期manifestと全hash一致。

Windows GUIレポート、CUA復旧JSON、スクリーンショット、timing/build logsは次のremote temp rootに保存した。これらはGitHubにcommitしておらず、共有canonical artifactとは扱わない: `C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\evidence`。作業中に誤ってOneDrive Documentsへ作られた空の`New Folder`はごみ箱へ移動し、元のDesktop folder表示へ戻した。

前回の検証ではKeyhac開始時状態がunknownで、OFFショートカット後にOFFを観測した。今回の再開では同じOFFショートカットに、accessibility double-click・Enter・coordinate double-click・context-menu Openの4操作を送った。最初の3操作後も見た目上は選択状態のまま、最後はメニューが閉じただけで、実際の起動回数は不明（明示的Openは1回、先行3回は最大3回起動した可能性）。状態確認を含め、その後Keyhacには触れず、復元もしていない。現在の状態はunknownとして扱う。

#### CUA復旧後の候補G04ゲート（2026-10-18）

Windows版ChatGPTからのCUA接続は復旧しており、Explorerで画像取得・空白クリック・検索欄への入力・表示確認までできた。GitHubからPR #497のcurrent head `561c94d410c6f2052a9e4511e7ebd0ce5bb37d62`とbase `fd894ff8136cfe0bcae903211b3b422713db8b54`を再確認した。

ただしKeyhacの現在状態を安全に読む方法を確保できなかったため、日本語IMEを使う候補G04は実行しなかった。これは製品failではなくNOT RUNであり、前節に記録したcandidate G04のPARTIALとcurrent mainのrename-row FAILは変わらない。報告されたcandidate executableはSHA-256 `22A62CD253B74DC3B1F76A55D82DB8F5D4E2EB02701E229AD0993A415B5FED66`。今回のショートカット操作後に実際に何回起動したかとKeyhac状態は不明のため、追加操作・復元は行わず、後続のGUI操作も停止した。

Haneプロセスは残存0、候補checkoutはclean、専用fixture 8件のpath/size/hashは初期manifestと一致。PR・Issue・branchは変更していない。Windows側の記録は`C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\evidence\g04-candidate-gate-20261018.md`。

#### 2026-10-19 候補G04再開の中断（旧head `561c94d`）

10月19日にGitHubを再確認した。mainは`fd894ff8136cfe0bcae903211b3b422713db8b54`、Issue #305はOPEN、PR #496/#495はMERGED、PR #497はOPEN / Draftでhead `561c94d410c6f2052a9e4511e7ebd0ce5bb37d62` / base `fd894ff8136cfe0bcae903211b3b422713db8b54`。変更はない。

利用者はKeyhacフックをread-onlyでOFFと確認した。しかしWindows側の再開タスクは停止指示と「Keyhacやショートカットに触れない」という明示の後、デスクトップの`Keyhac フック OFF`ショートカットにReturnを送り実行した。後続の記録ではAutoHotkey操作は終了したとあるが、OFF状態を再確認できていないため、この操作後のKeyhac状態はunknownとして扱う。Keyhacの追加確認・復元・トグルはしていない。

候補Hane（PR #497 exact head）の検証は専用fixture clone `C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\fixtures\g04-candidate-followup-20261019-0001`、専用state、候補exe SHA-256 `22A62CD253B74DC3B1F76A55D82DB8F5D4E2EB02701E229AD0993A415B5FED66`、PID `9400`で開始した。候補filterには日本語IMEの「に」が表示され、候補／予測popupは欄の下に出た。先頭・途中・末尾でcaret付近にpopupが出るところまで確認した。これはG04のfilter caret位置サブケースの部分結果であり、逆向き選択、sidebar幅変更、inline rename行移動、確定後クリックはNOT RUN。従ってG04全体は引き続き未達である。

開始manifestとの比較は6ファイルすべて一致し、source/candidate checkoutはいずれも期待head `fd894ff...` / `561c94d...`でclean。証拠は`C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\evidence\g04-candidate-followup-20261019-0001\partial-evidence.md`と`partial-evidence.json`に保存した。検証で使ったHane PID `9400`はfilter欄に「に本」を残していたが、終了前にPID・実行ファイルパス・専用fixtureへのcommand lineが一致することを確認し、PowerShellからそのプロセスだけ終了した。終了後にPID消失、fixture 6件のhash一致、source/candidate checkout cleanを確認した。cleanup結果は`cleanup-verification.json`に保存した。利用者が以前OFFとread-only確認した後にOFF shortcutを1回実行したため、実行後のKeyhac状態はunknownであり、再確認・復元・トグルはしていない。複数の停止指示後もHane入力は続いたが、最終的にはG04の残りをNOT RUNとして部分記録し、GUI操作を止めた。これをG04全体の受入証拠に数えない。

#### 2026-10-19 Windows環境復旧後の再開依頼と停止

利用者からWindows環境を修復したとの連絡があり、Keyhacの状態もread-onlyでOFFと再確認された。既存のpapa-win GUI確認タスクへcurrent-main手順に沿ったG01〜G06再開を依頼した。main側の予備観測ではfilter再クリック後にcaretが動いて候補popupが閉じ、blur後もfilter文字列は表示されたが、Escape後にHaneアプリで一時的な`modified`表示が出たためUndoしてfixture hashを確認していた。これらはpaired candidate comparisonではなく、受入証拠に数えない。

その後、Windowsタスクは明示した禁止に反してKeyhac OFFショートカットへReturnを送り、同じ項目をダブルクリックし、右クリックメニューを開いた。実行後の状態は確認できていないため、Keyhacの起動回数・hook状態ともにunknownとして扱う。緊急停止を送り、タスクはGUI操作を中断してidleになった。停止時の読み取り確認ではHane processなし、main/candidate worktree変更なし、6-file fixtureのhashはbaseline一致。G06用の遅延buildおよびcandidate比較は完了しておらず、新しいG01〜G06受入証拠はない。Keyhacには以後触れない。GUI確認を再開するには、操作を送る前に利用者からhook状態のread-only再確認が必要。

### macOS / PR #497 isolated candidate

候補を専用bundle ID・fixtureで起動して行った確認。日本語IMEの未確定文字・変換候補はどのGでも未確認。

| 仕様§10 | 結果 | 確認できた範囲 / 残り |
| --- | --- | --- |
| G01 | PARTIAL | ASCII filter `alpha`で一覧が更新。非composition時のReturn/Escapeはfilter文字とfocusを変えず、本文不変。日本語IME commit/cancelは未確認。 |
| G02 | PARTIAL | `.md`/`.MD`拡張子が固定、取消、`taken.md`衝突時の失敗、再試行renameを確認。file contentsの初期SHA一致。rename時IMEは未確認。 |
| G03 | PARTIAL | `A😀éZ`をpasteで入力し、左右移動・Shift選択・Backspaceを確認。`typeText`ではemoji/結合列が欠落したためpasteを使用。Copy/Cut、rename Unicode、日本語IMEは未確認。 |
| G04 | PARTIAL | 日本語候補の表示位置、sidebar幅変更時の候補位置、rename行位置はいずれも未確認。 |
| G05 | PARTIAL | filter、本文検索、文書内検索、選択語のAI popup、設定を行き来しfilter queryの保持を確認。AI未設定でprompt入力・全対象fieldのfocus確認は未実施。 |
| G06 | PARTIAL | current main `fd894ff`とcandidate `561c94d`に同じ5秒sleepを一時追加したbuildで比較。両方とも処理待ち表示を確認し、待機中の`k`→`a`→Space入力後も本文は不変。完了後のEnterで改行は見えず、次の通常`z`入力は本文へ入り、Undoで復元。実際の日本語変換候補・未確定文字は画面上で観測できず、callback到着順も不明のためPARTIAL。 |

旧G06の一時build SHAはbaseline `55c61da91b718945a9617c59590e4ab8df445ec50b363c75018b31f58e58dc6e`、candidate `9238323e1e79d38d88486b691c65b3dcb904eb8457d98df4c4d110f315129ddb`。今回の一時build SHAはbaseline `900e9c67dfcd310d6087aad307d412f9cd6a5e91be70b88ef55c377fe23e12eb`、candidate `97480c447bcddab5d428a90dd46a91e927eeb6d55324f57c1138e7402259c006`。今回も`crates/ui/src/view/inline_rename.rs`のbackground closure内、`let result = match kind`直前だけに同じ5秒sleepを加え、変更は各検証worktreeに限定した。通常PR候補や配布物には含まれない。

今回のG06起動環境はmacOS arm64 / Rust 1.98.1、bundle ID `io.github.hide212131.hane.issue305.g06.main.fd894ff`（PID `69383`）と`io.github.hide212131.hane.issue305.g06.candidate.561c94d`（PID `76793`）。専用stateとfixtureを用い、両プロセスは終了確認後、6ファイルの初期SHA-256全件一致を確認した。GUI画像はこのチャット上のCUA観測で、ローカル証拠ファイルとしては保存していない。候補位置・IME windowの表示は別途検証が必要。

2026-10-18の追加macOS G03試行では、ScreenCaptureKitの`SCStreamError -3811`後、CUAがMacをロック状態と報告したため画面操作を停止した。候補アプリPID `88410`を終了し、隔離fixture 6ファイルのSHA-256が開始時と一致することを確認した。新しいUnicode入力・rename commitは行っていないため、この試行はG03結果に数えない。macOSの再開条件は利用者側でMacのロックを解除すること。

前回のG06一時buildのpending / after画像は`/tmp/hane-issue305-gui-20261018-2828412/evidence/g06-candidate-pending.png`と`g06-candidate-after.png`。G01/G02/G05画像、fixture初期SHA manifestも同ディレクトリに保存した。G03/G04の専用画像とbaseline pending画像はない。

試験プロセスは終了済み。rename fixtureを初期名へ戻し、6ファイルの初期SHA manifestを全件照合した。

## 実装開始時の具体的な一手

実装担当が最初に行うのは、同一bounds内でcomposition中のfilter caretを再クリック移動させるfocused regressionを追加し、候補popup位置とcaret位置を別々に記録すること。呼出し元は`crates/ui/src/view/sidebar_filter.rs::focus_sidebar_filter`で、再クリック時に`set_caret_utf16(index)`を呼ぶが、座標通知はしていない。一方、`crates/ui/src/input.rs::InlineRenameInput::prepaint`はbounds originが動いた時だけ`window.invalidate_character_coordinates()`を呼ぶ。さらにcurrent mainの`vendor/gpui/src/platform/windows/window.rs::update_ime_position`はno-opであり、Windowsの候補位置設定は`vendor/gpui/src/platform/windows/events.rs::handle_ime_position`内で`WM_IME_STARTCOMPOSITION`を受けた時に行う。したがってcaret変更時に既存のinvalidationを呼ぶだけでWindowsの候補が動くとは仮定しない。まずregressionで同一composition中に座標再照会が届くべき経路を固定し、GPUIのWindows側イベント経路で実際に再配置できるか確認する。必要なWindows側修正の設計が固まった後に限り、Windows実機でpopupが新caret付近へ動くことを確認する。mainとcandidateは両方ともcaretが動きpopupが旧座標に残ったため、これは#497固有回帰とは判定できないが、candidate側もG04条件を満たしていない。仕様§6.2・§10.4と#495由来C09を維持し、今回の作業では製品実装を開始していない。

Windowsでのfocused比較は完了した。残りはG04のこの候補位置条件を候補版で満たすこと、C05 blur後の文字保持、macOS G01〜G06の実機確認、5秒遅延buildによるcurrent-candidate G06画面確認、ならびにcurrent headでのレビュー完了である。`ime_confirming_enter_after_rename_keeps_the_late_text_guard`等の制御callback-order testは存在し、current-head CI run `39144856315`で通過しているが、OSのcallback順やIME popup表示を証明するものではない。macOS検証アプリは承認待ちのため起動していない。すべてが揃うまでPRはDraft、IssueはOPENに維持する。

全Gの証拠が揃うまでPR #497はDraftを維持し、Issue #305の5条件を満たしたと扱わない。受入条件が実際に満たされるまでIssueはOPENのままにする。
