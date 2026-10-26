//! Planning for "insert the clipboard URL as a link over the selection".
//!
//! The plan is computed from the existing parser: applicability is decided from
//! the parsed tree and source ranges, and a generated link is re-parsed before
//! it is offered, so nothing is written unless the parser reads the result as
//! one link to the requested destination with the selection's decoration kept.

use crate::{MarkdownParse, NodeId, NodeKind, parse_document};
use hane_document::{Revision, SourceOffset, SourceRange};
use url::Url;

/// Why a selection cannot become a link. None of these touch the document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkInsertionRefusal {
    /// The selection is empty or only whitespace.
    EmptySelection,
    /// The range is outside the source or not on a character boundary.
    OutOfRange,
    /// The selection is not inside one heading, paragraph, list item or table
    /// cell (code block, several blocks or cells, quote/list markers, ...).
    UnsupportedContext,
    /// The selection overlaps a link, image, HTML, footnote reference or task
    /// marker.
    ExistingSyntax,
    /// The selection cuts through emphasis, strong, strikethrough or code.
    PartialSyntax,
    /// An end of the selection splits a backslash escape.
    UnsafeBoundary,
    /// The generated Markdown was not read back as the intended single link.
    Unverified,
}

impl LinkInsertionRefusal {
    /// Short Japanese status for a refused execution.
    pub const fn message(self) -> &'static str {
        match self {
            Self::EmptySelection => "リンクにする文字を選択してください",
            Self::OutOfRange => "選択範囲を確認できないためリンクを挿入しませんでした",
            Self::UnsupportedContext => "この範囲にはリンクを挿入できません",
            Self::ExistingSyntax => "既存のリンクなどを含む範囲にはリンクを挿入できません",
            Self::PartialSyntax => "装飾の途中を含む範囲にはリンクを挿入できません",
            Self::UnsafeBoundary => "エスケープの途中を含む範囲にはリンクを挿入できません",
            Self::Unverified => "安全にリンクを生成できないため挿入しませんでした",
        }
    }
}

/// One source replacement that turns the selection into a link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkInsertionPlan {
    /// The replaced range: exactly the selection.
    pub range: SourceRange,
    pub replacement: String,
    /// Caret position just after the generated link syntax.
    pub caret_after: SourceOffset,
}

/// The URL to insert for clipboard text, or `None` when the text is not exactly
/// one absolute HTTP(S) URL with a host. The returned value is the trimmed
/// input, not a normalised form.
pub fn clipboard_http_url(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return None;
    }
    let (scheme, rest) = trimmed.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return None;
    }
    if rest.is_empty() || rest.starts_with(['/', '\\']) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.starts_with('@') || authority.starts_with(':') {
        return None;
    }
    let parsed = Url::parse(trimmed).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.host_str().filter(|host| !host.is_empty())?;
    Some(trimmed.to_owned())
}

/// Plans the replacement of `selection` in `source` with `[selection](url)`.
///
/// `url` must come from [`clipboard_http_url`]. The selection text is kept as
/// the label verbatim, apart from backslash-escaping literal brackets when the
/// plain label is not read back as one link.
pub fn plan_link_insertion(
    source: &str,
    selection: SourceRange,
    url: &str,
) -> Result<LinkInsertionPlan, LinkInsertionRefusal> {
    use LinkInsertionRefusal as Refusal;

    let (start, end) = (selection.start.0, selection.end.0);
    if start >= end {
        return Err(Refusal::EmptySelection);
    }
    if end > source.len() || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return Err(Refusal::OutOfRange);
    }
    let label = &source[start..end];
    if label.trim().is_empty() {
        return Err(Refusal::EmptySelection);
    }
    if splits_escape(source, start) || splits_escape(source, end) {
        return Err(Refusal::UnsafeBoundary);
    }
    let trailing_backslashes = label.bytes().rev().take_while(|byte| *byte == b'\\').count();
    if trailing_backslashes % 2 == 1 {
        return Err(Refusal::UnsafeBoundary);
    }

    let old = parse_document(Revision(0), SourceRange::new(0, source.len()), source);
    let container = inline_container(&old, selection)?;
    check_inline_nodes(&old, selection)?;
    let in_table_cell = old
        .tree
        .node(container)
        .is_some_and(|node| node.kind == NodeKind::TableCell);
    let destination = escape_destination(url, in_table_cell);

    for escape_brackets in [false, true] {
        let label_text = if escape_brackets {
            escape_label_brackets(source, selection, &old)
        } else {
            label.to_owned()
        };
        let replacement = format!("[{label_text}]({destination})");
        if verify(source, selection, &replacement, url, &old) {
            let caret_after = SourceOffset(start + replacement.len());
            return Ok(LinkInsertionPlan {
                range: selection,
                replacement,
                caret_after,
            });
        }
    }
    Err(Refusal::Unverified)
}

