//! Visual blocks and lines, source mapping, and variable-height virtualization.

// Presentation query values are frequently inspected only while composing a frame.
#![allow(
    clippy::must_use_candidate,
    reason = "frame-composition query APIs are intentionally discardable"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "layout invariant panics are documented by their enforcing assertion"
)]
#![allow(
    clippy::doc_markdown,
    reason = "rendering documentation uses established Markdown terminology as prose"
)]
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "layout coordinates intentionally convert bounded counts between pixel and index representations"
)]
#![allow(
    clippy::float_cmp,
    reason = "layout tests and exact zero-width checks require deterministic float equality"
)]
#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "the trait implementation signature follows its related non-Copy API"
)]
#![allow(
    clippy::needless_pass_by_value,
    reason = "the layout API owns its input to keep call sites uniform"
)]
#![allow(
    clippy::struct_excessive_bools,
    reason = "the display policy mirrors independent Markdown presentation flags"
)]

mod display;
mod height_index;
mod layout;
mod projection;
mod source_map;
pub mod testing;

pub use hane_markdown::TableAlignment;

pub use display::{
    BlockDisplay, BlockKind, BlockSurface, BlockTint, BlockWeight, ImagePresentation,
    InlineDisplay, LineContext, ListAlignment, ListId, ListItemId, ListMarkerKind,
    ListMarkerMetadata, ListOwnerMetadata, ListRowMetadata, ListRowRole,
    ListStructuralPrefixMetadata, QuoteRowMetadata, StyleKind, StyleRun, TableCellDisplay,
    TableRowDisplay, VisualBlock, VisualLine, block_is_joinable, block_line_context,
    code_line_height,
};
pub use height_index::{HeightIndex, ScrollAnchor, anchored_scroll_y};
pub use layout::{
    BlockLayout, LIST_DEPTH_INDENT, LayoutLine, LayoutPoint, LineShaper, LineWrap, QUOTE_BAR_GAP,
    QUOTE_BAR_WIDTH, QUOTE_DEPTH_INDENT, TableCellFragment, TableCellLayout, VerticalMove,
    layout_block, line_visual_start,
};
pub use projection::{
    BlockLine, BlockWindow, JoinedParse, block_height_with_disclosure, block_heights,
    block_heights_with_disclosure, block_line_span, expected_disclosures, parse_joined_block,
    parse_joined_span, present_block, present_block_with_list_projection,
    present_block_with_table_projection, present_markdown, present_markdown_with_disclosure,
    present_polished_line, table_delimiter_is_collapsed, trailing_blank_lines,
};
pub use source_map::{
    BoundarySide, MappingSegment, MarkerEdge, PositionCandidate, SourceMap, Visibility,
    VisualOffset, VisualRange,
};
