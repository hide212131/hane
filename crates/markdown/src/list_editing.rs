//! Source-first list editing.
//!
//! This module deliberately stops at a source edit plan. It does not mutate a
//! buffer and it does not know anything about presentation or the editor's
//! undo stack. The caller can therefore apply one plan as one ordinary source
//! replacement and let the normal parse/presentation pipeline catch up.

#![allow(
    clippy::items_after_test_module,
    reason = "the tests stay next to the public planner contract while helpers remain private"
)]

use crate::{MarkdownParse, NodeId, NodeKind};
use hane_document::{BufferError, RopeBuffer, SourceOffset, SourceRange};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListEditIntent {
    Enter,
    ShiftEnter,
    Indent,
    Outdent,
    Backspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSelection {
    pub anchor: SourceOffset,
    pub active: SourceOffset,
}

impl SourceSelection {
    pub const fn caret(offset: SourceOffset) -> Self {
        Self {
            anchor: offset,
            active: offset,
        }
    }

    pub const fn range(self) -> SourceRange {
        if self.anchor.0 <= self.active.0 {
            SourceRange::new(self.anchor.0, self.active.0)
        } else {
            SourceRange::new(self.active.0, self.anchor.0)
        }
    }

    pub const fn is_caret(self) -> bool {
        self.anchor.0 == self.active.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownEditPlan {
    Replace {
        range: SourceRange,
        replacement: String,
        selection_after: SourceSelection,
    },
    NoOp {
        selection_after: SourceSelection,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListEditPlanResult {
    Handled(MarkdownEditPlan),
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListMarker {
    Unordered { bullet: u8 },
    Ordered { delimiter: u8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListEditItem {
    pub item_range: SourceRange,
    pub subtree_range: SourceRange,
    pub list_range: SourceRange,
    pub marker_range: SourceRange,
    pub prefix_range: SourceRange,
    pub body_range: SourceRange,
    pub marker: ListMarker,
    pub list_start: Option<u64>,
    pub ordinal: usize,
    pub depth: usize,
    pub task: Option<bool>,
    pub marker_indent_columns: usize,
    pub body_indent_columns: usize,
    pub previous_sibling: Option<usize>,
    pub parent_item: Option<usize>,
}

/// Exact-current-revision list metadata used by the input path. Unlike the
/// formal presentation projection this is allowed to be built synchronously
/// from the small block containing the caret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListEditProjection {
    pub source_range: SourceRange,
    pub items: Vec<ListEditItem>,
}

impl ListEditProjection {
    pub fn item_at(&self, offset: SourceOffset) -> Option<(usize, &ListEditItem)> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.prefix_range.start.0 <= offset.0
                    && (offset.0 < item.item_range.end.0
                        // A parser item range may include its terminating line
                        // break. The caret at the end of the final physical
                        // line is still in the item, but the caret immediately
                        // after that line break belongs to the following
                        // paragraph (or the document boundary).
                        || (offset.0 == item.item_range.end.0
                            && item.body_range.end.0 == item.item_range.end.0))
            })
            .max_by_key(|(_, item)| (item.depth, item.item_range.start.0))
    }
}

/// Builds the synchronous editing projection for one parser result.
pub fn build_list_edit_projection(parsed: &MarkdownParse, source: &str) -> ListEditProjection {
    let mut items = Vec::new();

    for (marker_range, item_id) in &parsed.list_item_markers {
        let Some(item_node) = parsed.tree.node(*item_id) else {
            continue;
        };
        let Some((list_id, ordinal)) = parsed.tree.list_item_ordinal(*item_id) else {
            continue;
        };
        let Some(list_node) = parsed.tree.node(list_id) else {
            continue;
        };
        let NodeKind::List { start: list_start } = list_node.kind else {
            continue;
        };

        let marker_rel = marker_range
            .start
            .0
            .saturating_sub(parsed.source_range.start.0);
        let marker_end_rel = marker_range
            .end
            .0
            .saturating_sub(parsed.source_range.start.0);
        let line_start_rel = line_start(source.as_bytes(), marker_rel);
        let line_end_rel = line_end(source.as_bytes(), line_start_rel);
        let marker_end_rel = marker_end_rel.min(line_end_rel);
        let marker_text = source
            .get(marker_rel..marker_end_rel)
            .unwrap_or_default()
            .trim_end_matches([' ', '\t']);
        let marker = if marker_text
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
        {
            ListMarker::Ordered {
                delimiter: marker_text.as_bytes().last().copied().unwrap_or(b'.'),
            }
        } else {
            ListMarker::Unordered {
                bullet: marker_text.as_bytes().first().copied().unwrap_or(b'-'),
            }
        };

        let task_range = find_task_marker(&parsed.tree, *item_id);
        let after_marker_rel = marker_end_rel;
        let mut body_start_rel = after_marker_rel;
        while matches!(source.as_bytes().get(body_start_rel), Some(b' ' | b'\t')) {
            body_start_rel += 1;
        }
        if let Some(task_range) = task_range {
            let task_end = task_range
                .end
                .0
                .saturating_sub(parsed.source_range.start.0)
                .min(line_end_rel);
            body_start_rel = task_end;
            while matches!(source.as_bytes().get(body_start_rel), Some(b' ' | b'\t')) {
                body_start_rel += 1;
            }
        }
        let body_start_rel = body_start_rel.min(line_end_rel);
        let marker_rel_start = marker_range.start.0 - parsed.source_range.start.0;
        let marker_indent_columns = columns(&source[line_start_rel..marker_rel_start]);
        let body_indent_columns = prefix_columns(&source[line_start_rel..body_start_rel]);
        let prefix_range = SourceRange::new(
            parsed.source_range.start.0 + line_start_rel,
            parsed.source_range.start.0 + body_start_rel,
        );
        let body_range = SourceRange::new(
            parsed.source_range.start.0 + body_start_rel,
            parsed.source_range.start.0 + line_end_rel,
        );
        items.push(ListEditItem {
            item_range: item_node.source_range,
            subtree_range: SourceRange::new(
                parsed.source_range.start.0 + line_start_rel,
                item_node.source_range.end.0,
            ),
            list_range: list_node.source_range,
            marker_range: *marker_range,
            prefix_range,
            body_range,
            marker,
            list_start,
            ordinal,
            depth: parsed.tree.list_depth(*item_id),
            task: match item_node.kind {
                NodeKind::ListItem { task } => task,
                _ => None,
            },
            marker_indent_columns,
            body_indent_columns,
            previous_sibling: None,
            parent_item: None,
        });
    }

    // CommonMark intentionally treats a trailing empty marker as an
    // incomplete setext/paragraph candidate in a few cases (for example the
    // source immediately after Tab can end in `  - `). The input path still
    // needs the list owner before another Enter, so recover only marker lines
    // that the formal tree did not expose. This is an editing projection, not
    // a second rendered Markdown parse; the next source edit will cause the
    // normal parser to decide the final structure.
    for (line_start_rel, line_end_rel) in line_spans(source) {
        let line = &source[line_start_rel..line_end_rel];
        let Some((indent_end, marker_end, marker)) = raw_list_marker(line) else {
            continue;
        };
        let marker_start_abs = parsed.source_range.start.0 + line_start_rel + indent_end;
        if parsed
            .list_item_markers
            .iter()
            .any(|(range, _)| range.start.0 == marker_start_abs)
        {
            continue;
        }
        let line_start_abs = parsed.source_range.start.0 + line_start_rel;
        let line_end_abs = parsed.source_range.start.0 + line_end_rel;
        let mut body_start = marker_end;
        while matches!(line.as_bytes().get(body_start), Some(b' ' | b'\t')) {
            body_start += 1;
        }
        let task = if line
            .as_bytes()
            .get(body_start..body_start + 3)
            .is_some_and(|bytes| {
                matches!(
                    bytes,
                    [b'[', b' ', b']'] | [b'[', b'x', b']'] | [b'[', b'X', b']']
                )
            }) {
            let checked = line.as_bytes()[body_start + 1] != b' ';
            body_start += 3;
            while matches!(line.as_bytes().get(body_start), Some(b' ' | b'\t')) {
                body_start += 1;
            }
            Some(checked)
        } else {
            None
        };
        let mut body_start = body_start.min(line.len());
        if line[body_start..].trim().is_empty() {
            body_start = line.len();
        }
        let marker_indent_columns = columns(&line[..indent_end]);
        let body_indent_columns = prefix_columns(&line[..body_start]);
        let absolute_prefix = SourceRange::new(line_start_abs, line_start_abs + body_start);
        let subtree_end = incomplete_subtree_end(
            source,
            line_start_rel,
            line_end_rel,
            marker_indent_columns,
            parsed.source_range.end.0,
        );
        let previous = items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.item_range.start.0 < marker_start_abs
                    && item.marker_indent_columns == marker_indent_columns
                    && same_marker_kind(item.marker, marker)
            })
            .max_by_key(|(_, item)| item.item_range.start.0);
        let parent = items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.item_range.start.0 < marker_start_abs
                    && item.marker_indent_columns < marker_indent_columns
                    && item.item_range.end.0 >= marker_start_abs
            })
            .max_by_key(|(_, item)| item.marker_indent_columns);
        let (list_range, list_start, depth, ordinal, parent_index) =
            if let Some((index, parent)) = parent {
                let ordinal = items
                    .iter()
                    .filter(|item| item.list_range == parent.list_range)
                    .count();
                (
                    parent.list_range,
                    parent.list_start,
                    parent.depth + 1,
                    ordinal,
                    Some(index),
                )
            } else if let Some((_index, previous)) = previous {
                let ordinal = previous.ordinal + 1;
                (
                    previous.list_range,
                    previous.list_start,
                    previous.depth,
                    ordinal,
                    previous.parent_item,
                )
            } else {
                (
                    SourceRange::new(line_start_abs, line_end_abs),
                    match marker {
                        ListMarker::Ordered { .. } => Some(1),
                        ListMarker::Unordered { .. } => None,
                    },
                    1,
                    0,
                    None,
                )
            };
        items.push(ListEditItem {
            item_range: SourceRange::new(marker_start_abs, line_end_abs),
            subtree_range: SourceRange::new(line_start_abs, subtree_end),
            list_range,
            marker_range: SourceRange::new(marker_start_abs, line_start_abs + marker_end),
            prefix_range: absolute_prefix,
            body_range: SourceRange::new(line_start_abs + body_start, line_end_abs),
            marker,
            list_start,
            ordinal,
            depth,
            task,
            marker_indent_columns,
            body_indent_columns,
            previous_sibling: previous.map(|(index, _)| index),
            parent_item: parent_index,
        });
    }

    // The parser's marker list is in source order, while parent/sibling lookup
    // above uses marker-list positions. Normalize those positions after all
    // items have been collected and discard accidental cross-block entries.
    items.sort_by_key(|item| (item.item_range.start.0, item.item_range.end.0));
    for index in 0..items.len() {
        let current = &items[index];
        let previous = current.ordinal.checked_sub(1).and_then(|ordinal| {
            items.iter().position(|candidate| {
                candidate.list_range == current.list_range && candidate.ordinal == ordinal
            })
        });
        let parent = items.iter().position(|candidate| {
            candidate.depth + 1 == current.depth
                && candidate.item_range.start.0 <= current.item_range.start.0
                && current.item_range.end.0 <= candidate.item_range.end.0
        });
        items[index].previous_sibling = previous;
        items[index].parent_item = parent;
    }

    ListEditProjection {
        source_range: parsed.source_range,
        items,
    }
}

