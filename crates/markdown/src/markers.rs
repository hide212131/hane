use crate::{MarkdownTree, NodeId, NodeKind, TaskMarkerParse};
use hane_document::SourceRange;

/// A single physical line's fence delimiter shape: which byte repeats
/// (`` ` `` or `~`) and how many times. Exposed so a caller presenting one
/// physical line at a time (`hane_presentation`) can classify a candidate
/// fence line without re-parsing the block it belongs to; only whether *this*
/// line closes a *specific* opening (same [`Self::marker`], [`Self::len`] at
/// least as long, per [`fence_closes`]) is Markdown-semantic, and that stays
/// the caller's own decision since only the caller knows which line is the
/// block's actual opening.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FenceDelimiter {
    pub marker: u8,
    pub len: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FenceMarkerEdge {
    Opening,
    Closing,
}

/// Opening or closing fence of a fenced code block, if this line is one. Used by
/// marker derivation to bound the fence delimiters it hides.
pub fn fence_delimiter(source: &str) -> Option<FenceDelimiter> {
    let trimmed = source.trim_start_matches(' ');
    if source.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = *trimmed.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let len = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (len >= 3).then_some(FenceDelimiter { marker, len })
}

/// Returns a delimiter only when the remainder of the physical line is valid
/// closing-fence whitespace. Opening fences intentionally use
/// [`fence_delimiter`] because their info string may follow the marker run;
/// closing fences may contain spaces or tabs, but no info string or other
/// content.
pub fn fence_closing_delimiter(source: &str) -> Option<FenceDelimiter> {
    let delimiter = fence_delimiter(source)?;
    let trimmed = source.trim_start_matches(' ');
    trimmed[delimiter.len..]
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        .then_some(delimiter)
}

/// Whether a line shaped like `candidate` legally closes a fence that opened
/// with `opening`: CommonMark requires the same marker character and a run at
/// least as long as the opening's. A closing-shaped line with the wrong
/// character or a shorter run is not a fence at all — it is literal code
/// content, and must stay visible rather than being hidden as markup.
pub const fn fence_closes(opening: FenceDelimiter, candidate: FenceDelimiter) -> bool {
    opening.marker == candidate.marker && candidate.len >= opening.len
}

pub fn is_table_delimiter(source: &str) -> bool {
    let content = source.trim_end_matches(['\r', '\n']).trim();
    let cells = content.trim_matches('|').split('|').collect::<Vec<_>>();
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let trimmed = cell.trim().trim_matches(':');
            trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-')
        })
}

pub(super) fn absolute_range(base: usize, range: std::ops::Range<usize>) -> SourceRange {
    SourceRange::new(base + range.start, base + range.end)
}

/// Inline kinds whose open/close delimiters are collapsible markup. Kept in one
/// place because marker derivation and presentation must agree on exactly which
/// nodes carry delimiters; `CodeBlock` is included because presentation styles it
/// as an inline run even though its fence markers are derived block-side.
pub const fn has_delimiter_markers(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Strong
            | NodeKind::Emphasis
            | NodeKind::Strikethrough
            | NodeKind::InlineCode
            | NodeKind::Link
            | NodeKind::CodeBlock
    )
}

/// A cursor in a physical line's container prefix. Tabs can be partially
/// consumed by CommonMark containers, so byte offsets and columns differ.
#[derive(Clone, Copy, Default)]
struct PrefixCursor {
    byte: usize,
    column: usize,
    pending_spaces: usize,
}

impl PrefixCursor {
    fn space(&mut self, line: &[u8]) -> bool {
        if self.pending_spaces > 0 {
            self.pending_spaces -= 1;
        } else {
            match line.get(self.byte) {
                Some(b' ') => {}
                Some(b'\t') => self.pending_spaces = 3 - self.column % 4,
                _ => return false,
            }
            self.byte += 1;
        }
        self.column += 1;
        true
    }

    fn indent(&mut self, line: &[u8], limit: usize) -> usize {
        let start = self.column;
        while self.column - start < limit && self.space(line) {}
        self.column - start
    }

    fn quote(&mut self, line: &[u8]) -> Option<std::ops::Range<usize>> {
        self.indent(line, 3);
        if self.pending_spaces != 0 || line.get(self.byte) != Some(&b'>') {
            return None;
        }
        let start = self.byte;
        self.byte += 1;
        self.column += 1;
        self.space(line);
        Some(start..self.byte)
    }

