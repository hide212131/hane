# Issue #305 実装・受入の引き継ぎ

更新日: 2026-10-19（日本時間）
対象: hide212131/hane Issue #305 / PR #497

## 方針と設計の根拠

採用済み設計を維持し、設計書を作り直さない。PR #497は短い入力欄の共通化というリファクタだけを扱い、current mainですでに確認した不具合の修正を混ぜない。IME候補popupの位置ずれはIssue #523、rename処理待ち中のIME入力が本文へ漏れる問題はIssue #524へ分け、修正はそれぞれ別PRで行う。設計の正本は[短い入力欄の実装仕様書](../refactor-rf3b-short-text-input-implementation-spec.md)で、§3・§5〜§10を維持する既存挙動の確認に使う。PR #496（設計）はmerge済み、PR #495（OS別文書端選択）はmerge済み。#495由来のC09を含む既決事項を維持する。

## GitHubの再開時点スナップショット

- mainは`fd894ff8136cfe0bcae903211b3b422713db8b54`（#522）。Issue #305はOPENで、受入条件5件はすべて未チェック。
- このスナップショット時点のPR #497はOPEN / Draft、head `f40744fa22a5b3e0786beead4990d6bebb8e9516`、製品コードhead `a2b1062f573aa28a5117ba1d50e8a9db23b33e70`、baseは上記main。36 commits ahead / 0 behind、12 files、mergeable。製品コードhead以降のPR差分は引き継ぎ文書。
- 文書更新前のhead `f40744f`に対するCI run [39178099818](https://github.com/hide212131/hane/actions/runs/39178099818)は、このhandoff文書更新の時点で実行中だった。最終判断では、GitHub PR画面で最新headのCI結果を確認する。
- 利用者指定のcurrent-head Codex代替review [5565587772](https://github.com/hide212131/hane/pull/497#pullrequestreview-5565587772)は文書差分に追加指摘なし。COMMENTでありapprovalではない。CodeRabbit full reviewはDraftのため未実施。
- Codex P1 review thread [PRRT_kwDOUETGuM6tsITw](https://github.com/hide212131/hane/pull/497#discussion_r4307731361)のpopup位置問題は、current mainとcandidateの同条件比較で両方に再現したため、#497固有の回帰ではない。比較結果とfollow-up Issue [#523](https://github.com/hide212131/hane/issues/523)を記録し、#497では解決済みにした。別PRで修正する。
- PR #496はMERGED（head `14def973e496c39b503c006db21fd187718ad683`）、PR #495はMERGED（head `76d53cbf5b33a51ed91dfa259ced76403064872a`）。

## 確認済みの受入状況

### Windows 11 Home ARM64 build 26200 / Microsoft Japanese IME

- **G04 focused比較（main対candidate）:** current main `fd894ff`とcandidate `a2b1062`で同じfilter操作を実施。未確定`かordinary`中にfilterを再クリックするとcaretは約`(26,115)`から`(50,115)`へ移動したが、IME popupは両版とも約`(27,143)`に残った。C05のcaret位置更新は両方PASS、G04のpopup追従は両方FAIL。PR固有回帰とは言えないが、candidateもこの受入条件を満たさない。Windows記録は`evidence/g04-main-vs-candidate-20261019-0001/`（Windows local temp、GitHub未upload）。
- **候補版C05 blur:** 未確定`かordinary`を表示中にfilterから本文へfocusを移し、filter文字列の保持と本文caret移動を確認。文字列保持はPASS。popupはblur後も旧filter座標に残ったためpopup位置の合格証拠ではない。候補PID `12228`の正常終了、fixture 6件の名前・サイズ・SHA-256一致、IMEのAモード復帰、checkout cleanを確認。CUA画像は画面上で確認したがローカル保存・decodeはしていない。
- **current-main G06遅延確認:** rename background closure内の処理直前に5秒sleepを加えた隔離buildで、pending中に送った未確定「あ」がrename完了後に本文へ入った。後続の通常入力は受け付けられたため、current mainのこのG06シナリオはFAIL。patch SHA-256 `902FF7AEA9ACF32C2D647CD844213B82E407130421E23E9DC1E0767E7FF40415`、exe SHA-256 `4D6DD6AC6F43CEF4C48DDEA9A6891808EAF3B9211B01A0BDAE2F0B49537C0742`。試験PID `7164`の正常終了、fixture 6件の復元・hash一致、checkout head/status一致を記録した。Windows evidenceは`evidence/g06-delayed-20261019-0001/`（local temp、GitHub未upload）。
- **candidate G06:** 未実施。current-mainのFAILをcandidateの結果として扱わない。過去の制御GPUI callback-order testも、現candidateの実OS GUI証拠の代用にしない。
- current-main G06のFAILはfollow-up Issue [#524](https://github.com/hide212131/hane/issues/524)・別PRで扱う。candidateのWindows G06比較はまだ未実施で、#497に回帰がないとの実画面証拠はない。

### Windows接続・Keyhac・停止後の状態

- 以前の比較では利用者がKeyhac hook OFFをread-onlyで確認し、その時点のデスクトップ状態表示もWindows CUAでOFFと読めた。その後の停止済みWindowsタスク履歴には、停止指示後に`Keyhac フック OFF` shortcut（`KeyhacHook.ahk off`）を実行した記録がある。実行結果・現在のhook状態は確認できていないため、**現在状態はunknown**とする。OFF shortcutの実行や再実行で確認しない。
- 停止済みcandidate試行について、Windows側記録に食い違いがある。一つのworker報告はcandidate Hane PID `7600`を起動してfilterをクリックした後、終了・fixture cleanup前に停止したとしている。別のhandoff記述は同PIDの正常終了とfixture一致を記録している。remote hostが現在unavailableなため照合できず、PID・fixtureの最終状態は未確認として扱う。該当fixture候補は`C:\Users\hide2\AppData\Local\Temp\hane-issue305-g04-main-retest-20261018-01\fixtures\g04-filter-blur-a2b106-20261019-0003`。
- 利用者は現在状態をデスクトップに表示できるようにしたと報告したが、本セッションのCUAにはmacOSアプリ・ブラウザしかなく、papa-win側のスレッド一覧も取得時にtimeoutした。新しい表示値はまだ読めていない。Windows GUIを再開する前にWindows ChatGPT CUAを再接続し、表示値を読み取り専用で確認するとともに、該当PIDと専用fixtureの状態を確認する。
- Windows操作手順はmainの[GUI recovery runbook](../gui-test-recovery.md)に従う。MacからVMware経由でWindows画面を操作しない。

### macOS

current candidate `a2b1062`のARM64 release buildは作成済み（binary SHA-256 `da6b80a2b16ff0482f81bf8a7ade93a432266183bef08e3bacbbd6191feeab89`）だが、macOS G01〜G06のnative GUI受入は未実施。既存CUA復旧記録やbuild成功をGUI受入の証拠に数えない。

## PR #497の範囲と次の実装

PR #497に製品コード修正を足さない。IME候補の位置ずれは#523、rename待ち中のIME入力漏れは#524で、原因調査・回帰テスト・製品修正を別PRとして行う。current mainとcandidateで結果が同じだったG04のpopup位置問題は、#497の回帰とは扱わない。ただしIssue #305は、PRをmergeしただけで閉じず、元の受入条件の達成状況を別途確認する。

Windows CUA接続が復旧したら、デスクトップに表示した現在のKeyhac状態を読み取り専用で確認し、candidate G06をcurrent mainと同じ遅延条件で比較する。状態確認のためにshortcutを起動しない。macOSのnative GUI確認も別途必要である。

## follow-up実装担当が最初に行うこと

まず`crates/ui/src/view/sidebar_filter.rs::focus_sidebar_filter`で、composition中に同一bounds内のfilter caretを再クリック移動させるfocused regressionを追加する。caret座標とWindows IME popup座標を別々に記録し、caret移動後にOS候補位置の再照会・再配置経路が実行されたことを検証する。

既存の`crates/ui/src/input.rs::InlineRenameInput::prepaint`はbounds originが動いた時に`window.invalidate_character_coordinates()`を呼ぶ。一方、current mainの`vendor/gpui/src/platform/windows/window.rs::update_ime_position`はno-opで、実際のWindows候補位置設定は`vendor/gpui/src/platform/windows/events.rs::handle_ime_position`の`WM_IME_STARTCOMPOSITION`経路にある。したがって既存invalidationをcaret移動時に呼ぶだけでpopupが動くとは仮定せず、必要なWindows event経路を回帰で固定してから実装案を決める。仕様§6.2・§10.4と#495由来C09を維持する。

## PR #497の残作業

1. PR本文はリファクタ範囲と既存不具合の分離に合わせて更新済み。#523/#524の修正は別PRで扱う。
2. Codex P1 threadはmain/candidate比較と#523を根拠に更新・解決済み。
3. PRをレビュー可能にし、CodeRabbitの通常レビューを取得して、#497の変更に起因する指摘だけを対応する。
4. macOS GUI確認を進め、#497による文字列・範囲編集の回帰がないことを確認する。Windows candidate G06は接続が復旧したときにmain比較を行う。
5. Issue #305はOPENのままにし、5受入条件の実際の達成を別途管理する。

この引き継ぎ更新では文書のみを変更し、製品実装やIssue #305の完了処理を行わない。