/// Whether `pos` sits between a backslash and the punctuation it escapes.
fn splits_escape(source: &str, pos: usize) -> bool {
    let bytes = source.as_bytes();
    if pos == 0 || pos >= bytes.len() {
        return false;
    }
    let run = bytes[..pos]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    run % 2 == 1 && bytes[pos].is_ascii_punctuation()
}

/// The heading, paragraph, tight list item or table cell holding the whole
/// selection in its inline content.
fn inline_container(
    parse: &MarkdownParse,
    selection: SourceRange,
) -> Result<NodeId, LinkInsertionRefusal> {
    use LinkInsertionRefusal as Refusal;

    let mut best: Option<(NodeId, usize)> = None;
    for (id, node) in parse.tree.blocks() {
        let range = node.source_range;
        if range.start <= selection.start
            && selection.end <= range.end
            && best.is_none_or(|(_, depth)| node.depth >= depth)
        {
            best = Some((id, node.depth));
        }
    }
    let (id, _) = best.ok_or(Refusal::UnsupportedContext)?;
    let node = parse.tree.node(id).ok_or(Refusal::UnsupportedContext)?;
    if !matches!(
        node.kind,
        NodeKind::Paragraph | NodeKind::Heading(_) | NodeKind::TableCell | NodeKind::ListItem { .. }
    ) {
        return Err(Refusal::UnsupportedContext);
    }
    let mut region: Option<(usize, usize)> = None;
    for child in &node.children {
        let Some(child) = parse.tree.node(*child) else {
            continue;
        };
        if child.kind.is_block() {
            if child.source_range.intersects(selection) {
                return Err(Refusal::UnsupportedContext);
            }
        } else if child.kind.is_inline() {
            let (start, end) = (child.source_range.start.0, child.source_range.end.0);
            region = Some(region.map_or((start, end), |(first, last)| {
                (first.min(start), last.max(end))
            }));
        }
    }
    let (first, last) = region.ok_or(Refusal::UnsupportedContext)?;
    if selection.start.0 < first || selection.end.0 > last {
        return Err(Refusal::UnsupportedContext);
    }
    Ok(id)
}

