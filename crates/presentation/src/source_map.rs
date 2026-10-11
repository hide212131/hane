use hane_document::{Bias, SourceOffset, SourceRange};

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct VisualOffset(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct VisualRange {
    pub start: VisualOffset,
    pub end: VisualOffset,
}

impl VisualRange {
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start: VisualOffset(start),
            end: VisualOffset(end),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Visible,
    HiddenMarkup,
    Synthesized,
    ExpandedMarkup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundarySide {
    Leading,
    Trailing,
}

/// Which end of a delimited construct (e.g. `**bold**`, `` `code` ``) a
/// [`Visibility::HiddenMarkup`] segment's marker sits at. A collapsed
/// zero-visual-width marker shares its visual position with the visible
/// content on one side and unrelated content on the other; only the parse
/// tree that produced the marker knows which side is which, so this is
/// resolved once here rather than guessed from position at click time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkerEdge {
    Opening,
    Closing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingSegment {
    pub source_range: SourceRange,
    pub visual_range: VisualRange,
    pub visibility: Visibility,
    /// `Some` only for a [`Visibility::HiddenMarkup`] segment whose marker
    /// opens or closes a delimited construct (bold, italic, inline code,
    /// links); `None` for plain visible text and for markup with no
    /// meaningful side (e.g. a quote/list prefix, a table pipe).
    pub marker_edge: Option<MarkerEdge>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionCandidate {
    pub source_offset: SourceOffset,
    pub visual_offset: VisualOffset,
    pub affinity: Bias,
    pub side: BoundarySide,
    pub visibility: Visibility,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceMap {
    pub segments: Vec<MappingSegment>,
}

impl SourceMap {
    pub fn visual_to_source(
        &self,
        visual: VisualOffset,
        affinity: Bias,
    ) -> Option<PositionCandidate> {
        let candidates = self.segments.iter().filter_map(|segment| {
            if visual.0 < segment.visual_range.start.0 || visual.0 > segment.visual_range.end.0 {
                return None;
            }
            let source = match segment.visibility {
                Visibility::Visible | Visibility::ExpandedMarkup => {
                    let delta = visual.0.saturating_sub(segment.visual_range.start.0);
                    SourceOffset(
                        (segment.source_range.start.0 + delta).min(segment.source_range.end.0),
                    )
                }
                Visibility::HiddenMarkup => match affinity {
                    Bias::Before => segment.source_range.start,
                    Bias::After => segment.source_range.end,
                },
                Visibility::Synthesized => segment.source_range.start,
            };
            Some(PositionCandidate {
                source_offset: source,
                visual_offset: visual,
                affinity,
                side: if affinity == Bias::Before {
                    BoundarySide::Leading
                } else {
                    BoundarySide::Trailing
                },
                visibility: segment.visibility,
            })
        });
        let mut candidates = candidates.collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.source_offset);
        let visible = candidates
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate.visibility,
                    Visibility::Visible | Visibility::ExpandedMarkup
                )
            })
            .copied()
            .collect::<Vec<_>>();
        match affinity {
            Bias::Before => visible
                .first()
                .copied()
                .or_else(|| candidates.first().copied()),
            Bias::After => visible
                .last()
                .copied()
                .or_else(|| candidates.last().copied()),
        }
    }

    pub fn source_to_visual(
        &self,
        source: SourceOffset,
        affinity: Bias,
    ) -> Option<PositionCandidate> {
        let mut candidates = self
            .segments
            .iter()
            .filter_map(|segment| {
                if source.0 < segment.source_range.start.0 || source.0 > segment.source_range.end.0
                {
                    return None;
                }
                let visual = match segment.visibility {
                    Visibility::Visible | Visibility::ExpandedMarkup => VisualOffset(
                        segment.visual_range.start.0
                            + source.0.saturating_sub(segment.source_range.start.0),
                    ),
                    Visibility::HiddenMarkup => match affinity {
                        Bias::Before => segment.visual_range.start,
                        Bias::After => segment.visual_range.end,
                    },
                    Visibility::Synthesized => segment.visual_range.start,
                };
                Some(PositionCandidate {
                    source_offset: source,
                    visual_offset: visual,
                    affinity,
                    side: if affinity == Bias::Before {
                        BoundarySide::Leading
                    } else {
                        BoundarySide::Trailing
                    },
                    visibility: segment.visibility,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.visual_offset);
        match affinity {
            Bias::Before => candidates.first().copied(),
            Bias::After => candidates.last().copied(),
        }
    }

    /// Hidden and synthesized segments can sit back-to-back with no visible
    /// content between them (e.g. a list marker's synthesized bullet
    /// immediately followed by a nested ATX heading's hidden marker). A
    /// single source→visual→source round trip only walks to the next such
    /// boundary rather than settling on an editable position (ADR-0004), so
    /// `next` can differ from `current` and itself still not be a fixed
    /// point. Repeating the round trip walks the whole chain; each affinity
    /// only ever advances toward candidates sorted later (`After`) or
    /// earlier (`Before`) for that same affinity, so the source offset it
    /// produces is monotonic and bounded, and it stabilizes in at most
    /// `segments.len()` steps.
    pub fn normalize_source(&self, source: SourceOffset, affinity: Bias) -> Option<SourceOffset> {
        let mut current = source;
        for _ in 0..=self.segments.len() {
            let visual = self.source_to_visual(current, affinity)?.visual_offset;
            let next = self.visual_to_source(visual, affinity)?.source_offset;
            if next == current {
                return Some(current);
            }
            current = next;
        }
        Some(current)
    }

    /// Mirrors [`Self::normalize_source`]'s chained-boundary walk in the
    /// visual direction.
    pub fn normalize_visual(&self, visual: VisualOffset, affinity: Bias) -> Option<VisualOffset> {
        let mut current = visual;
        for _ in 0..=self.segments.len() {
            let source = self.visual_to_source(current, affinity)?.source_offset;
            let next = self.source_to_visual(source, affinity)?.visual_offset;
            if next == current {
                return Some(current);
            }
            current = next;
        }
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::Revision;

    // The surrounding markdown parse/visual-text construction is exercised
    // elsewhere; this test specifically verifies the source map's own
    // normalization contract for hidden Unicode boundaries, so it stays
    // here even though it goes through `present_markdown` to build a map.
    #[test]
    fn hidden_unicode_boundaries_normalize_with_affinity() {
        let source = "**日本🙂**";
        let block = crate::present_markdown(
            0,
            Revision(1),
            SourceRange::new(100, 100 + source.len()),
            source,
            26.0,
        );
        assert_eq!(block.visual_text, "日本🙂");
        assert_eq!(
            block
                .source_map
                .normalize_source(SourceOffset(101), Bias::After),
            Some(SourceOffset(102))
        );
        assert_eq!(
            block
                .source_map
                .normalize_visual(VisualOffset("日本🙂".len()), Bias::Before),
            Some(VisualOffset("日本🙂".len()))
        );
        for segment in &block.source_map.segments {
            assert!(source.is_char_boundary(segment.source_range.start.0 - 100));
            assert!(source.is_char_boundary(segment.source_range.end.0 - 100));
        }
    }
}
