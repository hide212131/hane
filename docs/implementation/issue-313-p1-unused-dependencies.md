# Issue #313 P1: 未使用の直接依存宣言の撤去

## 目的と基準

Refs #313（RF7-D）、親 #297。基準mainは `7de394127383b50641b7345aee4866b6c3b16be0`。
この準備文書の作成時点では製品変更・実装後検証は未実施である。
RF0の構成別利用確認と、受入済み #300 の製品利用整理を起点にする。
通常・instrument・platformのRust参照を含めて確認した未使用宣言だけを撤去する。

## 実装範囲

| 変更箇所 | 変更 | 根拠と維持するもの |
| --- | --- | --- |
| `crates/app/Cargo.toml` | `hane-editor.workspace = true` を削除 | `src/`、`examples/`、`build.rs` に `hane_editor` 参照なし。instrumentにも参照なし。session/UI経由の必要なeditor依存は維持する。 |
| `crates/benchmark/Cargo.toml` | `hane-editor.workspace = true` を削除 | lib/bin双方に `hane_editor` 参照なし。benchmarkの全シナリオとworkspace memberは維持する。 |
| `Cargo.toml` | `[workspace.dependencies]` の `hane-benchmark` 宣言だけ削除 | 全memberに継承先なし。benchmarkのbinが自身のlibを `hane_benchmark` として参照することとは別。workspace membersから削除しない。 |
| `Cargo.lock` | 上記app/benchmark packageの直接依存リストから `hane-editor` を除く | package自体・version/checksum・他の依存辺を変えない。resolverが要求する変更だけにする。 |

外部コード生成・macroによる当該crate参照、featureによる依存指定、platform限定参照がないことも実装時に確認する。
workspaceの `hane-editor` 宣言は他memberが継承するため維持する。

## 受入条件と検証

1. 上記3宣言を撤去し、ソース・公開API・CLI・feature・workspace member・vendor・永続形式を変更しない。
2. 全manifestの依存宣言とRust参照の対応を確認し、通常・instrument・OS条件の削除根拠と、必要な間接依存の維持をPRに記録する。
3. `cargo metadata --locked --format-version 1` と通常/instrumentの `cargo tree` により、app/benchmarkの直接依存だけが変わり、解決するpackage/versionとfeatureに想定外の変更がないことを比較する。
4. `cargo test --workspace --all-features --locked` と `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、通常featuresの `cargo test --workspace --locked`、`cargo build --release --locked -p hane` を確認する。macOS/Windowsのrequired CIをcurrent headで確認する。検証はtrusted finalizer後の経路で行い、workerは実行しない。
5. 本差分は参照のないmanifest宣言だけを変え、製品ソース・vendor patch・有効featureが不変であることを確認できる場合、新しい実GUI・性能測定は不要とする。確認できなければその前提を推測しない。

専用の実装追従テストは追加しない。既存テスト・構成buildと依存グラフ比較を使う。
current headのCI、最終full reviewと受入判断はPRへ記録する。

## 対象外と後続

vendor patch解除、旧vendorディレクトリ削除、依存更新、feature削減、release/version変更、計測機能の変更は含めない。
並行する閲覧モード・sidebar・tab close・folder watcherの製品変更と重ねない。
本P1だけで #313 / #297 をcloseしない。全依存/feature/vendor patchの台帳、上流照合、残る構成・実OS検証は #313 に残す。
戻す場合は本PRのmanifest/lock変更を一緒にrevertする。データ変換や外部設定変更は不要。

## 判断の来歴

Jev CLI / TypeSafe `jev-latest`（実応答 `jev-1.14.0`）が、並行UI PRとの重なりを避ける次の調査対象としてRF7-Dを選択した。
利用状況の確認後、上記の限定撤去を選択（`remove_unused`、確率0.93、confidence 0.89、入力829/output46 tokens）。
Commanderは権限・current head・同一repository・実行契約を確認し、製品manifest/lockの変更はClaudeへ依頼する。
