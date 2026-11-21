//! Front Matter: the metadata block that opens a document.
//!
//! A document has Front Matter when byte 0 starts a `---` line and a later
//! `---` line closes it. Everything between is metadata source that is kept in
//! the document buffer verbatim; this module only decides where the block ends
//! and which simple `key: value` lines can be summarized. It is the single
//! recognizer shared by the full parse, the block index and presentation, so
//! none of them re-derive the boundary.

use crate::markers::markdown_lines;
use hane_document::{LineId, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use unicode_segmentation::UnicodeSegmentation;

/// Longest key or value, in grapheme clusters, a summary shows.
const MAX_SUMMARY_FIELD_GRAPHEMES: usize = 24;
/// Interpretable items other than the date a summary names before `+N`.
const MAX_SUMMARY_ITEMS: usize = 2;

/// A validated `date` value: a real Gregorian calendar day written as ASCII
/// `YYYY-MM-DD`. `value_range` covers the digits only, without any quotes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontMatterDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub value_range: SourceRange,
}

impl FrontMatterDate {
    /// The date as `YYYY-MM-DD`.
    pub fn text(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// What the metadata says about `date`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontMatterDateState {
    /// No line declares a `date` key.
    Absent,
    Valid(FrontMatterDate),
    /// A `date` key is declared but its value is not a valid day, is not an
    /// interpretable value, or the key is declared more than once.
    Invalid,
}

/// One interpretable `key: value` line other than `date`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontMatterEntry {
    pub key: String,
    /// The value with its outer spaces and one pair of matching quotes removed.
    pub value: String,
    /// Source range of `value`, which sits on a UTF-8 boundary.
    pub value_range: SourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontMatterMetadata {
    pub date: FrontMatterDateState,
    /// Interpretable items other than `date` in source order. A key repeated
    /// later keeps only its first interpretable value.
    pub entries: Vec<FrontMatterEntry>,
    /// Whether any line is neither blank nor a comment.
    pub has_content: bool,
}

/// A recognized Front Matter block. All ranges are half-open source bytes and
/// include line endings exactly as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontMatter {
    /// Opening line through the end of the closing line, including the closing
    /// line's own line ending. A blank line after it is not part of it.
    pub range: SourceRange,
    pub opening_line: SourceRange,
    /// Bytes between the two delimiter lines.
    pub content: SourceRange,
    pub closing_line: SourceRange,
    /// False when the closing line is the unterminated last line of the document.
    pub closing_has_line_ending: bool,
    pub metadata: FrontMatterMetadata,
}

/// The part of a [`FrontMatter`] that placement and disclosure need.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontMatterSpan {
    pub range: SourceRange,
    pub closing_has_line_ending: bool,
}

impl FrontMatterSpan {
    /// Whether a caret, selection or composition range makes the block show its
    /// source. A caret belongs to the block while it is inside the half-open
    /// range; the end offset is the next line's start, except when the closing
    /// line is the unterminated last line and the caret is at the document end.
    /// A range with length is disclosed by any intersection.
    pub fn is_disclosed_by(&self, disclosure: SourceRange) -> bool {
        if disclosure.is_empty() {
            self.range.start <= disclosure.start
                && (disclosure.start < self.range.end
                    || (!self.closing_has_line_ending && disclosure.start == self.range.end))
        } else {
            self.range.intersects(disclosure)
        }
    }
}

/// The result of reading a document's head for Front Matter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrontMatterHead {
    /// The document has no Front Matter.
    Absent,
    Present(FrontMatter),
    /// An opening line exists but neither a closing line nor the end of the
    /// document was reached within the byte budget.
    Unconfirmed,
}

impl FrontMatter {
    pub fn span(&self) -> FrontMatterSpan {
        FrontMatterSpan {
            range: self.range,
            closing_has_line_ending: self.closing_has_line_ending,
        }
    }

    /// The single source position a click or vertical move onto the summary
    /// lands on: the valid date's value start, else the first listed item's
    /// value start, else the opening line's start.
    pub fn anchor(&self) -> SourceOffset {
        match self.metadata.date {
            FrontMatterDateState::Valid(date) => date.value_range.start,
            _ => self
                .metadata
                .entries
                .first()
                .map_or(self.opening_line.start, |entry| entry.value_range.start),
        }
    }

