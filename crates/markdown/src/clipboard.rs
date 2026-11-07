//! External HTML → Markdown conversion for rich Paste (Issue #596).
//!
//! This is a pure, OS-independent conversion. The HTML is tokenised and built
//! into a small arena by an in-crate parser that enforces the input, node and
//! depth limits *while building*, so a hostile fragment is rejected before it
//! can grow. The arena is a flat `Vec`, so dropping an aborted or finished tree
//! never recurses. Only a handful of semantic elements survive; every attribute
//! is dropped as soon as it has been turned into meaning (or ignored), no raw
//! HTML is ever written out, and nothing is fetched.
//!
//! The Markdown is written for the parser configured in `parser_options()`
//! (CommonMark plus the enabled GFM extensions): text, link labels, link
//! destinations and code are escaped separately.

use std::borrow::Cow;
use std::sync::Arc;

use url::Url;

/// Maximum accepted HTML size in UTF-8 bytes.
pub const MAX_HTML_BYTES: usize = 8 * 1024 * 1024;
/// Maximum number of DOM nodes (elements and text runs) built from one fragment.
pub const MAX_HTML_NODES: usize = 100_000;
/// Maximum element nesting depth.
pub const MAX_HTML_DEPTH: usize = 128;
/// Maximum generated Markdown size in UTF-8 bytes.
pub const MAX_MARKDOWN_BYTES: usize = 16 * 1024 * 1024;

/// Limits applied while parsing and rendering. The defaults are the product
/// limits; tests use smaller values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HtmlLimits {
    pub input_bytes: usize,
    pub nodes: usize,
    pub depth: usize,
    pub output_bytes: usize,
}

impl Default for HtmlLimits {
    fn default() -> Self {
        Self {
            input_bytes: MAX_HTML_BYTES,
            nodes: MAX_HTML_NODES,
            depth: MAX_HTML_DEPTH,
            output_bytes: MAX_MARKDOWN_BYTES,
        }
    }
}

/// Why a fragment produced no text. The caller falls back to the plain text of
/// the same clipboard snapshot, or refuses when there is none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtmlImportError {
    /// The fragment holds no visible content.
    Empty,
    /// The fragment holds a construct that would be lost (for example a table).
    Unsupported,
    /// An input, node, depth or output limit was exceeded.
    TooLarge,
}

/// Converts an external HTML fragment to Markdown with the product limits.
pub fn html_to_markdown(html: &str) -> Result<String, HtmlImportError> {
    html_to_markdown_with_limits(html, &HtmlLimits::default())
}

/// [`html_to_markdown`] with explicit limits.
pub fn html_to_markdown_with_limits(
    html: &str,
    limits: &HtmlLimits,
) -> Result<String, HtmlImportError> {
    render(html, limits, false)
}

/// Extracts the visible text of an HTML fragment without producing any
/// Markdown syntax (used when only HTML is on the clipboard and the user asked
/// for plain text).
pub fn html_to_plain_text(html: &str) -> Result<String, HtmlImportError> {
    render(html, &HtmlLimits::default(), true)
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Element {
    tag: String,
    /// Only the attributes that carry meaning for the conversion.
    attrs: Vec<(String, String)>,
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
enum Kind {
    Root,
    Element(Element),
    Text(String),
}

#[derive(Debug)]
struct Node {
    kind: Kind,
    parent: usize,
    children: Vec<usize>,
}

const WANTED_ATTRIBUTES: [&str; 7] = [
    "href", "start", "class", "alt", "value", "type", "reversed",
];

fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Elements whose content is raw text in HTML. They are dropped entirely and
/// their content is never tokenised as markup.
fn is_raw_text(tag: &str) -> bool {
    matches!(
        tag,
        "script" | "style" | "iframe" | "noembed" | "noframes" | "xmp" | "textarea" | "title"
    )
}

/// Subtrees that are never converted.
fn is_dropped(tag: &str) -> bool {
    matches!(
        tag,
        "head"
            | "template"
            | "object"
            | "embed"
            | "svg"
            | "math"
            | "noscript"
            | "applet"
            | "script"
            | "style"
            | "iframe"
    )
}

fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "caption"
            | "center"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "li"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

/// Start tags that implicitly close an open `p`.
fn closes_paragraph(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "center"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "ul"
    )
}

/// Largest ordered-list number CommonMark reads as a list marker (nine digits).
const MAX_LIST_NUMBER: u32 = 999_999_999;

const PARAGRAPH_BOUNDARIES: [&str; 6] = ["table", "td", "th", "caption", "template", "html"];

struct Builder<'l> {
    nodes: Vec<Node>,
    stack: Vec<usize>,
    limits: &'l HtmlLimits,
}

impl<'l> Builder<'l> {
    fn new(limits: &'l HtmlLimits) -> Self {
        Self {
            nodes: vec![Node {
                kind: Kind::Root,
                parent: 0,
                children: Vec::new(),
            }],
            stack: Vec::new(),
            limits,
        }
    }

    fn current(&self) -> usize {
        self.stack.last().copied().unwrap_or(0)
    }

    fn tag_at(&self, index: usize) -> &str {
        match &self.nodes[index].kind {
            Kind::Element(element) => element.tag.as_str(),
            _ => "",
        }
    }

    fn push_node(&mut self, kind: Kind) -> Result<usize, HtmlImportError> {
        if self.nodes.len() >= self.limits.nodes {
            return Err(HtmlImportError::TooLarge);
        }
        let parent = self.current();
        let index = self.nodes.len();
        self.nodes.push(Node {
            kind,
            parent,
            children: Vec::new(),
        });
        self.nodes[parent].children.push(index);
        Ok(index)
    }

