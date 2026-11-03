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

/// Presents block `ordinal` of `source`. `only_line` limits both the lines read
/// and the lines drawn to that one document line (a viewport-only window, with
/// no joined parse); `formal` hands the published index's projections to the
/// presenter, and `false` leaves them out as an unconfirmed block has none.
fn present_window(
    source: &str,
    ordinal: usize,
    only_line: Option<usize>,
    formal: bool,
) -> VisualBlock {
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    let block = index.block(ordinal).expect("block");
    let span = block_line_span(&buffer, &block).expect("block spans lines");
    let read = only_line.map_or(span.clone(), |line| line..line + 1);
    let ranges = read
        .clone()
        .map(|line| buffer.line_range(LineId(line)).expect("line in range"))
        .collect::<Vec<_>>();
    let texts = ranges
        .iter()
        .map(|range| buffer.text(*range).expect("line text"))
        .collect::<Vec<_>>();
    let lines = read
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
    let list_projection = if formal {
        index.list_projection(&block)
    } else {
        None
    };
    let table_projection = if formal {
        index.table_projection(&block)
    } else {
        None
    };
    present_block_with_table_projection(
        &block,
        buffer.revision(),
        &BlockWindow {
            trailing_blank_lines: trailing_blank_lines(&buffer, &span),
            render: read,
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
        list_projection,
        table_projection,
    )
}

fn targets(block: &VisualBlock) -> Vec<(String, SourceRange, bool)> {
    block
        .lines
        .iter()
        .flat_map(|line| &line.links)
        .map(|link| (link.destination.to_string(), link.source_range, link.uses_reference))
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

/// Index of the one row whose targets point at `url`. Every other row (blank
/// or not) must carry no link target at all, so an extra link is not missed.
fn url_line_index(block: &VisualBlock, url: &str) -> usize {
    let rows = block
        .lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.links.iter().any(|link| &*link.destination == url))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "exactly one row targets {url}: {rows:?}");
    for (index, line) in block.lines.iter().enumerate() {
        if index != rows[0] {
            assert!(
                line.links.is_empty(),
                "row {index} must not carry link targets: {:?}",
                line.links
            );
        }
    }
    rows[0]
}

