//! Issue #673: a bare URL reaches presentation as a link target and a link
//! color, and every visual row and glyph of it resolves to the right source
//! range and destination — through the formal block projections, not a
//! re-implementation of them.

use hane_document::{LineId, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_markdown::BlockIndex;
use hane_presentation::testing::FixedAdvanceShaper;
use hane_presentation::{
    BlockKind, BlockLine, BlockWindow, StyleKind, VisualBlock, VisualLine, block_line_span,
    layout_block, present_block_with_table_projection, trailing_blank_lines,
};

const LINE_HEIGHT: f32 = 26.0;

fn shaper() -> FixedAdvanceShaper {
    FixedAdvanceShaper::new(8.0)
}

fn present(source: &str) -> Vec<VisualBlock> {
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    index
        .blocks()
        .map(|block| {
            let span = block_line_span(&buffer, &block).expect("block spans lines");
            let ranges = span
                .clone()
                .map(|line| buffer.line_range(LineId(line)).expect("line in range"))
                .collect::<Vec<_>>();
            let texts = ranges
                .iter()
                .map(|range| buffer.text(*range).expect("line text"))
                .collect::<Vec<_>>();
            let lines = span
                .clone()
                .zip(&ranges)
                .zip(&texts)
                .map(|((line, range), text)| BlockLine {
                    line,
                    range: *range,
                    text,
                    disclosure: None,
                })
                .collect::<Vec<_>>();
            present_block_with_table_projection(
                &block,
                buffer.revision(),
                &BlockWindow {
                    trailing_blank_lines: trailing_blank_lines(&buffer, &span),
                    render: span.clone(),
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
                index.list_projection(&block),
                index.table_projection(&block),
            )
        })
        .collect()
}

fn range_of(source: &str, needle: &str) -> SourceRange {
    let start = source.find(needle).expect("needle in source");
    SourceRange::new(start, start + needle.len())
}

/// Resolves every character of the single-block `source` to the glyph hit the
/// UI uses, and checks the link target found there. Returns, per URL, how many
/// distinct visual rows its glyphs were found on.
fn assert_glyph_targets(source: &str, width: f32, urls: &[&str]) -> Vec<usize> {
    let blocks = present(source);
    assert_eq!(blocks.len(), 1, "fixture is one block");
    let block = &blocks[0];
    let shaper = shaper();
    let layout = layout_block(block, width, &shaper);
    let ranges = urls
        .iter()
        .map(|url| range_of(source, url))
        .collect::<Vec<_>>();
    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); urls.len()];
    for (offset, character) in source.char_indices() {
        let point = layout
            .point_for_source(block, SourceOffset(offset), &shaper)
            .expect("every character has a point");
        let row = &layout.lines[point.row];
        let line = &block.lines[row.line];
        let glyph = layout.glyph_source_at_xy(block, point.row, point.x + 1.0, 1.0, &shaper);
        let owner = ranges
            .iter()
            .position(|range| range.start.0 <= offset && offset < range.end.0);
        if let Some(owner) = owner {
            assert_eq!(
                glyph,
                Some(SourceOffset(offset)),
                "glyph {character:?} at {offset} must resolve to itself"
            );
            let target = line
                .links
                .iter()
                .find(|link| link.contains(SourceOffset(offset)))
                .unwrap_or_else(|| panic!("no target at {offset} ({character:?})"));
            assert_eq!(&*target.destination, urls[owner], "offset {offset}");
            assert_eq!(target.source_range, ranges[owner], "offset {offset}");
            assert!(!target.uses_reference);
            if !rows[owner].contains(&point.row) {
                rows[owner].push(point.row);
            }
        } else if let Some(glyph) = glyph {
            assert!(
                line.links.iter().all(|link| !link.contains(glyph)),
                "{character:?} at {offset} outside any URL must not hit a target"
            );
        }
    }
    rows.iter().map(Vec::len).collect()
}

fn link_runs(line: &VisualLine) -> Vec<(usize, usize)> {
    line.style_runs
        .iter()
        .filter(|run| run.kind == StyleKind::Link)
        .map(|run| (run.visual_range.start.0, run.visual_range.end.0))
        .collect()
}

fn assert_colored_and_targeted(line: &VisualLine, source: &str, url: &str) {
    let range = range_of(source, url);
    let visual = line.visual_text.find(url).expect("url is visible");
    assert_eq!(
        link_runs(line)
            .iter()
            .filter(|run| **run == (visual, visual + url.len()))
            .count(),
        1,
        "exactly one Link style for {url}: {:?}",
        line.style_runs
    );
    let targets = line
        .links
        .iter()
        .filter(|link| &*link.destination == url)
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 1, "exactly one target for {url}");
    assert_eq!(targets[0].source_range, range);
    assert!(!targets[0].contains(SourceOffset(range.end.0)));
}