fn raw_list_marker(line: &str) -> Option<(usize, usize, ListMarker)> {
    let bytes = line.as_bytes();
    let mut indent_end = 0;
    while matches!(bytes.get(indent_end), Some(b' ' | b'\t')) {
        indent_end += 1;
    }
    let marker_start = indent_end;
    let marker_end = match bytes.get(marker_start) {
        Some(b'-' | b'+' | b'*') => marker_start + 1,
        Some(b'0'..=b'9') => {
            let mut end = marker_start;
            while end < bytes.len() && end - marker_start < 9 && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if !matches!(bytes.get(end), Some(b'.' | b')')) {
                return None;
            }
            end + 1
        }
        _ => return None,
    };
    if marker_end < bytes.len() && !matches!(bytes[marker_end], b' ' | b'\t') {
        return None;
    }
    let marker = if bytes[marker_start].is_ascii_digit() {
        ListMarker::Ordered {
            delimiter: bytes[marker_end - 1],
        }
    } else {
        ListMarker::Unordered {
            bullet: bytes[marker_start],
        }
    };
    Some((
        indent_end,
        marker_end + usize::from(marker_end < bytes.len()),
        marker,
    ))
}

fn same_marker_kind(left: ListMarker, right: ListMarker) -> bool {
    matches!(
        (left, right),
        (ListMarker::Unordered { .. }, ListMarker::Unordered { .. })
            | (ListMarker::Ordered { .. }, ListMarker::Ordered { .. })
    )
}

