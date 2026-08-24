//! Phase 0 local parsing. Full CommonMark parsing is intentionally deferred.

use hane_document::{Revision, SourceRange};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineKind {
    Bold,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineSpan {
    pub kind: InlineKind,
    pub full_range: SourceRange,
    pub content_range: SourceRange,
    pub open_marker: SourceRange,
    pub close_marker: SourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalParse {
    pub revision: Revision,
    pub source_range: SourceRange,
    pub spans: Vec<InlineSpan>,
}

/// Finds non-nested `**bold**` pairs in one block. Escapes and cross-block spans are
/// deliberately excluded from the Phase 0 experiment.
pub fn parse_bold(revision: Revision, source_range: SourceRange, source: &str) -> LocalParse {
    let mut spans = Vec::new();
    let bytes = source.as_bytes();
    let mut cursor = 0;
    while cursor + 1 < bytes.len() {
        let Some(open_relative) = source[cursor..].find("**") else {
            break;
        };
        let open = cursor + open_relative;
        let content_start = open + 2;
        let Some(close_relative) = source[content_start..].find("**") else {
            break;
        };
        let close = content_start + close_relative;
        if close > content_start && !source[content_start..close].contains('\n') {
            let base = source_range.start.0;
            spans.push(InlineSpan {
                kind: InlineKind::Bold,
                full_range: SourceRange::new(base + open, base + close + 2),
                content_range: SourceRange::new(base + content_start, base + close),
                open_marker: SourceRange::new(base + open, base + content_start),
                close_marker: SourceRange::new(base + close, base + close + 2),
            });
        }
        cursor = close + 2;
    }
    LocalParse {
        revision,
        source_range,
        spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_unicode_bold_ranges_in_bytes() {
        let parsed = parse_bold(Revision(3), SourceRange::new(10, 30), "a**日本🙂**z");
        assert_eq!(parsed.spans[0].content_range, SourceRange::new(13, 23));
        assert_eq!(parsed.spans[0].full_range, SourceRange::new(11, 25));
    }

    #[test]
    fn ignores_empty_and_unclosed_markers() {
        assert!(
            parse_bold(Revision(0), SourceRange::new(0, 4), "****")
                .spans
                .is_empty()
        );
        assert!(
            parse_bold(Revision(0), SourceRange::new(0, 5), "**abc")
                .spans
                .is_empty()
        );
    }
}