    /// The one-line summary shown while the block is collapsed.
    pub fn summary(&self) -> String {
        let metadata = &self.metadata;
        let mut parts: Vec<String> = Vec::new();
        match metadata.date {
            FrontMatterDateState::Valid(date) => parts.push(date.text()),
            FrontMatterDateState::Invalid => {
                parts.push("メタデータ".to_owned());
                parts.push("日付無効".to_owned());
            }
            FrontMatterDateState::Absent => {
                if metadata.entries.is_empty() {
                    return if metadata.has_content {
                        "メタデータ".to_owned()
                    } else {
                        "メタデータなし".to_owned()
                    };
                }
                parts.push("メタデータ".to_owned());
            }
        }
        for entry in metadata.entries.iter().take(MAX_SUMMARY_ITEMS) {
            parts.push(format!(
                "{}: {}",
                summary_field(&entry.key),
                summary_field(&entry.value)
            ));
        }
        let hidden = metadata.entries.len().saturating_sub(MAX_SUMMARY_ITEMS);
        if hidden > 0 {
            parts.push(format!("+{hidden}"));
        }
        parts.join(" · ")
    }
}

/// Collapses runs of whitespace to one space and limits the text to
/// [`MAX_SUMMARY_FIELD_GRAPHEMES`] grapheme clusters, ending in `…` when cut.
fn summary_field(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut graphemes = collapsed.graphemes(true);
    let kept = graphemes
        .by_ref()
        .take(MAX_SUMMARY_FIELD_GRAPHEMES)
        .collect::<String>();
    if graphemes.next().is_none() {
        return kept;
    }
    let mut clipped = collapsed
        .graphemes(true)
        .take(MAX_SUMMARY_FIELD_GRAPHEMES - 1)
        .collect::<String>();
    clipped.push('…');
    clipped
}