    /// Closes the nearest open element named in `targets`, unless an element
    /// named in `boundaries` is met first.
    fn close_in_scope(&mut self, targets: &[&str], boundaries: &[&str]) {
        let mut found = None;
        for (position, &index) in self.stack.iter().enumerate().rev() {
            let tag = self.tag_at(index);
            if targets.contains(&tag) {
                found = Some(position);
                break;
            }
            if boundaries.contains(&tag) || is_dropped(tag) {
                break;
            }
        }
        if let Some(position) = found {
            self.stack.truncate(position);
        }
    }

    fn start_tag(
        &mut self,
        tag: String,
        attrs: Vec<(String, String)>,
    ) -> Result<(), HtmlImportError> {
        match tag.as_str() {
            "li" => self.close_in_scope(&["li"], &["ul", "ol"]),
            "dt" | "dd" => self.close_in_scope(&["dt", "dd"], &["dl"]),
            other if closes_paragraph(other) => {
                self.close_in_scope(&["p"], &PARAGRAPH_BOUNDARIES);
            }
            _ => {}
        }
        let void = is_void(&tag);
        // A void element is a leaf one level below the open stack, so it must
        // respect the depth limit as well.
        if self.stack.len() >= self.limits.depth {
            return Err(HtmlImportError::TooLarge);
        }
        let index = self.push_node(Kind::Element(Element { tag, attrs }))?;
        if !void {
            self.stack.push(index);
        }
        Ok(())
    }

    fn end_tag(&mut self, name: &str) {
        if is_void(name) {
            return;
        }
        // Stop at a dropped element met before the target so that an end tag
        // cannot close a hidden subtree from the outside.
        let mut position = None;
        for (at, &index) in self.stack.iter().enumerate().rev() {
            let tag = self.tag_at(index);
            if tag == name {
                position = Some(at);
                break;
            }
            if is_dropped(tag) {
                break;
            }
        }
        if let Some(position) = position {
            self.stack.truncate(position);
        }
    }

    fn text(&mut self, text: &str) -> Result<(), HtmlImportError> {
        if text.is_empty() {
            return Ok(());
        }
        let sanitized = text.replace('\0', "\u{fffd}");
        let parent = self.current();
        if let Some(&last) = self.nodes[parent].children.last()
            && let Kind::Text(existing) = &mut self.nodes[last].kind
        {
            existing.push_str(&sanitized);
            return Ok(());
        }
        self.push_node(Kind::Text(sanitized))?;
        Ok(())
    }
}

fn parse(html: &str, limits: &HtmlLimits) -> Result<Vec<Node>, HtmlImportError> {
    if html.len() > limits.input_bytes {
        return Err(HtmlImportError::TooLarge);
    }
    let bytes = html.as_bytes();
    let mut builder = Builder::new(limits);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &html[i..];
            if let Some(comment) = rest.strip_prefix("<!--") {
                i = match comment.find("-->") {
                    Some(at) => i + 4 + at + 3,
                    None => bytes.len(),
                };
                continue;
            }
            match bytes.get(i + 1).copied() {
                Some(b'!' | b'?') => {
                    i = skip_past_gt(bytes, i);
                    continue;
                }
                Some(b'/') if bytes.get(i + 2).is_some_and(u8::is_ascii_alphabetic) => {
                    let (name, end) = read_tag_name(html, i + 2);
                    i = skip_past_gt(bytes, end);
                    builder.end_tag(&name);
                    continue;
                }
                Some(next) if next.is_ascii_alphabetic() => {
                    let (name, attrs, end) = parse_start_tag(html, i + 1);
                    i = end;
                    if is_raw_text(&name) {
                        i = skip_raw_text(html, i, &name);
                    } else {
                        builder.start_tag(name, attrs)?;
                    }
                    continue;
                }
                _ => {}
            }
            builder.text("<")?;
            i += 1;
            continue;
        }
        let end = html[i..].find('<').map_or(bytes.len(), |at| i + at);
        builder.text(&decode_entities(&html[i..end], false))?;
        i = end;
    }
    Ok(builder.nodes)
}

fn skip_past_gt(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&byte| byte == b'>')
        .map_or(bytes.len(), |at| from + at + 1)
}

fn read_tag_name(html: &str, start: usize) -> (String, usize) {
    let bytes = html.as_bytes();
    let mut end = start;
    while end < bytes.len()
        && !bytes[end].is_ascii_whitespace()
        && bytes[end] != b'/'
        && bytes[end] != b'>'
    {
        end += 1;
    }
    (html[start..end].to_ascii_lowercase(), end)
}

/// Skips the raw-text content of `tag` (already past its start tag) up to and
/// including the matching end tag. Returns the index to continue from.
fn skip_raw_text(html: &str, from: usize, tag: &str) -> usize {
    let bytes = html.as_bytes();
    for (at, _) in html[from..].match_indices("</") {
        let name_start = from + at + 2;
        let name_end = name_start + tag.len();
        if name_end <= bytes.len()
            && bytes[name_start..name_end].eq_ignore_ascii_case(tag.as_bytes())
            && bytes
                .get(name_end)
                .is_none_or(|&byte| byte.is_ascii_whitespace() || byte == b'>' || byte == b'/')
        {
            return skip_past_gt(bytes, name_end);
        }
    }
    bytes.len()
}