    /// `line`-relative byte range of the marker symbol (bullet char, or
    /// digits plus their `.`/`)` terminator) parsed by [`Self::list_item`],
    /// together with the column width of the whole prefix a continuation
    /// line on the same item must match.
    fn list_item(&mut self, line: &[u8]) -> Option<ListItemPrefix> {
        let start_column = self.column;
        self.indent(line, 3);
        if self.pending_spaces != 0 {
            return None;
        }
        let marker = self.byte;
        match line.get(self.byte) {
            Some(b'-' | b'+' | b'*') => self.byte += 1,
            Some(b'0'..=b'9') => {
                while self.byte - marker < 9 && line.get(self.byte).is_some_and(u8::is_ascii_digit)
                {
                    self.byte += 1;
                }
                if !matches!(line.get(self.byte), Some(b'.' | b')')) {
                    return None;
                }
                self.byte += 1;
            }
            _ => return None,
        }
        self.column += self.byte - marker;
        let symbol = marker..self.byte;
        // An empty opening line has one implicit padding column, including
        // when the marker touches EOL or has several trailing spaces/tabs.
        // This matches the parser's empty-list-item continuation indentation.
        if line[self.byte..]
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            return Some(ListItemPrefix {
                symbol,
                width: self.column - start_column + 1,
            });
        }
        let after_marker = *self;
        let padding = self.indent(line, 5);
        if padding == 0 {
            return None;
        }
        // Five or more spaces mean one padding column followed by content
        // indentation, rather than a wider list-item container.
        if padding > 4 {
            *self = after_marker;
            self.space(line);
        }
        Some(ListItemPrefix {
            symbol,
            width: self.column - start_column,
        })
    }
}

struct ListItemPrefix {
    symbol: std::ops::Range<usize>,
    width: usize,
}

#[derive(Clone, Copy)]
enum PrefixContainer {
    Quote,
    ListItem { start: usize, indent: usize },
}

fn consume_containers(
    containers: &[PrefixContainer],
    line_start: usize,
    line: &[u8],
) -> Option<PrefixCursor> {
    let mut cursor = PrefixCursor::default();
    for container in containers {
        match *container {
            PrefixContainer::Quote => {
                cursor.quote(line)?;
            }
            PrefixContainer::ListItem { start, indent } => {
                if (line_start..line_start + line.len()).contains(&start) {
                    cursor.list_item(line)?;
                } else if cursor.indent(line, indent) != indent {
                    return None;
                }
            }
        }
    }
    Some(cursor)
}

/// CommonMark treats CR, LF and CRLF as line endings, independently of the
/// editor's LF-based RopeBuffer lines. Keep the original bytes for source maps.
fn markdown_line_start(source: &str, mut at: usize) -> usize {
    if source.as_bytes().get(at) == Some(&b'\n') && at > 0 && source.as_bytes()[at - 1] == b'\r' {
        at -= 1;
    }
    source[..at].rfind(['\r', '\n']).map_or(0, |at| at + 1)
}

pub(crate) fn markdown_lines(mut source: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if source.is_empty() {
            return None;
        }
        let end = source.find(['\r', '\n']).map_or(source.len(), |at| {
            at + if source.as_bytes()[at] == b'\r' && source.as_bytes().get(at + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            }
        });
        let (line, rest) = source.split_at(end);
        source = rest;
        Some(line)
    })
}

/// Recover a quote's markers from its actual ancestor containers, not a fixed
/// byte width per depth. Lazy continuation lines fail the prefix scan and have
/// no marker. List-item padding is measured on that item's opening line.
fn ancestor_containers(
    tree: &MarkdownTree,
    id: NodeId,
    range: SourceRange,
    source: &str,
) -> Option<Vec<PrefixContainer>> {
    let ancestors: Vec<_> = tree.ancestors(id).skip(1).collect();
    let mut containers = Vec::new();
    for ancestor in ancestors.into_iter().rev() {
        let node = tree.node(ancestor).expect("ancestor exists");
        match node.kind {
            NodeKind::Quote => containers.push(PrefixContainer::Quote),
            NodeKind::ListItem { .. } => {
                let start = node.source_range.start.0 - range.start.0;
                let line_start = markdown_line_start(source, start);
                let line = markdown_lines(&source[line_start..]).next().unwrap_or("");
                let mut cursor = consume_containers(&containers, line_start, line.as_bytes())?;
                let indent = cursor.list_item(line.as_bytes())?.width;
                containers.push(PrefixContainer::ListItem { start, indent });
            }
            _ => {}
        }
    }
    Some(containers)
}

