//! Text that is drawn in a line but is not in the document.
//!
//! Voice composition shows recognized speech where it would be inserted
//! without writing it into the buffer. A presented line is a visual string
//! plus a source map, so the speech becomes a [`Visibility::Synthesized`]
//! stretch of the visual string: it lays out, wraps and paints like any other
//! text, has no source width, and every position after it keeps mapping to
//! the same confirmed source offset.

use crate::{
    MappingSegment, StyleKind, StyleRun, Visibility, VisualLine, VisualOffset, VisualRange,
};
use hane_document::{Bias, SourceOffset, SourceRange};

/// One piece of virtual text to draw at a confirmed source offset.
#[derive(Clone, Debug)]
pub struct VirtualInsert<'a> {
    /// Identifies the piece in the placement the splice returns.
    pub id: u64,
    pub source: SourceOffset,
    pub text: &'a str,
    /// The piece the user is operating on; drawn stronger.
    pub emphasized: bool,
}

/// Where a piece ended up in the line's visual text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualPlacement {
    pub id: u64,
    pub visual_range: VisualRange,
}

/// Whether `line` can take virtual text at `visual`: not before a list marker,
/// a quote prefix or a table cell, and not on a line drawn as a picture or a
/// rule. A caller that cannot place a piece falls back to listing it elsewhere.
pub fn accepts_virtual_text(line: &VisualLine, visual: VisualOffset) -> bool {
    use crate::BlockKind;
    if line.table_row.is_some() || line.table_header || line.image.is_some() {
        return false;
    }
    if matches!(line.kind, BlockKind::Rule | BlockKind::Image) {
        return false;
    }
    if let Some(list) = &line.list
        && visual.0 < list.body_visual_start.0
    {
        return false;
    }
    line.quote_marker_visual_ranges
        .iter()
        .all(|range| visual.0 >= range.end.0)
}

/// Where in `line`'s visual text the caret at `source` is drawn.
pub fn visual_position(line: &VisualLine, source: SourceOffset) -> Option<VisualOffset> {
    let candidate = line.source_map.source_to_visual(source, Bias::After)?;
    let offset = candidate.visual_offset;
    line.visual_text
        .is_char_boundary(offset.0)
        .then_some(offset)
}