fn parse_start_tag(html: &str, start: usize) -> (String, Vec<(String, String)>, usize) {
    let bytes = html.as_bytes();
    let len = bytes.len();
    let (name, mut i) = read_tag_name(html, start);
    let mut attrs: Vec<(String, String)> = Vec::new();
    loop {
        while i < len && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= len {
            break;
        }
        if bytes[i] == b'>' {
            i += 1;
            break;
        }
        let name_start = i;
        while i < len
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        if i == name_start {
            // A stray '='.
            i += 1;
            continue;
        }
        let attr_name = html[name_start..i].to_ascii_lowercase();
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < len && bytes[i] == b'=' {
            i += 1;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < len && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let value_start = i;
                while i < len && bytes[i] != quote {
                    i += 1;
                }
                value = decode_entities(&html[value_start..i], true);
                if i < len {
                    i += 1;
                }
            } else {
                let value_start = i;
                while i < len && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = decode_entities(&html[value_start..i], true);
            }
        }
        if WANTED_ATTRIBUTES.contains(&attr_name.as_str())
            && !attrs.iter().any(|(key, _)| *key == attr_name)
        {
            attrs.push((attr_name, value));
        }
    }
    (name, attrs, i)
}

fn decode_entities(text: &str, in_attribute: bool) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some((decoded, used)) = decode_entity(rest, in_attribute) {
            out.push_str(&decoded);
            rest = &rest[used..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Decodes one entity at the start of `text` (which begins with `&`). Returns
/// the decoded text (named references can expand to several code points) and
/// the number of bytes consumed. Numeric references accept a missing `;`, as do
/// the legacy named references HTML allows without one; the scan only advances
/// over consumed characters, so a run of bare `&` stays linear.
fn decode_entity(text: &str, in_attribute: bool) -> Option<(Cow<'static, str>, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(1) == Some(&b'#') {
        let (hex, start) = match bytes.get(2) {
            Some(b'x' | b'X') => (true, 3),
            _ => (false, 2),
        };
        let digits = bytes[start..]
            .iter()
            .take_while(|byte| {
                if hex {
                    byte.is_ascii_hexdigit()
                } else {
                    byte.is_ascii_digit()
                }
            })
            .count();
        if digits == 0 {
            return None;
        }
        let end = start + digits;
        let mut value: u32 = 0;
        let mut overflow = false;
        for &byte in &bytes[start..end] {
            let digit = char::from(byte).to_digit(if hex { 16 } else { 10 })?;
            match value
                .checked_mul(if hex { 16 } else { 10 })
                .and_then(|v| v.checked_add(digit))
            {
                Some(next) => value = next,
                None => {
                    overflow = true;
                    break;
                }
            }
        }
        let decoded = if overflow {
            '\u{fffd}'
        } else {
            numeric_reference(value)
        };
        let used = if bytes.get(end) == Some(&b';') {
            end + 1
        } else {
            end
        };
        return Some((Cow::Owned(decoded.to_string()), used));
    }
    let name_len = bytes[1..]
        .iter()
        .take(32)
        .take_while(|byte| byte.is_ascii_alphanumeric())
        .count();
    if name_len == 0 {
        return None;
    }
    let name = &text[1..=name_len];
    if bytes.get(name_len + 1) == Some(&b';')
        && let Some(decoded) = named_entity(name)
    {
        return Some((Cow::Borrowed(decoded), name_len + 2));
    }
    const LEGACY: [&str; 7] = ["amp", "quot", "nbsp", "copy", "reg", "lt", "gt"];
    LEGACY
        .iter()
        .find(|legacy| name.starts_with(**legacy))
        .filter(|legacy| {
            // In attribute values a semicolon-less reference followed by `=`
            // or an ASCII alphanumeric stays literal (HTML spec).
            !in_attribute
                || !matches!(
                    bytes.get(legacy.len() + 1),
                    Some(b'=') | Some(b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
                )
        })
        .and_then(|legacy| {
            named_entity(legacy).map(|text| (Cow::Borrowed(text), legacy.len() + 1))
        })
}

/// Maps a numeric character reference to a character: NUL, surrogates and
/// out-of-range values become U+FFFD, and the C1 range follows windows-1252.
fn numeric_reference(value: u32) -> char {
    const C1: [u32; 32] = [
        0x20ac, 0x81, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8d, 0x017d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013,
        0x2014, 0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0x9d, 0x017e, 0x0178,
    ];
    let value = if (0x80..=0x9f).contains(&value) {
        C1[(value - 0x80) as usize]
    } else {
        value
    };
    match char::from_u32(value) {
        Some(ch) if ch != '\0' => ch,
        _ => '\u{fffd}',
    }
}

/// Looks a name (without `&` and `;`) up in the complete HTML5 named character
/// reference table shared with the Markdown parser; some names expand to more
/// than one code point.
fn named_entity(name: &str) -> Option<&'static str> {
    pulldown_cmark::get_entity(name.as_bytes())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Strong,
    Emphasis,
    Strike,
}

impl Mark {
    const fn delimiter(self) -> &'static str {
        match self {
            Self::Strong => "**",
            Self::Emphasis => "*",
            Self::Strike => "~~",
        }
    }
}

/// Inline formatting inherited by blocks nested inside an inline element
/// (`<a><div>…</div></a>`), and the "already inside" state used to avoid
/// doubling delimiters.
#[derive(Clone, Default)]
struct Fmt {
    strong: bool,
    emphasis: bool,
    strike: bool,
    /// Shared so inheriting through nested elements never copies the URL.
    link: Option<Arc<str>>,
}

impl Fmt {
    fn has(&self, mark: Mark) -> bool {
        match mark {
            Mark::Strong => self.strong,
            Mark::Emphasis => self.emphasis,
            Mark::Strike => self.strike,
        }
    }

    fn with(&self, mark: Mark) -> Self {
        let mut next = self.clone();
        match mark {
            Mark::Strong => next.strong = true,
            Mark::Emphasis => next.emphasis = true,
            Mark::Strike => next.strike = true,
        }
        next
    }
}

struct Block {
    text: String,
    list: bool,
}

enum Class {
    Skip,
    Inline,
    Block,
    Flatten,
}