fn line_content(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

/// A delimiter line is `---` followed only by ASCII spaces or tabs, with no
/// indentation and no further hyphens.
fn is_delimiter(line: &str) -> bool {
    line_content(line)
        .strip_prefix("---")
        .is_some_and(|rest| rest.bytes().all(|byte| matches!(byte, b' ' | b'\t')))
}

/// Recognizes Front Matter at byte 0 of `source`, which must be the document
/// start. Offsets are relative to `source`. A missing closing line means no
/// Front Matter.
pub fn recognize_front_matter(source: &str) -> Option<FrontMatter> {
    let mut lines = markdown_lines(source);
    let opening = lines.next()?;
    if !is_delimiter(opening) {
        return None;
    }
    let mut offset = opening.len();
    for line in lines {
        if is_delimiter(line) {
            let closing_start = offset;
            let end = offset + line.len();
            return Some(FrontMatter {
                range: SourceRange::new(0, end),
                opening_line: SourceRange::new(0, opening.len()),
                content: SourceRange::new(opening.len(), closing_start),
                closing_line: SourceRange::new(closing_start, end),
                closing_has_line_ending: line.ends_with(['\r', '\n']),
                metadata: parse_metadata(source, opening.len(), closing_start),
            });
        }
        offset += line.len();
    }
    None
}

/// Reads the head of `buffer` for Front Matter, looking at no more than about
/// `byte_budget` bytes past the opening line. A document whose first line is
/// not an opening delimiter is decided from that one line.
pub fn front_matter_head(buffer: &RopeBuffer, byte_budget: usize) -> FrontMatterHead {
    // Almost every document fails this check, and it reads three bytes
    // however long the first line is.
    if buffer.len_bytes().0 < 3
        || buffer.text(SourceRange::new(0, 3)).ok().as_deref() != Some("---")
    {
        return FrontMatterHead::Absent;
    }
    let read = |line: usize| -> Option<String> {
        let range = buffer.line_range(LineId(line)).ok()?;
        buffer.text(range).ok()
    };
    let Some(opening) = read(0) else {
        return FrontMatterHead::Absent;
    };
    if !is_delimiter(&opening) {
        return FrontMatterHead::Absent;
    }
    let mut text = opening;
    for line in 1..buffer.line_count() {
        let Some(next) = read(line) else {
            return FrontMatterHead::Unconfirmed;
        };
        let closes = is_delimiter(&next);
        text.push_str(&next);
        if closes {
            return recognize_front_matter(&text)
                .map_or(FrontMatterHead::Absent, FrontMatterHead::Present);
        }
        if text.len() > byte_budget {
            return FrontMatterHead::Unconfirmed;
        }
    }
    FrontMatterHead::Absent
}

enum ParsedLine {
    /// Blank, or a comment.
    Skipped,
    /// Not interpretable. `date_key` is true when the key still reads `date`.
    Opaque { date_key: bool },
    Entry {
        key: String,
        value: String,
        value_range: SourceRange,
    },
}

fn parse_metadata(source: &str, content_start: usize, content_end: usize) -> FrontMatterMetadata {
    let mut has_content = false;
    let mut date_lines = 0usize;
    let mut date_entry: Option<(String, SourceRange)> = None;
    let mut entries: Vec<FrontMatterEntry> = Vec::new();
    let mut offset = content_start;
    for line in markdown_lines(&source[content_start..content_end]) {
        let parsed = parse_metadata_line(offset, line_content(line));
        offset += line.len();
        match parsed {
            ParsedLine::Skipped => {}
            ParsedLine::Opaque { date_key } => {
                has_content = true;
                date_lines += usize::from(date_key);
            }
            ParsedLine::Entry {
                key,
                value,
                value_range,
            } => {
                has_content = true;
                if key == "date" {
                    date_lines += 1;
                    date_entry = Some((value, value_range));
                } else if !entries.iter().any(|entry| entry.key == key) {
                    entries.push(FrontMatterEntry {
                        key,
                        value,
                        value_range,
                    });
                }
            }
        }
    }
    let date = match (date_lines, date_entry) {
        (0, _) => FrontMatterDateState::Absent,
        (1, Some((value, value_range))) => parse_date(&value).map_or(
            FrontMatterDateState::Invalid,
            |(year, month, day)| {
                FrontMatterDateState::Valid(FrontMatterDate {
                    year,
                    month,
                    day,
                    value_range,
                })
            },
        ),
        _ => FrontMatterDateState::Invalid,
    };
    FrontMatterMetadata {
        date,
        entries,
        has_content,
    }
}

fn parse_metadata_line(line_start: usize, content: &str) -> ParsedLine {
    let trimmed = content.trim_start_matches([' ', '\t']);
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return ParsedLine::Skipped;
    }
    if trimmed.len() != content.len() {
        // An indented item belongs to a nested structure.
        return ParsedLine::Opaque { date_key: false };
    }
    let Some(colon) = content.find(':') else {
        return ParsedLine::Opaque { date_key: false };
    };
    let key = content[..colon].trim_end_matches([' ', '\t']);
    if !is_key(key) {
        return ParsedLine::Opaque { date_key: false };
    }
    let date_key = key == "date";
    let after_colon = &content[colon + 1..];
    let leading = after_colon.len() - after_colon.trim_start_matches([' ', '\t']).len();
    let value_text = after_colon.trim_matches([' ', '\t']);
    let value_start = line_start + colon + 1 + leading;
    match interpret_value(value_text) {
        Some((inner_offset, inner)) => ParsedLine::Entry {
            key: key.to_owned(),
            value: inner.to_owned(),
            value_range: SourceRange::new(
                value_start + inner_offset,
                value_start + inner_offset + inner.len(),
            ),
        },
        None => ParsedLine::Opaque { date_key },
    }
}

fn is_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// The value's content and its byte offset inside `value_text`, or `None` when
/// the value needs YAML semantics this reader does not model.
fn interpret_value(value_text: &str) -> Option<(usize, &str)> {
    let first = value_text.chars().next()?;
    if first == '"' || first == '\'' {
        let inner = value_text.strip_prefix(first)?.strip_suffix(first)?;
        if inner.contains(first) || (first == '"' && inner.contains('\\')) {
            return None;
        }
        return Some((1, inner));
    }
    if value_text.starts_with(['[', '{', '|', '>', '&', '*', '!', '#'])
        || value_text == "-"
        || value_text.starts_with("- ")
        || value_text.starts_with("-\t")
    {
        return None;
    }
    let bytes = value_text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'#' && index > 0 && matches!(bytes[index - 1], b' ' | b'\t') {
            return None;
        }
        if *byte == b':'
            && bytes
                .get(index + 1)
                .is_none_or(|next| matches!(next, b' ' | b'\t'))
        {
            return None;
        }
    }
    Some((0, value_text))
}

/// `YYYY-MM-DD` in ASCII naming a real Gregorian day in years 1 through 9999.
fn parse_date(value: &str) -> Option<(u16, u8, u8)> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return None;
    }
    let year: u16 = value[0..4].parse().ok()?;
    let month: u8 = value[5..7].parse().ok()?;
    let day: u8 = value[8..10].parse().ok()?;
    if year == 0 || !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 31,
    };
    (day <= days).then_some((year, month, day))
}
