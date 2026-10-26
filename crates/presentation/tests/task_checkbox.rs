//! Issue #15: GFM task checkboxes. An inactive confirmed `[ ]` / `[x]` / `[X]`
//! is presented as a checkbox after the item's own bullet/number, keeps the
//! real source triple reachable through the SourceMap, and is hit-tested with
//! the same row geometry that draws it.

use hane_document::{LineId, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::BlockIndex;
use hane_presentation::testing::FixedAdvanceShaper;
use hane_presentation::{
    BlockLine, BlockWindow, LIST_DEPTH_INDENT, ListTaskMetadata, VisualBlock, VisualRange,
    block_line_span, layout_block, present_block, trailing_blank_lines,
};
use std::ops::Range;

const LINE_HEIGHT: f32 = 26.0;

/// Presents the document's single block the way the editor does. `cursor`
/// discloses the line that owns it; `render` limits the drawn lines while the
/// whole block stays available as parse context.
fn present(source: &str, cursor: Option<usize>, render: Option<Range<usize>>) -> VisualBlock {
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    assert_eq!(index.len(), 1, "fixture must be one block: {source:?}");
    let count = buffer.line_count();
    let ranges = (0..count)
        .map(|line| buffer.line_range(LineId(line)).expect("line in range"))
        .collect::<Vec<_>>();
    let texts = ranges
        .iter()
        .map(|range| buffer.text(*range).expect("line text"))
        .collect::<Vec<_>>();
    let block = index
        .block_at(ranges[0].start)
        .expect("first line has a block");
    let lines = (0..count)
        .map(|at| BlockLine {
            line: at,
            range: ranges[at],
            text: &texts[at],
            disclosure: cursor
                .filter(|cursor| {
                    ranges[at].start.0 <= *cursor
                        && (*cursor < ranges[at].end.0
                            || (at + 1 == count && *cursor == ranges[at].end.0))
                })
                .map(SourceRange::empty),
        })
        .collect::<Vec<_>>();
    let span = block_line_span(&buffer, &block).expect("block spans lines");
    present_block(
        &block,
        buffer.revision(),
        &BlockWindow {
            trailing_blank_lines: trailing_blank_lines(&buffer, &span),
            render: render.unwrap_or(0..count),
            span,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            joined: None,
            block_disclosure: None,
        },
        LINE_HEIGHT,
    )
}

fn task_of(block: &VisualBlock, line: usize) -> Option<ListTaskMetadata> {
    block.lines[line].list.as_ref()?.marker.as_ref()?.task
}

#[test]
fn inactive_task_items_show_a_checkbox_after_the_bullet() {
    let block = present("- [ ] todo\n- [x] done\n- [X] BIG", None, None);
    let texts = block
        .lines
        .iter()
        .map(|line| line.visual_text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        ["\u{2022} \u{2610} todo", "\u{2022} \u{2611} done", "\u{2022} \u{2611} BIG"]
    );

    let first = task_of(&block, 0).expect("unchecked task");
    assert!(!first.checked);
    assert_eq!(first.marker_range, SourceRange::new(2, 5));
    // "• " is four bytes; the glyph is three more.
    assert_eq!(first.checkbox_visual_range, Some(VisualRange::new(4, 7)));

    let upper = task_of(&block, 2).expect("uppercase task");
    assert!(upper.checked, "`[X]` is checked");
    // Viewing never rewrites the source: the real triple still says `[X]`.
    let line = &block.lines[2];
    let triple = &"- [ ] todo\n- [x] done\n- [X] BIG"[upper.marker_range.as_usize()];
    assert_eq!(triple, "[X]");
    assert!(
        line.source_map
            .segments
            .iter()
            .any(|segment| segment.source_range == SourceRange::new(upper.marker_range.start.0, upper.marker_range.end.0 + 1)),
        "the triple and its separator stay mapped as one hidden segment"
    );
}

#[test]
fn ordered_nested_and_quoted_items_keep_their_own_marker_before_the_checkbox() {
    let ordered = present("1. [ ] a\n2. [x] b", None, None);
    assert_eq!(ordered.lines[0].visual_text, "1. \u{2610} a");
    assert_eq!(ordered.lines[1].visual_text, "2. \u{2611} b");

    let nested = present("- A\n  - [x] child\n- [ ] B", None, None);
    assert_eq!(nested.lines[0].visual_text, "\u{2022} A");
    assert!(task_of(&nested, 0).is_none(), "a parent never owns a child's marker");
    assert_eq!(nested.lines[1].visual_text, "\u{2022} \u{2611} child");
    assert_eq!(
        task_of(&nested, 1).map(|task| task.marker_range),
        Some(SourceRange::new(8, 11))
    );
    assert_eq!(nested.lines[2].visual_text, "\u{2022} \u{2610} B");

    let quoted = present("> - [ ] q", None, None);
    assert!(
        quoted.lines[0].visual_text.contains('\u{2610}'),
        "{:?}",
        quoted.lines[0].visual_text
    );
    assert!(task_of(&quoted, 0).is_some());
}

#[test]
fn unicode_bodies_and_false_positives_are_not_converted() {
    let unicode = present("- [x] \u{65e5}\u{672c}\u{8a9e}", None, None);
    assert_eq!(unicode.lines[0].visual_text, "\u{2022} \u{2611} \u{65e5}\u{672c}\u{8a9e}");

    for source in [
        "- [] empty",
        "- [ x] spaced",
        "- [ ]no separator",
        "- text [ ] later",
        "-   [ ] padded",
    ] {
        let block = present(source, None, None);
        assert!(
            task_of(&block, 0).is_none() && !block.lines[0].visual_text.contains('\u{2610}'),
            "{source:?} must not become a checkbox: {:?}",
            block.lines[0].visual_text
        );
    }
}

#[test]
fn a_touched_item_discloses_the_original_source_and_has_no_control() {
    let source = "- [x] done\n- [ ] next";
    for cursor in [0, 2, 3, 5, 6, 9] {
        let block = present(source, Some(cursor), None);
        assert_eq!(block.lines[0].visual_text, "- [x] done", "cursor {cursor}");
        let marker = block.lines[0]
            .list
            .as_ref()
            .and_then(|list| list.marker.as_ref())
            .expect("disclosed opening marker");
        assert!(!marker.synthesized);
        let task = marker.task.expect("the disclosed row still knows its task");
        assert_eq!(task.checkbox_visual_range, None, "no control while disclosed");
        // The sibling stays inactive.
        assert_eq!(block.lines[1].visual_text, "\u{2022} \u{2610} next");
    }
}

#[test]
fn viewport_rendering_matches_the_whole_block_presentation() {
    let source = "- a\n  - b\n    - [x] c\n- [ ] d";
    let whole = present(source, None, None);
    let window = present(source, None, Some(2..4));
    for line in 2..4 {
        let from_whole = &whole.lines[line];
        let from_window = window
            .lines
            .iter()
            .find(|candidate| candidate.line_id == from_whole.line_id)
            .expect("rendered line");
        assert_eq!(from_window.visual_text, from_whole.visual_text);
        assert_eq!(
            from_window.list.as_ref().and_then(|list| list.marker.clone()),
            from_whole.list.as_ref().and_then(|list| list.marker.clone())
        );
    }
    assert_eq!(whole.lines[2].visual_text, "\u{2022} \u{2611} c");
}

#[test]
fn checkbox_hit_area_is_the_drawn_glyph_on_its_own_row() {
    let shaper = FixedAdvanceShaper::default();
    let block = present("- [ ] todo\n- A\n  - [x] child", None, None);
    let layout = layout_block(&block, 400.0, &shaper);

    // Row 0: "• ☐ todo". The glyph is the third column.
    let row = &layout.lines[0];
    let left = row.text_x_origin + 16.0;
    let hit = layout.task_checkbox_at_x(&block, 0, left + 1.0, &shaper);
    assert_eq!(hit.map(|task| task.checked), Some(false));
    assert_eq!(
        hit.map(|task| task.marker_range),
        Some(SourceRange::new(2, 5))
    );
    assert!(layout.task_checkbox_at_x(&block, 0, left - 1.0, &shaper).is_none());
    assert!(layout.task_checkbox_at_x(&block, 0, left + 8.5, &shaper).is_none());
    assert!(layout.task_checkbox_at_x(&block, 0, row.text_x_origin + 4.0, &shaper).is_none());
    assert!(layout.task_checkbox_at_x(&block, 0, row.body_x_origin + 20.0, &shaper).is_none());

    // A parent without a task has nothing to press anywhere on its row.
    for x in (0..200u8).map(f32::from) {
        assert!(layout.task_checkbox_at_x(&block, 1, x, &shaper).is_none());
    }

    // The nested child is positioned by the shared list geometry, and the
    // same x on the parent's row never reaches the child's checkbox.
    let child = &layout.lines[2];
    assert!(child.text_x_origin >= LIST_DEPTH_INDENT);
    let child_left = child.text_x_origin + 16.0;
    let child_hit = layout.task_checkbox_at_x(&block, 2, child_left + 1.0, &shaper);
    assert_eq!(child_hit.map(|task| task.checked), Some(true));
    assert_eq!(
        child_hit.map(|task| task.marker_range),
        Some(SourceRange::new(19, 22))
    );
    assert!(layout.task_checkbox_at_x(&block, 1, child_left + 1.0, &shaper).is_none());
}

/// Loose lists (blank lines between items) stay one list block, so the
/// single-block `present` helper is the right context for these fixtures.
#[test]
fn loose_task_items_show_checkboxes_with_their_own_marker_ranges() {
    let source = "- [ ] A\n\n- [x] B\n\n- [X] C";
    let block = present(source, None, None);
    for (line, glyph, range, checked) in [
        (0, '\u{2610}', (2, 5), false),
        (2, '\u{2611}', (11, 14), true),
        (4, '\u{2611}', (20, 23), true),
    ] {
        let visual = &block.lines[line].visual_text;
        assert!(visual.contains(glyph), "line {line}: {visual:?}");
        assert!(!visual.contains('['), "line {line} hides the ASCII brackets: {visual:?}");
        let task = task_of(&block, line).expect("loose task");
        assert_eq!(task.checked, checked, "line {line}");
        assert_eq!(task.marker_range, SourceRange::new(range.0, range.1), "line {line}");
        assert!(task.checkbox_visual_range.is_some(), "line {line}");
        assert!(
            block.lines[line]
                .source_map
                .segments
                .iter()
                .any(|segment| segment.source_range.start.0 <= range.0
                    && range.1 <= segment.source_range.end.0),
            "line {line}: the real marker bytes stay covered by the SourceMap"
        );
    }
    // Viewing never rewrites the capital.
    assert_eq!(&source[20..23], "[X]");

    // Touching one item's own line discloses only that item.
    let touched = present(source, Some(12), None);
    assert_eq!(touched.lines[2].visual_text, "- [x] B");
    let task = task_of(&touched, 2).expect("disclosed row still knows its task");
    assert_eq!(task.checkbox_visual_range, None, "no control while disclosed");
    assert!(task_of(&touched, 0).and_then(|task| task.checkbox_visual_range).is_some());
    assert!(task_of(&touched, 4).and_then(|task| task.checkbox_visual_range).is_some());
}

#[test]
fn loose_ordered_nested_quoted_and_multibyte_items_keep_their_own_markers() {
    let ordered = present("1. [ ] a\n\n2. [x] b", None, None);
    assert_eq!(ordered.lines[0].visual_text, "1. \u{2610} a");
    assert_eq!(ordered.lines[2].visual_text, "2. \u{2611} b");
    assert_eq!(
        task_of(&ordered, 2).map(|task| task.marker_range),
        Some(SourceRange::new(13, 16))
    );

    // The parent never borrows the child's marker.
    let nested = present("- A\n\n  - [x] child\n\n- [ ] B", None, None);
    assert_eq!(nested.lines[0].visual_text, "\u{2022} A");
    assert!(task_of(&nested, 0).is_none());
    assert_eq!(
        task_of(&nested, 2).map(|task| (task.checked, task.marker_range)),
        Some((true, SourceRange::new(9, 12)))
    );
    assert_eq!(
        task_of(&nested, 4).map(|task| (task.checked, task.marker_range)),
        Some((false, SourceRange::new(22, 25)))
    );

    let quoted = present("> - [ ] a\n>\n> - [x] b", None, None);
    assert_eq!(task_of(&quoted, 0).map(|task| task.checked), Some(false));
    assert_eq!(task_of(&quoted, 2).map(|task| task.checked), Some(true));

    // CRLF and Japanese bodies: `- [ ] a\r\n\r\n- [x] 日本`.
    let crlf = present("- [ ] a\r\n\r\n- [x] \u{65e5}\u{672c}", None, None);
    assert_eq!(crlf.lines[0].visual_text, "\u{2022} \u{2610} a");
    assert_eq!(crlf.lines[2].visual_text, "\u{2022} \u{2611} \u{65e5}\u{672c}");
    assert_eq!(
        task_of(&crlf, 2).map(|task| task.marker_range),
        Some(SourceRange::new(13, 16))
    );
}

#[test]
fn wrapped_continuation_rows_and_disclosed_rows_have_no_checkbox() {
    let shaper = FixedAdvanceShaper::default();
    let source = "- [ ] aaaa bbbb cccc dddd eeee ffff";
    let block = present(source, None, None);
    let layout = layout_block(&block, 80.0, &shaper);
    assert!(layout.lines.len() > 1, "the fixture must wrap");
    let left = layout.lines[0].text_x_origin + 16.0;
    assert!(layout.task_checkbox_at_x(&block, 0, left + 1.0, &shaper).is_some());
    for row in 1..layout.lines.len() {
        assert!(layout.task_checkbox_at_x(&block, row, left + 1.0, &shaper).is_none());
    }

    let disclosed = present(source, Some(4), None);
    let layout = layout_block(&disclosed, 400.0, &shaper);
    for x in (0..200u8).map(f32::from) {
        assert!(layout.task_checkbox_at_x(&disclosed, 0, x, &shaper).is_none());
    }
}