struct Renderer<'a> {
    tree: &'a [Node],
    has_block: Vec<bool>,
    plain: bool,
    limit: usize,
    used: usize,
    prev_space: bool,
    /// The mark just closed and the output length right after it, so an
    /// immediately following run of the same mark can be merged into it.
    last_mark: Option<(Mark, usize)>,
    /// The code span just emitted: its start offset in the output and its text,
    /// so an immediately following code span is merged into one safe span.
    last_code: Option<(usize, String)>,
}

fn compute_has_block(nodes: &[Node]) -> Vec<bool> {
    let mut has = vec![false; nodes.len()];
    for (index, node) in nodes.iter().enumerate().rev() {
        if let Kind::Element(element) = &node.kind {
            if is_dropped(&element.tag) {
                has[index] = false;
                continue;
            }
            if is_block(&element.tag) {
                has[index] = true;
            }
        }
        if index > 0 && has[index] {
            has[node.parent] = true;
        }
    }
    has
}

fn render(html: &str, limits: &HtmlLimits, plain: bool) -> Result<String, HtmlImportError> {
    let nodes = parse(html, limits)?;
    let tree: &[Node] = &nodes;
    let mut renderer = Renderer {
        tree,
        has_block: compute_has_block(tree),
        plain,
        limit: limits.output_bytes,
        used: 0,
        prev_space: true,
        last_mark: None,
        last_code: None,
    };
    let blocks = renderer.blocks(&tree[0].children, &Fmt::default())?;
    let text = join_blocks(&blocks, if plain { "\n" } else { "\n\n" });
    if text.trim().is_empty() {
        return Err(HtmlImportError::Empty);
    }
    if text.len() > limits.output_bytes {
        return Err(HtmlImportError::TooLarge);
    }
    Ok(text)
}

fn join_blocks(blocks: &[Block], separator: &str) -> String {
    let mut out = String::new();
    for (position, block) in blocks.iter().enumerate() {
        if position > 0 {
            out.push_str(separator);
            // Markdown merges ordered lists separated only by a blank line, so
            // the later list would lose its own start number.
            if separator == "\n\n" && starts_ordered_list(block) && ends_ordered_list(&blocks[position - 1])
            {
                out.push_str("<!-- -->");
                out.push_str(separator);
            }
        }
        out.push_str(&block.text);
    }
    out
}

fn starts_ordered_list(block: &Block) -> bool {
    block.list && is_ordered_marker(block.text.lines().next().unwrap_or(""))
}

/// A list block is ordered when its top-level (unindented) items are numbered;
/// every item of one list uses the same marker kind.
fn ends_ordered_list(block: &Block) -> bool {
    starts_ordered_list(block)
}

fn starts_at_one(block: &Block) -> bool {
    let first = block.text.lines().next().unwrap_or("");
    let digits = first.bytes().take_while(u8::is_ascii_digit).count();
    first[..digits].parse::<u32>() == Ok(1)
}

fn is_ordered_marker(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &line[digits..];
    digits > 0 && (rest == "." || rest.starts_with(". "))
}

fn prefix_lines(text: &str, prefix: &str, empty: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.is_empty() {
                empty.to_owned()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_edge_space(ch: char) -> bool {
    ch.is_whitespace() || ch == '\0'
}

/// Wraps `inner` in a delimiter pair, keeping leading and trailing whitespace
/// (and hard-break markers) outside so the delimiters stay flanking. An empty
/// or whitespace-only `inner` gets no delimiters. Content without any
/// alphanumeric character cannot be flanked by `*` / `~~` next to a word, so it
/// uses the equivalent inline HTML tag instead, as does content whose first or
/// last character is a non-ASCII punctuation or symbol (e.g. `「`), which
/// CommonMark does not flank next to a word.
fn wrap_delimited(inner: &str, open: &str, close: &str) -> String {
    let core = inner.trim_matches(is_edge_space);
    if core.is_empty() {
        return inner.to_owned();
    }
    let lead = &inner[..inner.len() - inner.trim_start_matches(is_edge_space).len()];
    let trail = &inner[inner.trim_end_matches(is_edge_space).len()..];
    let unicode_edge = |ch: char| !ch.is_ascii() && !ch.is_alphanumeric();
    // ASCII punctuation from literal text breaks flanking next to a word too.
    // Characters that open or close nested Markdown constructs (code spans,
    // nested marks, links, inline HTML) are not literal text, so they stay.
    // A `)` only closes a nested link when the text holds an unescaped `](`;
    // literal text escapes `]`, so a bare trailing `)` is plain punctuation.
    let has_link = core
        .match_indices("](")
        .any(|(at, _)| !core[..at].ends_with('\\'));
    let structural = |ch: char| match ch {
        ')' => has_link,
        _ => matches!(ch, '`' | '*' | '~' | '[' | '>' | '<'),
    };
    let ascii_edge = |ch: char, escaped: bool| {
        ch.is_ascii_punctuation() && (escaped || !structural(ch))
    };
    let mut chars = core.chars();
    let first = chars.next();
    let last = chars.next_back().or(first);
    // Slice by the last character's UTF-8 length, never by a raw byte offset.
    let last_escaped = last.is_some_and(|ch| {
        core.len() > ch.len_utf8() && core[..core.len() - ch.len_utf8()].ends_with('\\')
    });
    let punctuated_edge = first.is_some_and(|ch| unicode_edge(ch) || ascii_edge(ch, false))
        || last.is_some_and(|ch| unicode_edge(ch) || ascii_edge(ch, last_escaped));
    let html = html_tag(open)
        .filter(|_| punctuated_edge || !core.chars().any(char::is_alphanumeric));
    if let Some(tag) = html {
        return format!("{lead}<{tag}>{core}</{tag}>{trail}");
    }
    format!("{lead}{open}{core}{close}{trail}")
}

fn html_tag(delimiter: &str) -> Option<&'static str> {
    match delimiter {
        "**" => Some("strong"),
        "*" => Some("em"),
        "~~" => Some("del"),
        _ => None,
    }
}

fn wrap_link(inner: &str, destination: &str) -> String {
    let core = inner.trim_matches(is_edge_space);
    if core.is_empty() {
        return format!("<{destination}>");
    }
    let lead = &inner[..inner.len() - inner.trim_start_matches(is_edge_space).len()];
    let trail = &inner[inner.trim_end_matches(is_edge_space).len()..];
    format!("{lead}[{core}]({}){trail}", markdown_destination(destination))
}

fn apply_marks(text: &str, fmt: &Fmt) -> String {
    let mut out = text.to_owned();
    if fmt.strike {
        out = wrap_delimited(&out, Mark::Strike.delimiter(), Mark::Strike.delimiter());
    }
    if fmt.emphasis {
        out = wrap_delimited(&out, Mark::Emphasis.delimiter(), Mark::Emphasis.delimiter());
    }
    if fmt.strong {
        out = wrap_delimited(&out, Mark::Strong.delimiter(), Mark::Strong.delimiter());
    }
    if let Some(destination) = &fmt.link {
        out = wrap_link(&out, destination);
    }
    out
}

/// The absolute HTTP(S) destination of `href`, normalised by `url`; `None` for
/// other schemes, relative references, control characters or a missing host.
fn safe_http_destination(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.chars().any(char::is_control) {
        return None;
    }
    let (scheme, _) = href.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return None;
    }
    let parsed = Url::parse(href).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.host_str().filter(|host| !host.is_empty())?;
    Some(parsed.to_string())
}

