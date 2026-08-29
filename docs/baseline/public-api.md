# Public API snapshot

R0時点（HEAD `3efbaac`）のworkspace crate公開面。`cargo-public-api`は環境に未導入だったため、
各crate rootのexportと公開型のimplをソースから棚卸しした。R5ではこの一覧との差分を確認する。

## hane-benchmark

- Types: `Distribution`, `Environment`, `Fixture`
- Constants: `FIXTURES`
- Functions: `distribution`, `process_memory_bytes`, `generate_fixtures`,
  `run_buffer_edit_scenario`, `run_file_open_scenario`, `run_presentation_scenario`,
  `run_layout_scenario`, `markdown_report`

## hane-document

- Value types: `SourceOffset`, `ByteLen`, `CharLen`, `Revision`, `TransactionId`, `LineId`, `LineCol`
- Data types: `SourceRange`, `Bias`, `Anchor`, `BufferError`, `InverseEdit`, `RevisionDelta`,
  `EditSummary`, `BufferSlice`, `RopeBuffer`
- Trait: `TextBuffer`
- `RopeBuffer` methods: `new`, `from_text`, `from_reader`, `full_text`, `write_to`, `line_count`,
  `resolve_anchor`, `deltas_since`, plus the `TextBuffer` implementation

## hane-editor

- Re-exports: `ImeCancelOutcome`, `ImeState`, `utf16_range_to_byte`, `Selection`
- Types: `InputMeasurement`, `InputMeasurementKind`, `EditorCommand`, `Editor`
- `Editor` methods: `new`, `from_document`, `document`, `selection`, `ime`, `set_selection`,
  `dispatch`, `insert_text`, `mark_frame_painted`, `selected_text`, `can_undo`, `can_redo`

## hane-markdown

- Types: `InlineKind`, `BlockKind`, `MarkdownSpan`, `MarkdownBlock`, `MarkdownParse`,
  `FenceDelimiter`, `BlockContextIndex`, `InlineSpan`, `LocalParse`
- Functions: `fence_delimiter`, `parse_block_context`, `parse_document`, `parse_bold`

## hane-metrics

- Types: `DurationDistribution`, `RollingWindow`, `FrameMetrics`
- Functions: `process_memory_bytes`, `duration_distribution`, `percentile`

## hane-presentation

- Types: `VisualOffset`, `VisualRange`, `Visibility`, `BoundarySide`, `MappingSegment`,
  `PositionCandidate`, `SourceMap`, `StyleKind`, `StyleRun`, `BlockKind`, `ImagePresentation`,
  `VisualBlock`, `LineSpan`, `HeightIndex`, `ScrollAnchor`
- Functions: `line_spans`, `present_bold`, `present_plain`, `present_markdown_with_disclosure`,
  `present_polished_line`, `is_table_delimiter`, `is_pipe_row`, `present_markdown`,
  `paragraph_blocks`, `anchored_scroll_y`

## hane-ui

- Re-exports: `register_key_bindings`, `EditorView`
- `EditorView` methods: `new`, `open`, `editor`, `arm_startup_timing`,
  `record_phase0_idle_memory`, `apply_phase0_background_presentation`,
  `enable_display_linked_scroll_measurement`, `set_cursor_offset_for_measurement`,
  `move_cursor_down_for_development`

## Reproduction

公開宣言の機械的な確認には次を使う。semanticな到達可能性はcrate rootの`mod` / `pub use`も
併せて確認する。

```sh
rg -n '^(pub (struct|enum|trait|type|const|fn|mod|use)|    pub fn )' crates/*/src
```

## R5 implementation diff (2026-08-29)

This snapshot remains a comparison point, not a compatibility promise. R5 deliberately narrowed
only APIs with no cross-crate product, benchmark, or integration-test consumer:

- `hane-markdown`: the incremental-parser budgets, local fallback lookback, and unused
  `LocalBlockIndex::{revision, window, len, is_empty}` accessors are implementation details;
  `has_delimiter_markers` replaces the misleading `is_delimited_inline` name.
- `hane-presentation`: `layout` is now private and reached through its existing root re-exports;
  plain/raw fallback presenters and the obsolete line-by-line paragraph helper are private.
- `hane-session`: `atomic_write_bytes` is private to state persistence and the unused
  `untitled_target` helper was removed from the crate surface.

The only addition is `IndexedBlock::provisional_paragraph`, the Markdown-owned construction path
for the UI's empty-viewport fallback. It removes the UI's direct dependency on `NodeKind` while
keeping fallback semantics unchanged.
