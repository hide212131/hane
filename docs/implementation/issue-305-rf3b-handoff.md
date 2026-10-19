# Issue #305 実装・受入の引き継ぎ

更新日: 2026-10-19（日本時間）
対象: hide212131/hane Issue #305 / PR #497

## 方針と設計の根拠

採用済み設計を維持し、設計書を作り直さない。PR #497は短い入力欄の共通化というリファクタだけを扱い、current mainですでに確認した不具合の修正を混ぜない。IME候補popupの位置ずれはIssue #523、rename処理待ち中のIME入力が本文へ漏れる問題はIssue #524へ分け、修正はそれぞれ別PRで行う。設計の正本は[短い入力欄の実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md)で、§3・§5〜§10を維持する既存挙動の確認に使う。PR #496（設計）はmerge済み、PR #495（OS別文書端選択）はmerge済み。#495由来のC09を含む既決事項を維持する。

## 2026-10-19の文書更新着手時点スナップショット

- current mainは`cf7ede089aa868542bf64a92e31b44e8e68f616d`（PR #525）。このmainの`fd894ff`以降の差分は`docs/gui-test-recovery.md`だけで、製品コードは変わっていない。したがって、製品比較はmain product SHA `fd894ff8136cfe0bcae903211b3b422713db8b54`を使い、GUI手順はcurrent mainのrunbookに従った。
- PR #497はOPEN / Ready for review、head `28d9178c5806f41eef7cabf88daa45bc2f333172`、GitHubが報告するbase SHA `fd894ff8136cfe0bcae903211b3b422713db8b54`。current mainとのcompareは39 commits ahead / 1 behind、12 files。behindの1件は上記のGUI runbook文書だけで、製品差分・今回のGUIシナリオに影響しないためbase syncは行っていない。GitHubはmergeable / CLEANと報告している。
- current-head CI run [39178241971](https://github.com/hide212131/hane/actions/runs/39178241971)はmacOS / Windowsのcargo test・Clippyがsuccess。release専用checkは対象外としてskip。
- CodeRabbitはこのOSSでmanual reviewが必要としてfull reviewをskipした。利用者指定のCodex代替review [6256886633](https://github.com/hide212131/hane/pull/497#issuecomment-6256886633)はcurrent head `28d9178`に対して「Didn't find any major issues」。REST / GraphQLでcurrent head review receiptと全review thread resolvedを確認した。Codex reviewはCodeRabbitと同等範囲の全指摘なしやformal approvalを意味しない。
- Codex P1 review thread [PRRT_kwDOUETGuM6tsITw](https://github.com/hide212131/hane/pull/497#discussion_r4307731361)のpopup位置問題は、current mainとcandidateの同条件比較で両方に再現したため、#497固有の回帰ではない。比較結果とfollow-up Issue [#523](https://github.com/hide212131/hane/issues/523)を記録し、#497では解決済みにした。別PRで修正する。
- PR #496はMERGED（head `14def973e496c39b503c006db21fd187718ad683`）、PR #495はMERGED（head `76d53cbf5b33a51ed91dfa259ced76403064872a`）。

## 確認済みの受入状況

### Windows 11 Home ARM64 build 26200 / Microsoft Japanese IME

- **G04 focused比較（main対candidate）:** current main `fd894ff`とcandidate `a2b1062`で同じfilter操作を実施。未確定`かordinary`中にfilterを再クリックするとcaretは約`(26,115)`から`(50,115)`へ移動したが、IME popupは両版とも約`(27,143)`に残った。C05のcaret位置更新は両方PASS、G04のpopup追従は両方FAIL。PR固有回帰とは言えないが、candidateもこの受入条件を満たさない。Windows記録は`evidence/g04-main-vs-candidate-20261019-0001/`（Windows local temp、GitHub未upload）。
- **候補版C05 blur:** 未確定`かordinary`を表示中にfilterから本文へfocusを移し、filter文字列の保持と本文caret移動を確認。文字列保持はPASS。popupはblur後も旧filter座標に残ったためpopup位置の合格証拠ではない。候補PID `12228`の正常終了、fixture 6件の名前・サイズ・SHA-256一致、IMEのAモード復帰、checkout cleanを確認。CUA画像は画面上で確認したがローカル保存・decodeはしていない。
- **Windows G06（main対candidate）:** 2026-10-19、Windows 11 Home ARM64 build 26200 / Microsoft Japanese IMEで完了。隔離したmain product SHA `fd894ff`とPR #497 head `28d9178`をRust 1.98.1でdebug buildし、各buildにのみrename処理前5秒sleepの検証patchを加えた。patchはPRや製品checkoutへ入れていない。main exe SHA-256 `8ED1ED2F1C524AD4F347FCE41ADE72CF6418D33FAC8C0F48B2F034E04624A2CE`、candidate exe SHA-256 `926B89CB60921B17BC59E6EEDD39BA93930EEDD16E6AA2F5F61362C4AC03D041`。
- 両buildでrename待機中にMicrosoft Japanese IMEの候補`1 あ`を画面で確認した。rename完了後にEnterを送った後、本文に「あ」や改行は見えず、本文ファイルも初期30 bytesのままだった。その後の通常入力`b`は両方で本文末尾に保存された。最終本文は31 bytes、SHA-256 `E64BF782FC253E4259D7A28987DC5E9AE1EF3FD631526C473CB1945C5A7A6E5A`で一致。ファイル名は両方`rename-completed.md`、元ファイル内容のSHA-256 `0D287EFCE3BCC4DEBEF2F5BCF53F5634FE45313808066A9B70437BBB5B92D99`を保った。
- candidateはrename完了時のIME候補表示が試行間で異なり、表示が消えた試行と`1 あ`が残った試行があった。どちらもEnter後に本文への漏れはなく、その後の`b`入力が成功した。比較ではこの差も観測事実として記録し、全条件へ一般化しない。
- G06の画面証拠、build log、hash、PID、fixture / state情報はWindows側の`C:\Users\hide2\source\Hane-Windows\pr497-g06-validation-20261019-094310-03bc79\g06-validation-report.md`と`metadata\g06-results.json`に保存した（GitHub未upload）。main PID `3664`とcandidate PID `11380`はウインドウを閉じた後に消失し、2つの登録済みGit test worktreeも削除済み。Keyhac shortcutや状態切替は行っていない。
- G06のこの比較では#524の入力漏れを再現しなかった。これはIssue [#524](https://github.com/hide212131/hane/issues/524)が解決済みであることを意味しない。IssueはOPENのまま別作業として扱い、今回の比較条件と過去証拠の差をその担当が確認する。
- 隔離tempの`build`、`bundles`、`testdata`は自動審査により再帰削除を拒否されたため`C:\Users\hide2\AppData\Local\Temp\hane-pr497-g06-20261019-094310-03bc79`に残っている。証拠と残存物を明記し、拒否を別形式で迂回しない。

### Windows接続・Keyhac・後片付け

- Windows GUIは接続先`papa-win`のCodexがComputer Useで直接操作し、main / candidate双方のG06画面証拠を保存した。手順はcurrent main `cf7ede0`の[GUI recovery runbook](../gui-test-recovery.md)に従った。
- 利用者はKeyhacの現在状態をデスクトップに表示できるようにした。今回のG06ではKeyhac状態の読み取りも切替も不要だったため、表示・shortcut・トレイには触れていない。
- G06専用のHane processと登録済みtest worktreeはすべて終了・削除済み。隔離tempの残りは上記の通りで、自動審査に拒否された削除を迂回しない。以前のG04試行のPID/fixtureに関する履歴の食い違いは今回のG06 evidenceで上書きせず、G04 reportを正とする。

### macOS

隔離したARM64検証アプリ（binary SHA-256 `da6b80a2b16ff0482f81bf8a7ade93a432266183bef08e3bacbbd6191feeab89`）で、G03とG05の一部を確認した。G03では`A😀éZ`の表示、結合文字全体の選択と削除、続けて絵文字全体の削除を確認したが、copy/cut/paste一式は未確認。G05では本文とfilterのfocus切替、bodyへの`x`入力、undo後に本文が復元されることを確認したが、全focus・selection条件の証拠ではない。検証後にアプリを終了し、fixtureは元の内容hashへ戻っていた。

macOS G01/G02/G04/G06、日本語IME変換、Unicode rename、G03のcopy/cut/pasteは未確認。Unicode rename試行は入力がfilterへ行ったため受入結果に数えない。既存CUA復旧やbuild成功をGUI受入の証拠に数えない。

## PR #497の範囲と次の実装

PR #497に製品コード修正を足さない。IME候補の位置ずれは#523、rename待ち中のIME入力漏れは#524で、原因調査・回帰テスト・製品修正を別PRとして行う。current mainとcandidateで結果が同じだったG04のpopup位置問題は、#497の回帰とは扱わない。ただしIssue #305は、PRをmergeしただけで閉じず、元の受入条件の達成状況を別途確認する。

Windows Codexがcurrent mainのGUI recovery runbookを読み、G06のmain/candidate比較を完了した。Keyhacの状態確認や切替はこのシナリオに不要だったため行っていない。残るmacOSのnative GUI確認はIssue #305の全体受入を判断する際に進める。

## follow-up実装担当が最初に行うこと

まず`crates/ui/src/view/sidebar_filter.rs::focus_sidebar_filter`で、composition中に同一bounds内のfilter caretを再クリック移動させるfocused regressionを追加する。caret座標とWindows IME popup座標を別々に記録し、caret移動後にOS候補位置の再照会・再配置経路が実行されたことを検証する。

既存の`crates/ui/src/input.rs::InlineRenameInput::prepaint`はbounds originが動いた時に`window.invalidate_character_coordinates()`を呼ぶ。一方、current mainの`vendor/gpui/src/platform/windows/window.rs::update_ime_position`はno-opで、実際のWindows候補位置設定は`vendor/gpui/src/platform/windows/events.rs::handle_ime_position`の`WM_IME_STARTCOMPOSITION`経路にある。したがって既存invalidationをcaret移動時に呼ぶだけでpopupが動くとは仮定せず、必要なWindows event経路を回帰で固定してから実装案を決める。仕様§6.2・§10.4と#495由来C09を維持する。

## PR #497の最終判断

PR #497は短い入力欄モデルへのリファクタを対象にし、#523のIME候補位置ずれと#524のrename待ち中入力漏れの修正を含めない。G04ではmain / candidate双方に同じpopup位置の問題があり、G06では今回の比較条件で入力漏れが両方とも起きなかった。どちらもPR固有の回帰を示さないが、G04のpopup位置問題はIssue #523、#524は別の追跡対象として残す。

残る確認範囲はmacOSで未確認のGUIシナリオ、G03 copy/cut/paste、native IME関連である。これらはIssue #305全体の完了を判断する際に確認し、Issueの5受入条件を満たすまでIssue #305はOPENにする。PR #497をmergeしてもIssue #305や#523/#524を自動的に閉じない。

この文書更新は検証状態だけを反映し、製品コードは変更しない。文書commit後はPRのcurrent headでCIと、CodeRabbitがskipする場合の利用者指定Codexレビューを取り直してから、PRのmerge判断を行う。