fn quote_markers(
    tree: &MarkdownTree,
    id: NodeId,
    range: SourceRange,
    source: &str,
) -> Vec<SourceRange> {
    let Some(containers) = ancestor_containers(tree, id, range, source) else {
        return Vec::new();
    };
    let node = tree.node(id).expect("quote exists");
    let start = node.source_range.start.0 - range.start.0;
    let end = node.source_range.end.0 - range.start.0;
    let mut line_start = markdown_line_start(source, start);
    let mut markers = Vec::new();
    for line in markdown_lines(&source[line_start..end]) {
        if let Some(mut cursor) = consume_containers(&containers, line_start, line.as_bytes())
            && let Some(marker) = cursor.quote(line.as_bytes())
        {
            markers.push(absolute_range(range.start.0 + line_start, marker));
        }
        line_start += line.len();
    }
    markers
}

pub(super) struct ParsedCodeSpan {
    pub(super) node: NodeId,
    pub(super) content: String,
}

/// Project code content back to source before choosing padding. Source spans
/// include container prefixes; those bytes are not part of Event::Code's text.
pub(super) fn code_padding(
    tree: &MarkdownTree,
    codes: &[ParsedCodeSpan],
    range: SourceRange,
    source: &str,
) -> Vec<SourceRange> {
    let mut padding = Vec::new();
    for code in codes {
        let node = tree.node(code.node).expect("code node exists");
        let start = node.source_range.start.0 - range.start.0;
        let end = node.source_range.end.0 - range.start.0;
        let delimiter = source[start..end]
            .bytes()
            .take_while(|b| *b == b'`')
            .count();
        let Some(containers) = ancestor_containers(tree, code.node, range, source) else {
            continue;
        };
        let mut cursor = start + delimiter;
        let end = end - delimiter;
        let mut normalized = String::new();
        let mut first = None;
        let mut last = None;
        while cursor < end {
            let from = cursor;
            let byte = source.as_bytes()[cursor];
            let is_break = matches!(byte, b'\r' | b'\n');
            let character = if is_break {
                cursor += 1;
                if byte == b'\r' && source.as_bytes().get(cursor) == Some(&b'\n') {
                    cursor += 1;
                }
                ' '
            } else {
                let character = source[cursor..].chars().next().expect("source character");
                cursor += character.len_utf8();
                character
            };
            let unit = absolute_range(range.start.0, from..cursor);
            first.get_or_insert(unit);
            last = Some(unit);
            normalized.push(character);
            if is_break {
                // Lazy continuation may carry only some ancestor prefixes.
                let line = &source.as_bytes()[cursor..end];
                let mut prefix = PrefixCursor::default();
                for container in &containers {
                    let before = prefix;
                    let matched = match container {
                        PrefixContainer::Quote => prefix.quote(line).is_some(),
                        PrefixContainer::ListItem { indent, .. } => {
                            prefix.indent(line, *indent) == *indent
                        }
                    };
                    if !matched {
                        prefix = before;
                        break;
                    }
                }
                cursor += prefix.byte;
            }
        }
        // The parser is authoritative: only emit padding when removing one
        // semantic space from each end reproduces its actual code content.
        if normalized.starts_with(' ')
            && normalized.ends_with(' ')
            && normalized.bytes().any(|byte| byte != b' ')
            && normalized.get(1..normalized.len() - 1) == Some(code.content.as_str())
        {
            padding.extend([first.expect("nonempty code"), last.expect("nonempty code")]);
        }
    }
    padding
}