fn incomplete_subtree_end(
    source: &str,
    current_start: usize,
    current_end: usize,
    current_indent: usize,
    source_end: usize,
) -> usize {
    for (start, end) in line_spans(source) {
        if start <= current_start || start < current_end {
            continue;
        }
        let line = &source[start..end];
        if line.trim().is_empty() {
            continue;
        }
        let indent_end = line
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        if columns(&line[..indent_end]) <= current_indent {
            return source_end.min(start);
        }
    }
    source_end
}

fn line_spans(source: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= source.len() {
            return None;
        }
        let rest = &source[start..];
        let line_start = start;
        let end = rest
            .find(['\r', '\n'])
            .map_or(source.len(), |offset| start + offset);
        let next = rest.find(['\r', '\n']).map_or(source.len(), |offset| {
            let at = start + offset;
            at + usize::from(
                source.as_bytes()[at] == b'\r' && source.as_bytes().get(at + 1) == Some(&b'\n'),
            ) + 1
        });
        start = next;
        Some((line_start, end.min(source.len())))
    })
}

pub fn plan_list_edit(
    document: &RopeBuffer,
    projection: &ListEditProjection,
    selection: SourceSelection,
    intent: ListEditIntent,
) -> Result<ListEditPlanResult, BufferError> {
    if !selection.is_caret() {
        return Ok(ListEditPlanResult::NotApplicable);
    }
    let caret = selection.active;
    let Some((_index, item)) = projection.item_at(caret) else {
        return Ok(ListEditPlanResult::NotApplicable);
    };
    let source = document.full_text();
    let opening_line_start = line_start(source.as_bytes(), item.prefix_range.start.0);
    let on_opening_line = caret.0 >= item.prefix_range.start.0
        && caret.0 <= item.body_range.end.0
        && line_start(source.as_bytes(), caret.0) == opening_line_start;

    match intent {
        ListEditIntent::Indent => {
            if !on_opening_line {
                return Ok(ListEditPlanResult::NotApplicable);
            }
            let Some(previous) = item
                .previous_sibling
                .and_then(|at| projection.items.get(at))
            else {
                return Ok(ListEditPlanResult::Handled(MarkdownEditPlan::NoOp {
                    selection_after: selection,
                }));
            };
            let delta = previous
                .body_indent_columns
                .saturating_sub(item.marker_indent_columns);
            if delta == 0 {
                return Ok(ListEditPlanResult::Handled(MarkdownEditPlan::NoOp {
                    selection_after: selection,
                }));
            }
            Ok(ListEditPlanResult::Handled(transform_subtree(
                &source,
                item,
                delta as isize,
                caret,
            )))
        }
        ListEditIntent::Outdent => {
            if !on_opening_line || item.depth <= 1 {
                return Ok(ListEditPlanResult::Handled(MarkdownEditPlan::NoOp {
                    selection_after: selection,
                }));
            }
            let Some(parent) = item.parent_item.and_then(|at| projection.items.get(at)) else {
                return Ok(ListEditPlanResult::Handled(MarkdownEditPlan::NoOp {
                    selection_after: selection,
                }));
            };
            let delta = item
                .marker_indent_columns
                .saturating_sub(parent.marker_indent_columns);
            if delta == 0 {
                return Ok(ListEditPlanResult::Handled(MarkdownEditPlan::NoOp {
                    selection_after: selection,
                }));
            }
            Ok(ListEditPlanResult::Handled(transform_subtree(
                &source,
                item,
                -(delta as isize),
                caret,
            )))
        }
        ListEditIntent::ShiftEnter => {
            if !on_opening_line {
                return Ok(ListEditPlanResult::NotApplicable);
            }
            let replacement = format!("\n{}", " ".repeat(item.body_indent_columns));
            let new_caret = SourceOffset(caret.0 + replacement.len());
            Ok(ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
                range: SourceRange::empty(caret.0),
                replacement,
                selection_after: SourceSelection::caret(new_caret),
            }))
        }
        ListEditIntent::Enter | ListEditIntent::Backspace => {
            let empty = source
                .get(item.body_range.as_usize())
                .is_some_and(|text| text.trim().is_empty());
            let empty_action = matches!(intent, ListEditIntent::Enter)
                || (matches!(intent, ListEditIntent::Backspace)
                    && caret.0 <= item.body_range.start.0);
            if empty && empty_action && on_opening_line {
                if item.depth <= 1 {
                    return Ok(ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
                        range: item.prefix_range,
                        replacement: String::new(),
                        selection_after: SourceSelection::caret(item.prefix_range.start),
                    }));
                }
                let Some(parent) = item.parent_item.and_then(|at| projection.items.get(at)) else {
                    return Ok(ListEditPlanResult::NotApplicable);
                };
                let delta = item
                    .marker_indent_columns
                    .saturating_sub(parent.marker_indent_columns);
                return Ok(ListEditPlanResult::Handled(transform_subtree(
                    &source,
                    item,
                    -(delta as isize),
                    caret,
                )));
            }
            if !on_opening_line || !matches!(intent, ListEditIntent::Enter) {
                return Ok(ListEditPlanResult::NotApplicable);
            }
            let prefix = item_prefix(&source, item);
            let replacement = format!("\n{prefix}");
            let new_caret = SourceOffset(caret.0 + replacement.len());
            Ok(ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
                range: SourceRange::empty(caret.0),
                replacement,
                selection_after: SourceSelection::caret(new_caret),
            }))
        }
    }
}

