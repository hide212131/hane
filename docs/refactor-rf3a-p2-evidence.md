# RF3-A P2 検証記録

Issue: [#304](https://github.com/hide212131/hane/issues/304)

設計正本: [入力先判定の実装仕様](https://github.com/hide212131/hane/blob/cbf468b3d622f9c3f0f2823fdb5f03bc9cc58ee0/docs/refactor-rf3a-input-routing-implementation-spec.md)

作業ブランチ: `refactor/rf3a-p2-input-routing-20261013`

開始時のmain / branch base: `af981629011f04f68b25c8b58e476eb3eeffa624`（P1 PR #477 のmerge commit）

## P2の範囲

P2では設計仕様§6–7に従い、本文専用キーとEnter / Shift+Enter、Tab / Shift+Tab、上下、Escape、ファイルタブ切替、検索の次 / 前を、既存の短絡評価順・副作用順を保って入力先判定へ接続する。必要な回帰テストはR07–R10とR12の対象部分。本文のlist planner、記録付き編集、selection、Undo/Redoの既存処理を維持する。PR #472はmerge済みであり、そのリスト変更を再実装しない。

P3のplatform入力8 callback、capture登録判定、IME接続、および文字列・selectionモデルの共通化は対象外。

## 開始時の状態

この初期記録の作成時点で、P2の製品コード変更とP2テスト変更はまだない。PR #477のP1はmerge済みで、開始時のmainと作業ブランチは上記SHA。変更前のP2テスト一覧、テスト実行結果、CI、review、macOS / Windowsの実画面確認はまだ記録していない。

設計§9に沿い、最初の変更では既存handlerに対する契約テストを追加し、その変更前候補の検証を確認してから、別のコミットでP2 routingを変更する。

## 検証状況

| 確認 | 状態 | 証拠 |
|---|---|---|
| P2契約テストの追加（R07–R10、対象R12） | 未実施 | — |
| 変更前P2候補のテスト一覧・workspace tests / Clippy | 未実施 | — |
| P2 routing変更後の対象テスト・workspace tests / Clippy | 未実施 | — |
| current-head CI（macOS / Windowsを含む該当check） | 未実施 | — |
| current-head独立review | 未実施 | — |
| macOSの変更対象キー入力・日本語IMEの実画面確認 | 未実施 | — |
| Windowsの変更対象キー入力・日本語IMEの実画面確認 | 未実施 | — |
| 最終diff・旧分岐・復旧方法の確認 | 未実施 | — |

「未実施」は失敗・成功のどちらも意味しない。P2の候補head、実行環境、コマンド、終了状態、およびリンクを証拠が得られた時点で追記する。未確認をpassとして扱わず、Issue #304はP1–P3全体の受入までopenのままとする。
