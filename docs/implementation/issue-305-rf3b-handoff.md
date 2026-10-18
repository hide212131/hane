# Issue #305 実装・GUI受入の引き継ぎ

更新日: 2026-10-18（日本時間）
対象: hide212131/hane Issue #305 / PR #497

## 現在の判定

採用設計を維持し、仕様書の作り直しは行わない。設計の正本は[短い入力欄の実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md)で、§3・§5〜§10を実装・受入契約とする。

Issue #305はOPENで、受入条件5件はすべて未チェック。設計PR #496のmerge、CI、Codexレビュー、部分的なOS別GUI確認をIssue完了と扱わない。今回、製品実装とIssue本文・受入チェックは変更せず、GUI再確認の結果をこのhandoff文書に反映した。

## current main と関連PR

- current main: `fd894ff8136cfe0bcae903211b3b422713db8b54`（#522）。
- Issue #305本文はcurrent mainを古い`aab1dd5a...`と記載している。本文・チェック状態の更新はしていない。
- PR #496: MERGED、head `14def973e496c39b503c006db21fd187718ad683`。CodeRabbit full reviewは最終headで完了し、actionable commentなし。
- PR #495: MERGED、head `76d53cbf5b33a51ed91dfa259ced76403064872a`。OS別の文書端選択挙動を受け入れ済み。#497のC09がその挙動を保護する。
- PR #497: OPEN / Draft。GUI候補を確認したheadは`d2ebf5cfa98d9d35e470dde5f9d24cc20887da5f`、baseは`aab1dd5a28dc1383d34e09e0e2f01b634ad6301a`。そのheadではcurrent mainより14 commits behind / 24 ahead、GitHub merge state `CLEAN`だった。hand-off文書のcommitでPR headは進むため、次の作業前にGitHubのlive stateを読み直す。
- `git merge-tree --write-tree origin/main HEAD` は競合なしでtree `e790e6a8247f1b5e809fc20ea74009e73edc5c78`を返した。
- 前回確認したmain `71650a59...`からの差分は`docs/gui-test-recovery.md`の更新確認手順12行のみ（#522）。ChatGPTアプリの更新状態をGUI前に確認し、更新後はCUA接続を初期化してシナリオを再実行する文書変更で、製品コード変更はない。Windows側でもこの現行手順を確認した。
- #501、#503、運用基盤専用PR #514もmainへmerge済み。

## PR #497 の候補・レビュー・CI

- 製品コード・テスト候補の基点: `a043ba76c9337eecbf1b78a88f8db8bac7cb502e`。ShortTextInputへのfilter/rename移行、M01〜M15、C01〜C09を含む。現在のPR head `d2ebf5c`には後続のhandoff文書更新があり、製品コード候補SHAは変わっていない。
- GUI候補head `d2ebf5c`のCI run [39082800146](https://github.com/hide212131/hane/actions/runs/39082800146) はmacOS / Windows workspace tests、OS別チェック、Clippyがsuccess。handoff更新commit `38089b06bcae34a059a88a92fc30d34517abd038`に対するrun [39122500854](https://github.com/hide212131/hane/actions/runs/39122500854) も、両OSのworkspace tests・OS別check・Clippyがsuccess。この文書がさらに更新される場合、同runは新headのcurrent evidenceではなくなる。
- 候補コードでの`view::input_routing::tests` 59件、`short_text_input::tests` 19件、および`cargo build --locked -p hane`成功記録がある。これらはnative GUI受入の代替ではない。
- 利用者指定のCodex代替レビュー [5560801376](https://github.com/hide212131/hane/pull/497#pullrequestreview-5560801376) は`d2ebf5c`を対象に追加指摘なし。CodeRabbitはDraftのためskipで、CodeRabbit full reviewではない。

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

Windows側はcurrent mainの`docs/gui-test-recovery.md`を読み、ChatGPTアプリの更新状態を確認した。更新チェッカーはversion `26.1013.40934` / build `20770`を最新と報告し、更新は不要だった。その後、接続先のnative Computer Useが復旧し、Explorerで画面取得・無害なクリック・テキスト入力を確かめてからHaneへ戻った。復旧の原因は確定していない。

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

実装開始時は別branchで同じ機能を重複実装せず、既存PR #497のcurrent headと14 commits behindの状態をcurrent main `fd894ff...`へ同期する手順から始める。そのうえで最初の製品作業として、G04で観測した「sidebar resize後にrename行が移動してもIME候補anchorが旧位置に残る」ケースのfocused regression testを追加し、失敗を確認してからanchor更新経路を修正する。仕様§10.2の既存回帰テスト、#495由来C09と採用設計を保つ。G06の5秒sleepは検証checkout限定とし、製品差分に入れない。

製品修正後はまずWindowsでG04失敗ケースを再現・確認し、macOSでは未確認のIMEケースを含むG01〜G06の不足条件を補う。Windows G04の残り（filter、caretの複数位置、逆向き選択、確定後クリック）も実行する。G06は仕様§10に従い、5秒遅延をmain baselineと候補の同じ隔離checkoutへ適用し、build SHA・画像・操作順を分けて保存する。遅延入りbuildを完成candidateの受入証拠に混ぜない。

全Gの証拠が揃うまでPR #497はDraftを維持し、Issue #305の5条件を満たしたと扱わない。受入条件が実際に満たされるまでIssueはOPENのままにする。
