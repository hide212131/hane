# RF2-B P5 検証記録 — block projection の分割

確認日: 2026-10-12
Issue: [#302](https://github.com/hide212131/hane/issues/302)
実装PR: [#475](https://github.com/hide212131/hane/pull/475)
設計: [`docs/refactor-rf2b-implementation-spec.md`](refactor-rf2b-implementation-spec.md) §4.2、§5.4、§6、§8〜§11

この記録はP5の検証証拠をまとめる。ここに記載した `f1f6cc3086cd3607b2d7bc87edcdf41960bdec2c` は、実装コードを含む検証済みheadである。この後に追加する本記録と実行計画の変更は文書だけである。最終PR head、そのheadに対する必須CIと独立レビューの状態はPR #475のChecksとConversationを参照する。

## 移動範囲とAPI

受入済みP4 base `feb846cb68bb9f0abf4dbfbd64722ce513a4076d` から、block assemblyを親の `projection/mod.rs` からprivateな子module `projection/block.rs` へ分割した。

| 責務 | `projection/block.rs`へ移した項目 | 親 `projection/mod.rs`に残した項目 |
| --- | --- | --- |
| 入力と範囲 | `BlockLine`、`BlockWindow`、`block_line_span`、`trailing_blank_lines` | `JoinedParse`、`ProjectionIndex`、`SharedParse` |
| block単位の高さ | `block_heights`、`block_heights_with_disclosure`、`block_height_with_disclosure` | line kind、fence/table/listのprojectionと共有補助 |
| block表示 | `present_block`、`present_block_with_list_projection`、`present_block_with_table_projection` | `parse_joined_block`、`parse_joined_span`と行生成補助 |
| disclosureとjoined run | `disclosure_runs`、`run_uses_shared_parse`、`expected_disclosures`、`merged_disclosure`、`union_disclosure`、`joined_run_disclosure`、`present_joined_run_with_list_projection`、test-only `present_joined_run` | parse結果とindexの所有権、再利用条件 |

`projection/block.rs`はprivateのままにした。crate rootの `lib.rs` に差分はなく、従来の公開名を親moduleから再exportしている。新しい外部公開名、adapter、重複実装は加えていない。P5差分はMarkdownのblock組立配置だけで、処理順、条件、fallback、source/visual契約は変えていない。

## unit testの移動

P4 baseでは `projection/tests.rs` に54件あったunit testが、P5後も合計54件である。12件を `projection/block/tests.rs` へ移し、42件を親の `projection/tests.rs` に残した。移動前後でtest名、`#[test]`属性、signatureと関数本体を比較し、差分がないことを確認した。追加・削除されたtestはない。

| 移動前 | 移動後 |
| --- | --- |
| `projection::tests::nested_rule_projects_container_prefixes_into_source_map_and_layout_metadata` | `projection::block::tests::nested_rule_projects_container_prefixes_into_source_map_and_layout_metadata` |
| `projection::tests::a_bare_cr_hard_break_hides_its_syntax_without_touching_the_line_ending` | `projection::block::tests::a_bare_cr_hard_break_hides_its_syntax_without_touching_the_line_ending` |
| `projection::tests::an_ordinary_bare_cr_soft_break_keeps_each_physical_line_its_own_visible_text` | `projection::block::tests::an_ordinary_bare_cr_soft_break_keeps_each_physical_line_its_own_visible_text` |
| `projection::tests::line_break_padding_hides_insignificant_spaces_from_rendered_presentation` | `projection::block::tests::line_break_padding_hides_insignificant_spaces_from_rendered_presentation` |
| `projection::tests::line_break_padding_hides_lazy_continuation_indentation_from_rendered_presentation` | `projection::block::tests::line_break_padding_hides_lazy_continuation_indentation_from_rendered_presentation` |
| `projection::tests::shared_heading_kinds_follow_each_physical_lines_source_range` | `projection::block::tests::shared_heading_kinds_follow_each_physical_lines_source_range` |
| `projection::tests::list_rows_expose_semantic_geometry_policy_without_reparsing_in_layout` | `projection::block::tests::list_rows_expose_semantic_geometry_policy_without_reparsing_in_layout` |
| `projection::tests::list_row_metadata_distinguishes_continuation_and_loose_paragraphs` | `projection::block::tests::list_row_metadata_distinguishes_continuation_and_loose_paragraphs` |
| `projection::tests::ordered_row_metadata_uses_inactive_labels_for_shared_alignment` | `projection::block::tests::ordered_row_metadata_uses_inactive_labels_for_shared_alignment` |
| `projection::tests::disclosed_ordered_marker_metadata_keeps_leading_zero_and_delimiter` | `projection::block::tests::disclosed_ordered_marker_metadata_keeps_leading_zero_and_delimiter` |
| `projection::tests::code_fence_disclosure_only_expands_the_touched_fence_marker` | `projection::block::tests::code_fence_disclosure_only_expands_the_touched_fence_marker` |
| `projection::tests::nested_code_block_kind_wins_over_its_enclosing_quote` | `projection::block::tests::nested_code_block_kind_wins_over_its_enclosing_quote` |

## 検証結果

| 確認 | 対象 | 結果 |
| --- | --- | --- |
| Trusted verifier [run #38236527917](https://github.com/hide212131/hane/actions/runs/38236527917) | code head `f1f6cc3`; `cargo test --workspace --all-features --locked` と `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | 成功 |
| PR CI [run #38236675589](https://github.com/hide212131/hane/actions/runs/38236675589), attempt 2 | code head `f1f6cc3`; macOS・Windows all-features workspace testsとClippy、macOS固有2件、Windows IME通知test | 成功 |
| 通常構成 workspace tests | macOS arm64、Rust 1.98.1、`cargo test --workspace --locked`、code head `f1f6cc3` | exit 0 |
| presentation rustdoc | P4 baseとcode head `f1f6cc3`、`cargo doc --locked -p hane-presentation --no-deps` | 両方exit 0。warningは両方9件で同一。private itemへの既存リンクとtest-only `present_joined_run`への既存リンクで、P5による増加なし |
| `git diff --check` | P4 baseからcode head `f1f6cc3` | exit 0 |
| `cargo fmt --all -- --check` | workspace全体 | **失敗**。多数の既存format差分を検出。P4 baseにも `projection/mod.rs` と `projection/tests.rs` の同じ既存差分があり、移したtest本体も移動前と同じ内容だった。修正範囲外を一括formatせず、P5でformat checkが成功したとは扱わない |

macOS/Windowsのall-features CIは、通常構成で無効になるcfgの動作まで代替したものではない。通常構成は別途、上記macOS Rust 1.98.1のworkspace testで確認した。

## GUI、性能、復旧

UI配線、描画、layout、native inputに変更はない。設計書§9.4に照らして新規の実画面検証は不要と判断した。性能測定は行っておらず、性能改善を主張しない。

分割を戻す場合はPR #475のrevertを使う。後続の変更が `projection/block.rs` に入った場合は、該当する変更を確認してから戻す。

## 最終受入の参照

上記CI・通常構成・API/test移動比較は検証済みcode head `f1f6cc3` に対する記録である。文書だけを更新した最終PR headの必須CI、CodeRabbit full review、未解決thread、merge結果は[PR #475](https://github.com/hide212131/hane/pull/475)に記録する。Issue #302はP1〜P5とIssue全体の受入条件を確認するまでopenのままにする。
