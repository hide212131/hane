//! Source highlighting for the plain-text presentation (Issue #738).
//!
//! Classification only: a parse result (or, for a top-level code block, the
//! block's shape) becomes [`StyleRun`]s. Nothing here touches `visual_text` or a
//! source map, and nothing knows a color; the UI resolves a [`SyntaxTone`]
//! through the theme.

use crate::display::{StyleKind, StyleRun, SyntaxTone};
use crate::source_map::VisualRange;
use hane_document::SourceRange;
use hane_markdown::{FenceDelimiter, FenceMarkerEdge, MarkdownParse, MarkdownTree, NodeId, NodeKind};

/// What [`crate::present_block_plain`] colors a block from.
pub enum PlainHighlightInput<'a> {
    /// Contiguous block lines, joined and parsed once. Either every content
    /// line of the block (within the synchronous budget) or just the presented
    /// window (a degraded parse for an over-budget block).
    Parse { lines: &'a [crate::BlockLine<'a>] },
    /// A top-level code block. Never parsed: the lines decide.
    Code(PlainCodeBlock),
}

/// Shape of a top-level code block. Line numbers are document line numbers, the
/// same coordinates as [`crate::BlockLine::line`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainCodeBlock {
    /// The opening fence line; `None` for an indented code block.
    pub opening: Option<(usize, FenceDelimiter)>,
    /// The line that closes the opening fence, when there is one.
    pub closing_line: Option<usize>,
    /// Exclusive end of the block's content; later lines are owned blank lines.
    pub content_end: usize,
}

/// Highlight runs of one block, kept as source ranges and cut per line.
pub struct PlainHighlight {
    /// Sorted by start. Runs overlap; `InlineDisplay::union` resolves overlaps.
    runs: Vec<(SourceRange, StyleKind)>,
    /// Running maximum of the run ends, so the first run that can reach a line
    /// is found by binary search.
    reach: Vec<usize>,
}

fn push(runs: &mut Vec<(SourceRange, StyleKind)>, start: usize, end: usize, kind: StyleKind) {
    if start < end {
        runs.push((SourceRange::new(start, end), kind));
    }
}

/// Source span of a node's children, first start to last end.
fn children_span(tree: &MarkdownTree, id: NodeId) -> Option<(usize, usize)> {
    let children = tree.children(id);
    let first = tree.node(*children.first()?)?;
    let last = tree.node(*children.last()?)?;
    let (start, end) = (first.source_range.start.0, last.source_range.end.0);
    (start <= end).then_some((start, end))
}

