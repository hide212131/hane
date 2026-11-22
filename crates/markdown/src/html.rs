//! Allow-listed inline HTML recognition (Issue #757, phase 1).
//!
//! pulldown-cmark stays the authority: only its `InlineHtml` event ranges are
//! tag candidates, so escaped tags, code spans and Front Matter never produce
//! one. This module only pairs those candidates and validates the pair; it never
//! rewrites the source, and a pair that fails any check simply yields nothing,
//! leaving its bytes as ordinary visible text.

use crate::parser::is_external_http_url;
use crate::syntax::{MarkdownParse, MarkdownTree, NodeId, NodeKind};
use hane_document::SourceRange;
use std::sync::Arc;

/// Longest single tag (`<` through `>`) that is considered.
const MAX_TAG_BYTES: usize = 4 * 1024;
/// Largest recognition unit (one top-level paragraph or heading).
const MAX_UNIT_BYTES: usize = 64 * 1024;
/// Most inline HTML tags one recognition unit may hold.
const MAX_UNIT_TAGS: usize = 256;
/// Deepest nesting of open tags inside one inline scope.
const MAX_DEPTH: usize = 16;

/// What a recognized HTML element displays as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtmlKind {
    /// `<b>` / `<strong>`.
    Bold,
    /// `<i>` / `<em>`.
    Italic,
    /// `<s>` / `<del>` / `<strike>`.
    Strikethrough,
    /// `<code>`: the content is literal, so Markdown inside it is not styled.
    Code,
    /// `<a href="https://…">` with a validated absolute destination.
    Link,
}

/// Whether a tag opens or closes its element.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtmlTagEdge {
    Opening,
    Closing,
}

/// A verified, correctly closed pair of allow-listed tags on one physical line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtmlElement {
    pub kind: HtmlKind,
    /// Opening tag start through closing tag end.
    pub range: SourceRange,
    /// The whole opening tag, `<` through `>`, attributes included.
    pub open_tag: SourceRange,
    /// The whole closing tag, `<` through `>`.
    pub close_tag: SourceRange,
    /// The bytes between the two tags. Never empty.
    pub content_range: SourceRange,
    /// The `InlineHtml` tree node of the opening tag.
    pub open_node: NodeId,
    /// The validated destination of a [`HtmlKind::Link`], decoded once.
    pub destination: Option<Arc<str>>,
}

impl HtmlElement {
    /// Which of this element's two tags `tag` is, if either.
    pub fn tag_edge(&self, tag: SourceRange) -> Option<HtmlTagEdge> {
        if self.open_tag == tag {
            Some(HtmlTagEdge::Opening)
        } else if self.close_tag == tag {
            Some(HtmlTagEdge::Closing)
        } else {
            None
        }
    }
}

impl MarkdownParse {
    /// The element a tag range belongs to, with which of its tags it is.
    pub fn html_tag_owner(&self, tag: SourceRange) -> Option<(&HtmlElement, HtmlTagEdge)> {
        self.html_elements
            .iter()
            .take_while(|element| element.range.start <= tag.start)
            .find_map(|element| element.tag_edge(tag).map(|edge| (element, edge)))
    }

    /// Whether `range` lies inside the literal content of a `<code>` element,
    /// where Markdown constructs must not be styled, hidden or linked.
    pub fn html_literal_contains(&self, range: SourceRange) -> bool {
        self.html_elements.iter().any(|element| {
            element.kind == HtmlKind::Code
                && element.content_range.start <= range.start
                && range.end <= element.content_range.end
        })
    }
}

const NAMES: [(&str, HtmlKind); 9] = [
    ("b", HtmlKind::Bold),
    ("strong", HtmlKind::Bold),
    ("i", HtmlKind::Italic),
    ("em", HtmlKind::Italic),
    ("s", HtmlKind::Strikethrough),
    ("del", HtmlKind::Strikethrough),
    ("strike", HtmlKind::Strikethrough),
    ("code", HtmlKind::Code),
    ("a", HtmlKind::Link),
];

/// Index of `code` in [`NAMES`].
const CODE_NAME: usize = 7;

#[derive(Clone, Debug)]
enum Href {
    Absent,
    Valid(Arc<str>),
    Invalid,
}

#[derive(Clone, Debug)]
struct LexedTag {
    /// Index into [`NAMES`]; `<b>` and `</strong>` are different names.
    name: usize,
    closing: bool,
    self_closing: bool,
    href: Href,
}

struct Token {
    node: NodeId,
    parent: Option<NodeId>,
    range: SourceRange,
    tag: LexedTag,
}

struct OpenTag {
    name: usize,
    token: usize,
}

fn is_ws(ch: char) -> bool {
    ch.is_ascii_whitespace()
}