/// Bytes a soft or hard line break makes insignificant, so presentation can
/// hide them like other derived padding while keeping their source bytes
/// addressable. Two distinct sources, per the CommonMark line-break rules:
///
/// - The single trailing U+0020 space CommonMark folds into a soft break (two
///   or more would have made it a hard break instead; CommonMark §6.8 defines
///   `space` as U+0020 specifically, so a trailing tab/VT/FF is not part of
///   this rule and stays visible/addressable textual content instead).
///   pulldown-cmark's own `Text` item ends before this byte and its
///   `SoftBreak` item starts at the line ending itself, so — unlike a hard
///   break's own syntax, which is a real `HardBreak` item covering it — this
///   one byte is not part of any parsed item's range at all; it is recovered
///   here directly from `source`.
/// - The leading indentation of the physical line either break kind
///   continues onto: CommonMark drops any amount of it when forming a
///   paragraph's inline content, independent of the break that precedes it.
pub(super) fn line_break_padding(
    tree: &MarkdownTree,
    range: SourceRange,
    source: &str,
) -> Vec<SourceRange> {
    let mut padding = Vec::new();
    for (id, span) in tree
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::SoftBreak | NodeKind::HardBreak))
    {
        let end = span.source_range.end.0.saturating_sub(range.start.0);
        if end > source.len() {
            continue;
        }
        if span.kind == NodeKind::SoftBreak {
            let break_start = span.source_range.start.0;
            if break_start > range.start.0
                && let Some(byte) = source
                    .as_bytes()
                    .get(break_start - range.start.0 - 1)
                    .copied()
                && byte == b' '
            {
                padding.push(SourceRange::new(break_start - 1, break_start));
            }
        }
        let Some(containers) = ancestor_containers(tree, id, range, source) else {
            continue;
        };
        // `end` is the byte right after the line ending this break owns (see
        // the `HardBreak` marker derivation above), i.e. the start of the
        // physical line it continues onto.
        let line_start = end;
        let line = source
            .get(line_start..)
            .and_then(|rest| markdown_lines(rest).next())
            .unwrap_or("");
        // Lazy continuation may carry only some ancestor prefixes, or none:
        // stop at the first container this line does not actually have, the
        // same way `code_padding` does for code spans. The parser already
        // decided this line is part of the same inline content (that is why
        // it produced this SoftBreak/HardBreak at all), so whatever leading
        // whitespace remains past the prefixes this line really has is still
        // insignificant, regardless of how many ancestor levels went missing.
        let mut cursor = PrefixCursor::default();
        for container in &containers {
            let before = cursor;
            let matched = match *container {
                PrefixContainer::Quote => cursor.quote(line.as_bytes()).is_some(),
                PrefixContainer::ListItem { start, indent } => {
                    if (line_start..line_start + line.len()).contains(&start) {
                        cursor.list_item(line.as_bytes()).is_some()
                    } else {
                        cursor.indent(line.as_bytes(), indent) == indent
                    }
                }
            };
            if !matched {
                cursor = before;
                break;
            }
        }
        let before = cursor.byte;
        cursor.indent(line.as_bytes(), line.len());
        if cursor.byte > before {
            padding.push(absolute_range(
                range.start.0 + line_start,
                before..cursor.byte,
            ));
        }
    }
    padding
}

pub(super) struct DerivedMarkers {
    pub(super) markers: Vec<SourceRange>,
    pub(super) fence_markers: Vec<SourceRange>,
    pub(super) fence_marker_edges: Vec<(SourceRange, FenceMarkerEdge)>,
    pub(super) quote_markers: Vec<(SourceRange, NodeId)>,
    pub(super) list_item_markers: Vec<(SourceRange, NodeId)>,
}

