use hane_document::{Revision, SourceRange};
use hane_markdown::parse_document;

#[test]
fn dump_list_hard_break_tree_issue_177() {
    let source = "- foo  \n   baz";
    let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
    let rows = parsed
        .tree
        .iter()
        .map(|(id, node)| format!("{id:?} depth={} kind={:?} range={:?} parent={:?}", node.depth, node.kind, node.source_range, node.parent))
        .collect::<Vec<_>>()
        .join("\n");
    panic!("\n{rows}\n");
}
