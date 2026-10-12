# RF3-A P1 検証記録

Issue: [#304](https://github.com/hide212131/hane/issues/304)

実装PR: [#477](https://github.com/hide212131/hane/pull/477)

設計正本: [入力先判定の実装仕様](https://github.com/hide212131/hane/blob/cbf468b3d622f9c3f0f2823fdb5f03bc9cc58ee0/docs/refactor-rf3a-input-routing-implementation-spec.md)

作業ブランチ: `refactor/rf3a-p1-input-routing-20261012`

開始base/current base: `c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e`

P1製品コードhead: `c7fa87e1657eb62194f7297382ff708d075fd105`

今回の記録更新直前PR head: `25c5249ee4b095b55f5bbed2239d1d78057a95e4`

## P1 の範囲と候補差分

通常編集キー14 action（Backspace/Delete、左右移動と選択、Home/Endと選択、SelectAll、Copy/Cut/Paste）の入力先判定をprivateな `view/input_routing.rs` にまとめた。優先順はsidebar filter、inline rename、本文。各actionは一度だけ対象を解決し、既存Editorまたは既存のfilter/rename helperへ渡す。Cutは同じ解決結果をコピーと削除の両方に使う。

変更ファイルは `crates/ui/src/actions.rs`、`crates/ui/src/view.rs`、新規 `crates/ui/src/view/input_routing.rs`、本記録。本文のselection/IME/Undo/Redo、filter/renameの状態所有者は移していない。特別キー（P2）、platform入力とIME callback（P3）、短い入力欄の内部状態共通化、list planner/source-first transactionの変更は含まない。

## 変更前の契約テスト

| 確認 | 結果 | 対象SHA・証拠 |
|---|---|---|
| 追加した契約テスト | GPUIテスト7件。本文/filter/renameの14 action、pending renameのCut、Unicode grapheme境界を確認 | `f538017afe51ba9b4b7091cdfbd455177bf9d73f` |
| 既存テストの削除・ignore・改名 | PR差分に該当なし | [PR #477の差分](https://github.com/hide212131/hane/pull/477/files) |
| 変更前候補のworkspace all-features tests | 成功（`cargo test --workspace --all-features --locked`） | [exact-head run 38248422587](https://github.com/hide212131/hane/actions/runs/38248422587) |
| 変更前候補のworkspace Clippy | 成功（`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`） | [exact-head run 38248422587](https://github.com/hide212131/hane/actions/runs/38248422587) |

変更前候補はテスト追加後・routing変更前のhead `f538017afe51ba9b4b7091cdfbd455177bf9d73f`。上記runはそのheadに対して実行した。

## P1 routing候補の自動検証

| 確認 | 結果 | 対象SHA・証拠 |
|---|---|---|
| workspace all-features tests、macOS/Windows | 成功 | `c7fa87e1657eb62194f7297382ff708d075fd105`、[PR CI run 38256599965 attempt 2](https://github.com/hide212131/hane/actions/runs/38256599965) |
| OS固有チェックとworkspace Clippy | 成功。macOS fallback glyph/input source、Windows IME notification dispatchを含む | 同上 |
| P1記録更新前headのcurrent-head CI | 成功。macOS/Windows workspace tests、OS固有チェック、Clippy | `25c5249ee4b095b55f5bbed2239d1d78057a95e4`、[run 38261671394](https://github.com/hide212131/hane/actions/runs/38261671394) |
| 前のcandidate verifier | 成功（workspace regression tests、Clippy） | `44762e39485da9d95fe539449802aff9abf0b15e`、[run 38256022657](https://github.com/hide212131/hane/actions/runs/38256022657) |
| `git diff --check` | 成功。base `c17dd0e` からP1候補 `c7fa87e` まで | `c7fa87e1657eb62194f7297382ff708d075fd105` |
| `cargo fmt --all -- --check` | 失敗。固定Rust 1.98.1で、baseにもある既存の127箇所の差分を検出。candidateとの比較では追加行周辺に新しいformatter差分なし | `c17dd0e` と `c7fa87e` の比較 |
| UI crate通常構成テスト | macOS・Rust/Cargo 1.99.0で426件すべて成功 | candidate `c7fa87e` の一時checkout |
| workspace通常構成locked tests | 環境エラー。開始時の空き容量121 MiBで終了コード101。テスト失敗として扱わない | candidate `c7fa87e` の一時checkout |

PR-triggered CI run 38256599965の初回attemptは`action_required`・jobなしだった。ユーザー権限で同じheadをrerunしたattempt 2が実行され、上表の全checkが成功した。

## P1の画面確認

2026-10-12にmacOS 15 ARM64で、正確なcandidate head `c7fa87e`から作った一時アプリを起動した。アプリ状態とテストファイルは専用の一時ディレクトリに分離した。

| 操作 | 結果 |
|---|---|
| 本文でRightを4回、Backspace、その後Undo | 本文の`Body target line`が一時的に`Bod target line`になり、Undoで復元。さらにHome/DeleteとUndoで元の本文に復元した |
| sidebarの`Filter files...`に`Route`を入力 | ファイル一覧だけが絞られ、本文は`Body target line`のまま |
| 本文検索欄に`fixture`を入力 | 検索欄に値が入り、1文書の検索結果を表示。本文は`Rename fixture`のまま |
| `RenameTarget`のinline renameに`RenameProbe`を入力しBackspace、Escape | 名前欄内だけで編集され、Escapeでキャンセル。ファイル名`RenameTarget.md`と本文`Rename fixture`を保持 |

すべて一時ファイル上で行い、最後に本文とファイル名が元の内容であることを確認した。この手動確認はmacOSの直接入力経路を対象とし、Windows GUI確認や日本語IMEの受入確認を代替しない。

Hosted macOS GUI run [38258736475](https://github.com/hide212131/hane/actions/runs/38258736475) は同じcandidate head `c7fa87e`で実施した。clean build、本文のASCII編集・保存・Undo/Redo・再オープン、基本的な日本語IME composition、OS scroll、GPUI click/source-offset probe（8/8）は成功。包括シナリオは`inline_syntax_boundary`で失敗した。OCR box clickがcanonical offsetと一致せず、procedure自身が独立した製品失敗として扱わないよう記録している。併せて観測されたUp移動、IME Undo、drag selection、formatting toggle/save/reopenの結果はP2/P3またはIssue #101由来の既知境界ケースを含むため、このrun単独でP1 routingの合否を決めない。[evidence artifact](https://github.com/hide212131/hane/actions/runs/38258736475)を参照。

CodeRabbit full reviewは記録更新直前head `25c5249`・base `c17dd0e`を対象に完了した（run `ee08b3b0-8027-45b8-b0c4-ea560c2be801`、[review 5492367991](https://github.com/hide212131/hane/pull/477#pullrequestreview-5492367991)）。1件のnitpickはinline renameのUnicode前方Deleteのaction-path test追加。確認した現行経路ではDeleteが既存の`next_inline_rename_boundary`を呼び、そのhelperのgrapheme境界は既存unit testで確認済みである。Jev dispositionは`follow_up`で、現行PRの製品不具合ではなく追加テスト候補として記録する。該当するaction-path testがない点は残るため、後続候補として扱う。

## 受入に残る確認

| 確認 | 状態 |
|---|---|
| macOS P1直接画面確認 | 完了（上記一時アプリ） |
| Windows P1直接画面確認 | 未実施 |
| 記録更新前head `25c5249`のCodeRabbit full review | 完了（上記review）。1件のtest-only follow-up findingを記録 |
| 記録更新後headのcurrent-head CI | 未実施。今回のcommitはこの記録のみを変更する |
| 記録更新後headのCodeRabbit full review | 未実施。current-head CI成功後、利用枠が戻ったら再依頼する |
| final head/base・差分・復旧方法の記録 | 未完了 |

P1はまだ受入済みではない。このPRはopen、Issue #304もopenのまま。Windows画面確認、記録更新後headのCI、CodeRabbit full review、最終差分と復旧方法を確認してから受入を判断する。未実施の確認をpassとして扱わない。
