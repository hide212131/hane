# RF2-B P1 実装記録

## 状態

P1 の変更前証拠を記録した段階。実装・変更後検証・独立レビューは未完了であり、この記録だけでP1を受入済みとは扱わない。

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

## 受入まで未実施

P1実装、変更後のテスト/API比較、macOS・Windows CI、CodeRabbitのcurrent-head full review、実画面検証の要否確認、最終merge、実行計画とIssue本文への完了記録。配置変更のみで関数本体・型契約・layout・UI接続を変えないと確認できた場合に限り、設計書第9.4節とRF0-04を根拠に追加GUI検証不要と記録する。
