# RF3-A P2 検証記録

Issue: [#304](https://github.com/hide212131/hane/issues/304)

実装PR: [#488（Draft）](https://github.com/hide212131/hane/pull/488)

設計正本: [入力先判定の実装仕様](https://github.com/hide212131/hane/blob/cbf468b3d622f9c3f0f2823fdb5f03bc9cc58ee0/docs/refactor-rf3a-input-routing-implementation-spec.md)

作業ブランチ: `refactor/rf3a-p2-input-routing-20261013`

開始時のmain / branch base: `af981629011f04f68b25c8b58e476eb3eeffa624`（P1 PR #477 のmerge commit）

この検証記録を初期化したPR head: `85343f1f66981b1ec4822aace8a42d44b47c2c5f`

P2契約テスト最終候補（製品コード変更前）: `e14238a6a123c2c0b92e30916b6db0c6e625b745`

## P2の範囲

P2では設計仕様§6–7に従い、本文専用キーとEnter / Shift+Enter、Tab / Shift+Tab、上下、Escape、ファイルタブ切替、検索の次 / 前を、既存の短絡評価順・副作用順を保って入力先判定へ接続する。必要な回帰テストはR07–R10とR12の対象部分。本文のlist planner、記録付き編集、selection、Undo/Redoの既存処理を維持する。PR #472はmerge済みであり、そのリスト変更を再実装しない。

P3のplatform入力8 callback、capture登録判定、IME接続、および文字列・selectionモデルの共通化は対象外。

## 開始時の状態

初期PR head `85343f1f66981b1ec4822aace8a42d44b47c2c5f`には、この検証記録のみが追加されており、P2の製品コード変更とP2テスト変更はまだない。PR #477のP1はmerge済みで、開始時のmainと作業ブランチの親は上記SHA。

設計§9に沿い、既存handlerに対する契約テストを先に追加し、その変更前候補を検証してから、別のコミットでP2 routingを変更する。以下の4回はP2契約テストだけの変更前候補。routingの製品コード変更はまだ行っていない。

## 変更前候補の検証

| Candidate head | Workflow run | 結果 |
|---|---|---|
| `73fe4fbc1de1f00580ef8739a0eb3db25e73c311` | [38411102761](https://github.com/hide212131/hane/actions/runs/38411102761) | macOS workspace all-features tests と Clippy 成功 |
| `2e03ce600473bd28511f187a80dff1026cdcfb5c` | [38415088569](https://github.com/hide212131/hane/actions/runs/38415088569) | workspace tests 453 passed / 1 failed、Clippy成功。追加したAI IME Escapeテストの期待値が誤っていた。テスト環境では合成中の本文が `ばdef\n` になるため、次のテスト専用変更で期待値を修正。製品動作の失敗ではない |
| `132d12d8243295bd0a2e3ad33671d637d5dc9b16` | [38418233529](https://github.com/hide212131/hane/actions/runs/38418233529) | 修正後のmacOS workspace all-features tests と Clippy 成功 |
| `e14238a6a123c2c0b92e30916b6db0c6e625b745` | [38419394944](https://github.com/hide212131/hane/actions/runs/38419394944) | macOS 26 arm64 runner。workspace tests 455 passed / 0 failed、Clippy成功 |

最後の候補には、本文・フィルタ・名前変更欄の入力分離、Tabのlist plannerとUndo履歴、上下・Escapeの優先順、検索UIのEscape/F3などの既存動作を固定するP2契約テストを追加した。これらはP2 routing切替前に合格した基準結果である。

## 検証状況

| 確認 | 状態 | 証拠 |
|---|---|---|
| P2契約テストの追加（R07–R10、対象R12） | 完了（製品コード変更前） | [PR #488](https://github.com/hide212131/hane/pull/488)、上記候補run |
| 変更前P2候補の workspace tests / Clippy | 完了。最終候補は455 passed、Clippy成功 | [run 38419394944](https://github.com/hide212131/hane/actions/runs/38419394944) |
| P2 routing変更後の対象テスト・workspace tests / Clippy | 未実施 | — |
| routing変更後のcurrent-head CI（macOS / Windowsを含む該当check） | 未実施 | — |
| routing変更後のcurrent-head独立review | 未実施 | — |
| macOSの変更対象キー入力・日本語IMEの実画面確認 | 未実施 | — |
| Windowsの変更対象キー入力・日本語IMEの実画面確認 | 未実施 | — |
| 最終diff・旧分岐・復旧方法の確認 | 未実施 | — |

「未実施」は失敗・成功のどちらも意味しない。routing変更後の候補head、実行環境、コマンド、終了状態、およびリンクを証拠が得られた時点で追記する。未確認をpassとして扱わず、Issue #304はP1–P3全体の受入までopenのままとする。
