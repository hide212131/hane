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

設計§9に沿い、既存handlerに対する契約テストを先に追加し、その変更前候補を検証してから、別のコミットでP2 routingを変更する。以下の5候補にはP2契約テストだけを追加した。routing製品コードを切り替える前に、各契約とworkspace tests / Clippyを検証している。最後の候補は、先行するrouting部分候補でも未接続だった§6本文専用操作6 actionだけの契約テスト。

## 変更前候補の検証

| Candidate head | Workflow run | 結果 |
|---|---|---|
| `73fe4fbc1de1f00580ef8739a0eb3db25e73c311` | [38411102761](https://github.com/hide212131/hane/actions/runs/38411102761) | macOS workspace all-features tests と Clippy 成功 |
| `2e03ce600473bd28511f187a80dff1026cdcfb5c` | [38415088569](https://github.com/hide212131/hane/actions/runs/38415088569) | workspace tests 453 passed / 1 failed、Clippy成功。追加したAI IME Escapeテストの期待値が誤っていた。テスト環境では合成中の本文が `ばdef\n` になるため、次のテスト専用変更で期待値を修正。製品動作の失敗ではない |
| `132d12d8243295bd0a2e3ad33671d637d5dc9b16` | [38418233529](https://github.com/hide212131/hane/actions/runs/38418233529) | 修正後のmacOS workspace all-features tests と Clippy 成功 |
| `e14238a6a123c2c0b92e30916b6db0c6e625b745` | [38419394944](https://github.com/hide212131/hane/actions/runs/38419394944) | macOS 26 arm64 runner。workspace tests 455 passed / 0 failed、Clippy成功 |
| `0f1edda0f467d52e77edb2784bef4ec92cded277` | [38424963164](https://github.com/hide212131/hane/actions/runs/38424963164) | §6の文書端移動4種・Undo/Redoをfilter / rename中に本文へ漏らさない2件の契約テストを追加。macOS 26 arm64、workspace tests 457 passed / 0 failed、Clippy成功。commitは `view.rs` のテスト追加のみ |

4つの先行候補には、本文・filter・renameの入力分離、Tabのlist plannerとUndo履歴、上下・Escapeの優先順、検索UIのEscape/F3などを固定するP2契約テストを追加した。後続候補 `0f1edda0` は§6の本文専用6 actionを、filterとrenameそれぞれで本文のtext / selection / undo-redo履歴を保つ契約として固定した。これらは対象routing切替前に合格した基準結果である。

## P2 routingの部分候補

| Candidate head | Workflow run | 結果 |
|---|---|---|
| `8cac74fe702b2f075010fbf81ee2b2191f895dc6` | [38422215097](https://github.com/hide212131/hane/actions/runs/38422215097) | macOS 26 arm64 runner。workspace tests 455 passed / 0 failed、Clippy成功。Enter / Shift+Enter、Tab / Shift+Tab、上下、Escape、tab切替、F3の一部をresolverへ接続した部分候補。diff監査で §6 の DocumentStart/End、SelectDocumentStart/End、Undo/Redo の6 actionが旧guardのままと判明したため、P2受入候補としては未完了。 |

この6 actionは、filter / rename中に本文操作も履歴操作も行わない変更前契約を `0f1edda0` で固定・検証した。その後、以下の候補で同じ既存guardの後にresolverを使うよう接続した。

## P2 routing候補

| Candidate head | Workflow run | 結果 |
|---|---|---|
| `b1f27640a5699ce8d6963542b588fcf96d0def00` | [38427825319](https://github.com/hide212131/hane/actions/runs/38427825319) | DocumentStart/End、SelectDocumentStart/End、Undo/Redoを共通resolverへ接続。macOS 26 arm64 runner、workspace tests 457 passed / 0 failed、Clippy成功 |

この候補では対象P2 actionすべてが共通の `input_target()` を使う。P2の受入判定はcurrent-headのPR CI、独立review、必要なmacOS / Windows実画面確認、旧分岐とdiff監査の完了後に行う。

## 検証状況

| 確認 | 状態 | 証拠 |
|---|---|---|
| P2契約テストの追加（R07–R10、対象R12、§6本文専用6 action） | 完了（対応するrouting変更前） | [PR #488](https://github.com/hide212131/hane/pull/488)、特に[run 38424963164](https://github.com/hide212131/hane/actions/runs/38424963164) |
| 変更前P2候補の workspace tests / Clippy | 完了。最終候補は455 passed、Clippy成功 | [run 38419394944](https://github.com/hide212131/hane/actions/runs/38419394944) |
| P2 routing部分候補のworkspace tests / Clippy | 成功（ただし§6の6 action未接続につき部分結果） | [run 38422215097](https://github.com/hide212131/hane/actions/runs/38422215097) |
| P2 routing全対象の候補workspace tests / Clippy | 成功。workspace tests 457 passed、Clippy成功（macOSのみ） | [run 38427825319](https://github.com/hide212131/hane/actions/runs/38427825319) |
| routing変更後のcurrent-head CI（macOS / Windowsを含む該当check） | 未実施 | — |
| routing変更後のcurrent-head独立review | 未実施 | — |
| macOSの変更対象キー入力・日本語IMEの実画面確認 | 未実施 | — |
| Windowsの変更対象キー入力・日本語IMEの実画面確認 | 未実施 | — |
| 最終diff・旧分岐・復旧方法の確認 | 未実施 | — |

「未実施」は失敗・成功のどちらも意味しない。§6の6 actionにはrouting変更前テストを追加・検証し、routing変更後候補 `b1f27640` のworkspace tests / ClippyもmacOSで成功した。Windowsを含むcurrent-head CI、GUI確認、独立reviewは別途記録し、未確認をpassへ変えない。未確認をpassとして扱わず、Issue #304はP1–P3全体の受入までopenのままとする。