/// Decodes `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;` and numeric references
/// exactly once. An unknown named reference stays literal; an invalid numeric
/// one rejects the value.
fn decode_entities_once(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity.strip_prefix('#').and_then(|digits| {
                let code = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => digits.parse::<u32>().ok()?,
                };
                char::from_u32(code)
            }),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None if entity.starts_with('#') => return None,
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Some(out)
}

fn validate_href(value: &str) -> Href {
    let Some(decoded) = decode_entities_once(value) else {
        return Href::Invalid;
    };
    let valid = is_external_http_url(&decoded)
        && url::Url::parse(&decoded).is_ok_and(|parsed| {
            matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some()
        });
    if valid {
        Href::Valid(Arc::from(decoded))
    } else {
        Href::Invalid
    }
}

/// Reads only `href` from an opening tag's attribute text. A duplicate `href`,
/// an `href` without a value, or malformed quoting is invalid; every other
/// attribute is skipped without effect.
fn scan_href(mut rest: &str) -> Href {
    let mut found: Option<Option<&str>> = None;
    loop {
        rest = rest.trim_start_matches(is_ws);
        if rest.is_empty() || rest == "/" {
            break;
        }
        let name_end = rest
            .find(|ch: char| is_ws(ch) || matches!(ch, '=' | '/'))
            .unwrap_or(rest.len());
        if name_end == 0 {
            return Href::Invalid;
        }
        let name = &rest[..name_end];
        rest = rest[name_end..].trim_start_matches(is_ws);
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start_matches(is_ws);
            match after.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let Some(close) = body.find(quote) else {
                        return Href::Invalid;
                    };
                    rest = &body[close + 1..];
                    Some(&body[..close])
                }
                _ => {
                    let end = after.find(is_ws).unwrap_or(after.len());
                    rest = &after[end..];
                    Some(&after[..end])
                }
            }
        } else {
            None
        };
        if name.eq_ignore_ascii_case("href") {
            if found.is_some() {
                return Href::Invalid;
            }
            found = Some(value);
        }
    }
    match found {
        None => Href::Absent,
        Some(None) => Href::Invalid,
        Some(Some(value)) => validate_href(value),
    }
}

/// Lexes one `InlineHtml` event text. `None` when it is not an allow-listed tag.
fn lex_tag(tag: &str) -> Option<LexedTag> {
    let inner = tag.strip_prefix('<')?.strip_suffix('>')?;
    let (closing, rest) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let name_len = rest.bytes().take_while(|byte| byte.is_ascii_alphanumeric()).count();
    let name = &rest[..name_len];
    let index = NAMES
        .iter()
        .position(|(candidate, _)| candidate.eq_ignore_ascii_case(name))?;
    let attrs = &rest[name_len..];
    if !attrs.is_empty() && !attrs.starts_with(|ch: char| is_ws(ch) || ch == '/') {
        return None;
    }
    if closing {
        return attrs.trim().is_empty().then_some(LexedTag {
            name: index,
            closing: true,
            self_closing: false,
            href: Href::Absent,
        });
    }
    let href = if NAMES[index].1 == HtmlKind::Link {
        scan_href(attrs)
    } else {
        Href::Absent
    };
    Some(LexedTag {
        name: index,
        closing: false,
        self_closing: attrs.trim_end().ends_with('/'),
        href,
    })
}

/// The top-level paragraph or heading that owns the inline node `id`, or `None`
/// when the node sits in a list, quote, table, footnote or image.
fn eligible_block(tree: &MarkdownTree, id: NodeId) -> Option<NodeId> {
    let mut current = tree.node(id)?.parent?;
    loop {
        let node = tree.node(current)?;
        match node.kind {
            NodeKind::Image => return None,
            NodeKind::Paragraph | NodeKind::Heading(_) => {
                return (node.parent == Some(MarkdownTree::ROOT)).then_some(current);
            }
            kind if kind.is_block() => return None,
            _ => {}
        }
        current = node.parent?;
    }
}

/// Recognizes every valid allow-listed element in `tree`, sorted by start.
///
/// `source` is the text the tree was built from, starting at absolute offset
/// `base`.
pub(crate) fn recognize_html(tree: &MarkdownTree, source: &str, base: usize) -> Vec<HtmlElement> {
    let mut elements = Vec::new();
    let mut group: Vec<NodeId> = Vec::new();
    let mut current: Option<NodeId> = None;
    for (id, node) in tree.iter() {
        if node.kind != NodeKind::InlineHtml {
            continue;
        }
        let Some(block) = eligible_block(tree, id) else {
            continue;
        };
        if current != Some(block) {
            if let Some(previous) = current {
                recognize_unit(tree, source, base, previous, &group, &mut elements);
            }
            group.clear();
            current = Some(block);
        }
        group.push(id);
    }
    if let Some(previous) = current {
        recognize_unit(tree, source, base, previous, &group, &mut elements);
    }
    elements.sort_by_key(|element| (element.range.start, element.range.end));
    elements
}