fn item_prefix(source: &str, item: &ListEditItem) -> String {
    let indent = source
        .get(item.prefix_range.start.0..item.marker_range.start.0)
        .unwrap_or_default();
    let marker = match item.marker {
        ListMarker::Unordered { bullet } => String::from_utf8_lossy(&[bullet]).into_owned(),
        ListMarker::Ordered { delimiter } => {
            let number = item
                .list_start
                .unwrap_or(1)
                .saturating_add(item.ordinal as u64)
                .saturating_add(1);
            format!("{number}{}", delimiter as char)
        }
    };
    let separator = if item.task.is_some() { " [ ] " } else { " " };
    format!("{indent}{marker}{separator}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_document;
    use hane_document::{Revision, RopeBuffer};

    fn plan(text: &str, caret: usize, intent: ListEditIntent) -> MarkdownEditPlan {
        let source_range = SourceRange::new(0, text.len());
        let parsed = parse_document(Revision(1), source_range, text);
        let projection = build_list_edit_projection(&parsed, text);
        match plan_list_edit(
            &RopeBuffer::from_text(text),
            &projection,
            SourceSelection::caret(SourceOffset(caret)),
            intent,
        )
        .unwrap()
        {
            ListEditPlanResult::Handled(plan) => plan,
            ListEditPlanResult::NotApplicable => panic!("list edit was not applicable"),
        }
    }

    fn replacement(text: &str, caret: usize, intent: ListEditIntent) -> (String, usize) {
        let MarkdownEditPlan::Replace {
            range,
            replacement,
            selection_after,
        } = plan(text, caret, intent)
        else {
            panic!("expected replacement")
        };
        let mut result = text.to_owned();
        result.replace_range(range.as_usize(), &replacement);
        (result, selection_after.active.0)
    }

    #[test]
    fn enter_commits_same_level_item() {
        assert_eq!(
            replacement("- ABC", 5, ListEditIntent::Enter),
            ("- ABC\n- ".into(), 8)
        );
    }

    #[test]
    fn ordered_enter_uses_next_semantic_number() {
        assert_eq!(
            replacement("3. A\n4. B", 4, ListEditIntent::Enter),
            ("3. A\n4. \n4. B".into(), 8)
        );
    }

    #[test]
    fn ordered_enter_preserves_marker_width_for_two_digit_items() {
        assert_eq!(
            replacement("10. A\n11. B", 5, ListEditIntent::Enter),
            ("10. A\n11. \n11. B".into(), 10)
        );
    }

    #[test]
    fn task_enter_creates_unchecked_item() {
        assert_eq!(
            replacement("- [x] A", 7, ListEditIntent::Enter),
            ("- [x] A\n- [ ] ".into(), 14)
        );
    }

    #[test]
    fn tab_moves_item_and_subtree_under_previous_sibling() {
        assert_eq!(
            replacement("- A\n- B\n  - C", 7, ListEditIntent::Indent),
            ("- A\n  - B\n    - C".into(), 9)
        );
    }

    #[test]
    fn first_item_tab_is_a_noop() {
        assert!(matches!(
            plan("- A\n- B", 2, ListEditIntent::Indent),
            MarkdownEditPlan::NoOp { .. }
        ));
    }

    #[test]
    fn shift_enter_continues_the_current_item_body() {
        assert_eq!(
            replacement("- ABC", 5, ListEditIntent::ShiftEnter),
            ("- ABC\n  ".into(), 8)
        );
    }

    #[test]
    fn shift_tab_moves_a_nested_subtree_to_its_parent_level() {
        assert_eq!(
            replacement("- A\n  - B\n    - C", 7, ListEditIntent::Outdent),
            ("- A\n- B\n  - C".into(), 5)
        );
    }

    #[test]
    fn empty_nested_enter_outdents_and_top_level_enter_removes_marker() {
        assert_eq!(
            replacement("- A\n  - \n", 8, ListEditIntent::Enter),
            ("- A\n- \n".into(), 6)
        );
        assert_eq!(replacement("- ", 2, ListEditIntent::Enter), ("".into(), 0));
    }

    #[test]
    fn empty_nested_backspace_outdents_and_top_level_backspace_removes_marker() {
        assert_eq!(
            replacement("- A\n  - ", 6, ListEditIntent::Backspace),
            ("- A\n- ".into(), 4)
        );
        assert_eq!(
            replacement("- ", 2, ListEditIntent::Backspace),
            ("".into(), 0)
        );
    }

    #[test]
    fn structural_planner_does_not_claim_multi_selection() {
        let source = "- A\n- B";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let projection = build_list_edit_projection(&parsed, source);
        let result = plan_list_edit(
            &RopeBuffer::from_text(source),
            &projection,
            SourceSelection {
                anchor: SourceOffset(0),
                active: SourceOffset(source.len()),
            },
            ListEditIntent::Indent,
        )
        .unwrap();
        assert_eq!(result, ListEditPlanResult::NotApplicable);
    }
}

fn find_task_marker(tree: &crate::MarkdownTree, id: NodeId) -> Option<SourceRange> {
    for child in tree.children(id) {
        let node = tree.node(*child)?;
        if matches!(node.kind, NodeKind::TaskMarker(_)) {
            return Some(node.source_range);
        }
        if let Some(found) = find_task_marker(tree, *child) {
            return Some(found);
        }
    }
    None
}

fn transform_subtree(
    source: &str,
    item: &ListEditItem,
    delta: isize,
    caret: SourceOffset,
) -> MarkdownEditPlan {
    let range = item.subtree_range;
    let original = source.get(range.as_usize()).unwrap_or_default();
    let mut replacement =
        String::with_capacity(original.len().saturating_add(delta.max(0) as usize));
    let mut at = 0;
    for part in original.split_inclusive('\n') {
        let ending_len = if part.ends_with("\r\n") { 2 } else { 1 };
        let content_end = part.len().saturating_sub(ending_len);
        let content = &part[..content_end];
        let ending = &part[content_end..];
        if delta >= 0 {
            replacement.push_str(&" ".repeat(delta as usize));
            replacement.push_str(content);
        } else {
            replacement.push_str(&remove_columns(content, (-delta) as usize));
        }
        replacement.push_str(ending);
        at += part.len();
    }
    if at < original.len() {
        let content = &original[at..];
        if delta >= 0 {
            replacement.push_str(&" ".repeat(delta as usize));
            replacement.push_str(content);
        } else {
            replacement.push_str(&remove_columns(content, (-delta) as usize));
        }
    }
    let line_start = line_start(source.as_bytes(), caret.0);
    let leading = caret.0.saturating_sub(line_start);
    let mapped = if delta >= 0 {
        caret.0 + delta as usize
    } else {
        caret.0.saturating_sub((-delta) as usize).max(line_start)
    };
    let _ = leading;
    MarkdownEditPlan::Replace {
        range,
        replacement,
        selection_after: SourceSelection::caret(SourceOffset(mapped)),
    }
}

fn remove_columns(line: &str, columns_to_remove: usize) -> String {
    let mut removed = 0;
    let mut bytes = 0;
    for byte in line.as_bytes() {
        match *byte {
            b' ' => {
                if removed >= columns_to_remove {
                    break;
                }
                removed += 1;
                bytes += 1;
            }
            b'\t' => {
                if removed >= columns_to_remove {
                    break;
                }
                removed += 4;
                bytes += 1;
            }
            _ => break,
        }
    }
    line[bytes.min(line.len())..].to_owned()
}

fn columns(text: &str) -> usize {
    let mut column = 0;
    for byte in text.bytes() {
        match byte {
            b' ' => column += 1,
            b'\t' => column += 4 - column % 4,
            _ => break,
        }
    }
    column
}

fn prefix_columns(text: &str) -> usize {
    let mut column = 0;
    for byte in text.bytes() {
        match byte {
            b' ' => column += 1,
            b'\t' => column += 4 - column % 4,
            _ => column += 1,
        }
    }
    column
}

fn line_start(source: &[u8], offset: usize) -> usize {
    source[..offset.min(source.len())]
        .iter()
        .rposition(|byte| matches!(*byte, b'\n' | b'\r'))
        .map_or(0, |at| at + 1)
}

fn line_end(source: &[u8], offset: usize) -> usize {
    source[offset.min(source.len())..]
        .iter()
        .position(|byte| matches!(*byte, b'\n' | b'\r'))
        .map_or(source.len(), |at| offset + at)
}