/// An edit in the block in front moves a presented bare URL by `rebase`: the
/// target range, destination and the glyph-to-source mapping follow the new
/// UTF-8 offsets and agree with presenting the edited source from scratch.
#[test]
fn rebase_after_a_multibyte_edit_in_front_keeps_targets_and_glyphs_aligned() {
    let url = "https://keep.example/k";
    let source = format!("head\n\nsee {url} tail\n");
    let mut block = present(&source).remove(1);
    let shaper = shaper();
    let mut layout = layout_block(&block, 2_000.0, &shaper);

    let mut buffer = RopeBuffer::from_text(&source);
    let before = buffer.revision();
    buffer
        .edit(SourceRange::new(4, 4), "🙂")
        .expect("edit in front of the URL block");
    let deltas = buffer.deltas_since(before).expect("history is available");
    let now = buffer.text(SourceRange::new(0, buffer.len_bytes().0)).expect("text");
    assert_eq!(now, format!("head🙂\n\nsee {url} tail\n"));

    assert!(block.rebase(&deltas, buffer.revision()));
    assert!(layout.rebase(&deltas, buffer.revision()));

    let expected = range_of(&now, url);
    assert_eq!(expected.start.0, range_of(&source, url).start.0 + "🙂".len());
    let line = &block.lines[url_line_index(&block, url)];
    assert_eq!(line.links.len(), 1);
    assert_eq!(&*line.links[0].destination, url);
    assert_eq!(line.links[0].source_range, expected);
    assert_colored_and_targeted(line, &now, url);

    // The rebased presentation equals presenting the edited source again.
    let fresh = present(&now).remove(1);
    assert_eq!(
        fresh.lines[url_line_index(&fresh, url)]
            .links
            .iter()
            .map(|link| (link.destination.to_string(), link.source_range))
            .collect::<Vec<_>>(),
        [(url.to_string(), expected)]
    );

    // Every glyph of the URL resolves to its own new source offset and target.
    for offset in expected.start.0..expected.end.0 {
        let point = layout
            .point_for_source(&block, SourceOffset(offset), &shaper)
            .expect("every URL character has a point");
        let glyph = layout.glyph_source_at_xy(&block, point.row, point.x + 1.0, 1.0, &shaper);
        assert_eq!(glyph, Some(SourceOffset(offset)), "glyph at {offset}");
        let row = &block.lines[layout.lines[point.row].line];
        let target = row
            .links
            .iter()
            .find(|link| link.contains(SourceOffset(offset)))
            .unwrap_or_else(|| panic!("no target at {offset}"));
        assert_eq!(&*target.destination, url);
        assert_eq!(target.source_range, expected);
    }
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
    // Existing behavior (same on main): an inactive table row keeps the raw
    // `[m](https://m.example/)` visual and source, and the Markdown link gets a
    // target only, with no individual Link color. Issue #673 adds the bare-URL
    // color/target only; it must not decorate the Markdown link, re-target its
    // raw destination as a bare URL, or widen the target beyond the label.
    let markdown = "[m](https://m.example/)";
    assert!(
        row.visual_text.contains(markdown),
        "raw Markdown link stays visible: {:?}",
        row.visual_text
    );
    let label = range_of(source, "[m]");
    let label = SourceRange::new(label.start.0 + 1, label.start.0 + 2);
    let markdown_targets = row
        .links
        .iter()
        .filter(|link| &*link.destination == "https://m.example/")
        .collect::<Vec<_>>();
    assert_eq!(markdown_targets.len(), 1, "exactly one target for the label");
    assert_eq!(markdown_targets[0].source_range, label);
    assert!(markdown_targets[0].contains(label.start));
    assert!(!markdown_targets[0].contains(label.end));
    assert!(!markdown_targets[0].uses_reference);
    // The raw destination text is not a separate bare-URL target.
    let raw_destination = range_of(source, "(https://m.example/)");
    assert!(
        row.links
            .iter()
            .filter(|link| &*link.destination != "https://m.example/")
            .all(|link| link.source_range.end <= raw_destination.start
                || link.source_range.start >= raw_destination.end),
        "no other target overlaps the raw Markdown destination"
    );
    // No Link color is added over the Markdown link; only the two bare URLs
    // (checked above) are colored.
    let visual = row.visual_text.find(markdown).expect("markdown link visible");
    assert!(
        link_runs(row)
            .iter()
            .all(|run| run.1 <= visual || run.0 >= visual + markdown.len()),
        "Markdown link is not given a Link color: {:?}",
        row.style_runs
    );
    assert_eq!(link_runs(row).len(), 2, "only the two bare URLs are colored");
}

fn table_body_row(blocks: &[VisualBlock]) -> &VisualLine {
    blocks
        .iter()
        .flat_map(|block| &block.lines)
        .filter(|line| line.kind == BlockKind::TableRow)
        .last()
        .expect("table body row")
}

/// Issue #673: a table cell's Link color follows the formal parser's origin
/// (a real `Text` owner), never a source-equals-destination string guess.
#[test]
fn table_link_color_follows_formal_origin_not_label_text() {
    let source = "| h | k |\n| --- | --- |\n| [https://a.example](https://a.example) [m](https://m.example/) | https://bare.example |\n";
    let blocks = present(source);
    let row = table_body_row(&blocks);
    // Markdown links: raw text and targets kept, no Link color.
    let bare = "https://bare.example";
    let visual = row.visual_text.rfind(bare).expect("bare url visible");
    assert_eq!(link_runs(row), [(visual, visual + bare.len())]);
    let destinations = row
        .links
        .iter()
        .map(|link| link.destination.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        destinations,
        ["https://a.example", "https://m.example/", bare]
    );
    let bare_range = SourceRange::new(
        source.rfind(bare).unwrap(),
        source.rfind(bare).unwrap() + bare.len(),
    );
    assert_eq!(row.links[2].source_range, bare_range);
    assert!(row.links.iter().all(|link| !link.uses_reference));
    for pair in row.links.windows(2) {
        assert!(pair[0].source_range.end <= pair[1].source_range.start);
    }
    // The first label is the text of the first Markdown link, not the raw
    // destination in parentheses.
    let label = range_of(source, "https://a.example");
    assert_eq!(row.links[0].source_range, label);
    assert!(!row.links[0].contains(SourceOffset(label.end.0)));
}

