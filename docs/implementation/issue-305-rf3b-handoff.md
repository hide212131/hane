# Issue #305 実装・GUI受入の引き継ぎ

更新日: 2026-10-18（日本時間）
対象: hide212131/hane Issue #305 / PR #497

## 現在の判定

採用設計を維持し、仕様書の作り直しは行わない。設計の正本は[短い入力欄の実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md)で、§3・§5〜§10を実装・受入契約とする。

Issue #305はOPENで、受入条件5件はすべて未チェック。設計PR #496のmerge、CI、Codexレビュー、部分的なOS別GUI確認をIssue完了と扱わない。このhandoff更新では製品コードを変更しない。

## current main と関連PR

- current main: `fd894ff8136cfe0bcae903211b3b422713db8b54`（#522）。
- Issue #305本文の「実装・検証状況」は、current main `fd894ff...`、PR #497 remote head `581ceee...`、CI run `39123002036`を反映していた。今回mainをローカルbranchへmergeしたため、pushとcurrent-head CIの確認後にIssue本文も更新する。受入5条件はすべて未チェック。
- PR #496: MERGED、head `14def973e496c39b503c006db21fd187718ad683`。CodeRabbit full reviewは最終headで完了し、actionable commentなし。
- PR #495: MERGED、head `76d53cbf5b33a51ed91dfa259ced76403064872a`。OS別の文書端選択挙動を受け入れ済み。#497のC09がその挙動を保護する。
- PR #497はOPEN / Draft、GitHub remote head `581ceee3d1e179ac6e77eb6fb6d6f502c7409e91`、base `aab1dd5a28dc1383d34e09e0e2f01b634ad6301a`だった。今回`origin/main` `fd894ff8136cfe0bcae903211b3b422713db8b54`をローカルbranchへmergeし、merge commitは`271453ba0b6f71270ff46b2b2b2d07dd0deee5c9`。branchはorigin/mainより0 commits behind / 27 ahead。merge-treeは競合なし、`view.rs`は自動mergeされた。変更はまだpushしていない。
- GitHub上のPRはpush前のhead `581ceee`のままで、手元の同期済みbranchとは一致していない。push後にremote head、merge state、CIを再確認する。GUI証拠はmain/candidate双方ともこの同期済みheadに対して取り直す。
- current main `fd894ff...`にはGUI失敗時の復旧runbook更新（#522）が含まれる。今回その文書と`AGENTS.md`を読み直し、Windows GUIは接続済みWindows版ChatGPTから直接操作し、Mac経由のVMware操作は使わないことを確認した。
- Windows側の既存papa-winスレッドは、`read_thread` / `wait_threads`でremote host unavailable、`send_message_to_thread`で`Codex app-server is not available`となり、更新手順の再依頼を届けられなかった。Windows側で復旧したという利用者の報告は受け取ったが、このセッションから接続復旧は独立確認できていない。Mac上のVMwareでは内包画面を取得できたが、試したキー入力では画面変化がなく、座標クリックは`noWindowsAvailableWindow`となった。current mainのrunbookがMac経由のVM操作を禁止しているため、受入操作・証拠には使わず中止した。Keyhac状態は依然unknownで、切り替えていない。current-main G04 baselineは未実施。
- #501、#503、運用基盤専用PR #514もmainへmerge済み。

## PR #497 の候補・レビュー・CI