/// Rejects selections that overlap syntax a link cannot safely wrap.
fn check_inline_nodes(
    parse: &MarkdownParse,
    selection: SourceRange,
) -> Result<(), LinkInsertionRefusal> {
    use LinkInsertionRefusal as Refusal;

    for (_, node) in parse.tree.iter() {
        let range = node.source_range;
        if !range.intersects(selection) {
            continue;
        }
        let contained = selection.start <= range.start && range.end <= selection.end;
        match node.kind {
            NodeKind::Link
            | NodeKind::Image
            | NodeKind::InlineHtml
            | NodeKind::Html
            | NodeKind::HtmlBlock
            | NodeKind::FootnoteReference
            | NodeKind::TaskMarker(_)
            | NodeKind::CodeBlock
            | NodeKind::Unsupported => return Err(Refusal::ExistingSyntax),
            NodeKind::InlineCode => {
                if !contained {
                    return Err(Refusal::PartialSyntax);
                }
            }
            NodeKind::Strong | NodeKind::Emphasis | NodeKind::Strikethrough => {
                if contained {
                    continue;
                }
                let interior = node
                    .children
                    .first()
                    .zip(node.children.last())
                    .and_then(|(first, last)| {
                        Some((
                            parse.tree.node(*first)?.source_range.start,
                            parse.tree.node(*last)?.source_range.end,
                        ))
                    });
                let inside = interior
                    .is_some_and(|(first, last)| first <= selection.start && selection.end <= last);
                if !inside {
                    return Err(Refusal::PartialSyntax);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Backslash-escapes the label's literal `[` and `]` (outside code spans and
/// not already escaped), leaving every other byte of the selection untouched.
fn escape_label_brackets(source: &str, selection: SourceRange, parse: &MarkdownParse) -> String {
    let start = selection.start.0;
    let codes: Vec<SourceRange> = parse
        .tree
        .inlines()
        .filter(|(_, node)| node.kind == NodeKind::InlineCode)
        .map(|(_, node)| node.source_range)
        .collect();
    let label = &source[start..selection.end.0];
    let mut out = String::with_capacity(label.len() + 4);
    let mut backslashes = 0usize;
    for (offset, ch) in label.char_indices() {
        let at = start + offset;
        let in_code = codes
            .iter()
            .any(|code| code.start.0 <= at && at < code.end.0);
        if (ch == '[' || ch == ']') && !in_code && backslashes.is_multiple_of(2) {
            out.push('\\');
        }
        out.push(ch);
        if ch == '\\' && !in_code {
            backslashes += 1;
        } else {
            backslashes = 0;
        }
    }
    out
}

/// The URL as a CommonMark link destination. Parentheses, angle brackets and
/// backslashes are backslash-escaped, as is an `&` that would read as an entity
/// reference and, inside a table cell, `|`. Percent-encoding is left alone.
fn escape_destination(url: &str, escape_pipe: bool) -> String {
    let mut out = String::with_capacity(url.len() + 8);
    for (index, ch) in url.char_indices() {
        match ch {
            '\\' | '(' | ')' | '<' | '>' => {
                out.push('\\');
                out.push(ch);
            }
            '|' if escape_pipe => out.push_str("\\|"),
            '&' if starts_entity(&url[index + 1..]) => out.push_str("\\&"),
            _ => out.push(ch),
        }
    }
    out
}

/// Whether `rest` (the text after an `&`) reads as `name;` or `#digits;`.
fn starts_entity(rest: &str) -> bool {
    let rest = rest.strip_prefix('#').unwrap_or(rest);
    let name = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
    name > 0 && rest.as_bytes().get(name) == Some(&b';')
}

/// Re-parses the edited source and checks that the replacement is read as one
/// link to `url`, with the selection's decoration and everything outside it
/// keeping its syntax.
fn verify(
    source: &str,
    selection: SourceRange,
    replacement: &str,
    url: &str,
    old: &MarkdownParse,
) -> bool {
    let (start, end) = (selection.start.0, selection.end.0);
    let old_len = end - start;
    let new_len = replacement.len();
    let mut next = String::with_capacity(source.len() - old_len + new_len);
    next.push_str(&source[..start]);
    next.push_str(replacement);
    next.push_str(&source[end..]);
    let new = parse_document(Revision(0), SourceRange::new(0, next.len()), &next);
    let inserted = SourceRange::new(start, start + new_len);

    // Exactly one link inside the inserted text, spanning all of it.
    let links_inside: Vec<NodeId> = new
        .tree
        .inlines()
        .filter(|(_, node)| {
            node.kind == NodeKind::Link
                && inserted.start <= node.source_range.start
                && node.source_range.end <= inserted.end
        })
        .map(|(id, _)| id)
        .collect();
    let [link_node] = links_inside.as_slice() else {
        return false;
    };
    let Some(node) = new.tree.node(*link_node) else {
        return false;
    };
    if node.source_range != inserted {
        return false;
    }
    let destination_matches = new
        .links
        .iter()
        .any(|link| link.node == *link_node && &*link.destination == url);
    if !destination_matches {
        return false;
    }

    // The label keeps the selection's decoration.
    let old_kinds: Vec<NodeKind> = old
        .tree
        .inlines()
        .filter(|(_, node)| {
            node.kind != NodeKind::Text
                && selection.start <= node.source_range.start
                && node.source_range.end <= selection.end
        })
        .map(|(_, node)| node.kind)
        .collect();
    let new_kinds: Vec<NodeKind> = new
        .tree
        .inlines()
        .filter(|(id, node)| {
            node.kind != NodeKind::Text
                && new.tree.ancestors(*id).skip(1).any(|a| a == *link_node)
        })
        .map(|(_, node)| node.kind)
        .collect();
    if old_kinds != new_kinds {
        return false;
    }

    // Blocks keep their structure, and every other non-text node keeps its
    // kind and (shifted) range.
    let old_blocks: Vec<NodeKind> = old.tree.blocks().map(|(_, node)| node.kind).collect();
    let new_blocks: Vec<NodeKind> = new.tree.blocks().map(|(_, node)| node.kind).collect();
    if old_blocks != new_blocks {
        return false;
    }
    let outside_old: Vec<(NodeKind, usize, usize)> = old
        .tree
        .iter()
        .filter(|(_, node)| {
            node.kind != NodeKind::Text
                && (node.source_range.end.0 <= start || node.source_range.start.0 >= end)
        })
        .map(|(_, node)| {
            let (s, e) = (node.source_range.start.0, node.source_range.end.0);
            if s >= end {
                (node.kind, s - old_len + new_len, e - old_len + new_len)
            } else {
                (node.kind, s, e)
            }
        })
        .collect();
    let outside_new: Vec<(NodeKind, usize, usize)> = new
        .tree
        .iter()
        .filter(|(_, node)| {
            node.kind != NodeKind::Text
                && (node.source_range.end.0 <= inserted.start.0
                    || node.source_range.start.0 >= inserted.end.0)
        })
        .map(|(_, node)| (node.kind, node.source_range.start.0, node.source_range.end.0))
        .collect();
    outside_old == outside_new
}