fn looks_like_entity(rest: &str) -> bool {
    let rest = rest.strip_prefix('#').unwrap_or(rest);
    let len = rest
        .bytes()
        .take(32)
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    len > 0 && rest.as_bytes().get(len) == Some(&b';')
}

/// Escapes a normalised URL for the `(destination)` part of an inline link.
fn markdown_destination(url: &str) -> String {
    let mut depth = 0_i32;
    let mut balanced = true;
    for ch in url.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    balanced = false;
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        balanced = false;
    }
    let mut out = String::with_capacity(url.len());
    for (at, ch) in url.char_indices() {
        match ch {
            '(' if !balanced => out.push_str("%28"),
            ')' if !balanced => out.push_str("%29"),
            '&' if looks_like_entity(&url[at + 1..]) => out.push_str("\\&"),
            other => out.push(other),
        }
    }
    out
}

fn code_span(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for ch in text.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let delimiter = "`".repeat(longest + 1);
    let pad = text.starts_with('`')
        || text.ends_with('`')
        || (text.starts_with(' ') && text.ends_with(' ') && !text.trim_matches(' ').is_empty());
    if pad {
        format!("{delimiter} {text} {delimiter}")
    } else {
        format!("{delimiter}{text}{delimiter}")
    }
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_space = false;
    for ch in text.chars() {
        if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0c') {
            if !previous_space {
                out.push(' ');
            }
            previous_space = true;
        } else {
            out.push(ch);
            previous_space = false;
        }
    }
    out
}

fn safe_language(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 32
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'#' | b'.' | b'-'))
}

fn code_language(tree: &[Node], pre: usize) -> String {
    let mut candidates = vec![pre];
    for &child in &tree[pre].children {
        if let Kind::Element(element) = &tree[child].kind
            && element.tag == "code"
        {
            candidates.push(child);
            break;
        }
    }
    for index in candidates {
        let Kind::Element(element) = &tree[index].kind else {
            continue;
        };
        let Some(class) = element.attr("class") else {
            continue;
        };
        for token in class.split_whitespace() {
            let language = token
                .strip_prefix("language-")
                .or_else(|| token.strip_prefix("lang-"));
            if let Some(language) = language
                && safe_language(language)
            {
                return language.to_owned();
            }
        }
    }
    String::new()
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for ch in text.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// Escapes a paragraph so no line is read as a heading, list item, thematic
/// break or setext underline.
fn escape_line_starts(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = &line[indent..];
        let bytes = body.as_bytes();
        let digits = bytes.iter().take_while(|byte| byte.is_ascii_digit()).count();
        let at = match bytes.first().copied() {
            Some(b'#') => Some(0),
            Some(first @ (b'-' | b'+'))
                if bytes.len() == 1
                    || bytes[1] == b' '
                    || bytes.iter().all(|&byte| byte == first)
                    || (first == b'-'
                        && bytes.iter().all(|&byte| matches!(byte, b'-' | b' ' | b'\t'))
                        && bytes.iter().filter(|&&byte| byte == b'-').count() >= 3) =>
            {
                Some(0)
            }
            Some(b'=') if bytes.iter().all(|&byte| byte == b'=') => Some(0),
            Some(_)
                if (1..=9).contains(&digits)
                    && matches!(bytes.get(digits), Some(b'.' | b')'))
                    && (bytes.len() == digits + 1 || bytes[digits + 1] == b' ') =>
            {
                Some(digits)
            }
            _ => None,
        };
        out.push_str(&line[..indent]);
        match at {
            Some(at) => {
                out.push_str(&body[..at]);
                out.push('\\');
                out.push_str(&body[at..]);
            }
            None => out.push_str(body),
        }
    }
    out
}