#[test]
fn table_same_url_in_markdown_label_and_bare_cell_colors_only_the_bare() {
    let url = "https://same.example";
    let source =
        format!("| h | k |\n| --- | --- |\n| [{url}]({url}) | {url} |\n");
    let blocks = present(&source);
    let row = table_body_row(&blocks);
    let visual = row.visual_text.rfind(url).expect("bare url visible");
    assert_eq!(link_runs(row), [(visual, visual + url.len())]);
    assert_eq!(row.links.len(), 2);
    assert_eq!(row.links[1].source_range.start.0, source.rfind(url).unwrap());
}

#[test]
fn table_reference_label_is_not_colored_and_keeps_the_formal_destination() {
    let source = format!(
        "| h | k |\n| --- | --- |\n| [{LABEL}][r] | https://bare.example |\n\n[r]: {DEST}\n"
    );
    let blocks = present(&source);
    let row = table_body_row(&blocks);
    let reference = row
        .links
        .iter()
        .find(|link| &*link.destination == DEST)
        .expect("reference target");
    assert!(reference.uses_reference);
    assert_eq!(reference.source_range, range_of(&source, LABEL));
    let bare = "https://bare.example";
    let visual = row.visual_text.rfind(bare).expect("bare url visible");
    assert_eq!(link_runs(row), [(visual, visual + bare.len())]);
}

#[test]
fn projection_creators_keep_text_origin_only_for_bare_urls() {
    let source = "| h | k |\n| --- | --- |\n| [https://a.example](https://a.example) [m](https://m.example/) <https://auto.example> | https://bare.example |\n";
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    let whole = SourceRange::new(0, source.len());
    let mut table_origins = Vec::new();
    let mut list_origins = Vec::new();
    for block in index.blocks() {
        if let Some(table) = index.table_projection(&block) {
            for row in table.rows.iter() {
                table_origins.extend(
                    row.links
                        .iter()
                        .map(|link| (link.destination.to_string(), link.is_bare_url)),
                );
            }
        }
        if let Some(projection) = index.list_projection(&block) {
            list_origins.extend(
                projection
                    .links_in(whole)
                    .map(|link| (link.destination.to_string(), link.is_bare_url)),
            );
        }
    }
    let expected = vec![
        ("https://a.example".to_owned(), false),
        ("https://m.example/".to_owned(), false),
        ("https://auto.example".to_owned(), false),
        ("https://bare.example".to_owned(), true),
    ];
    assert_eq!(table_origins, expected);
    assert_eq!(list_origins, expected);
}

const LABEL: &str = "https://label.example";
const DEST: &str = "https://dest.example";

/// A reference label that is a URL, with its definition in another block: the
/// formal destination is the only target and the label is not a bare URL.
#[test]
fn reference_label_url_targets_only_the_formal_destination() {
    let source = format!("[{LABEL}][r]\n\nother\n\n[r]: {DEST}\n");
    let label = range_of(&source, LABEL);
    let block = present_window(&source, 0, None, true);
    assert_eq!(targets(&block), [(DEST.to_owned(), label, true)]);
    let line = &block.lines[0];
    let visual = line.visual_text.find(LABEL).expect("label is visible");
    assert_eq!(
        link_runs(line)
            .iter()
            .filter(|run| **run == (visual, visual + LABEL.len()))
            .count(),
        1,
        "the label is colored once, over exactly its text: {:?}",
        line.style_runs
    );

    // Defined right after the paragraph, the same label resolves the same way.
    let near = format!("[{LABEL}][r]\n\n[r]: {DEST}\n");
    let block = present_window(&near, 0, None, true);
    assert_eq!(
        targets(&block),
        [(DEST.to_owned(), range_of(&near, LABEL), true)]
    );

    // Alongside an inline link and an autolink the label keeps its own target.
    let mixed = format!(
        "[a](https://inline.example) <https://auto.example> [{LABEL}][r]\n\n[r]: {DEST}\n"
    );
    let block = present_window(&mixed, 0, None, true);
    let destinations = targets(&block)
        .into_iter()
        .map(|(destination, _, _)| destination)
        .collect::<Vec<_>>();
    assert!(destinations.contains(&"https://inline.example".to_owned()));
    assert!(destinations.contains(&DEST.to_owned()));
    assert!(
        !destinations.contains(&LABEL.to_owned()),
        "the label is never a bare URL: {destinations:?}"
    );
}

