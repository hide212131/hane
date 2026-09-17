use hane_document::{Revision, SourceRange};
use hane_markdown::parse_document;

#[test]
fn hard_break_syntax_is_derived_without_consuming_the_line_ending() {
    for newline in ["\n", "\r\n", "\r"] {
        let source = format!("日本  {newline}emoji🙂\\{newline}終");
        let base = 37;
        let parsed = parse_document(
            Revision(1),
            SourceRange::new(base, base + source.len()),
            &source,
        );

        let marker_text = parsed
            .markers
            .iter()
            .map(|marker| {
                &source[marker.start.0 - base..marker.end.0 - base]
            })
            .collect::<Vec<_>>();

        assert_eq!(marker_text, ["  ", "\\"], "newline={newline:?}");
        assert!(parsed.markers.iter().all(|marker| {
            source.is_char_boundary(marker.start.0 - base)
                && source.is_char_boundary(marker.end.0 - base)
        }));
    }
}
