use hane_document::{Revision, SourceRange};
use hane_markdown::{NodeKind, parse_document};

#[test]
fn dump_commonmark_break_ranges_for_issue_177() {
    let cases = [
        ("hard-spaces-leading", "foo  \n     bar"),
        ("hard-backslash-leading", "foo\\\n     bar"),
        ("soft-surrounding-spaces", "foo \n baz"),
    ];
    let mut dump = String::new();
    for (name, source) in cases {
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        dump.push_str(&format!("CASE {name} source={source:?}\n"));
        for (_, node) in parsed.tree.iter() {
            if matches!(node.kind, NodeKind::Text | NodeKind::SoftBreak | NodeKind::HardBreak) {
                let r = node.source_range;
                dump.push_str(&format!(
                    "  {:?} {}..{} {:?}\n",
                    node.kind,
                    r.start.0,
                    r.end.0,
                    &source[r.start.0..r.end.0]
                ));
            }
        }
        dump.push_str("  markers:");
        for marker in &parsed.markers {
            dump.push_str(&format!(
                " {}..{} {:?}",
                marker.start.0,
                marker.end.0,
                &source[marker.start.0..marker.end.0]
            ));
        }
        dump.push('\n');
    }
    panic!("\n{dump}");
}