fn recognize_unit(
    tree: &MarkdownTree,
    source: &str,
    base: usize,
    block: NodeId,
    group: &[NodeId],
    out: &mut Vec<HtmlElement>,
) {
    let Some(block_node) = tree.node(block) else {
        return;
    };
    if block_node.source_range.end.0.saturating_sub(block_node.source_range.start.0)
        > MAX_UNIT_BYTES
        || group.len() > MAX_UNIT_TAGS
    {
        return;
    }
    let mut tokens = Vec::new();
    for id in group {
        let Some(node) = tree.node(*id) else {
            return;
        };
        let (Some(start), Some(end)) = (
            node.source_range.start.0.checked_sub(base),
            node.source_range.end.0.checked_sub(base),
        ) else {
            return;
        };
        let Some(text) = source.get(start..end) else {
            return;
        };
        if text.len() > MAX_TAG_BYTES {
            return;
        }
        if let Some(tag) = lex_tag(text) {
            tokens.push(Token {
                node: *id,
                parent: node.parent,
                range: node.source_range,
                tag,
            });
        }
    }
    if tokens.is_empty() {
        return;
    }
    // Markdown links and images of the unit; an `<a>` must not nest with them.
    let blockers = tree
        .iter()
        .skip(block.0)
        .take_while(|(id, _)| tree.ancestors(*id).any(|ancestor| ancestor == block))
        .filter(|(_, node)| matches!(node.kind, NodeKind::Link | NodeKind::Image))
        .map(|(_, node)| node.source_range)
        .collect::<Vec<_>>();

    let mut scopes: Vec<(Option<NodeId>, Vec<OpenTag>)> = Vec::new();
    let mut found: Vec<HtmlElement> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let position = match scopes.iter().position(|(scope, _)| *scope == token.parent) {
            Some(position) => position,
            None => {
                scopes.push((token.parent, Vec::new()));
                scopes.len() - 1
            }
        };
        let stack = &mut scopes[position].1;
        if stack.last().is_some_and(|open| open.name == CODE_NAME)
            && !(token.tag.closing && token.tag.name == CODE_NAME)
        {
            // Everything inside `<code>` is literal text.
            continue;
        }
        if !token.tag.closing {
            if token.tag.self_closing {
                continue;
            }
            if stack.len() >= MAX_DEPTH {
                return;
            }
            stack.push(OpenTag {
                name: token.tag.name,
                token: index,
            });
            continue;
        }
        match stack.iter().rposition(|open| open.name == token.tag.name) {
            Some(at) if at + 1 == stack.len() => {
                let open = stack.pop().expect("stack is non-empty");
                if let Some(element) = build_element(
                    tree,
                    source,
                    base,
                    &tokens[open.token],
                    token,
                    &blockers,
                ) {
                    found.push(element);
                }
            }
            // Crossing pairs: everything from the match up is malformed.
            Some(at) => stack.truncate(at),
            None => {}
        }
    }
    // Nested links are ambiguous; neither side is shown as a link.
    let links = found
        .iter()
        .enumerate()
        .filter(|(_, element)| element.kind == HtmlKind::Link)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut dropped = vec![false; found.len()];
    for outer in &links {
        for inner in &links {
            if outer != inner
                && found[*outer].range.start <= found[*inner].range.start
                && found[*inner].range.end <= found[*outer].range.end
            {
                dropped[*outer] = true;
                dropped[*inner] = true;
            }
        }
    }
    out.extend(
        found
            .into_iter()
            .zip(dropped)
            .filter_map(|(element, dropped)| (!dropped).then_some(element)),
    );
}

fn build_element(
    tree: &MarkdownTree,
    source: &str,
    base: usize,
    open: &Token,
    close: &Token,
    blockers: &[SourceRange],
) -> Option<HtmlElement> {
    let kind = NAMES[open.tag.name].1;
    let range = SourceRange {
        start: open.range.start,
        end: close.range.end,
    };
    let content_range = SourceRange {
        start: open.range.end,
        end: close.range.start,
    };
    if content_range.start >= content_range.end {
        return None;
    }
    let text = source.get(range.start.0.checked_sub(base)?..range.end.0.checked_sub(base)?)?;
    if text.contains(['\n', '\r']) {
        return None;
    }
    let mut destination = None;
    if kind == HtmlKind::Link {
        let Href::Valid(valid) = &open.tag.href else {
            return None;
        };
        let inside_link = open.parent.is_some_and(|scope| {
            tree.ancestors(scope)
                .any(|ancestor| tree.node(ancestor).is_some_and(|node| node.kind == NodeKind::Link))
        });
        if inside_link || blockers.iter().any(|blocker| blocker.intersects(content_range)) {
            return None;
        }
        destination = Some(valid.clone());
    }
    Some(HtmlElement {
        kind,
        range,
        open_tag: open.range,
        close_tag: close.range,
        content_range,
        open_node: open.node,
        destination,
    })
}
