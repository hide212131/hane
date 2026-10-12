# RF2-B P4：projection/mod.rs 分割の検証記録

## 変更前基準

- 変更前main / P4 base: `b55ffaca14b9741c0bff44c66b55d2e6b4714d59`
- 製品コード受入候補: `a61645195eaf276eaeee3d9b99e87fe2de06f12e`
- PR: [#473](https://github.com/hide212131/hane/pull/473)
- 環境: macOS arm64、Rust `1.98.1 (48a229cea 2026-09-01)`

変更前mainの `cargo test --workspace --locked` はexit 0。P4用のpresentation test inventoryはbaseと製品コード受入候補で同じ結果だった。

| presentation test target | base | P4候補 |
| --- | ---: | ---: |
| unit tests | 62 | 62 |
| `atx_headings` | 1 | 1 |
| `height_index_contract` | 4 | 4 |
| `layout_contract` | 66 | 66 |
| `line_break_whitespace` | 1 | 1 |
| `markdown_features` | 1 | 1 |
| `source_map_contract` | 9 | 9 |
| 合計（doc testsを除く） | 144 | 144 |
| doc tests | 0 | 0 |

これは `cargo test -p hane-presentation -- --list` の結果。以前の基準欄に記録した「unit 66 / integration 11」はtarget分類と件数の誤記だったため、同じbase SHAで一覧を再取得して訂正した。

## P4の変更

P4は残った行・ブロックprojectionをまとめて `projection/mod.rs` へ移す段階。P5の `projection/block.rs` は追加していない。

| 旧位置 | 新位置 | 移動した項目 |
| --- | --- | --- |
| `lib.rs` | `projection/mod.rs` | 公開入口: `BlockLine`、`BlockWindow`、`JoinedParse`、`block_line_span`、`trailing_blank_lines`、`block_heights`、`block_heights_with_disclosure`、`block_height_with_disclosure`、`present_block`、`present_block_with_list_projection`、`present_block_with_table_projection`、`present_markdown`、`present_markdown_with_disclosure`、`present_polished_line`、`parse_joined_block`、`parse_joined_span`、`expected_disclosures`、`table_delimiter_is_collapsed` |
| `lib.rs` | `projection/mod.rs` | 共有解析と内部索引: `SharedParse`、`SourceIndex<T>`、`ProjectedMarker`、`ProjectionIndex`、`ListParagraphRegion`、`ListItemProjection`とそれらの構築・検索処理 |
| `lib.rs` | `projection/mod.rs` | 行表示・開示・記号処理、list/quote/fence/image/table projection、table cell生成とsource mapping、関連private helperとimpl一式 |
| `lib.rs` | `projection/tests.rs` | root直下にあったprojection unit test 54件 |

crate rootは `mod projection;` のprivate宣言と明示的な `pub use projection::{...}` を維持。従来rootにあった公開名とsignatureを同じ定義から再exportし、新しい公開名は追加していない。P4ではblock組立を子moduleへ先取りしていない。

失敗したexact-head verifierで見つかった内部call pathは、`present_table_line` と `table_row_from_projection` をcrate内helperとして保持し、rootのprivate `use` から既存の `crate::...` pathを解決することで修正した。外部向け `pub use` には含めていない。P3の `display.rs` で一時的に広がった可視性は、P4 baseと同じ `pub(super)` に戻した。最終PR diffに `display.rs`、`layout.rs` またはintegration testの変更はない。

## テスト移動の比較

baseの `lib.rs` から `projection/tests.rs` へ移した54 test functionについて、名前、`#[test]`属性、本文を比較し、54件すべてで一致、追加・欠落・変更は0件。fixture、assertion、期待値も本文比較に含まれる。presentation test inventoryは前表のとおり144件で前後一致。integration test treeとcrate-root importにdiffはない。

## 検証

- P4製品コード候補 `a61645195eaf276eaeee3d9b99e87fe2de06f12e`: `cargo test --workspace --locked` はmacOS arm64・Rust 1.98.1でexit 0。
- 同じexact code headのtrusted verifier [run #38220846721](https://github.com/hide212131/hane/actions/runs/38220846721): workspace all-features regression testsとClippyがsuccess。
- 変更前base `b55ffaca14b9741c0bff44c66b55d2e6b4714d59`: `cargo test --workspace --locked` はexit 0。
- current PR headに対するmulti-OS CIとCodeRabbit full reviewは、GitHub上の [PR checks](https://github.com/hide212131/hane/pull/473/checks) と [PR review timeline](https://github.com/hide212131/hane/pull/473) を参照。これらの結果とそのhead SHAはPR本文にも記録する。

## GUI・復旧・次段階

設計書§9.4に基づきGUI確認は不要。P4はprivate Rust moduleへの機械的移動と内部call pathの維持だけで、UI配線、layout、描画、native inputは変更していない。

P4を戻す場合はPR #473のmerge commitをrevertする。P4受入後にmainを基準として別PRでP5を開始し、`projection/block.rs`へblock組立だけを移す。#302はP4だけでは閉じず、P5の受入後に全段階を記録して完了する。