/// Splices every piece into `line` that the line accepts and returns where
/// each went. `inserts` must be in drawing order; pieces at the same offset
/// keep that order. Pieces the line does not accept are left out.
pub fn splice_virtual_text(
    line: &mut VisualLine,
    inserts: &[VirtualInsert<'_>],
) -> Vec<VirtualPlacement> {
    // Resolve every position against the untouched line first, then insert
    // from the back so earlier positions stay valid.
    let mut resolved: Vec<(usize, &VirtualInsert<'_>)> = inserts
        .iter()
        .filter_map(|insert| {
            let at = visual_position(line, insert.source)?;
            accepts_virtual_text(line, at).then_some((at.0, insert))
        })
        .collect();
    resolved.sort_by_key(|(at, _)| *at);
    let mut placements = Vec::with_capacity(resolved.len());
    let mut groups: Vec<(usize, Vec<&VirtualInsert<'_>>)> = Vec::new();
    for (at, insert) in resolved {
        match groups.last_mut() {
            Some((group_at, members)) if *group_at == at => members.push(insert),
            _ => groups.push((at, vec![insert])),
        }
    }
    // Final positions: every earlier group has pushed this one right.
    let mut shift = 0;
    let mut finals = Vec::with_capacity(groups.len());
    for (at, members) in &groups {
        finals.push(*at + shift);
        shift += members.iter().map(|member| member.text.len()).sum::<usize>();
    }
    for (at, members) in groups.iter().rev() {
        splice_group(line, *at, members);
    }
    for ((_, members), final_at) in groups.iter().zip(&finals) {
        let mut cursor = *final_at;
        for member in members {
            let end = cursor + member.text.len();
            placements.push(VirtualPlacement {
                id: member.id,
                visual_range: VisualRange::new(cursor, end),
            });
            cursor = end;
        }
    }
    placements
}

fn splice_group(line: &mut VisualLine, at: usize, members: &[&VirtualInsert<'_>]) {
    let total: usize = members.iter().map(|member| member.text.len()).sum();
    let joined: String = members.iter().map(|member| member.text).collect();
    line.visual_text.insert_str(at, &joined);

    // Source map: shift what starts at or after the point; split a visible
    // stretch that contains it.
    let mut segments = Vec::with_capacity(line.source_map.segments.len() + members.len() + 1);
    let mut inserted = false;
    let synthesized = |at: usize, member_offset: usize, member: &VirtualInsert<'_>| {
        MappingSegment {
            source_range: SourceRange::empty(member.source.0),
            visual_range: VisualRange::new(
                at + member_offset,
                at + member_offset + member.text.len(),
            ),
            visibility: Visibility::Virtual,
            marker_edge: None,
        }
    };
    let push_synthesized = |segments: &mut Vec<MappingSegment>| {
        let mut cursor = 0;
        for member in members {
            segments.push(synthesized(at, cursor, member));
            cursor += member.text.len();
        }
    };
    for segment in &line.source_map.segments {
        let start = segment.visual_range.start.0;
        let end = segment.visual_range.end.0;
        let splits = matches!(
            segment.visibility,
            Visibility::Visible | Visibility::ExpandedMarkup
        ) && start < at
            && at < end;
        if splits {
            let cut = segment.source_range.start.0 + (at - start);
            segments.push(MappingSegment {
                source_range: SourceRange::new(segment.source_range.start.0, cut),
                visual_range: VisualRange::new(start, at),
                ..*segment
            });
            push_synthesized(&mut segments);
            inserted = true;
            segments.push(MappingSegment {
                source_range: SourceRange::new(cut, segment.source_range.end.0),
                visual_range: VisualRange::new(at + total, end + total),
                ..*segment
            });
            continue;
        }
        if start >= at {
            if !inserted {
                push_synthesized(&mut segments);
                inserted = true;
            }
            segments.push(MappingSegment {
                visual_range: VisualRange::new(start + total, end + total),
                ..*segment
            });
        } else {
            segments.push(*segment);
        }
    }
    if !inserted {
        push_synthesized(&mut segments);
    }
    line.source_map.segments = segments;

    // Style runs: shift the later ones, stretch the one that contains the
    // point, then mark each piece.
    for run in &mut line.style_runs {
        let (start, end) = (run.visual_range.start.0, run.visual_range.end.0);
        if start >= at {
            run.visual_range = VisualRange::new(start + total, end + total);
        } else if end > at {
            run.visual_range = VisualRange::new(start, end + total);
        }
    }
    let mut cursor = at;
    for member in members {
        let range = VisualRange::new(cursor, cursor + member.text.len());
        line.style_runs.push(StyleRun {
            visual_range: range,
            kind: StyleKind::MarkedText,
        });
        if member.emphasized {
            line.style_runs.push(StyleRun {
                visual_range: range,
                kind: StyleKind::Bold,
            });
        }
        cursor = range.end.0;
    }
    for range in &mut line.quote_marker_visual_ranges {
        if range.start.0 >= at {
            *range = VisualRange::new(range.start.0 + total, range.end.0 + total);
        }
    }
    if let Some(list) = &mut line.list
        && list.body_visual_start.0 >= at
    {
        list.body_visual_start = VisualOffset(list.body_visual_start.0 + total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::present_markdown;
    use hane_document::Revision;

    fn line(source: &str) -> VisualLine {
        present_markdown(
            0,
            Revision(1),
            SourceRange::new(0, source.len()),
            source,
            26.0,
        )
    }

    fn insert(id: u64, source: usize, text: &str) -> VirtualInsert<'_> {
        VirtualInsert {
            id,
            source: SourceOffset(source),
            text,
            emphasized: false,
        }
    }

    #[test]
    fn a_piece_in_the_middle_of_a_visible_run_splits_it() {
        let mut line = line("前後の文");
        let placements = splice_virtual_text(&mut line, &[insert(7, "前".len(), "音声")]);
        assert_eq!(line.visual_text, "前音声後の文");
        assert_eq!(
            placements,
            [VirtualPlacement {
                id: 7,
                visual_range: VisualRange::new(3, 9)
            }]
        );
        // The characters after the piece still map to their own source bytes.
        let after = line
            .source_map
            .visual_to_source(VisualOffset(9), Bias::After)
            .unwrap();
        assert_eq!(after.source_offset, SourceOffset(3));
        let before = line
            .source_map
            .visual_to_source(VisualOffset(3), Bias::Before)
            .unwrap();
        assert_eq!(before.source_offset, SourceOffset(3));
        // Clicking inside the piece resolves to the insertion point.
        let inside = line
            .source_map
            .visual_to_source(VisualOffset(6), Bias::After)
            .unwrap();
        assert_eq!(inside.source_offset, SourceOffset(3));
        // The insertion offset resolves in front of the piece, for either
        // affinity: the caret, a selection end and typed text go before it.
        for affinity in [Bias::Before, Bias::After] {
            let at = line
                .source_map
                .source_to_visual(SourceOffset(3), affinity)
                .unwrap();
            assert_eq!(at.visual_offset, VisualOffset(3));
        }
        // The text after it still resolves to its own source byte.
        let tail = line
            .source_map
            .source_to_visual(SourceOffset(6), Bias::After)
            .unwrap();
        assert_eq!(tail.visual_offset, VisualOffset(12));
        // The source never gained a byte.
        for segment in &line.source_map.segments {
            assert!(segment.source_range.end.0 <= "前後の文".len());
        }
    }

    #[test]
    fn pieces_at_one_offset_keep_their_order_and_later_offsets_shift() {
        let mut line = line("abcd");
        let placements = splice_virtual_text(
            &mut line,
            &[insert(1, 1, "X"), insert(2, 1, "YY"), insert(3, 3, "Z")],
        );
        assert_eq!(line.visual_text, "aXYYbcZd");
        assert_eq!(
            placements.iter().map(|p| (p.id, p.visual_range)).collect::<Vec<_>>(),
            vec![
                (1, VisualRange::new(1, 2)),
                (2, VisualRange::new(2, 4)),
                (3, VisualRange::new(6, 7)),
            ]
        );
        let d = line
            .source_map
            .visual_to_source(VisualOffset(8), Bias::Before)
            .unwrap();
        assert_eq!(d.source_offset, SourceOffset(4));
    }

    #[test]
    fn a_piece_at_the_end_of_the_line_is_appended() {
        let mut line = line("終わり");
        let end = "終わり".len();
        splice_virtual_text(&mut line, &[insert(1, end, "続き")]);
        assert_eq!(line.visual_text, "終わり続き");
        let back = line
            .source_map
            .visual_to_source(VisualOffset("終わり続き".len()), Bias::After)
            .unwrap();
        assert_eq!(back.source_offset, SourceOffset(end));
        let marked: Vec<_> = line
            .style_runs
            .iter()
            .filter(|run| run.kind == StyleKind::MarkedText)
            .collect();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].visual_range, VisualRange::new(end, end + 6));
    }

    #[test]
    fn hidden_markup_keeps_its_place_and_style_runs_follow() {
        let mut line = line("**太字**あ");
        // Between the bold text and the closing marker's right edge.
        let at = "**太字**".len();
        let before_runs = line.style_runs.clone();
        splice_virtual_text(&mut line, &[insert(1, at, "音")]);
        assert_eq!(line.visual_text, "太字音あ");
        // The bold run still covers exactly the bold text.
        assert!(before_runs.iter().any(|run| run.kind == StyleKind::Bold));
        let bold = line
            .style_runs
            .iter()
            .find(|run| run.kind == StyleKind::Bold && run.visual_range.start.0 == 0)
            .unwrap();
        assert!(bold.visual_range.end.0 >= "太字".len());
    }

    #[test]
    fn emphasis_adds_a_bold_run() {
        let mut line = line("あ");
        let mut piece = insert(1, 3, "い");
        piece.emphasized = true;
        splice_virtual_text(&mut line, &[piece]);
        assert!(line.style_runs.iter().any(|run| run.kind == StyleKind::Bold));
    }
}