/// Derives marker source ranges by lexing only inside the source ranges that
/// pulldown-cmark already attributed to each node. The event ranges stay
/// authoritative; this only recovers open/close delimiter positions that the
/// event stream does not expose. Returned ranges are sorted and merged.
pub(super) fn derive_markers(
    tree: &MarkdownTree,
    range: SourceRange,
    source: &str,
) -> DerivedMarkers {
    let mut markers = Vec::new();
    let mut fence_markers = Vec::new();
    let mut fence_marker_edges = Vec::new();
    let mut quote_owners = Vec::new();
    let mut list_item_owners = Vec::new();
    for (id, block) in tree.blocks() {
        let relative = block.source_range.start.0.saturating_sub(range.start.0);
        let tail = source.get(relative..).unwrap_or_default();
        match block.kind {
            NodeKind::Heading(_) => {
                // Only the parser decides whether this is ATX (Setext starts
                // with content). Recover delimiters inside its authoritative
                // heading range, leaving escapes and inline delimiters intact.
                let body = &tail[..block.source_range.end.0 - block.source_range.start.0];
                let hashes = body.bytes().take_while(|byte| *byte == b'#').count();
                if (1..=6).contains(&hashes)
                    && body
                        .as_bytes()
                        .get(hashes)
                        .is_none_or(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
                {
                    let opening_end = hashes
                        + body[hashes..]
                            .bytes()
                            .take_while(|byte| matches!(byte, b' ' | b'\t'))
                            .count();
                    let start = block.source_range.start.0;
                    let end = start + body.trim_end_matches(['\r', '\n']).len();
                    markers.push(SourceRange::new(start, start + opening_end));
                    // The parser has already excluded the optional closer and
                    // trailing whitespace from its last direct child. Do not
                    // re-scan hashes: escaped/literal hashes belong to content.
                    let content_end = block
                        .children
                        .last()
                        .and_then(|id| tree.node(*id))
                        .map_or(start + opening_end, |node| node.source_range.end.0);
                    if content_end < end {
                        markers.push(SourceRange::new(content_end, end));
                    }
                }
            }
            NodeKind::Quote => {
                for marker in quote_markers(tree, id, range, source) {
                    markers.push(marker);
                    quote_owners.push((marker, id));
                }
            }
            NodeKind::ListItem { .. } => {
                // The marker (bullet, or digits plus `.`/`)`) appears only on
                // the item's own opening physical line; unlike a quote's
                // per-line prefix, no ancestor container prefix precedes it
                // here, since the item's own source range already starts at
                // that line's indentation.
                let line = markdown_lines(tail).next().unwrap_or("");
                if let Some(prefix) = PrefixCursor::default().list_item(line.as_bytes()) {
                    // Exactly one separator byte (space or tab) is markup,
                    // matching the one CommonMark requires; an item whose
                    // marker touches the line ending has none. Any further
                    // padding is ordinary indentation, not marker syntax.
                    let mut end = prefix.symbol.end;
                    if matches!(line.as_bytes().get(end), Some(b' ' | b'\t')) {
                        end += 1;
                    }
                    let marker = SourceRange::new(
                        block.source_range.start.0 + prefix.symbol.start,
                        block.source_range.start.0 + end,
                    );
                    markers.push(marker);
                    list_item_owners.push((marker, id));
                }
            }
            NodeKind::CodeBlock => {
                // Only the fence delimiter lines are markup; the code between
                // them is literal content and must stay visible.
                let node_start = block.source_range.start.0 - range.start.0;
                let node_end = block.source_range.end.0 - range.start.0;
                let opening_line = markdown_lines(&source[node_start..node_end])
                    .next()
                    .unwrap_or("");
                if let Some(opening_fence) = fence_delimiter(opening_line) {
                    let opening_content_len = opening_line.trim_end_matches(['\r', '\n']).len();
                    // Keep the delimiter run (leading indentation plus the
                    // repeated `` ` `` or `~` bytes) as its own marker. The
                    // presenter maps the remaining info string/trailing
                    // whitespace separately so the opening edge remains
                    // source-addressable while the inactive row can collapse
                    // as a whole.
                    let indent = opening_content_len
                        - opening_line[..opening_content_len]
                            .trim_start_matches(' ')
                            .len();
                    let opening = indent + opening_fence.len;
                    if opening > 0 {
                        let marker = SourceRange::new(
                            block.source_range.start.0,
                            block.source_range.start.0 + opening,
                        );
                        markers.push(marker);
                        fence_markers.push(marker);
                        fence_marker_edges.push((marker, FenceMarkerEdge::Opening));
                    }
                    // A quote or list item nested fence still carries its
                    // container's own prefix on every continuation line,
                    // inside this block's own raw source range (the parser
                    // reports the node's range in the original, un-stripped
                    // source, the same reason `quote_markers`/`code_padding`
                    // exist). Skip exactly that prefix on each physical line
                    // so the block's real last logical line — not a
                    // prefix-contaminated lookalike such as `"> ```"` — is
                    // what gets checked against the opening fence.
                    let containers =
                        ancestor_containers(tree, id, range, source).unwrap_or_default();
                    let mut line_start = node_start + opening_line.len();
                    let mut last_logical: Option<(usize, &str)> = None;
                    for line in markdown_lines(&source[line_start..node_end]) {
                        let this_line_start = line_start;
                        line_start += line.len();
                        let logical_start =
                            consume_containers(&containers, this_line_start, line.as_bytes())
                                .map_or(this_line_start, |cursor| this_line_start + cursor.byte);
                        last_logical = Some((
                            logical_start,
                            &source[logical_start..this_line_start + line.len()],
                        ));
                    }
                    // A closing-shaped last line only really closes the fence
                    // when it repeats the opening's own marker character at
                    // least as many times (CommonMark's closing-fence rule);
                    // otherwise it is an unterminated fence and this line is
                    // literal code content, not markup, however much it may
                    // look like a shorter or differently-charactered fence.
                    if let Some((closing_start, closing_line)) = last_logical
                        && let Some(closing_fence) = fence_closing_delimiter(closing_line)
                        && fence_closes(opening_fence, closing_fence)
                    {
                        let closing_len = closing_line.trim_end_matches(['\r', '\n']).len();
                        let marker = SourceRange::new(
                            range.start.0 + closing_start,
                            range.start.0 + closing_start + closing_len,
                        );
                        markers.push(marker);
                        fence_markers.push(marker);
                        fence_marker_edges.push((marker, FenceMarkerEdge::Closing));
                    }
                }
            }
            _ => {}
        }
    }
    for (_, span) in tree
        .iter()
        .filter(|(_, node)| has_delimiter_markers(node.kind))
    {
        let start = span.source_range.start.0;
        let end = span.source_range.end.0;
        if start < range.start.0 || end > range.end.0 || start >= end {
            continue;
        }
        let text = &source[start - range.start.0..end - range.start.0];
        let marker_len = match span.kind {
            NodeKind::Strong | NodeKind::Emphasis => text
                .as_bytes()
                .first()
                .filter(|marker| matches!(marker, b'*' | b'_'))
                .map_or(0, |marker| {
                    text.as_bytes()
                        .iter()
                        .take_while(|byte| *byte == marker)
                        .count()
                }),
            NodeKind::Strikethrough => 2,
            NodeKind::InlineCode => text.bytes().take_while(|byte| *byte == b'`').count(),
            NodeKind::Link => {
                if let (Some(open), Some(close)) = (text.find('['), text.find("](")) {
                    markers.push(SourceRange::new(start + open, start + open + 1));
                    markers.push(SourceRange::new(start + close, end));
                }
                0
            }
            _ => 0,
        };
        if marker_len > 0 && marker_len * 2 <= text.len() {
            markers.push(SourceRange::new(start, start + marker_len));
            markers.push(SourceRange::new(end - marker_len, end));
        }
    }
    for (_, span) in tree
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::HardBreak)
    {
        let start = span.source_range.start.0;
        let end = span.source_range.end.0;
        if start < range.start.0 || end > range.end.0 || start >= end {
            continue;
        }
        // pulldown-cmark's hard-break range covers the break syntax (trailing
        // spaces or a backslash) plus the physical line ending it precedes.
        // Only the syntax is markup; the line-ending bytes stay ordinary
        // source so they remain addressable/preserved rather than hidden.
        let text = &source[start - range.start.0..end - range.start.0];
        let marker_len = text.trim_end_matches(['\r', '\n']).len();
        if marker_len > 0 {
            markers.push(SourceRange::new(start, start + marker_len));
        }
    }
    markers.sort_by_key(|marker| (marker.start, marker.end));
    let mut merged: Vec<SourceRange> = Vec::with_capacity(markers.len());
    for marker in markers {
        if let Some(previous) = merged.last_mut()
            && marker.start < previous.end
        {
            previous.end = previous.end.max(marker.end);
        } else {
            merged.push(marker);
        }
    }
    fence_markers.sort_by_key(|marker| (marker.start, marker.end));
    fence_markers.dedup();
    fence_marker_edges.sort_by_key(|(marker, _)| (marker.start, marker.end));
    fence_marker_edges.dedup_by_key(|(marker, _)| (marker.start, marker.end));
    DerivedMarkers {
        markers: merged,
        fence_markers,
        fence_marker_edges,
        quote_markers: quote_owners,
        list_item_markers: list_item_owners,
    }
}