- 最後のClaude製品実装commitは`a043ba76c9337eecbf1b78a88f8db8bac7cb502e`で、ShortTextInputへのfilter/rename移行、M01〜M15、C01〜C09を含む。今回のmerge commitではcurrent main側の`view.rs`等も入ったため、これまでのGUI結果は同期済みPR候補の受入証拠ではない。
- GUI候補head `d2ebf5c`のCI run [39082800146](https://github.com/hide212131/hane/actions/runs/39082800146) はmacOS / Windows workspace tests、OS別チェック、Clippyがsuccess。handoff更新commit `38089b06bcae34a059a88a92fc30d34517abd038`のrun [39122500854](https://github.com/hide212131/hane/actions/runs/39122500854) とremote PR head `581ceee`のrun [39123002036](https://github.com/hide212131/hane/actions/runs/39123002036) も両OSのworkspace tests・OS別check・Clippyがsuccess。今回のmerge・文書更新後のheadではCI未実施。
- 候補コードでの`view::input_routing::tests` 59件、`short_text_input::tests` 19件、および`cargo build --locked -p hane`成功記録がある。これらはnative GUI受入の代替ではない。
- 利用者指定のCodex代替レビュー [5560801376](https://github.com/hide212131/hane/pull/497#pullrequestreview-5560801376) は`d2ebf5c`を対象に追加指摘なし。CodeRabbitはDraftのためskipで、CodeRabbit full reviewではない。
- 製品コード候補は`a043ba76c9337eecbf1b78a88f8db8bac7cb502e`のまま。以後のPR変更はhandoff文書のみである。今回の静的差分再確認ではこの候補から新しい実装修正を確定していない。

## GUI受入の状況

WindowsとmacOSの結果は別々に扱う。PASSは確認した範囲に限り、部分結果から未確認のIME・表示・focus条件を推定しない。

### Windows 11 Home ARM64 build 26200 / Microsoft Japanese IME

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

### macOS / PR #497 isolated candidate

候補を専用bundle ID・fixtureで起動して行った確認。日本語IMEの未確定文字・変換候補はどのGでも未確認。

| 仕様§10 | 結果 | 確認できた範囲 / 残り |
| --- | --- | --- |
| G01 | PARTIAL | ASCII filter `alpha`で一覧が更新。非composition時のReturn/Escapeはfilter文字とfocusを変えず、本文不変。日本語IME commit/cancelは未確認。 |
| G02 | PARTIAL | `.md`/`.MD`拡張子が固定、取消、`taken.md`衝突時の失敗、再試行renameを確認。file contentsの初期SHA一致。rename時IMEは未確認。 |
| G03 | PARTIAL | `A😀éZ`をpasteで入力し、左右移動・Shift選択・Backspaceを確認。`typeText`ではemoji/結合列が欠落したためpasteを使用。Copy/Cut、rename Unicode、日本語IMEは未確認。 |
| G04 | PARTIAL | 日本語候補の表示位置、sidebar幅変更時の候補位置、rename行位置はいずれも未確認。 |
| G05 | PARTIAL | filter、本文検索、文書内検索、選択語のAI popup、設定を行き来しfilter queryの保持を確認。AI未設定でprompt入力・全対象fieldのfocus確認は未実施。 |
| G06 | PARTIAL | 同じ5秒sleepを入れた隔離main baseline / candidateでASCII入力を比較。pending中に`late`はrename field/bodyへ入らず、完了後のReturnも本文改行にならないことを確認。IME入力での同条件比較は未実施。 |

G06の一時build SHAはbaseline `55c61da91b718945a9617c59590e4ab8df445ec50b363c75018b31f58e58dc6e`、candidate `9238323e1e79d38d88486b691c65b3dcb904eb8457d98df4c4d110f315129ddb`。同じ5秒遅延patchは隔離worktreeだけに入り、製品PRには含まれない。候補側のpending / after画像は`/tmp/hane-issue305-gui-20261018-2828412/evidence/g06-candidate-pending.png`と`g06-candidate-after.png`。G01/G02/G05画像、fixture初期SHA manifestも同ディレクトリに保存した。G03/G04の専用画像とbaseline pending画像はない。

試験プロセスは終了済み。rename fixtureを初期名へ戻し、6ファイルの初期SHA manifestを全件照合した。

## 実装開始時の具体的な一手

次の実作業は、同期済みPR branchをpushしてcurrent-head CIを確認し、接続済みWindows ChatGPTからGUI復旧手順を適用してcurrent main `fd894ff...`のG04 baselineを隔離環境で完了すること。その後、同期済みPR候補headでG04を再実行して原因を分類する。candidateでも失敗する場合、既存PR上でfocused regressionを追加してからanchor更新経路を修正する。実装はAADWに従いClaudeへ依頼し、Codexは製品コードを直接変更しない。仕様§10.2、#495由来C09、採用設計を保ち、G06の5秒sleepは検証checkoutに限定する。

製品修正後はまずWindowsでG04失敗ケースを再現・確認し、macOSでは未確認のIMEケースを含むG01〜G06の不足条件を補う。Windows G04の残り（filter、caretの複数位置、逆向き選択、確定後クリック）も実行する。G06は仕様§10に従い、5秒遅延をmain baselineと候補の同じ隔離checkoutへ適用し、build SHA・画像・操作順を分けて保存する。遅延入りbuildを完成candidateの受入証拠に混ぜない。

全Gの証拠が揃うまでPR #497はDraftを維持し、Issue #305の5条件を満たしたと扱わない。受入条件が実際に満たされるまでIssueはOPENのままにする。