impl PlainHighlight {
    /// Classifies `parsed`. `source` is the text that was parsed, starting at
    /// `parsed.source_range.start`.
    pub fn from_parse(parsed: &MarkdownParse, source: &str) -> Self {
        let tree = &parsed.tree;
        let mut runs = Vec::new();
        let mut tables = Vec::new();
        let mut cells = Vec::new();
        for (id, node) in tree.iter() {
            let (start, end) = (node.source_range.start.0, node.source_range.end.0);
            match node.kind {
                NodeKind::Heading(level) => {
                    let tone = if level <= 2 {
                        SyntaxTone::Heading
                    } else {
                        SyntaxTone::HeadingMinor
                    };
                    if let Some((text_start, text_end)) = children_span(tree, id) {
                        push(&mut runs, text_start, text_end, StyleKind::Syntax(tone));
                        push(&mut runs, text_start, text_end, StyleKind::Bold);
                        let punctuation = StyleKind::Syntax(SyntaxTone::Punctuation);
                        push(&mut runs, start, text_start, punctuation);
                        push(&mut runs, text_end, end, punctuation);
                    } else {
                        push(
                            &mut runs,
                            start,
                            end,
                            StyleKind::Syntax(SyntaxTone::Punctuation),
                        );
                    }
                }
                NodeKind::Strong => push(&mut runs, start, end, StyleKind::Bold),
                NodeKind::Emphasis => push(&mut runs, start, end, StyleKind::Italic),
                NodeKind::Strikethrough => push(&mut runs, start, end, StyleKind::Strikethrough),
                NodeKind::InlineCode | NodeKind::CodeBlock => push(
                    &mut runs,
                    start,
                    end,
                    StyleKind::Syntax(SyntaxTone::Code),
                ),
                NodeKind::Link | NodeKind::Image => {
                    let punctuation = StyleKind::Syntax(SyntaxTone::Punctuation);
                    if let Some((text_start, text_end)) = children_span(tree, id) {
                        push(
                            &mut runs,
                            text_start,
                            text_end,
                            StyleKind::Syntax(SyntaxTone::Link),
                        );
                        push(&mut runs, start, text_start, punctuation);
                        push(&mut runs, text_end, end, punctuation);
                    } else {
                        push(&mut runs, start, end, punctuation);
                    }
                }
                NodeKind::Quote => push(&mut runs, start, end, StyleKind::Syntax(SyntaxTone::Quote)),
                NodeKind::Rule | NodeKind::HtmlBlock | NodeKind::Html | NodeKind::InlineHtml => {
                    push(
                        &mut runs,
                        start,
                        end,
                        StyleKind::Syntax(SyntaxTone::Punctuation),
                    );
                }
                NodeKind::Table => tables.push((start, end)),
                NodeKind::TableCell => {
                    cells.push((start, end));
                    if tree
                        .ancestors(id)
                        .any(|ancestor| tree.node(ancestor).is_some_and(|n| n.kind == NodeKind::TableHead))
                    {
                        push(&mut runs, start, end, StyleKind::Bold);
                    }
                }
                _ => {}
            }
        }

        // Everything in a table outside its cells is syntax: pipes, the
        // delimiter row, and the blanks around the cells.
        for (table_start, table_end) in tables {
            let first = cells.partition_point(|(cell_start, _)| *cell_start < table_start);
            let mut cursor = table_start;
            for (cell_start, cell_end) in &cells[first..] {
                if *cell_start >= table_end {
                    break;
                }
                push(
                    &mut runs,
                    cursor,
                    *cell_start,
                    StyleKind::Syntax(SyntaxTone::Punctuation),
                );
                cursor = cursor.max(*cell_end);
            }
            push(
                &mut runs,
                cursor,
                table_end,
                StyleKind::Syntax(SyntaxTone::Punctuation),
            );
        }

        // The info string of a nested fence, after the opening delimiter run.
        let base = parsed.source_range.start.0;
        for (marker, edge) in &parsed.fence_marker_edges {
            if *edge != FenceMarkerEdge::Opening {
                continue;
            }
            let Some(rest) = marker
                .end
                .0
                .checked_sub(base)
                .and_then(|relative| source.get(relative..))
            else {
                continue;
            };
            let line = rest.split(['\r', '\n']).next().unwrap_or("");
            let trimmed = line.trim_start();
            let lead = line.len() - trimmed.len();
            let info = trimmed.trim_end();
            push(
                &mut runs,
                marker.end.0 + lead,
                marker.end.0 + lead + info.len(),
                StyleKind::Syntax(SyntaxTone::Structure),
            );
        }

        for marker in &parsed.markers {
            push(
                &mut runs,
                marker.start.0,
                marker.end.0,
                StyleKind::Syntax(SyntaxTone::Punctuation),
            );
        }
        for (marker, _) in parsed
            .quote_markers
            .iter()
            .chain(parsed.list_item_markers.iter())
        {
            push(
                &mut runs,
                marker.start.0,
                marker.end.0,
                StyleKind::Syntax(SyntaxTone::Structure),
            );
        }
        for task in &parsed.task_markers {
            push(
                &mut runs,
                task.marker_range.start.0,
                task.marker_range.end.0,
                StyleKind::Syntax(SyntaxTone::Structure),
            );
        }
        // A bare URL is plain text to the tree; only the link table knows it.
        for link in &parsed.links {
            if tree
                .node(link.node)
                .is_some_and(|node| node.kind == NodeKind::Text)
            {
                push(
                    &mut runs,
                    link.text_range.start.0,
                    link.text_range.end.0,
                    StyleKind::Syntax(SyntaxTone::Link),
                );
            }
        }

        runs.sort_by_key(|(range, _)| range.start.0);
        let mut reach = Vec::with_capacity(runs.len());
        let mut furthest = 0;
        for (range, _) in &runs {
            furthest = furthest.max(range.end.0);
            reach.push(furthest);
        }
        Self { runs, reach }
    }