/// Without a formal projection the same block is unconfirmed: the label is held
/// back rather than shown as a bare URL, while an ordinary bare URL in a block
/// the local parse reads whole is not.
#[test]
fn unconfirmed_block_holds_back_only_what_a_definition_could_change() {
    let source = format!("[{LABEL}][r]\n\nother\n\n[r]: {DEST}\n");
    let block = present_window(&source, 0, None, false);
    assert!(
        targets(&block).is_empty(),
        "a bracketed URL is not a target before the formal parse confirms it: {:?}",
        targets(&block)
    );

    let plain = "see https://plain.example/a here\n\nother\n";
    let block = present_window(plain, 0, None, false);
    assert_eq!(
        targets(&block),
        [(
            "https://plain.example/a".to_owned(),
            range_of(plain, "https://plain.example/a"),
            false
        )]
    );
}

/// A block larger than the synchronous join budget is presented from one line:
/// the window cannot see the code span, label or inline HTML that opened
/// outside it. Formally those lines carry no bare target; unconfirmed they carry
/// none either; a true bare URL of the same size shows once confirmed.
#[test]
fn viewport_only_window_never_turns_outside_syntax_into_a_bare_url() {
    let prefix = "x".repeat(300_000);
    let cases = [
        (
            "code span",
            format!("`{prefix}\nhttps://inside.example\nend`\n"),
            "https://inside.example",
            None,
        ),
        (
            "markdown label",
            format!("[{prefix}\nhttps://label.example\nend](https://dest.example)\n"),
            "https://label.example",
            Some("https://dest.example"),
        ),
        (
            "inline html",
            format!("<span>{prefix}\nhttps://outside.example\nend</span>\n"),
            "https://outside.example",
            None,
        ),
    ];
    for (name, source, url, formal_destination) in cases {
        let formal = present_window(&source, 0, Some(1), true);
        let found = targets(&formal);
        match formal_destination {
            None => assert!(found.is_empty(), "{name}: formal has no target: {found:?}"),
            Some(destination) => {
                assert_eq!(found.len(), 1, "{name}: {found:?}");
                assert_eq!(found[0].0, destination, "{name}");
                assert!(!found[0].2, "{name}: an inline destination");
            }
        }
        assert!(
            found.iter().all(|(destination, _, _)| destination != url),
            "{name}: the line's own text is not a bare URL: {found:?}"
        );
        assert!(
            link_runs(&formal.lines[0]).len() <= usize::from(formal_destination.is_some()),
            "{name}: no Link color for text that is not a target: {:?}",
            formal.lines[0].style_runs
        );

        let unconfirmed = present_window(&source, 0, Some(1), false);
        assert!(
            targets(&unconfirmed).is_empty(),
            "{name}: a viewport-only window cannot confirm a bare URL: {:?}",
            targets(&unconfirmed)
        );
    }

    let real = format!("{prefix}\nhttps://real.example/x\nend\n");
    let confirmed = present_window(&real, 0, Some(1), true);
    assert_eq!(
        targets(&confirmed),
        [(
            "https://real.example/x".to_owned(),
            range_of(&real, "https://real.example/x"),
            false
        )]
    );
    let line = &confirmed.lines[0];
    let visual = line.visual_text.find("https://real.example/x").expect("url");
    assert!(link_runs(line).contains(&(visual, visual + "https://real.example/x".len())));
    assert!(targets(&present_window(&real, 0, Some(1), false)).is_empty());
}

/// Adding the definition in another block moves the target from the bare label
/// to the formal destination, and removing it moves it back, whichever way the
/// document is presented from scratch.
#[test]
fn definition_added_and_removed_in_another_block_changes_the_target_both_ways() {
    let without = format!("[{LABEL}][r]\n\nother\n");
    let with = format!("{without}\n[r]: {DEST}\n");
    let label = range_of(&without, LABEL);
    for _ in 0..2 {
        assert_eq!(
            targets(&present_window(&without, 0, None, true)),
            [(LABEL.to_owned(), label, false)]
        );
        assert_eq!(
            targets(&present_window(&with, 0, None, true)),
            [(DEST.to_owned(), label, true)]
        );
    }
}