impl Renderer<'_> {
    fn charge(&mut self, bytes: usize) -> Result<(), HtmlImportError> {
        self.used = self.used.saturating_add(bytes);
        if self.used > self.limit {
            Err(HtmlImportError::TooLarge)
        } else {
            Ok(())
        }
    }

    fn classify(&self, index: usize) -> Class {
        match &self.tree[index].kind {
            Kind::Text(_) => Class::Inline,
            Kind::Root => Class::Skip,
            Kind::Element(element) => {
                let tag = element.tag.as_str();
                if is_dropped(tag) {
                    Class::Skip
                } else if is_block(tag) {
                    Class::Block
                } else if self.has_block[index] {
                    Class::Flatten
                } else {
                    Class::Inline
                }
            }
        }
    }

    /// Formatting an inline element contributes to the blocks nested in it.
    fn flatten_fmt(&self, index: usize, fmt: &Fmt) -> Fmt {
        if self.plain {
            return fmt.clone();
        }
        let Kind::Element(element) = &self.tree[index].kind else {
            return fmt.clone();
        };
        match element.tag.as_str() {
            "strong" | "b" => fmt.with(Mark::Strong),
            "em" | "i" => fmt.with(Mark::Emphasis),
            "s" | "del" | "strike" => fmt.with(Mark::Strike),
            "a" if fmt.link.is_none() => {
                let mut next = fmt.clone();
                next.link = element
                    .attr("href")
                    .and_then(safe_http_destination)
                    .map(Arc::from);
                next
            }
            _ => fmt.clone(),
        }
    }

    fn blocks(&mut self, children: &[usize], fmt: &Fmt) -> Result<Vec<Block>, HtmlImportError> {
        let tree = self.tree;
        let mut out = Vec::new();
        let mut run: Vec<usize> = Vec::new();
        for &child in children {
            match self.classify(child) {
                Class::Skip => {}
                Class::Inline => run.push(child),
                Class::Flatten => {
                    self.flush_run(&mut run, fmt, &mut out)?;
                    let inner_fmt = self.flatten_fmt(child, fmt);
                    let inner = self.blocks(&tree[child].children, &inner_fmt)?;
                    out.extend(inner);
                }
                Class::Block => {
                    self.flush_run(&mut run, fmt, &mut out)?;
                    self.block(child, fmt, &mut out)?;
                }
            }
        }
        self.flush_run(&mut run, fmt, &mut out)?;
        Ok(out)
    }

    fn flush_run(
        &mut self,
        run: &mut Vec<usize>,
        fmt: &Fmt,
        out: &mut Vec<Block>,
    ) -> Result<(), HtmlImportError> {
        if run.is_empty() {
            return Ok(());
        }
        let nodes = std::mem::take(run);
        let inner = self.inline_run(&nodes, fmt)?;
        if inner.is_empty() {
            return Ok(());
        }
        let text = inner.replace('\0', if self.plain { "\n" } else { "\\\n" });
        let text = if self.plain {
            text
        } else {
            escape_line_starts(&text)
        };
        self.charge(text.len())?;
        out.push(Block { text, list: false });
        Ok(())
    }

    /// Renders inline nodes to one string. Hard breaks are `\0` until the
    /// caller turns them into the output form. Empty when nothing is visible.
    fn inline_run(&mut self, nodes: &[usize], fmt: &Fmt) -> Result<String, HtmlImportError> {
        self.prev_space = true;
        let mut text = String::new();
        for &node in nodes {
            self.inline(node, fmt, &mut text)?;
        }
        let core = text.trim_matches([' ', '\0']);
        if core.trim_matches(is_edge_space).is_empty() {
            return Ok(String::new());
        }
        Ok(if self.plain {
            core.to_owned()
        } else {
            apply_marks(core, fmt)
        })
    }

    fn emit_text(&mut self, text: &str, out: &mut String) {
        let mut previous: Option<char> = None;
        let mut chars = text.char_indices().peekable();
        while let Some((at, ch)) = chars.next() {
            let next = chars.peek().map(|(_, next)| *next);
            if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0c') {
                if !self.prev_space {
                    out.push(' ');
                    self.prev_space = true;
                }
            } else {
                self.prev_space = false;
                if self.plain {
                    out.push(ch);
                } else {
                    match ch {
                        '\\' | '`' | '*' | '~' | '[' | ']' | '<' | '>' => {
                            out.push('\\');
                            out.push(ch);
                        }
                        '_' if previous.is_none_or(|prev| !prev.is_alphanumeric())
                            || next.is_none_or(|next| !next.is_alphanumeric()) =>
                        {
                            out.push_str("\\_");
                        }
                        '&' if looks_like_entity(&text[at + 1..]) => out.push_str("\\&"),
                        other => out.push(other),
                    }
                }
            }
            previous = Some(ch);
        }
    }

    fn inline_children(
        &mut self,
        index: usize,
        fmt: &Fmt,
        out: &mut String,
    ) -> Result<(), HtmlImportError> {
        let tree = self.tree;
        for &child in &tree[index].children {
            self.inline(child, fmt, out)?;
        }
        Ok(())
    }

    fn inline_marked(
        &mut self,
        index: usize,
        fmt: &Fmt,
        mark: Mark,
        previous: Option<(Mark, usize)>,
        out: &mut String,
    ) -> Result<(), HtmlImportError> {
        if fmt.has(mark) {
            return self.inline_children(index, fmt, out);
        }
        let inner_fmt = fmt.with(mark);
        let mut inner = String::new();
        self.inline_children(index, &inner_fmt, &mut inner)?;
        let delimiter = mark.delimiter();
        let wrapped = wrap_delimited(&inner, delimiter, delimiter);
        let tail = wrapped.strip_prefix(delimiter);
        // Directly adjacent runs of one mark would put two delimiters side by
        // side, so they are joined into a single run instead.
        let merged = match tail {
            Some(tail)
                if previous == Some((mark, out.len()))
                    && out.ends_with(delimiter)
                    && !tail.starts_with(delimiter.chars().next().unwrap_or('*')) =>
            {
                out.truncate(out.len() - delimiter.len());
                out.push_str(tail);
                true
            }
            _ => false,
        };
        if !merged {
            out.push_str(&wrapped);
        }
        if wrapped.ends_with(delimiter) {
            self.last_mark = Some((mark, out.len()));
        }
        Ok(())
    }

    fn raw_text(&self, index: usize, line_break: &str, out: &mut String) {
        let tree = self.tree;
        match &tree[index].kind {
            Kind::Text(text) => out.push_str(text),
            Kind::Root => {}
            Kind::Element(element) => {
                if is_dropped(&element.tag) {
                    return;
                }
                if element.tag == "br" {
                    out.push_str(line_break);
                    return;
                }
                for &child in &tree[index].children {
                    self.raw_text(child, line_break, out);
                }
            }
        }
    }

    fn inline(&mut self, index: usize, fmt: &Fmt, out: &mut String) -> Result<(), HtmlImportError> {
        let tree = self.tree;
        let previous = self.last_mark.take();
        let previous_code = self.last_code.take();
        let element = match &tree[index].kind {
            Kind::Text(text) => {
                self.emit_text(text, out);
                return Ok(());
            }
            Kind::Root => return Ok(()),
            Kind::Element(element) => element,
        };
        let tag = element.tag.as_str();
        if is_dropped(tag) {
            return Ok(());
        }
        match tag {
            "br" => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\0');
                self.prev_space = true;
            }
            "img" => {
                if let Some(alt) = element.attr("alt") {
                    self.emit_text(alt, out);
                }
            }
            "code" | "kbd" | "samp" | "tt" if !self.plain => {
                let mut raw = String::new();
                self.raw_text(index, " ", &mut raw);
                let text = collapse_whitespace(&raw);
                if !text.is_empty() {
                    // Adjacent code spans would put their backtick runs side
                    // by side and fuse them, so they become one span.
                    let (start, text) = match previous_code {
                        Some((start, before))
                            if out.len() == start + code_span(&before).len() =>
                        {
                            out.truncate(start);
                            (start, before + &text)
                        }
                        _ => (out.len(), text),
                    };
                    out.push_str(&code_span(&text));
                    self.last_code = Some((start, text));
                    self.prev_space = false;
                }
                return Ok(());
            }
            "strong" | "b" if !self.plain => {
                self.inline_marked(index, fmt, Mark::Strong, previous, out)?;
            }
            "em" | "i" if !self.plain => {
                self.inline_marked(index, fmt, Mark::Emphasis, previous, out)?;
            }
            "s" | "del" | "strike" if !self.plain => {
                self.inline_marked(index, fmt, Mark::Strike, previous, out)?;
            }
            "a" if !self.plain => {
                let destination = if fmt.link.is_some() {
                    None
                } else {
                    element
                        .attr("href")
                        .and_then(safe_http_destination)
                        .map(Arc::<str>::from)
                };
                if let Some(destination) = destination {
                    let mut inner_fmt = fmt.clone();
                    inner_fmt.link = Some(Arc::clone(&destination));
                    let mut inner = String::new();
                    self.inline_children(index, &inner_fmt, &mut inner)?;
                    out.push_str(&wrap_link(&inner, &destination));
                } else {
                    self.inline_children(index, fmt, out)?;
                }
            }
            _ => self.inline_children(index, fmt, out)?,
        }
        // Offsets recorded by nested code spans belong to a different buffer.
        self.last_code = None;
        Ok(())
    }

    fn block(
        &mut self,
        index: usize,
        fmt: &Fmt,
        out: &mut Vec<Block>,
    ) -> Result<(), HtmlImportError> {
        let tree = self.tree;
        let Kind::Element(element) = &tree[index].kind else {
            return Ok(());
        };
        let children = &tree[index].children;
        match element.tag.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = usize::from(element.tag.as_bytes()[1] - b'0');
                let inner = self.inline_run(children, fmt)?;
                let line = inner.replace('\0', " ");
                let line = line.trim();
                if line.is_empty() {
                    return Ok(());
                }
                let text = if self.plain {
                    line.to_owned()
                } else if let Some(stripped) = line.strip_suffix('#') {
                    format!("{} {stripped}\\#", "#".repeat(level))
                } else {
                    format!("{} {line}", "#".repeat(level))
                };
                self.charge(text.len())?;
                out.push(Block { text, list: false });
            }
            "pre" => {
                let mut raw = String::new();
                self.raw_text(index, "\n", &mut raw);
                let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
                let body = normalized.strip_prefix('\n').unwrap_or(&normalized);
                if body.trim().is_empty() {
                    return Ok(());
                }
                let text = if self.plain {
                    body.to_owned()
                } else {
                    let language = code_language(tree, index);
                    let fence = "`".repeat((longest_backtick_run(body) + 1).max(3));
                    // A body that already ends with a newline keeps its last
                    // line break as the one before the closing fence.
                    let break_before_fence = if body.ends_with('\n') { "" } else { "\n" };
                    format!("{fence}{language}\n{body}{break_before_fence}{fence}")
                };
                self.charge(text.len())?;
                out.push(Block { text, list: false });
            }
            "hr" => {
                if !self.plain {
                    self.charge(3)?;
                    out.push(Block {
                        text: "---".to_owned(),
                        list: false,
                    });
                }
            }
            "blockquote" => {
                let inner = self.blocks(children, fmt)?;
                if inner.is_empty() {
                    return Ok(());
                }
                let text = if self.plain {
                    join_blocks(&inner, "\n")
                } else {
                    let joined = join_blocks(&inner, "\n\n");
                    let quoted = prefix_lines(&joined, "> ", ">");
                    self.charge(quoted.len().saturating_sub(joined.len()))?;
                    quoted
                };
                out.push(Block { text, list: false });
            }
            "ul" | "ol" => self.list(index, element, fmt, out)?,
            "table" => {
                if !self.plain {
                    return Err(HtmlImportError::Unsupported);
                }
                let mut rows = Vec::new();
                self.collect_rows(index, &mut rows)?;
                if !rows.is_empty() {
                    let text = rows.join("\n");
                    self.charge(text.len())?;
                    out.push(Block { text, list: false });
                }
            }
            _ => {
                let inner = self.blocks(children, fmt)?;
                out.extend(inner);
            }
        }
        Ok(())
    }

    fn collect_rows(&mut self, index: usize, rows: &mut Vec<String>) -> Result<(), HtmlImportError> {
        let tree = self.tree;
        for &child in &tree[index].children {
            let Kind::Element(element) = &tree[child].kind else {
                continue;
            };
            match element.tag.as_str() {
                "thead" | "tbody" | "tfoot" => self.collect_rows(child, rows)?,
                "tr" => {
                    let mut cells = Vec::new();
                    for &cell in &tree[child].children {
                        let Kind::Element(cell_element) = &tree[cell].kind else {
                            continue;
                        };
                        if matches!(cell_element.tag.as_str(), "td" | "th") {
                            let blocks = self.blocks(&tree[cell].children, &Fmt::default())?;
                            cells.push(join_blocks(&blocks, " ").replace('\n', " "));
                        }
                    }
                    rows.push(cells.join("\t"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn list(
        &mut self,
        index: usize,
        element: &Element,
        fmt: &Fmt,
        out: &mut Vec<Block>,
    ) -> Result<(), HtmlImportError> {
        let tree = self.tree;
        let ordered = element.tag == "ol";
        let mut number = 1_u32;
        if ordered && !self.plain {
            if let Some(kind) = element.attr("type")
                && kind.trim() != "1"
            {
                return Err(HtmlImportError::Unsupported);
            }
            // A reversed list or an unreadable / out-of-range start cannot be
            // written as a Markdown ordered list without changing its numbers.
            if element.attr("reversed").is_some() {
                return Err(HtmlImportError::Unsupported);
            }
            if let Some(start) = element.attr("start") {
                match start.trim().parse::<u32>() {
                    Ok(value) if value <= MAX_LIST_NUMBER => number = value,
                    _ => return Err(HtmlImportError::Unsupported),
                }
            }
        }
        let mut items: Vec<String> = Vec::new();
        for &child in &tree[index].children {
            let is_item = matches!(&tree[child].kind, Kind::Element(li) if li.tag == "li");
            let blocks = if is_item {
                if !self.plain
                    && let Kind::Element(li) = &tree[child].kind
                    && li.attr("value").is_some()
                {
                    return Err(HtmlImportError::Unsupported);
                }
                self.blocks(&tree[child].children, fmt)?
            } else {
                if matches!(self.classify(child), Class::Skip) {
                    continue;
                }
                let stray = self.blocks(std::slice::from_ref(&child), fmt)?;
                if stray.is_empty() {
                    continue;
                }
                stray
            };
            let marker = if self.plain {
                String::new()
            } else if ordered {
                // CommonMark list markers have at most nine digits.
                if number > MAX_LIST_NUMBER {
                    return Err(HtmlImportError::Unsupported);
                }
                format!("{number}. ")
            } else {
                "- ".to_owned()
            };
            if ordered {
                number = number.saturating_add(1);
            }
            items.push(self.render_item(&marker, &blocks)?);
        }
        if items.is_empty() {
            return Ok(());
        }
        let loose = !self.plain && items.iter().any(|item| item.contains("\n\n"));
        let text = items.join(if loose { "\n\n" } else { "\n" });
        out.push(Block { text, list: true });
        Ok(())
    }

    fn render_item(&mut self, marker: &str, blocks: &[Block]) -> Result<String, HtmlImportError> {
        if blocks.is_empty() {
            return Ok(marker.trim_end().to_owned());
        }
        let mut body = String::new();
        for (position, block) in blocks.iter().enumerate() {
            if position > 0 {
                let previous = &blocks[position - 1];
                if self.plain {
                    body.push('\n');
                } else if starts_ordered_list(block) && ends_ordered_list(previous) {
                    // Adjacent ordered lists would merge into one and lose the
                    // later start number.
                    body.push_str("\n\n<!-- -->\n\n");
                } else if starts_ordered_list(block) && !previous.list && !starts_at_one(block) {
                    // Only an ordered list starting at 1 can interrupt a paragraph.
                    body.push_str("\n\n");
                } else {
                    body.push_str(if block.list { "\n" } else { "\n\n" });
                }
            }
            body.push_str(&block.text);
        }
        let indent = " ".repeat(marker.len());
        let mut item = String::with_capacity(body.len() + marker.len());
        for (position, line) in body.split('\n').enumerate() {
            if position == 0 {
                item.push_str(marker);
                item.push_str(line);
            } else {
                item.push('\n');
                if !line.is_empty() {
                    item.push_str(&indent);
                    item.push_str(line);
                }
            }
        }
        self.charge(item.len().saturating_sub(body.len()))?;
        Ok(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_fmt_shares_the_link_destination() {
        let link: Arc<str> = Arc::from("https://example.com/");
        let fmt = Fmt {
            link: Some(Arc::clone(&link)),
            ..Fmt::default()
        };
        let inherited = fmt.with(Mark::Strong).with(Mark::Emphasis).clone();
        let shared = inherited.link.as_ref().expect("link is inherited");
        assert!(Arc::ptr_eq(shared, &link));
        assert!(inherited.strong && inherited.emphasis);
    }
}