#[test]
fn several_urls_in_one_text_each_hit_only_their_own_glyphs() {
    let source = "see https://a.example/x, then https://b.example/y; https://c.example/z end";
    let urls = [
        "https://a.example/x",
        "https://b.example/y",
        "https://c.example/z",
    ];
    let rows = assert_glyph_targets(source, 2_000.0, &urls);
    assert_eq!(rows, [1, 1, 1]);
    let line = &present(source)[0].lines[0];
    assert_eq!(line.links.len(), 3);
    for url in urls {
        assert_colored_and_targeted(line, source, url);
    }
}

#[test]
fn adjacent_urls_do_not_leak_across_the_separator() {
    let source = "https://a.example/x https://b.example/y";
    assert_glyph_targets(
        source,
        2_000.0,
        &["https://a.example/x", "https://b.example/y"],
    );
    let blocks = present(source);
    let line = &blocks[0].lines[0];
    let separator = SourceOffset("https://a.example/x".len());
    assert!(line.links.iter().all(|link| !link.contains(separator)));
}

#[test]
fn soft_wrapped_long_url_resolves_on_every_visual_row() {
    let long = "https://example.com/a/very/long/path/segment?query=value&other=two#fragment-part";
    let short = "https://short.example/q";
    let source = format!("go {long} stop {short}");
    let rows = assert_glyph_targets(&source, 80.0, &[long, short]);
    assert!(
        rows[0] >= 4,
        "the long URL must really wrap over several rows, got {rows:?}"
    );
}

#[test]
fn url_end_is_half_open_for_punctuation_space_and_next_text() {
    let source = "(https://a.example/x). https://b.example/y。次";
    let blocks = present(source);
    let line = &blocks[0].lines[0];
    let first = range_of(source, "https://a.example/x");
    let second = range_of(source, "https://b.example/y");
    let targets = line
        .links
        .iter()
        .map(|link| (link.source_range, link.destination.to_string()))
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        [
            (first, "https://a.example/x".to_string()),
            (second, "https://b.example/y".to_string())
        ]
    );
    for after in [first.end.0, first.end.0 + 1, second.end.0] {
        assert!(
            line.links
                .iter()
                .all(|link| !link.contains(SourceOffset(after))),
            "offset {after} directly after a URL is not a hit"
        );
    }
    assert!(!line.links[0].contains(SourceOffset(first.start.0 - 1)));
}

#[test]
fn quote_list_and_table_blocks_color_and_target_bare_urls() {
    let quote_url = "https://q.example/a";
    let list_url = "https://l.example/b";
    let cell_url = "https://t.example/c";
    let source = format!(
        "> quote {quote_url}\n\n- item {list_url}\n\n| h | k |\n| --- | --- |\n| {cell_url} | x |\n"
    );
    let blocks = present(&source);
    let find = |url: &str| {
        blocks
            .iter()
            .flat_map(|block| &block.lines)
            .find(|line| line.links.iter().any(|link| &*link.destination == url))
            .unwrap_or_else(|| panic!("no presented line has a target for {url}"))
    };
    for url in [quote_url, list_url] {
        assert_colored_and_targeted(find(url), &source, url);
    }
    let row = find(cell_url);
    assert_eq!(row.kind, BlockKind::TableRow);
    // The inactive table row is presented without a Markdown Link node, so the
    // color and the target both come from the formal projection's metadata.
    assert_colored_and_targeted(row, &source, cell_url);
}

#[test]
fn table_row_keeps_one_target_each_for_markdown_and_bare_links() {
    let source = "| h | k |\n| --- | --- |\n| [m](https://m.example/) https://t.example/c | https://u.example/d |\n";
    let blocks = present(source);
    let row = blocks
        .iter()
        .flat_map(|block| &block.lines)
        .find(|line| line.kind == BlockKind::TableRow && line.links.len() == 3)
        .expect("body row with three targets");
    let destinations = row
        .links
        .iter()
        .map(|link| link.destination.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        destinations,
        [
            "https://m.example/",
            "https://t.example/c",
            "https://u.example/d"
        ]
    );
    for pair in row.links.windows(2) {
        assert!(pair[0].source_range.end <= pair[1].source_range.start);
    }
    for url in ["https://t.example/c", "https://u.example/d"] {
        assert_colored_and_targeted(row, source, url);
    }
    // The Markdown link text is colored once, not doubled by the bare path.
    let label = row.visual_text.find('m').expect("link label");
    assert_eq!(
        link_runs(row)
            .iter()
            .filter(|run| run.0 <= label && label < run.1)
            .count(),
        1
    );
}
