# Issue #304 P3 実装準備

Issue: https://github.com/hide212131/hane/issues/304
実装仕様: [refactor-rf3a-input-routing-implementation-spec.md](../refactor-rf3a-input-routing-implementation-spec.md)
仕様の原本commit: `cbf468b3d622f9c3f0f2823fdb5f03bc9cc58ee0`
P3開始main / PR base: `a0eef8800b91aefec083d8a5cc97359da191c802`（P2 PR #488 merge後）

## 今回のPR範囲

P3だけを実装する。設計仕様§8–11を正本とし、platform文字入力8 callbackと本文InputCaptureの登録判定を既存の状態所有者のまま共通queryへ接続する。広いsidebar focus判定の定義を新しい `view/input_routing.rs` へ移す。既存の文字列・選択範囲・IME composition・undo/redo・保存・renameの所有者を変えない。

### 必須条件

- 設計§5、§8の入口別の優先順、UTF-16範囲変換、reversed selection、composition commit/cancel、after_input、caret geometry、fallback boundsを保持する。
- InputCaptureでは登録条件だけをqueryへ委譲し、handler登録→mark_frame_painted→metrics更新の順序と回数を保持する。条件を勝手に追加しない。
- 変更前契約テストを別commitで追加し、その候補で関連テスト・workspace tests・Clippyを実行してから、別commitでroutingを変更する。
- R02/R06/R11/R12と既存関連テストを実行する。PR本文・検証記録にはbefore/afterの呼出関係、8 callback、残る入口、対象SHA、実行結果、復旧方法を記録する。
- macOSとWindowsで、§11.2にある本文/filter/renameの日本語入力、IME確定/取消、選択置換、Copy/Cut/Paste、rename pending保護、検索/AIのEnter/Tab/Escape、本文Undo/Redoと保存後の再読込を隔離fixtureで確認する。P2の確認をP3後の全体確認で再実施する。未確認をpassにしない。

### 変更対象と対象外

製品ファイルは原則 `crates/ui/src/input.rs`、`crates/ui/src/capture.rs`、`crates/ui/src/view/sidebar_filter.rs`、必要最小限の `crates/ui/src/view.rs` / `view/input_routing.rs`。判定/接続に必要な回帰testとP3検証記録を追加できる。

本文・filter・renameの編集helper、文字列/UTF-16/grapheme共通化、selection/IME state移動、描画やbounds helperの大規模移動、mouse/list/planner/source transaction、検索/AI機能、Cargo/vendor/workflow/保護パスは対象外。

## 実装依頼の前提

このbranchは設計仕様と本準備文書だけを含む。製品コード・テスト変更はまだない。Claude workerは実装と関連testの変更だけを行い、shell/Git/test/build/lint/commit/pushを行わない。次工程の選択、PR merge、Issue closeもしない。CommanderはAADW v3のV3 handoffで一度だけClaudeに依頼し、Sonnet 5.5使用、trusted finalizer、exact-head検証、CI、review、GUIを別々に確認する。