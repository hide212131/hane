# RF2-B P3：表示型・表示方針の分割 記録

## 受入状態

2026-10-12、P3実装PR [#469](https://github.com/hide212131/hane/pull/469) はmerge済み。実装ブランチは `refactor/rf2b-p3-display-20261012`。変更前のcurrent mainは `6191362e64ba9e2d8776b523ee3f4a9b62ef35e7)、PRの受入候補headは `0260e994362e93cf2ce31851ea39e7678da11329)、merge commitおよび現在のmainは `b55ffaca14b9741c0bff44c66b55d2e6b4714d59`。
受入候補headはP3製品コードと、base更新後のPR全差分に対する検証・レビューの対象を示す。merge commitは受入後のmainを示す。現在のこの文書更新は、merge済みPRの作業ブランチ上の文書だけを追記するもので、上記のP3製品コード検証結果を変更しない。

対象設計は `docs/refactor-rf2b-implementation-spec.md` の第5.2節、第6節、第8節、第9節。P3のみを扱い、P4/P5は先取りしない。

## 移動内容

P3の製品変更は `crates/presentation/src/lib.rs` と `crates/presentation/src/display.rs`。

| 旧 `lib.rs` の責務 | 新 `display.rs` の配置 |
| --- | --- |
| inline/table表示方針 | `StyleKind`, `StyleRun`, `TableCellDisplay`, `TableRowDisplay`, `InlineDisplay` と各表示変換 |
| 行の表示文脈 | `LineContext`, `block_line_context`, `block_is_joinable`, `code_line_height` |
| block/list/quote/imageの表示metadata | `BlockKind`, `ListId`, `ListItemId`, `ListMarkerKind`, `ListRowRole`, `ListAlignment`, `ListOwnerMetadata`, `ListMarkerMetadata`, `ListStructuralPrefixMetadata`, `ListRowMetadata`, `QuoteRowMetadata`, `BlockWeight`, `BlockSurface`, `BlockTint`, `BlockDisplay`, `ImagePresentation` |
| line/block表示値 | `VisualLine`, `VisualBlock` とそれらのimpl |
| syntax表示mapping・高さhelper | `SyntaxDisplay`, `block_display_kind`, `syntax_display`, `estimated_height`、表示高さ定数 |

crate rootはprivateな `mod display;` と明示的な `pub use display::{...}` を使い、従来の公開名を維持する。設計書で許可されたSyntaxDisplayの子module向け可視性だけを調整し、module自体はprivateのまま。 `node_block` と `inline_style` は `pub(super)`、`indexed_block` と `line_context` はprivate。静的比較では、移動した実装本体・型契約の差はなく、許可済み可視性調整とintra-doc linkのmodule参照だけを確認した。

| 旧test名 | 新しい所属 |
| --- | --- |
| `inline_and_fenced_code_keep_distinct_inline_background_policies` | `display::tests` |
| `fenced_code_rows_use_the_normal_line_height` | `display::tests` |

test名・属性・fixture・assertion・本体を維持した。

## 検証結果

- 実装commitは `6bc9f402cd446aaa84cc35b2fd5c0d557d2e1dd7`。その後、current main `6191362e64ba9e2d8776b523ee3f4a9b62ef35e7` をGitHub Update Branchで取り込み、PR headは `0260e994362e93cf2ce31851ea39e7678da11329`（compare: ahead 4 / behind 0）。
- 通常構成: macOS arm64・Rust 1.98.1で、受入候補head `0260e994362e93cf2ce31851ea39e7678da11329` に対して `cargo test --workspace --locked` がexit 0で完了。
- current-head PR CI [run 38210042590](https://github.com/hide212131/hane/actions/runs/38210042590) はsuccess。macOS/Windowsのall-features workspace testsとClippy、およびmacOS fallback glyph rasterization、macOS input source reactivation、Windows IME notification dispatchが成功。
- trusted workspace tests/Clippy [run 38209018101](https://github.com/hide212131/hane/actions/runs/38209018101) は旧head `6bc9f402` を対象とするsuccess。これは受入候補headの結果として扱わない。受入候補headに対する別個のtrusted verifierは未実施。
- CodeRabbit full review [run `f7a35424-2552-4a51-a6f1-a520922ead4f`](https://github.com/hide212131/hane/pull/469#pullrequestreview-PRR_kwDOUETGuM8AAAABRw9u3w) はbase `6191362e64ba9e2d8776b523ee3f4a9b62ef35e7` から受入候補head `0260e994362e93cf2ce31851ea39e7678da11329` の3ファイルをreviewした。製品コードのactionable findingはなかった。文書テンプレートにcurrent-headの受入記録を追記するminor follow-upが1件あり、この追記で内容を補った。
- CodeRabbit pre-merge checksは4件pass。Docstring Coverage 74.07%（threshold 80%）のwarningを記録した。機械的移動の範囲を広げるdocstring追加は行わなかった。
- 設計書§9.4の機械的移動条件を満たし、UI配線・描画・layout変更がないためGUI確認は不要と判断。性能改善は主張しない。

## P4への引継ぎと復旧

P3のrollbackはPR #469のrevert。P4/P5は本PRに含めず、P3受入後にmainから別PRで進める。Issue #302はP4・P5の受入までopenのままにする。