/// Confirms each parser-reported task marker. The marker must be the direct
/// `TaskMarker` child of its own `ListItem` (so a nested item's marker never
/// belongs to its ancestors), be exactly one of `[ ]`, `[x]` or `[X]` in the
/// source, and sit immediately after that item's own bullet/number marker. A
/// candidate failing any check is left as ordinary text.
///
/// `list_item_markers` must be in ascending owner order, which is how
/// [`derive_markers`] produces it.
pub(super) fn task_markers(
    tree: &MarkdownTree,
    list_item_markers: &[(SourceRange, NodeId)],
    range: SourceRange,
    source: &str,
) -> Vec<TaskMarkerParse> {
    let mut tasks = Vec::new();
    for (item, node) in tree.iter() {
        if !matches!(node.kind, NodeKind::ListItem { task: Some(_) }) {
            continue;
        }
        let Some((marker_node, checked)) = tree.children(item).iter().find_map(|child| {
            match tree.node(*child)?.kind {
                NodeKind::TaskMarker(checked) => Some((*child, checked)),
                _ => None,
            }
        }) else {
            continue;
        };
        let Some(reported) = tree.node(marker_node).map(|node| node.source_range) else {
            continue;
        };
        let Ok(position) = list_item_markers.binary_search_by_key(&item, |(_, owner)| *owner)
        else {
            continue;
        };
        // The triple must start right where the item's own marker ends, and
        // the parser's `TaskMarker` span must cover it. Only the triple is
        // trusted from the source; the reported span's exact bounds (which
        // may include trailing whitespace) are not.
        let list_marker_end = list_item_markers[position].0.end;
        if list_marker_end.0 < range.start.0
            || reported.start > list_marker_end
            || reported.end.0 < list_marker_end.0 + 3
        {
            continue;
        }
        let marker = SourceRange::new(list_marker_end.0, list_marker_end.0 + 3);
        let start = marker.start.0 - range.start.0;
        let literal = match source.as_bytes().get(start..start + 3) {
            Some([b'[', b' ', b']']) => false,
            Some([b'[', b'x' | b'X', b']']) => true,
            _ => continue,
        };
        if literal != checked {
            continue;
        }
        let end = if matches!(source.as_bytes().get(start + 3), Some(b' ' | b'\t')) {
            marker.end.0 + 1
        } else {
            marker.end.0
        };
        tasks.push(TaskMarkerParse {
            item,
            marker_range: marker,
            hidden_range: SourceRange::new(marker.start.0, end),
            checked,
        });
    }
    tasks
}