    /// The runs that touch `content`, the source range of one physical line
    /// without its line ending, as visual ranges within that line. Plain lines
    /// are identity mapped, so the visual range is a subtraction.
    pub fn style_runs_for_line(&self, content: SourceRange) -> Vec<StyleRun> {
        if content.is_empty() {
            return Vec::new();
        }
        let (line_start, line_end) = (content.start.0, content.end.0);
        let low = self.reach.partition_point(|reach| *reach <= line_start);
        let high = self
            .runs
            .partition_point(|(range, _)| range.start.0 < line_end);
        if low >= high {
            return Vec::new();
        }
        self.runs[low..high]
            .iter()
            .filter_map(|(range, kind)| {
                let start = range.start.0.max(line_start);
                let end = range.end.0.min(line_end);
                (start < end).then(|| StyleRun {
                    visual_range: VisualRange::new(start - line_start, end - line_start),
                    kind: *kind,
                })
            })
            .collect()
    }
}

/// Runs for one line of a top-level code block. `text` is the line without its
/// line ending.
pub fn plain_code_line_runs(code: &PlainCodeBlock, line: usize, text: &str) -> Vec<StyleRun> {
    if line >= code.content_end || text.is_empty() {
        return Vec::new();
    }
    let run = |start: usize, end: usize, tone: SyntaxTone| StyleRun {
        visual_range: VisualRange::new(start, end),
        kind: StyleKind::Syntax(tone),
    };
    if let Some((opening_line, delimiter)) = code.opening
        && opening_line == line
    {
        let indent = text.len() - text.trim_start_matches(' ').len();
        let fence_end = (indent + delimiter.len).min(text.len());
        if !text.is_char_boundary(fence_end) {
            return vec![run(0, text.len(), SyntaxTone::Punctuation)];
        }
        let mut runs = vec![run(0, fence_end, SyntaxTone::Punctuation)];
        let rest = &text[fence_end..];
        let trimmed = rest.trim_start();
        let lead = rest.len() - trimmed.len();
        let info = trimmed.trim_end();
        if !info.is_empty() {
            runs.push(run(
                fence_end + lead,
                fence_end + lead + info.len(),
                SyntaxTone::Structure,
            ));
        }
        return runs;
    }
    if code.closing_line == Some(line) {
        return vec![run(0, text.len(), SyntaxTone::Punctuation)];
    }
    vec![run(0, text.len(), SyntaxTone::Code)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::InlineDisplay;
    use hane_document::Revision;
    use hane_markdown::parse_document;

    fn highlight(source: &str) -> (MarkdownParse, PlainHighlight) {
        let parsed = parse_document(Revision(0), SourceRange::new(0, source.len()), source);
        let highlight = PlainHighlight::from_parse(&parsed, source);
        (parsed, highlight)
    }

    /// Content range of every physical line of `source`, without line endings.
    fn line_contents(source: &str) -> Vec<SourceRange> {
        let mut offset = 0;
        source
            .split_inclusive('\n')
            .map(|line| {
                let start = offset;
                offset += line.len();
                SourceRange::new(start, start + line.trim_end_matches(['\r', '\n']).len())
            })
            .collect()
    }

    /// The folded display of every byte of `source`.
    fn displays(source: &str) -> Vec<InlineDisplay> {
        let (_, highlight) = highlight(source);
        let mut displays = vec![InlineDisplay::default(); source.len()];
        for content in line_contents(source) {
            for run in highlight.style_runs_for_line(content) {
                for offset in run.visual_range.start.0..run.visual_range.end.0 {
                    let at = content.start.0 + offset;
                    displays[at] = displays[at].union(run.kind.display());
                }
            }
        }
        displays
    }

    /// The displays of the first occurrence of `needle` in `source`.
    fn region(source: &str, needle: &str) -> Vec<InlineDisplay> {
        let at = source
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {source:?}"));
        displays(source)[at..at + needle.len()].to_vec()
    }

    fn assert_tone(source: &str, needle: &str, tone: Option<SyntaxTone>) {
        for (index, display) in region(source, needle).iter().enumerate() {
            assert_eq!(
                display.tone, tone,
                "byte {index} of {needle:?} in {source:?}"
            );
        }
    }

    fn assert_all(source: &str, needle: &str, check: impl Fn(&InlineDisplay) -> bool) {
        for (index, display) in region(source, needle).iter().enumerate() {
            assert!(check(display), "byte {index} of {needle:?} in {source:?}: {display:?}");
        }
    }

    #[test]
    fn atx_heading_splits_marker_and_text() {
        let source = "# Title";
        assert_tone(source, "# ", Some(SyntaxTone::Punctuation));
        assert_all(source, "# ", |d| !d.bold);
        assert_tone(source, "Title", Some(SyntaxTone::Heading));
        assert_all(source, "Title", |d| d.bold);
    }

    #[test]
    fn heading_levels_use_two_tones() {
        for level in 1..=6 {
            let source = format!("{} a", "#".repeat(level));
            let expected = if level <= 2 {
                SyntaxTone::Heading
            } else {
                SyntaxTone::HeadingMinor
            };
            let last = displays(&source)[source.len() - 1];
            assert_eq!(last.tone, Some(expected), "level {level}");
            assert!(last.bold);
        }
    }

    #[test]
    fn heading_closing_hashes_are_punctuation() {
        let source = "## a ##";
        assert_tone(source, " ##", Some(SyntaxTone::Punctuation));
        assert_tone(source, "a", Some(SyntaxTone::Heading));
    }

    #[test]
    fn empty_heading_is_all_punctuation() {
        assert_tone("#", "#", Some(SyntaxTone::Punctuation));
        assert_all("#", "#", |d| !d.bold);
        assert_tone("# ", "#", Some(SyntaxTone::Punctuation));
        assert!(displays("# ").iter().all(|d| !d.bold));
    }

    #[test]
    fn setext_heading_underline_is_punctuation() {
        for source in ["Title\n===", "Title\n---"] {
            assert_tone(source, "Title", Some(SyntaxTone::Heading));
            assert_all(source, "Title", |d| d.bold);
            let underline = &source[6..];
            assert_tone(source, underline, Some(SyntaxTone::Punctuation));
        }
    }

    #[test]
    fn non_heading_hash_is_plain() {
        for source in ["#a", "####### a"] {
            assert!(
                displays(source).iter().all(|d| *d == InlineDisplay::default()),
                "{source:?}"
            );
        }
    }

    #[test]
    fn strong_and_emphasis_set_flags_and_dim_delimiters() {
        let source = "**a** *b* ***c***";
        assert_all(source, "a", |d| d.bold && !d.italic && d.tone.is_none());
        assert_all(source, "b", |d| d.italic && !d.bold && d.tone.is_none());
        assert_all(source, "c", |d| d.bold && d.italic && d.tone.is_none());
        assert_tone(source, "**", Some(SyntaxTone::Punctuation));
        let at = |index: usize| displays(source)[index];
        assert_eq!(at(2).tone, None);
        assert_eq!(at(6).tone, Some(SyntaxTone::Punctuation));
        assert_eq!(at(8).tone, Some(SyntaxTone::Punctuation));
        assert_tone(source, "***", Some(SyntaxTone::Punctuation));
    }

    #[test]
    fn strikethrough_sets_flag() {
        let source = "~~a~~";
        assert_all(source, "a", |d| d.strikethrough && d.tone.is_none());
        assert_tone(source, "~~", Some(SyntaxTone::Punctuation));
    }

    #[test]
    fn unclosed_emphasis_is_plain() {
        assert!(
            displays("**a")
                .iter()
                .all(|d| *d == InlineDisplay::default())
        );
    }

    #[test]
    fn inline_code_is_code_tone_without_monospace_or_background() {
        let source = "`a`";
        assert_tone(source, "a", Some(SyntaxTone::Code));
        assert_all(source, "a", |d| !d.monospace && !d.code_background);
        assert_tone(source, "`", Some(SyntaxTone::Punctuation));
        let displays = displays(source);
        assert_eq!(displays[2].tone, Some(SyntaxTone::Punctuation));
    }

    #[test]
    fn inline_link_colors_text_and_dims_destination() {
        for source in ["[t](https://e.x)", "[t](./a.md)"] {
            assert_tone(source, "t", Some(SyntaxTone::Link));
            assert_all(source, "t", |d| !d.underline && !d.link_color);
            assert_tone(source, "[", Some(SyntaxTone::Punctuation));
            assert_tone(source, "](", Some(SyntaxTone::Punctuation));
            let destination = &source[2..];
            assert_tone(source, destination, Some(SyntaxTone::Punctuation));
        }
    }

    #[test]
    fn autolink_and_bare_url_are_link_tone() {
        assert_tone("<https://e.x>", "https://e.x", Some(SyntaxTone::Link));
        let source = "see https://e.x now";
        assert_tone(source, "https://e.x", Some(SyntaxTone::Link));
        assert_tone(source, "see", None);
    }

    #[test]
    fn image_follows_link_rule() {
        let source = "![alt](a.png)";
        assert_tone(source, "alt", Some(SyntaxTone::Link));
        assert_tone(source, "![", Some(SyntaxTone::Punctuation));
        assert_tone(source, "](a.png)", Some(SyntaxTone::Punctuation));
    }

    #[test]
    fn unresolved_reference_link_is_plain() {
        assert!(
            displays("[t][r]")
                .iter()
                .all(|d| *d == InlineDisplay::default())
        );
    }

    #[test]
    fn nested_inline_in_heading_prefers_inner_tone() {
        let source = "# a `b` [c](u)";
        assert_tone(source, "b", Some(SyntaxTone::Code));
        assert_all(source, "b", |d| d.bold);
        assert_tone(source, "c", Some(SyntaxTone::Link));
        assert_all(source, "c", |d| d.bold);
        assert_tone(source, "a", Some(SyntaxTone::Heading));
        assert_all(source, "a", |d| d.bold);
    }

    #[test]
    fn quote_marker_is_structure_and_body_is_quote() {
        assert_tone("> a", ">", Some(SyntaxTone::Structure));
        assert_tone("> a", "a", Some(SyntaxTone::Quote));
        let nested = displays("> > a");
        assert_eq!(nested[0].tone, Some(SyntaxTone::Structure));
        assert_eq!(nested[2].tone, Some(SyntaxTone::Structure));
        assert_eq!(nested[4].tone, Some(SyntaxTone::Quote));
        assert_tone("> **b**", "b", Some(SyntaxTone::Quote));
        assert_all("> **b**", "b", |d| d.bold);
        assert_tone("> **b**", "**", Some(SyntaxTone::Punctuation));
    }

    #[test]
    fn list_markers_are_structure() {
        for (source, marker_len) in [("- a", 2), ("* a", 2), ("+ a", 2), ("1. a", 3), ("1) a", 3)] {
            let displays = displays(source);
            for (index, display) in displays.iter().enumerate().take(marker_len) {
                assert_eq!(display.tone, Some(SyntaxTone::Structure), "{source:?} {index}");
            }
            assert_eq!(displays[marker_len].tone, None, "{source:?}");
        }
    }

    #[test]
    fn task_checkbox_is_structure() {
        for source in ["- [ ] a", "- [x] a"] {
            let displays = displays(source);
            for index in 2..5 {
                assert_eq!(displays[index].tone, Some(SyntaxTone::Structure), "{source:?} {index}");
            }
            assert_eq!(displays[6].tone, None, "{source:?}");
        }
    }

    #[test]
    fn nested_fence_in_list_colors_code_and_info() {
        let source = "- a\n\n  ```rs\n  x\n  ```";
        assert_tone(source, "```", Some(SyntaxTone::Punctuation));
        assert_tone(source, "rs", Some(SyntaxTone::Structure));
        assert_tone(source, "x", Some(SyntaxTone::Code));
    }

    #[test]
    fn rule_is_punctuation() {
        for source in ["---", "***"] {
            assert_tone(source, source, Some(SyntaxTone::Punctuation));
        }
    }

    #[test]
    fn html_is_punctuation() {
        assert_tone("<div>\nx\n</div>", "<div>", Some(SyntaxTone::Punctuation));
        let source = "a <b>c</b>";
        assert_tone(source, "<b>", Some(SyntaxTone::Punctuation));
        assert_tone(source, "</b>", Some(SyntaxTone::Punctuation));
        assert_tone(source, "c", None);
    }

    const TABLE: &str = "| a | b |\n| --- | --- |\n| c | d |";

    #[test]
    fn table_cell_ranges_exclude_pipes() {
        let (parsed, _) = highlight(TABLE);
        let mut cells = 0;
        for (_, node) in parsed.tree.iter() {
            if node.kind == NodeKind::TableCell {
                cells += 1;
                let text = &TABLE[node.source_range.start.0..node.source_range.end.0];
                assert!(!text.contains('|'), "{text:?}");
            }
        }
        assert_eq!(cells, 4);
    }

    #[test]
    fn table_pipes_and_delimiter_row_are_punctuation() {
        assert_eq!(displays(TABLE)[0].tone, Some(SyntaxTone::Punctuation));
        assert_tone(TABLE, "| --- | --- |", Some(SyntaxTone::Punctuation));
        assert_all(TABLE, "a", |d| d.bold && d.tone.is_none());
        assert_all(TABLE, "b", |d| d.bold && d.tone.is_none());
        assert_all(TABLE, "c", |d| !d.bold && d.tone.is_none());
        assert_all(TABLE, "d", |d| !d.bold && d.tone.is_none());
    }

    #[test]
    fn hard_break_backslash_is_punctuation() {
        assert_eq!(displays("a\\\nb")[1].tone, Some(SyntaxTone::Punctuation));
    }

    #[test]
    fn multi_line_strong_is_clipped_per_line() {
        let source = "**a\nb**";
        assert_all(source, "a", |d| d.bold);
        assert_all(source, "b", |d| d.bold);
        let (_, highlight) = highlight(source);
        for content in line_contents(source) {
            for run in highlight.style_runs_for_line(content) {
                assert!(run.visual_range.start.0 < run.visual_range.end.0);
                assert!(run.visual_range.end.0 <= content.len_bytes());
            }
        }
    }

    #[test]
    fn plain_highlight_runs_are_on_char_boundaries() {
        let base = "# 見出し **太字** `コード` [リンク](https://e.x) 😀\n> 引用 é\n- 項目 e\u{301}\n\n| あ | い |\n| --- | --- |\n| う | え |\n\n```日本\n中身\n```\n";
        for source in [base.to_owned(), base.replace('\n', "\r\n")] {
            let (_, highlight) = highlight(&source);
            for content in line_contents(&source) {
                let line = &source[content.start.0..content.end.0];
                for run in highlight.style_runs_for_line(content) {
                    let (start, end) = (run.visual_range.start.0, run.visual_range.end.0);
                    assert!(start < end && end <= line.len(), "{line:?} {start}..{end}");
                    assert!(line.is_char_boundary(start), "{line:?} {start}");
                    assert!(line.is_char_boundary(end), "{line:?} {end}");
                }
            }
        }
    }

    #[test]
    fn empty_and_blank_sources_yield_no_runs() {
        for source in ["", "\n\n"] {
            let (_, highlight) = highlight(source);
            for content in line_contents(source) {
                assert!(highlight.style_runs_for_line(content).is_empty());
            }
        }
    }

    fn code_runs(
        code: &PlainCodeBlock,
        line: usize,
        text: &str,
    ) -> Vec<(usize, usize, SyntaxTone)> {
        plain_code_line_runs(code, line, text)
            .into_iter()
            .map(|run| {
                let StyleKind::Syntax(tone) = run.kind else {
                    panic!("code runs are syntax runs: {run:?}");
                };
                (run.visual_range.start.0, run.visual_range.end.0, tone)
            })
            .collect()
    }

    fn fence(marker: u8) -> FenceDelimiter {
        FenceDelimiter { marker, len: 3 }
    }

    fn fenced(marker: u8) -> PlainCodeBlock {
        PlainCodeBlock {
            opening: Some((0, fence(marker))),
            closing_line: Some(2),
            content_end: 3,
        }
    }

    #[test]
    fn fence_opening_line_splits_delimiter_and_info() {
        assert_eq!(
            code_runs(&fenced(b'`'), 0, "```rust"),
            vec![(0, 3, SyntaxTone::Punctuation), (3, 7, SyntaxTone::Structure)]
        );
        assert_eq!(
            code_runs(&fenced(b'~'), 0, "~~~ py"),
            vec![(0, 3, SyntaxTone::Punctuation), (4, 6, SyntaxTone::Structure)]
        );
        assert_eq!(
            code_runs(&fenced(b'`'), 0, "   ```x"),
            vec![(0, 6, SyntaxTone::Punctuation), (6, 7, SyntaxTone::Structure)]
        );
    }

    #[test]
    fn fence_opening_without_info_has_only_punctuation() {
        assert_eq!(
            code_runs(&fenced(b'`'), 0, "```"),
            vec![(0, 3, SyntaxTone::Punctuation)]
        );
    }

    #[test]
    fn fence_content_and_closing_lines() {
        let code = fenced(b'`');
        assert_eq!(code_runs(&code, 1, "let x"), vec![(0, 5, SyntaxTone::Code)]);
        assert_eq!(code_runs(&code, 2, "```"), vec![(0, 3, SyntaxTone::Punctuation)]);
    }

    #[test]
    fn indented_code_is_all_code() {
        let code = PlainCodeBlock {
            opening: None,
            closing_line: None,
            content_end: 3,
        };
        for line in 0..3 {
            assert_eq!(code_runs(&code, line, "    x"), vec![(0, 5, SyntaxTone::Code)]);
        }
    }

    #[test]
    fn owned_trailing_blank_lines_have_no_runs() {
        let code = fenced(b'`');
        assert!(code_runs(&code, 3, "   ").is_empty());
        assert!(code_runs(&code, 4, "text").is_empty());
    }
}
