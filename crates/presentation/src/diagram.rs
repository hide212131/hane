//! Presentation of a fenced code block as one rendered diagram.
//!
//! A diagram never replaces source lines. Every physical line of the fenced
//! construct stays in the [`VisualBlock`] with its own source range, so
//! coverage, source↔point mapping, selection and vertical movement use the
//! block's ordinary layout. The first construct line owns the diagram's height;
//! every other construct line is folded to zero height, the same way an inactive
//! fence delimiter is. Blank lines before and after the construct are not part
//! of it and keep their normal height.

use crate::display::{BlockKind, LineContext, VisualBlock, VisualLine};
use crate::source_map::{MappingSegment, SourceMap, Visibility, VisualRange};
use std::ops::Range;

/// Space above and below the picture at 100% zoom.
pub const DIAGRAM_VERTICAL_PADDING: f32 = 8.0;

/// How an inactive fenced construct is shown as a diagram: which document lines
/// it covers and how tall the row that stands for it is.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagramPlan {
    /// Absolute document lines of the construct, from the opening fence line
    /// through the closing fence line.
    pub lines: Range<usize>,
    /// Height of the owning row, picture and padding included.
    pub height: f32,
}

/// The size a picture of `natural` size is drawn at: its natural size at the
/// current zoom, scaled down (never up) to fit `available_width`, with the
/// aspect ratio kept. Layout and painting both call this, so the row height
/// always matches what is drawn.
pub fn diagram_display_size(natural: (f32, f32), available_width: f32, zoom: f32) -> (f32, f32) {
    let (width, height) = (natural.0 * zoom, natural.1 * zoom);
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return (0.0, 0.0);
    }
    let fit = if available_width.is_finite() && available_width > 0.0 {
        (available_width / width).min(1.0)
    } else {
        1.0
    };
    (width * fit, height * fit)
}

/// Height of the row that owns a picture drawn at `display_height`.
pub fn diagram_row_height(display_height: f32, zoom: f32) -> f32 {
    display_height + 2.0 * DIAGRAM_VERTICAL_PADDING * zoom
}

/// Shows the construct of `plan` as a diagram. Returns false, leaving `block`
/// as the ordinary source presentation, when the block does not hold every
/// construct line (a clipped or stale presentation cannot promise the height
/// and ranges the diagram needs) or the plan is not usable.
pub fn apply_diagram(block: &mut VisualBlock, plan: &DiagramPlan) -> bool {
    if plan.lines.is_empty()
        || !(plan.height.is_finite() && plan.height > 0.0)
        || block.lines_before != 0
        || block.lines_after != 0
    {
        return false;
    }
    let covered = plan.lines.clone().all(|line| {
        block
            .lines
            .iter()
            .any(|presented| presented.line_id == line as u64)
    });
    if !covered {
        return false;
    }
    for line in &mut block.lines {
        let id = line.line_id as usize;
        if plan.lines.contains(&id) {
            fold_into_diagram(line, id == plan.lines.start, plan.height);
        }
    }
    true
}

/// The row height a diagram-owning line of `block` stands for, if any.
pub fn diagram_height(block: &VisualBlock) -> Option<f32> {
    block
        .lines
        .iter()
        .find(|line| line.kind == BlockKind::Diagram)
        .map(VisualLine::height)
}

/// The document line that owns the diagram of `block`, if it shows one.
pub fn diagram_owner_line(block: &VisualBlock) -> Option<usize> {
    block
        .lines
        .iter()
        .find(|line| line.kind == BlockKind::Diagram)
        .map(|line| line.line_id as usize)
}

fn fold_into_diagram(line: &mut VisualLine, owner: bool, height: f32) {
    line.visual_text.clear();
    line.style_runs.clear();
    line.links.clear();
    line.kind = if owner {
        BlockKind::Diagram
    } else {
        BlockKind::CodeBlock
    };
    // One hidden segment tiles the whole physical line, so every source byte of
    // the line still maps (to the row's start) and nothing is lost on save.
    line.source_map = SourceMap {
        segments: vec![MappingSegment {
            source_range: line.source_range,
            visual_range: VisualRange::new(0, 0),
            visibility: Visibility::HiddenMarkup,
            marker_edge: None,
        }],
    };
    line.estimated_height = if owner { height } else { 0.0 };
    line.measured_height = None;
    line.context = LineContext::FencedCode;
    line.disclosure = None;
    line.image = None;
    line.list = None;
    line.quote = None;
    line.quote_marker_visual_ranges.clear();
    line.quote_marker_source_ranges.clear();
    line.table_row = None;
    line.table_header = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_size_fits_the_width_keeps_the_ratio_and_never_scales_up() {
        assert_eq!(diagram_display_size((400.0, 200.0), 800.0, 1.0), (400.0, 200.0));
        assert_eq!(diagram_display_size((400.0, 200.0), 200.0, 1.0), (200.0, 100.0));
        assert_eq!(diagram_display_size((400.0, 200.0), 1_000.0, 2.0), (800.0, 400.0));
        assert_eq!(diagram_display_size((400.0, 200.0), 400.0, 2.0), (400.0, 200.0));
    }

    #[test]
    fn display_size_refuses_degenerate_geometry() {
        for natural in [(0.0, 10.0), (10.0, 0.0), (f32::NAN, 10.0), (10.0, f32::INFINITY)] {
            assert_eq!(diagram_display_size(natural, 100.0, 1.0), (0.0, 0.0));
        }
    }

    #[test]
    fn row_height_adds_zoomed_padding_on_both_sides() {
        assert_eq!(diagram_row_height(100.0, 1.0), 100.0 + 2.0 * DIAGRAM_VERTICAL_PADDING);
        assert_eq!(diagram_row_height(100.0, 2.0), 100.0 + 4.0 * DIAGRAM_VERTICAL_PADDING);
    }
}