/// Recovers a list item's own structural indentation on every physical line
/// its content occupies, including parser-accepted padding between an opening
/// marker and its body: source-addressable so an inactive presentation can
/// hide exactly the bytes the parser required for the item to keep owning that
/// line, and disclose the original bytes (spaces, tabs) once the item itself
/// is active.
///
/// Mirrors [`quote_markers`]'s per-owner tiling for nested containers: each
/// item is scanned independently, consuming its own ancestors first via
/// [`ancestor_containers`]/[`consume_containers`] and attributing only the
/// remainder — its own registered indent width — to itself, so a physical
/// line several nested items all continue onto ends up with one prefix range
/// per owner rather than one range that conflates them. A line whose
/// required indentation is not fully present (a lazy paragraph continuation,
/// or a blank line) is left alone: [`line_break_padding`] already hides that
/// shortfall — and any genuinely superfluous indentation past what every
/// level requires, since its own consumption only starts after every
/// container it can match has already consumed its own required width — as
/// one undivided run, because CommonMark discards it without regard to which
/// container needed which part of it.
pub(super) fn list_structural_prefixes(
    tree: &MarkdownTree,
    range: SourceRange,
    source: &str,
) -> Vec<(SourceRange, NodeId, usize)> {
    let mut prefixes = Vec::new();
    for (id, block) in tree.blocks() {
        if !matches!(block.kind, NodeKind::ListItem { .. }) {
            continue;
        }
        let Some(containers) = ancestor_containers(tree, id, range, source) else {
            continue;
        };
        let start = block.source_range.start.0 - range.start.0;
        let end = block.source_range.end.0 - range.start.0;
        let opening_line_start = markdown_line_start(source, start);
        let opening_line = markdown_lines(&source[opening_line_start..])
            .next()
            .unwrap_or("");
        let Some(mut opening_cursor) =
            consume_containers(&containers, opening_line_start, opening_line.as_bytes())
        else {
            continue;
        };
        let before_own_prefix = opening_cursor.byte;
        let mut indent_cursor = opening_cursor;
        let own_prefix_columns = indent_cursor.indent(opening_line.as_bytes(), 3);
        let marker_start_cursor = opening_cursor;
        let Some(prefix) = opening_cursor.list_item(opening_line.as_bytes()) else {
            continue;
        };
        if own_prefix_columns > 0 {
            prefixes.push((
                absolute_range(
                    range.start.0 + opening_line_start,
                    before_own_prefix..prefix.symbol.start,
                ),
                id,
                own_prefix_columns,
            ));
        }
        // The marker projection hides exactly one separator byte so that it
        // can preserve the source mapping of the written marker. CommonMark
        // still treats the remaining 1-3 columns of padding as structural
        // when there are at most four columns after the marker. Claim those
        // bytes separately so an inactive row renders one synthesized label
        // followed immediately by its body. For the 5+ case `list_item`
        // resets to one separator column, intentionally leaving the excess
        // spaces visible as content indentation.
        let marker_end = prefix.symbol.end
            + usize::from(matches!(
                opening_line.as_bytes().get(prefix.symbol.end),
                Some(b' ' | b'\t')
            ));
        if opening_cursor.byte > marker_end {
            let mut marker_end_cursor = marker_start_cursor;
            marker_end_cursor.indent(opening_line.as_bytes(), 3);
            marker_end_cursor.byte += prefix.symbol.len();
            marker_end_cursor.column += prefix.symbol.len();
            if marker_end_cursor.space(opening_line.as_bytes()) {
                while marker_end_cursor.pending_spaces > 0 {
                    marker_end_cursor.space(opening_line.as_bytes());
                }
            }
            let columns = opening_cursor
                .column
                .saturating_sub(marker_end_cursor.column);
            if columns > 0 {
                prefixes.push((
                    absolute_range(
                        range.start.0 + opening_line_start,
                        marker_end..opening_cursor.byte,
                    ),
                    id,
                    columns,
                ));
            }
        }
        let indent = prefix.width;

        let mut line_start = opening_line_start + opening_line.len();
        for line in markdown_lines(&source[line_start..end]) {
            if let Some(mut cursor) = consume_containers(&containers, line_start, line.as_bytes()) {
                let before = cursor.byte;
                if cursor.indent(line.as_bytes(), indent) == indent && cursor.byte > before {
                    prefixes.push((
                        absolute_range(range.start.0 + line_start, before..cursor.byte),
                        id,
                        indent,
                    ));
                }
            }
            line_start += line.len();
        }
    }
    prefixes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeKind, parse_document};
    use hane_document::Revision;

    #[test]
    fn markdown_line_scan_preserves_bytes_and_does_not_split_crlf() {
        let source = "日\r本\r\n語\nlast";
        assert_eq!(
            markdown_lines(source).collect::<Vec<_>>(),
            ["日\r", "本\r\n", "語\n", "last"]
        );
        assert_eq!(markdown_line_start(source, 4), 4);
        assert_eq!(markdown_line_start(source, 8), 4); // Inside CRLF.
        assert_eq!(markdown_line_start(source, 9), 9);
    }

    #[test]
    fn quote_markers_follow_list_ancestors_without_hiding_literal_greater_than() {
        for (source, expected) in [
            ("- > first\n  > second\n", vec![(2, 4), (12, 14)]),
            (
                "> - > first\n>   > second\n",
                vec![(0, 2), (4, 6), (12, 14), (16, 18)],
            ),
            ("1) > first\n   > second\n", vec![(3, 5), (14, 16)]),
            (
                "- > first\n  lazy > literal\n  > last\n",
                vec![(2, 4), (29, 31)],
            ),
            (
                "> ```\n> > literal\n> ```\n",
                vec![(0, 2), (6, 8), (18, 20)],
            ),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let actual: Vec<_> = parsed
                .tree
                .blocks()
                .filter(|(_, node)| node.kind == NodeKind::Quote)
                .flat_map(|(id, _)| quote_markers(&parsed.tree, id, parsed.source_range, source))
                .collect();
            let mut actual = actual;
            actual.sort_by_key(|marker| marker.start);
            let expected: Vec<_> = expected
                .into_iter()
                .map(|(start, end)| SourceRange::new(start, end))
                .collect();
            assert_eq!(actual, expected, "source: {source:?}");
        }
    }

    #[test]
    fn quote_markers_follow_list_items_with_an_empty_opening_line() {
        for marker in ["-", "1.", "1)"] {
            for padding in ["", " ", "   ", "\t"] {
                for newline in ["\n", "\r", "\r\n"] {
                    let indent = " ".repeat(marker.len() + 1);
                    let source = format!(
                        "{marker}{padding}{newline}{indent}> first{newline}{indent}> second"
                    );
                    let base = 37;
                    let parsed = parse_document(
                        Revision(1),
                        SourceRange::new(base, base + source.len()),
                        &source,
                    );
                    let (quote_id, quote) = parsed
                        .tree
                        .blocks()
                        .find(|(_, node)| node.kind == NodeKind::Quote)
                        .expect("the parser recognizes the nested quote");
                    let item_id = quote.parent.unwrap();
                    assert!(matches!(
                        parsed.tree.node(item_id).unwrap().kind,
                        NodeKind::ListItem { .. }
                    ));
                    // The item's own marker is unaffected by its opening line
                    // being empty: exactly one separator byte is markup when
                    // one exists in the source, none when the marker touches
                    // the line ending directly.
                    let separator = usize::from(!padding.is_empty());
                    let expected_item_marker =
                        SourceRange::new(base, base + marker.len() + separator);
                    assert_eq!(
                        parsed.list_item_markers,
                        vec![(expected_item_marker, item_id)],
                        "source: {source:?}"
                    );
                    assert!(parsed.markers.contains(&expected_item_marker));
                    let expected: Vec<_> = source
                        .match_indices('>')
                        .map(|(at, _)| SourceRange::new(base + at, base + at + 2))
                        .collect();
                    assert_eq!(
                        quote_markers(&parsed.tree, quote_id, parsed.source_range, &source),
                        expected,
                        "source: {source:?}"
                    );
                    for range in expected {
                        assert!(
                            parsed.markers.contains(&range),
                            "missing {range:?} in {source:?}"
                        );
                        assert!(parsed.quote_markers.contains(&(range, quote_id)));
                    }
                }
            }
        }
    }
}
