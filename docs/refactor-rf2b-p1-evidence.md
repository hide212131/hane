# RF2-B P1 実装記録

## 状態

第5.1節の8型とunit test 1件のファイル移動を、実装workerがファイル編集のみで適用した段階。この実装workerはshell/test/build/git実行権限を持たず、`cargo test`等は一切実行していない（下記はこの適用直後の記録であり、変更していない）。その後、対象head `7ec7b7f7751e3ffc811e23f1177ae364c655e2cb` に対して変更後の検証を実施し、結果を「変更後の検証」節に追記した。current-headに対するCodeRabbitのfull reviewと最終mergeは本docs更新時点でも未実施であり、この記録だけでP1を受入済みとは扱わない。実際に実行していない検証を成功とは書かない。

## 対象

- Issue: [#302](https://github.com/hide212131/hane/issues/302)（openを維持）
- 設計: [PR #454](https://github.com/hide212131/hane/pull/454)、merge commit `90f51e1a965588b1796f05a6744c150ac5ba4a9f`
- 詳細設計: [第5.1節・第8.1節・第9〜11節](refactor-rf2b-implementation-spec.md)
- P1の変更前main: `90f51e1a965588b1796f05a6744c150ac5ba4a9f`
- P1ブランチ: `refactor/302-source-map-p1`

## 変更前の対象

`crates/presentation/src/lib.rs`から、次の8型と全実装を `crates/presentation/src/source_map.rs` へ機械的に移す。

| 旧ファイル | 移動対象 |
| --- | --- |
| `crates/presentation/src/lib.rs` | `VisualOffset`、`VisualRange`（`new`を含む）、`Visibility`、`BoundarySide`、`MarkerEdge`、`MappingSegment`、`PositionCandidate`、`SourceMap`（全メソッドを含む） |
| crate root | 非公開 `mod source_map;` と、既存の8公開名の明示的な再export |
| crate rootの既存unit tests | `hidden_unicode_boundaries_normalize_with_affinity` は型の正規化契約を直接検証するため、名前・属性・入力・期待値・本体を保って移動予定。生成側のテストは呼出関係を確認して維持する。 |
| `crates/presentation/tests/*` | 既存integration testsは変更せず、crate root経由の利用を維持する。 |

`append_segment` と `segments_tile_range`、表示生成・layout・UI接続、依存・feature・toolchain・manifest/lockfileは対象外。

## 変更前検証

対象SHA: `90f51e1a965588b1796f05a6744c150ac5ba4a9f`。ローカル実行OSはmacOS (Darwin 25.6.0, ARM64)。実行環境の `rustc/cargo` は1.99.0、repositoryの `rust-toolchain.toml` は1.98.1を指定する。ローカルに `rustup` はなく、ローカル成功はGitHub ActionsのmacOS/Windows結果を代替しない。

| コマンド | 結果 |
| --- | --- |
| `cargo test --locked -p hane-presentation -- --list` | exit 0。lib 62、atx_headings 1、layout_contract 66、line_break_whitespace 1、markdown_features 1、source_map_contract 9。合計140件。 |
| `cargo test --locked -p hane-presentation` | exit 0。140 passed、0 failed/ignored。 |
| `cargo test --locked -p hane-presentation --all-features` | exit 0。同じ140 passed、0 failed/ignored。 |
| `cargo test --workspace --locked` | exit 0。1,060 passed、0 failed/ignored。 |
| `cargo test --workspace --locked --all-features` | exit 0。1,066 passed、0 failed/ignored。 |
| `cargo clippy --workspace --locked --all-targets --all-features -- -D warnings` | exit 0。依存crate `block v0.1.6` の将来互換性warningあり。 |
| `cargo doc --locked -p hane-presentation --no-deps` | exit 0。既存warning 9件（lib.rs内のtest-only `present_joined_run`参照を含む）。 |
| `cargo fmt --all -- --check` | exit 1。未変更の `crates/ui/src/view.rs:14674` にローカルrustfmt 1.99.0とrepository指定1.98.1の差による既存差分を検出。P1と無関係なため未修正。 |
| `git diff --check` | exit 0。 |

テスト一覧と実行ログは作業環境の `work/p1-base-test-list.log`、`work/p1-base-*` に保存済み。変更後は現在のPR headに対して同じ検証を再実行し、テスト名・属性・入力・期待値の差分と、公開API全項目を比較して追記する。CIのmacOS/Windows結果は各runの対象headとbaseを確認して別途記録する。

## 実装：移動表（ファイル編集のみ、コンパイル未確認）

編集対象のPR head（開始時点）: `13114321122ba70096e8c63a9492125e1f77d5fd`（[PR #456](https://github.com/hide212131/hane/pull/456)）。

| # | シンボル／項目 | 旧配置 | 新配置 | 扱い |
| --- | --- | --- | --- | --- |
| 1 | `VisualOffset` | `lib.rs` | `source_map.rs` | derive・フィールドそのまま移動。 |
| 2 | `VisualRange`（`VisualRange::new`を含む） | `lib.rs` | `source_map.rs` | derive・フィールド・implそのまま移動。 |
| 3 | `Visibility` | `lib.rs` | `source_map.rs` | 全variant・deriveそのまま移動。 |
| 4 | `BoundarySide` | `lib.rs` | `source_map.rs` | 全variant・deriveそのまま移動。 |
| 5 | `MarkerEdge` | `lib.rs` | `source_map.rs` | 全variant・derive・直結doc commentそのまま移動。 |
| 6 | `MappingSegment` | `lib.rs` | `source_map.rs` | 公開フィールド・derive・フィールドdoc commentそのまま移動。 |
| 7 | `PositionCandidate` | `lib.rs` | `source_map.rs` | 公開フィールド・deriveそのまま移動。 |
| 8 | `SourceMap`（`visual_to_source`、`source_to_visual`、`normalize_source`、`normalize_visual`を含む全impl） | `lib.rs` | `source_map.rs` | メソッド本体・処理順・doc commentそのまま移動。`segments`フィールドの公開も維持。 |
| 9 | crate root module宣言 | （なし） | `lib.rs` | `mod source_map;`を`mod layout;`の次に追加。moduleは非公開のまま。 |
| 10 | crate root再export | `lib.rs`（各型を直接定義） | `lib.rs`（`pub use source_map::{...}`） | 従来の8公開名（`BoundarySide`、`MappingSegment`、`MarkerEdge`、`PositionCandidate`、`SourceMap`、`Visibility`、`VisualOffset`、`VisualRange`）を変更せず再export。新しい別名・wrapperは追加していない。 |
| 11 | `hidden_unicode_boundaries_normalize_with_affinity`（unit test） | `lib.rs`の`mod tests`（他の全unit testと同じ単一module） | `source_map.rs`の新規`#[cfg(test)] mod tests` | テスト名・属性・入力・期待値・本体は変更していない。配置理由は下記。 |

### テスト配置の理由（項目11）

このテストは`present_markdown`で`VisualLine`を組み立てた上で、隠れたUnicode境界に対する`SourceMap::normalize_source`／`normalize_visual`の正規化契約（丸め先の候補選択、UTF-8境界の保持）を直接検証している。アサーション対象はすべて`source_map`の契約であり、`present_markdown`はP1でまだ移動しない`projection`責務の既存入口を使ったセットアップに過ぎない。設計書第9.1節は「別責務をまたぐテストは既存呼出関係に沿って配置を選び、理由を記録する」としており、本テストの検証責務を基準に`source_map.rs`側へ配置した。`present_markdown`はP1では`lib.rs`に残るcrate root関数のままなので、新module側に`use crate::present_markdown;`を追加し、本体の呼出しは旧`lib.rs`時点と同じ`present_markdown(...)`のまま変更していない。

### 公開API比較（名前のみ、コンパイル未確認）

crate rootの公開名は移動前後で変化していない。`BoundarySide`、`MappingSegment`、`MarkerEdge`、`PositionCandidate`、`SourceMap`、`Visibility`、`VisualOffset`、`VisualRange`の8名は、定義元が`lib.rs`直下から`source_map`moduleの`pub use`経由に変わっただけで、signature・公開フィールド・variant・derive・trait実装・cfgは変更していない。`append_segment`、`segments_tile_range`、`display`・`height_index`・`projection`・`layout`・`testing`・integration tests・UI接続・Cargo設定は未変更。

このworkerはshell実行権限を持たないため、上記の比較は差分適用後のファイル内容を読み直した上での目視確認であり、`cargo test`／`cargo doc`／`cargo fmt`／Clippyによる機械確認ではない。

## 変更後の検証

対象head: `7ec7b7f7751e3ffc811e23f1177ae364c655e2cb`（本docs修正前の実装候補、[PR #456](https://github.com/hide212131/hane/pull/456)）。変更前main/base: `90f51e1a965588b1796f05a6744c150ac5ba4a9f`。

| 項目 | 結果 |
| --- | --- |
| presentation testインベントリ（移動前後比較） | 140件のまま変化なし。移動したunit testのmodule prefixだけが変わり、並べ替え後の一覧は変更前と一致。 |
| `cargo test --locked -p hane-presentation` | 140 passed、0 failed/ignored。 |
| `cargo test --locked -p hane-presentation --all-features` | 140 passed、0 failed/ignored。 |
| `cargo test --workspace --locked` | 1,060 passed、0 failed/ignored。変更前と同数。 |
| exact-head PR CI [run 38142125957](https://github.com/hide212131/hane/actions/runs/38142125957) | head `7ec7b7f...` でmacOS/Windows双方のworkspace tests、Clippy、macOS/Windows固有testがsuccess。 |
| trusted verification [run 38141736262](https://github.com/hide212131/hane/actions/runs/38141736262) | workspace regression testsとClippyがsuccess。current-headに対する正確なPR CI証拠は上記run 38142125957を使う。 |
| `git diff --check` | exit 0。移動した8型＋全implとunit test本体はbyte-identical、crate rootの公開8名も維持。 |
| ローカル `cargo fmt --all -- --check` | rustfmt 1.99.0で未変更の `crates/ui/src/view.rs:14674` に既存差を検出。repo指定は1.98.1でローカルrustupがなく、変更前も同じ結果。この既存差分はP1と無関係であり、成功とは書かない。 |
| GUI追加検証 | 設計書第9.4節により不要と判断。関数本体・条件・型契約・UI配線・layoutを変更していないことと、T1〜T7を含む契約/workspaceテスト成功およびRF0-04既存layout契約を根拠とする。実字形/paint/inputの未確認をunitテストで確認済みとは書かない。 |

full review run `528c7787-b67e-43ab-a431-68ed26d8cb67` は、上記検証結果が反映される前の本docsの「状態」節・未実施記載と実際の検証状況の食い違いを指摘した。本節の追記はその食い違いを解消するdocs-only更新であり、新しいheadに対する最終full reviewとmergeはこの時点でも未実施である。

## 受入まで未実施

変更後のコンパイル確認、テスト名・属性・入力・期待値の前後比較、`cargo test`/`--all-features`/workspace全体、exact-head CIでのClippy、macOS・Windows CI、GUI検証要否の確認は上記「変更後の検証」節に記録済みである。一方、`cargo doc`の変更後再実行、CodeRabbitの新しいheadに対するcurrent-head full review、最終merge、実行計画とIssue本文への完了記録は本docs更新時点でも未実施であり、今回のdocs-only修正の範囲外として独立した検証job・Commanderの再観測に委ねる。
