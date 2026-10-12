# RF2-B P5 検証記録 — block projection の分割

- 確認日: 2026-10-12
- Issue: [#302](https://github.com/hide212131/hane/issues/302)
- 実装PR: [#475](https://github.com/hide212131/hane/pull/475)
- 設計: [`docs/refactor-rf2b-implementation-spec.md`](refactor-rf2b-implementation-spec.md) §4.2、§5.4、§6、§8〜§11

## 移動範囲とAPI

P5の機械的分割は、受入済みP4 base `feb846cb68bb9f0abf4dbfbd64722ce513a4076d` からblock組立を親の `projection/mod.rs` からprivateな子module `projection/block.rs` へ移した。`f1f6cc3086cd3607b2d7bc87edcdf41960bdec2c` がこの移動の検証済みcode headである。

mainには後続PR #474（commit `20291f83ccfa5e4c238fe5a7de7f25983f22b3fc`）が入り、block単位の初期高さ計算を見出し表示倍率へ対応させ、回帰testを追加した。移動後の同じ計算を保つため、Claude workerの限定修正commit `91ec96cc89934e38b5a2b7d9295871dd3ee10033` でそのロジックを `projection/block.rs` の移動先へ反映した。現在のPR候補にはmainをmerge commit `43ea2d7cde1e167bedb2e83ab04ab3058de1d199` で取り込み、P5のmodule移動とmainの変更を同じ履歴に含めた。

| 責務 | `projection/block.rs`へ移した項目 | 親 `projection/mod.rs`に残した項目 |
| --- | --- | --- |
| 入力と範囲 | `BlockLine`、`BlockWindow`、`block_line_span`、`trailing_blank_lines` | `JoinedParse`、`ProjectionIndex`、`SharedParse` |
| block単位の高さ | `block_heights`、`block_heights_with_disclosure`、`block_height_with_disclosure` | line kind、fence/table/listのprojectionと共有補助 |
| block表示 | `present_block`、`present_block_with_list_projection`、`present_block_with_table_projection` | `parse_joined_block`、`parse_joined_span`と行生成補助 |
| disclosureとjoined run | `disclosure_runs`、`run_uses_shared_parse`、`expected_disclosures`、`merged_disclosure`、`union_disclosure`、`joined_run_disclosure`、`present_joined_run_with_list_projection`、test-only `present_joined_run` | parse結果とindexの所有権、再利用条件 |

`projection/block.rs`はprivateのままにした。crate rootの `lib.rs` に差分はなく、従来の公開名を親moduleから再exportしている。新しい外部公開名、adapter、重複実装は加えていない。#474の見出し高さ計算はP5の移動先で維持した。

## unit testの移動

P4 baseには `projection/tests.rs` に54件のunit testがあった。P5の移動時点でも合計54件で、そのうち12件を `projection/block/tests.rs` へ移し、42件を親に残した。移動前後でtest名、`#[test]`属性、signatureと関数本体を比較し、移動した12件は同一であることを確認した。

その後mainの#474が `initial_block_heights_account_for_heading_scale_and_trailing_blank_lines` を追加した。現在の統合候補にはこれを1件だけ保持し、親に43件、block子moduleに12件、合計55件ある。P5によるtestの追加・削除はない。

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
| Trusted verifier [run #38236527917](https://github.com/hide212131/hane/actions/runs/38236527917) とPR CI [run #38236675589](https://github.com/hide212131/hane/actions/runs/38236675589), attempt 2 | 当初のP5 code head `f1f6cc3` | all-features workspace tests、macOS/Windows CI、ClippyとOS固有testが成功。後続main同期前の履歴として記録 |
| Claude fix verifier [run #38240449733](https://github.com/hide212131/hane/actions/runs/38240449733) | `91ec96cc`。#474高さ計算を移動先へ反映 | workspace all-features regression testsとClippyが成功 |
| macOS通常構成workspace tests | 統合候補merge commit `43ea2d7c`、macOS arm64、Rust 1.98.1、`cargo test --workspace --locked` | exit 0 |
| presentation rustdoc | 同じ統合候補、`cargo doc --locked -p hane-presentation --no-deps` | exit 0、9件のwarning。P4 baseと元P5 code headでも同じ9件で、private itemへの既存リンクとtest-only `present_joined_run`への既存リンク |
| base同期と差分 | current main `20291f83` と統合候補 `43ea2d7c` | current mainはPR branchの祖先。P5の子module移動、共有API、既存testの差分が残り、`git diff --check` はexit 0 |
| current PR head CI / CodeRabbit full review / merge | 文書更新後の最終PR head | 対象head、base、実行状態と結果を[PR #475](https://github.com/hide212131/hane/pull/475)に記録する |
| `cargo fmt --all -- --check` | Rust 1.98.1、workspace全体 | **失敗**。workspaceの広い既存format差分を検出。P4 baseからの確認でもprojection内に同じ差分があり、今回format成功とは扱わない |

macOS/Windowsのall-features CIは通常構成で無効になるcfgの動作を代替しないため、通常構成は上記macOS workspace testで別に確認した。P5はUI配線・描画・layout・native inputを変更していない。設計書§9.4に沿い、追加の実画面検証は不要と判断した。性能測定は行わず、性能改善は主張しない。

## 復旧

分割を戻す場合はPR #475のrevertを使う。後続の変更が `projection/block.rs` に入った場合は、該当変更を確認してから戻す。最終PR headのCI、review、未解決thread、merge結果はPR #475に記録する。
