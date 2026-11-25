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
use hane_document::{BufferError, RopeBuffer, SourceOffset, SourceRange, TextBuffer};

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
    /// Whether the formal parser owns this item (from
    /// `MarkdownParse::list_item_markers`) rather than the opening-line-only
    /// lexical recovery fallback below. A recovered candidate with no
    /// confirmed link to a real list may just be ordinary text that happens
    /// to look like a marker (see `exit_is_trustworthy`).
    pub is_parser_item: bool,
    /// Whether this item sits inside a blockquote. The opening line's bytes
    /// include the quote marker, so a prefix edit here would also remove it.
    pub in_quote: bool,
}

/// Exact-current-revision list metadata used by the input path. Unlike the
/// formal presentation projection this is allowed to be built synchronously
/// from the small block containing the caret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListEditProjection {
    pub source_range: SourceRange,
    pub items: Vec<ListEditItem>,
    /// Source ranges of the parser's code blocks (fenced and indented), so the
    /// input path can tell code body lines from list continuation paragraphs
    /// by block membership instead of by the line text.
    pub code_block_ranges: Vec<SourceRange>,
}

impl ListEditProjection {
    /// Whether the physical line `[line_start, line_end]` belongs to a code
    /// block the parser recognized.
    pub fn line_in_code_block(&self, line_start: usize, line_end: usize) -> bool {
        self.code_block_ranges
            .iter()
            .any(|range| range.start.0 <= line_end && line_start < range.end.0)
    }

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
        let in_quote = parsed.tree.ancestors(*item_id).skip(1).any(|ancestor| {
            matches!(
                parsed.tree.node(ancestor).map(|node| node.kind),
                Some(NodeKind::Quote)
            )
        });
        let opening_line_end_abs = parsed.source_range.start.0 + line_end_rel;
        items.push(ListEditItem {
            item_range: item_node.source_range,
            subtree_range: SourceRange::new(
                parsed.source_range.start.0 + line_start_rel,
                item_subtree_end(&parsed.tree, *item_id, opening_line_end_abs),
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
            is_parser_item: true,
            in_quote,
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
        if is_ascii_blank(&line[body_start..]) {
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
            is_parser_item: false,
            in_quote: false,
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

    let code_block_ranges = parsed
        .tree
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::CodeBlock)
        .map(|(_, node)| node.source_range)
        .collect();

    ListEditProjection {
        source_range: parsed.source_range,
        items,
        code_block_ranges,
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

/// Whether `text` contains only ASCII space/tab bytes (including none at
/// all). Unlike `str::trim`, this does not treat other Unicode whitespace
/// (e.g. U+3000 IDEOGRAPHIC SPACE) as blank, so a line item whose body is
/// made of such characters keeps its real, non-empty content.
fn is_ascii_blank(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
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
    // Avoid the full-text copy on the input path when no list item can apply.
    if projection.items.is_empty() {
        return Ok(ListEditPlanResult::NotApplicable);
    }
    let positional_intent = matches!(intent, ListEditIntent::Enter | ListEditIntent::ShiftEnter);
    if !positional_intent && projection.item_at(caret).is_none() {
        return Ok(ListEditPlanResult::NotApplicable);
    }
    let source = document.full_text();
    let positional = if positional_intent {
        plan_positional_enter(&source, projection, caret.0)
    } else {
        None
    };
    if let Some(result) = positional {
        return Ok(result);
    }
    let Some((_index, item)) = projection.item_at(caret) else {
        return Ok(ListEditPlanResult::NotApplicable);
    };
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
            if let Some(plan) = plan_ordered_move(
                &source,
                projection,
                item,
                intent,
                previous.body_indent_columns,
                selection,
            ) {
                return Ok(ListEditPlanResult::Handled(plan));
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
            if let Some(plan) = plan_ordered_move(
                &source,
                projection,
                item,
                intent,
                parent.marker_indent_columns,
                selection,
            ) {
                return Ok(ListEditPlanResult::Handled(plan));
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
            let terminator = preferred_line_terminator(&source, caret.0);
            let replacement = format!(
                "{terminator}{}",
                " ".repeat(item.body_indent_columns)
            );
            let new_caret = SourceOffset(caret.0 + replacement.len());
            Ok(ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
                range: SourceRange::empty(caret.0),
                replacement,
                selection_after: SourceSelection::caret(new_caret),
            }))
        }
        ListEditIntent::Enter | ListEditIntent::Backspace => {
            // `body_range` already excludes any leading ASCII space/tab run
            // after the marker, so a literal empty slice is the only "blank
            // body" case; other Unicode whitespace is real content and must
            // not be treated as an empty item (see `is_ascii_blank`).
            let empty = source
                .get(item.body_range.as_usize())
                .is_some_and(str::is_empty);
            // Both Enter and Backspace exit the empty item only when the
            // caret sits exactly at the empty body's start; a caret inside
            // or before the marker prefix is ordinary editing, not an exit,
            // so it must not be claimed here or fall through to the generic
            // same-level Enter split below.
            if empty && on_opening_line {
                if caret.0 != item.body_range.start.0 {
                    return Ok(ListEditPlanResult::NotApplicable);
                }
                if !exit_is_trustworthy(item) {
                    return Ok(ListEditPlanResult::NotApplicable);
                }
                if !exit_is_safe_to_collapse(&source, item) {
                    return Ok(ListEditPlanResult::Handled(MarkdownEditPlan::NoOp {
                        selection_after: selection,
                    }));
                }
                if item.depth <= 1 {
                    return Ok(ListEditPlanResult::Handled(exit_top_level_item(
                        &source, item,
                    )));
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
            let terminator = preferred_line_terminator(&source, caret.0);
            let replacement = format!("{terminator}{prefix}");
            let new_caret = SourceOffset(caret.0 + replacement.len());
            Ok(ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
                range: SourceRange::empty(caret.0),
                replacement,
                selection_after: SourceSelection::caret(new_caret),
            }))
        }
    }
}

/// Enter / Shift+Enter decided by the source line the caret is on and the
/// caret position within it, not by screen coordinates.
///
/// - Left of an item's marker (or inside its leading indent): a blank line is
///   inserted above, keeping the item's bytes (including indent) intact.
/// - Inside the marker / task prefix: ordinary editing.
/// - A continuation line of a confirmed item: with an indent, the line is
///   split and the new line keeps the line's real leading whitespace (no new
///   marker); at an unindented left edge (or before existing indent) a blank
///   line is inserted above so the line leaves the item.
///
/// Returns `None` when the existing opening-line rules should decide.
fn plan_positional_enter(
    source: &str,
    projection: &ListEditProjection,
    caret: usize,
) -> Option<ListEditPlanResult> {
    let bytes = source.as_bytes();
    if caret > source.len() || !source.is_char_boundary(caret) {
        return None;
    }
    let ls = line_start(bytes, caret);
    let le = line_end(bytes, ls);
    if caret > le {
        return None;
    }
    let insert_above = || {
        let terminator = preferred_line_terminator(source, ls);
        ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
            range: SourceRange::empty(ls),
            replacement: terminator.to_owned(),
            selection_after: SourceSelection::caret(SourceOffset(caret + terminator.len())),
        })
    };

    if let Some(item) = projection
        .items
        .iter()
        .find(|item| item.prefix_range.start.0 == ls)
    {
        if caret >= item.body_range.start.0 {
            return None;
        }
        if !exit_is_trustworthy(item) {
            return Some(ListEditPlanResult::NotApplicable);
        }
        let marker_start = item.marker_range.start.0;
        if !item.in_quote
            && caret <= marker_start
            && source.get(ls..marker_start).is_some_and(is_ascii_blank)
        {
            return Some(insert_above());
        }
        return Some(ListEditPlanResult::NotApplicable);
    }

    // Code block bodies (and their fences) are ordinary text, whatever the
    // item that precedes them looks like.
    if projection.line_in_code_block(ls, le) {
        return None;
    }

    let owner = continuation_owner(source, projection, ls)?;
    if owner.in_quote || !owner.is_parser_item {
        return None;
    }
    let line = &source[ls..le];
    let indent_len = line
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let text_start = ls + indent_len;
    let blank = indent_len == line.len();
    if !blank {
        let text = &line[indent_len..];
        if raw_list_marker(line).is_some()
            || starts_interrupting_block(text)
            || ls >= owner.item_range.end.0
        {
            return None;
        }
    }

    if caret < text_start || (indent_len == 0 && caret == ls) {
        return Some(insert_above());
    }
    if indent_len == 0 {
        return None;
    }
    let required = continuation_indent_required(source, owner);
    if columns(&line[..indent_len]) < required {
        return None;
    }
    let terminator = preferred_line_terminator(source, ls);
    let replacement = format!("{terminator}{}", &line[..indent_len]);
    let new_caret = SourceOffset(caret + replacement.len());
    Some(ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
        range: SourceRange::empty(caret),
        replacement,
        selection_after: SourceSelection::caret(new_caret),
    }))
}

/// Whether trimmed line text opens a fence or quote, which this planner never
/// treats as a plain list continuation.
fn starts_interrupting_block(text: &str) -> bool {
    text.starts_with("```") || text.starts_with("~~~") || text.starts_with('>')
}

/// The minimum indent columns a line needs to continue `item`'s body.
fn continuation_indent_required(source: &str, item: &ListEditItem) -> usize {
    let marker_text = source
        .get(item.marker_range.as_usize())
        .unwrap_or_default()
        .trim_end_matches([' ', '\t']);
    item.marker_indent_columns + marker_text.len() + 1
}

/// The item whose opening line precedes the physical line at `ls` with only
/// non-quote lines and parser code blocks in between. Whitespace-only lines indented far
/// enough to continue the item (such as the ones Enter itself produces) are
/// crossed, but only while the parser still keeps the content after them
/// inside the item: a non-blank line following such a blank must start inside
/// the owner's `item_range`.
fn continuation_owner<'a>(
    source: &str,
    projection: &'a ListEditProjection,
    ls: usize,
) -> Option<&'a ListEditItem> {
    let bytes = source.as_bytes();
    let mut cursor = ls;
    // Start of the closest non-blank line passed so far, and the blank lines
    // crossed (by their leading whitespace), checked once the owner is known.
    let mut nearest_content: Option<usize> = None;
    let mut crossed_blank = false;
    let mut min_blank_columns = usize::MAX;
    while cursor > 0 {
        let content_end = cursor - line_terminator_before(source, cursor).len();
        let start = line_start(bytes, content_end);
        let in_code = projection.line_in_code_block(start, content_end);
        // A marker-like line inside a code block is not an owner; only a
        // parser-confirmed item can open at a line the code block overlaps.
        if let Some(item) = projection.items.iter().find(|item| {
            item.prefix_range.start.0 == start && (item.is_parser_item || !in_code)
        }) {
            if !crossed_blank {
                return Some(item);
            }
            let inside_item = nearest_content.is_none_or(|content| content < item.item_range.end.0);
            let required = continuation_indent_required(source, item);
            return (inside_item && min_blank_columns >= required).then_some(item);
        }
        let text = &source[start..content_end];
        if in_code {
            // Lines of a closed code block do not end the item's body; the
            // parser keeps what follows the closing fence in the same item.
            nearest_content.get_or_insert(start);
            cursor = start;
            continue;
        }
        let trimmed = text.trim_start_matches([' ', '\t']);
        if starts_interrupting_block(trimmed) {
            return None;
        }
        if is_ascii_blank(text) {
            crossed_blank = true;
            min_blank_columns = min_blank_columns.min(columns(text));
        } else {
            // The lowest non-blank line is the one most likely to be outside
            // the item, so only the first one seen is kept.
            nearest_content.get_or_insert(start);
        }
        cursor = start;
    }
    None
}

/// Whether an empty item is confirmed as a real list item rather than a raw
/// lexical recovery candidate with no confirmed link to a real list (it may
/// just be ordinary text that happens to look like a marker). An
/// untrustworthy candidate is not a list-aware exit target at all: it falls
/// back to ordinary editing instead of being consumed as a no-op, so that
/// plain prose that merely looks like a marker still edits normally.
///
/// At the top level (`depth <= 1`) a `parent_item`/`previous_sibling` link
/// only proves a chain between raw recovery candidates, not that the formal
/// parser recognizes a real list here, so top-level exit requires
/// `is_parser_item` itself. Nested items keep the existing, looser check:
/// a confirmed parent or sibling link there is still anchored to a real
/// list structure, because depth > 1 cannot arise without one.
fn exit_is_trustworthy(item: &ListEditItem) -> bool {
    if item.depth <= 1 {
        return item.is_parser_item;
    }
    item.is_parser_item || item.parent_item.is_some() || item.previous_sibling.is_some()
}

/// Whether a trustworthy empty item is safe to collapse into its surrounding
/// paragraph or outdent. Protects two cases the design calls "do not touch",
/// at the depths where each actually applies:
///
/// - An item still inside a blockquote, at any depth: its opening line's
///   bytes include the quote marker, so a prefix edit here would also remove
///   it, and neither `exit_top_level_item` nor `transform_subtree` knows how
///   to edit around that marker.
/// - A *top-level* item whose subtree reaches past its own opening line
///   (nested content or a continuation paragraph): `exit_top_level_item`
///   only ever removes that one opening line, so any descendant past it
///   would be orphaned by the exit (per ADR-0029's top-level contract).
///
/// A *nested* item (`depth > 1`) is not held to that second rule: per
/// ADR-0029, exiting it always outdents the whole matched subtree by one
/// level through `transform_subtree`, which shifts every line of
/// `subtree_range` — descendants included — so they keep their relative
/// structure under the new, shallower item. There is nothing here for that
/// subtree to be orphaned from.
///
/// A protected item is still a confirmed list item, so the exit key must
/// still be consumed as a no-op rather than falling back to ordinary
/// editing, which could otherwise mutate the protected content.
fn exit_is_safe_to_collapse(source: &str, item: &ListEditItem) -> bool {
    if item.in_quote {
        return false;
    }
    if item.depth > 1 {
        return true;
    }
    item.subtree_range.end.0 <= line_end_inclusive(source.as_bytes(), item.prefix_range.start.0)
}

/// Removes a top-level empty item's marker and, if needed, inserts a real
/// blank-line boundary on each side of the exit point instead of relying on
/// a single shared line terminator.
///
/// Deleting only the marker (`item.prefix_range`) already leaves this line
/// blank, because the item's body is empty by construction, but whatever the
/// caller types next at the caret becomes an ordinary paragraph that a
/// single shared terminator cannot protect from either neighbor: without a
/// blank line of its own on the left, the typed text is a lazy continuation
/// of the previous block's paragraph; without one on the right, content that
/// cannot itself interrupt a paragraph without a blank line (an ordered item
/// not starting at 1, or plain prose) gets absorbed into it instead of
/// staying separate. So each side is decided independently, built from that
/// side's own existing end-of-line bytes, and only when real content sits
/// directly against it with no existing blank line already there — a side
/// with nothing (document start/end) or an already-blank neighbor (including
/// a line of only ASCII space/tab) needs no separator, to avoid inserting a
/// spurious extra blank line or duplicating an existing boundary. Everything
/// beyond this item's own line, including any following item's raw bytes,
/// is left untouched.
fn exit_top_level_item(source: &str, item: &ListEditItem) -> MarkdownEditPlan {
    let line_start = item.prefix_range.start.0;
    let line_text_end = item.prefix_range.end.0;
    let after_line = line_end_inclusive(source.as_bytes(), line_start);
    let has_previous = line_start > 0;
    let has_following = after_line < source.len();

    // Nothing but ASCII space/tab and line terminators after this item: the
    // exit is at the document tail, so only the marker is removed (#767).
    let is_trailing = source.as_bytes()[after_line..]
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'));

    let left_separator = if has_previous && !is_trailing && !previous_line_is_blank(source, line_start)
    {
        line_terminator_before(source, line_start)
    } else {
        ""
    };
    let right_separator = if has_following && !following_line_is_blank(source, after_line) {
        &source[line_text_end..after_line]
    } else {
        ""
    };

    let mut replacement = String::with_capacity(left_separator.len() + right_separator.len());
    replacement.push_str(left_separator);
    // When a right separator exists, the caret must land between the two
    // separators so that whatever is typed next lands inside the new
    // paragraph rather than inside either separator.
    let new_caret = SourceOffset(line_start + replacement.len());
    replacement.push_str(right_separator);

    MarkdownEditPlan::Replace {
        range: item.prefix_range,
        replacement,
        selection_after: SourceSelection::caret(new_caret),
    }
}

/// The terminator to insert when splitting the physical line containing `at`.
///
/// Prefers the line's own terminator (even when it lies after `at`). An
/// unterminated last line borrows the previous line's terminator, and a
/// source with no terminator to borrow falls back to `"\n"`. Only the choice
/// of terminator is decided here; prefixes, exit rules and caret placement
/// belong to the callers.
fn preferred_line_terminator(source: &str, at: usize) -> &str {
    let bytes = source.as_bytes();
    let at = at.min(source.len());
    let start = line_start(bytes, at);
    let end = line_end(bytes, start);
    let after = line_end_inclusive(bytes, start);
    if after > end {
        &source[end..after]
    } else if start > 0 {
        line_terminator_before(source, start)
    } else {
        "\n"
    }
}

/// The line terminator (`"\n"`, `"\r\n"`, or `"\r"`) ending the line
/// immediately before `line_start`. `line_start` must be greater than `0`.
fn line_terminator_before(source: &str, line_start: usize) -> &str {
    let bytes = source.as_bytes();
    if line_start >= 2 && bytes[line_start - 2] == b'\r' && bytes[line_start - 1] == b'\n' {
        &source[line_start - 2..line_start]
    } else {
        &source[line_start - 1..line_start]
    }
}

/// Whether the physical line immediately before `line_start` is blank, i.e.
/// empty or made only of ASCII space/tab (see [`is_ascii_blank`]). Determined
/// from the actual line content rather than counting terminators, so a line
/// of only whitespace still counts as an existing boundary. `line_start` must
/// be greater than `0`.
fn previous_line_is_blank(source: &str, item_line_start: usize) -> bool {
    let bytes = source.as_bytes();
    let terminator_len = if item_line_start >= 2
        && bytes[item_line_start - 2] == b'\r'
        && bytes[item_line_start - 1] == b'\n'
    {
        2
    } else {
        1
    };
    let content_end = item_line_start - terminator_len;
    let previous_start = line_start(bytes, content_end);
    is_ascii_blank(&source[previous_start..content_end])
}

/// Whether the physical line starting at `following_line_start` is blank,
/// symmetric to [`previous_line_is_blank`]. `following_line_start` must be
/// less than `source.len()`.
fn following_line_is_blank(source: &str, following_line_start: usize) -> bool {
    let content_end = line_end(source.as_bytes(), following_line_start);
    is_ascii_blank(&source[following_line_start..content_end])
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

    fn assert_edit(text: &str, caret: usize, intent: ListEditIntent, expected: &str, at: usize) {
        assert_eq!(
            replacement(text, caret, intent),
            (expected.to_owned(), at)
        );
    }

    fn reparsed_items(text: &str) -> Vec<ListEditItem> {
        let parsed = parse_document(Revision(1), SourceRange::new(0, text.len()), text);
        build_list_edit_projection(&parsed, text).items
    }

    #[test]
    fn documents_without_list_items_are_not_applicable() {
        let text = "plain paragraph\n\nanother line\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, text.len()), text);
        let projection = build_list_edit_projection(&parsed, text);
        assert!(projection.items.is_empty());
        for intent in [
            ListEditIntent::Enter,
            ListEditIntent::ShiftEnter,
            ListEditIntent::Backspace,
            ListEditIntent::Indent,
            ListEditIntent::Outdent,
        ] {
            let result = plan_list_edit(
                &RopeBuffer::from_text(text),
                &projection,
                SourceSelection::caret(SourceOffset(5)),
                intent,
            )
            .unwrap();
            assert!(matches!(result, ListEditPlanResult::NotApplicable));
        }
    }

    #[test]
    fn enter_left_of_marker_inserts_blank_line_above() {
        let enter = ListEditIntent::Enter;
        assert_edit("- abc", 0, enter, "\n- abc", 1);
        assert_edit("+ abc", 0, enter, "\n+ abc", 1);
        assert_edit("* abc", 0, enter, "\n* abc", 1);
        assert_edit("10. abc", 0, enter, "\n10. abc", 1);
        assert_edit("1) abc", 0, enter, "\n1) abc", 1);
        assert_edit("- [x] abc", 0, enter, "\n- [x] abc", 1);
        assert_edit("- [ ] abc", 0, enter, "\n- [ ] abc", 1);
    }

    #[test]
    fn enter_left_of_nested_marker_keeps_indent() {
        let text = "- parent\n  - child";
        assert_edit(text, 9, ListEditIntent::Enter, "- parent\n\n  - child", 10);
        assert_edit(text, 11, ListEditIntent::Enter, "- parent\n\n  - child", 12);
        let items = reparsed_items("- parent\n\n  - child");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].depth, 2);
    }

    #[test]
    fn enter_left_of_marker_preserves_crlf() {
        assert_edit(
            "- a\r\n- b",
            5,
            ListEditIntent::Enter,
            "- a\r\n\r\n- b",
            7,
        );
    }

    #[test]
    fn enter_inside_marker_or_task_prefix_is_ordinary_editing() {
        assert!(not_applicable("- abc", 1, ListEditIntent::Enter));
        assert!(not_applicable("10. abc", 2, ListEditIntent::Enter));
        assert!(not_applicable("- [x] abc", 3, ListEditIntent::Enter));
    }

    #[test]
    fn enter_in_quoted_item_prefix_is_ordinary_editing() {
        assert!(not_applicable("> - abc", 0, ListEditIntent::Enter));
    }

    #[test]
    fn enter_at_unindented_continuation_left_edge_leaves_the_list() {
        assert_edit("- abc\n", 6, ListEditIntent::Enter, "- abc\n\n", 7);
        assert_edit("- abc\ndef", 6, ListEditIntent::Enter, "- abc\n\ndef", 7);
        let items = reparsed_items("- abc\n\ndef");
        assert_eq!(items.len(), 1);
        assert!(items[0].item_range.end.0 <= 7);
    }

    #[test]
    fn enter_before_existing_indent_preserves_it() {
        assert_edit(
            "- abc\n  def",
            6,
            ListEditIntent::Enter,
            "- abc\n\n  def",
            7,
        );
        assert_edit(
            "- abc\n  def",
            7,
            ListEditIntent::Enter,
            "- abc\n\n  def",
            8,
        );
    }

    #[test]
    fn enter_on_indented_continuation_keeps_the_item() {
        let enter = ListEditIntent::Enter;
        assert_edit("- abc\n  def", 11, enter, "- abc\n  def\n  ", 14);
        assert_edit("- abc\n  def", 10, enter, "- abc\n  de\n  f", 13);
        assert_edit("- abc\n  def", 8, enter, "- abc\n  \n  def", 11);
        assert_edit("- abc\n  ", 8, enter, "- abc\n  \n  ", 11);
        let items = reparsed_items("- abc\n  def\n  ");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn repeated_enter_on_generated_blank_continuation_keeps_item_context() {
        let enter = ListEditIntent::Enter;
        let lf = "- abc\n  \n  ";
        assert_edit(lf, lf.len(), enter, "- abc\n  \n  \n  ", lf.len() + 3);
        let crlf = "- abc\r\n  \r\n  ";
        assert_edit(
            crlf,
            crlf.len(),
            enter,
            "- abc\r\n  \r\n  \r\n  ",
            crlf.len() + 4,
        );
        // The parser keeps the content after a real blank outside the item.
        assert!(not_applicable("- abc\n\n  def", 12, enter));
        assert!(not_applicable("- abc\n\n  def\n  ", 15, enter));
    }

    #[test]
    fn enter_left_of_unconfirmed_marker_like_line_is_ordinary_editing() {
        let enter = ListEditIntent::Enter;
        assert!(not_applicable("```\n  - code\n```", 5, enter));
        assert!(not_applicable("    - code", 0, enter));
        assert_edit("- a\n  - b", 4, enter, "- a\n\n  - b", 5);
    }

    #[test]
    fn enter_on_continuation_uses_the_real_indent_width() {
        let enter = ListEditIntent::Enter;
        assert_edit("10. abc\n    def", 15, enter, "10. abc\n    def\n    ", 20);
        assert_edit(
            "- parent\n  - abc\n    def",
            24,
            enter,
            "- parent\n  - abc\n    def\n    ",
            29,
        );
        assert_edit("- [x] abc\n  def", 15, enter, "- [x] abc\n  def\n  ", 18);
        assert_edit("- abc\n\tdef", 10, enter, "- abc\n\tdef\n\t", 12);
    }

    #[test]
    fn enter_on_third_continuation_line_keeps_the_item() {
        assert_edit(
            "- abc\n  def\n  ghi",
            17,
            ListEditIntent::Enter,
            "- abc\n  def\n  ghi\n  ",
            20,
        );
    }

    #[test]
    fn enter_on_continuation_preserves_line_endings() {
        assert_edit(
            "- abc\r\n  def\r\n",
            12,
            ListEditIntent::Enter,
            "- abc\r\n  def\r\n  \r\n",
            16,
        );
        assert_edit("- abc\r\n", 7, ListEditIntent::Enter, "- abc\r\n\r\n", 9);
        assert_edit(
            "- abc\r\n  def\n",
            12,
            ListEditIntent::Enter,
            "- abc\r\n  def\n  \n",
            15,
        );
    }

    #[test]
    fn enter_on_continuation_uses_utf8_byte_offsets() {
        assert_edit(
            "- 日本\n  語🙂",
            "- 日本\n  語🙂".len(),
            ListEditIntent::Enter,
            "- 日本\n  語🙂\n  ",
            "- 日本\n  語🙂\n  ".len(),
        );
    }

    #[test]
    fn shift_enter_on_continuation_keeps_indent() {
        assert_edit(
            "- abc\n  def",
            11,
            ListEditIntent::ShiftEnter,
            "- abc\n  def\n  ",
            14,
        );
        assert_edit("- abc", 0, ListEditIntent::ShiftEnter, "\n- abc", 1);
        assert!(not_applicable("- abc", 1, ListEditIntent::ShiftEnter));
    }

    #[test]
    fn enter_on_explicit_marker_creates_a_sibling() {
        let enter = ListEditIntent::Enter;
        assert_edit("- abc\n- def", 11, enter, "- abc\n- def\n- ", 14);
        assert_edit("- abc\n+ def", 11, enter, "- abc\n+ def\n+ ", 14);
        assert_edit("1. abc\n2. def", 13, enter, "1. abc\n2. def\n3. ", 17);
        assert_edit("- abc\n- ", 8, enter, "- abc\n", 6);
        assert_edit("- abc", 4, enter, "- ab\n- c", 7);
    }

    #[test]
    fn enter_in_fence_quote_or_marker_like_text_is_ordinary_editing() {
        assert!(not_applicable("- abc\n  ```\n  code", 18, ListEditIntent::Enter));
        assert!(not_applicable("> abc\n> def", 11, ListEditIntent::Enter));
        assert!(not_applicable("abc\ndef", 7, ListEditIntent::Enter));
        assert!(not_applicable("abc\n\ndef", 0, ListEditIntent::Enter));
        // Blank line separates the continuation from the item.
        assert!(not_applicable("- abc\n\n  def", 12, ListEditIntent::Enter));
        // Under-indented line is not a recognised continuation.
        assert!(not_applicable("- abc\n def", 10, ListEditIntent::Enter));
    }

    #[test]
    fn enter_inside_fenced_code_body_is_ordinary_editing() {
        let enter = ListEditIntent::Enter;
        assert!(not_applicable("- ~~~\n  ab\n  ~~~", 7, enter));
        assert!(not_applicable("- ```\n  ab\n  ```", 7, enter));
        assert!(not_applicable("- abc\n  ~~~\n  code\n  ~~~", 14, enter));
        assert!(not_applicable("- abc\n  ```\n  code\n  ```", 14, enter));
    }

    #[test]
    fn enter_on_paragraph_after_closed_fence_keeps_the_item() {
        let enter = ListEditIntent::Enter;
        assert_edit(
            "- abc\n  ~~~\n  code\n  ~~~\n  def",
            30,
            enter,
            "- abc\n  ~~~\n  code\n  ~~~\n  def\n  ",
            33,
        );
        assert_edit(
            "- abc\n  ```\n  code\n  ```\n  def",
            30,
            enter,
            "- abc\n  ```\n  code\n  ```\n  def\n  ",
            33,
        );
    }

    #[test]
    fn preferred_line_terminator_selects_own_then_previous_then_lf() {
        assert_eq!(preferred_line_terminator("", 0), "\n");
        assert_eq!(preferred_line_terminator("abc", 3), "\n");
        assert_eq!(preferred_line_terminator("abc\r\n", 1), "\r\n");
        assert_eq!(preferred_line_terminator("abc\n", 3), "\n");
        assert_eq!(preferred_line_terminator("abc\r", 3), "\r");
        assert_eq!(preferred_line_terminator("a\r\nbc", 5), "\r\n");
        assert_eq!(preferred_line_terminator("a\nbc", 4), "\n");
        assert_eq!(preferred_line_terminator("a\rbc", 4), "\r");
        // The previous line wins over the document head for an unterminated
        // last line.
        assert_eq!(preferred_line_terminator("a\r\nb\nc", 6), "\n");
        // The own terminator wins over the previous line's.
        assert_eq!(preferred_line_terminator("a\r\nb\n", 4), "\n");
    }

    #[test]
    fn enter_continuation_preserves_crlf() {
        assert_edit("- abc\r\n", 5, ListEditIntent::Enter, "- abc\r\n- \r\n", 9);
    }

    #[test]
    fn enter_split_mid_body_preserves_crlf() {
        assert_edit(
            "- abcd\r\n",
            4,
            ListEditIntent::Enter,
            "- ab\r\n- cd\r\n",
            8,
        );
    }

    #[test]
    fn enter_continuation_preserves_lf() {
        assert_edit("- abc\n", 5, ListEditIntent::Enter, "- abc\n- \n", 8);
    }

    #[test]
    fn enter_continuation_preserves_bare_cr() {
        assert_edit("- abc\r", 5, ListEditIntent::Enter, "- abc\r- \r", 8);
    }

    #[test]
    fn enter_continuation_at_unterminated_eof_inherits_crlf() {
        assert_edit(
            "- A\r\n- bc",
            9,
            ListEditIntent::Enter,
            "- A\r\n- bc\r\n- ",
            13,
        );
    }

    #[test]
    fn enter_continuation_without_any_terminator_defaults_to_lf() {
        assert_edit("- abc", 5, ListEditIntent::Enter, "- abc\n- ", 8);
    }

    #[test]
    fn enter_mixed_endings_uses_current_lf() {
        assert_edit(
            "- A\r\n- bc\n",
            8,
            ListEditIntent::Enter,
            "- A\r\n- b\n- c\n",
            11,
        );
    }

    #[test]
    fn enter_mixed_endings_uses_current_crlf() {
        assert_edit(
            "- A\n- bc\r\n",
            7,
            ListEditIntent::Enter,
            "- A\n- b\r\n- c\r\n",
            11,
        );
    }

    #[test]
    fn shift_enter_preserves_crlf() {
        assert_edit(
            "- abc\r\n",
            5,
            ListEditIntent::ShiftEnter,
            "- abc\r\n  \r\n",
            9,
        );
    }

    #[test]
    fn shift_enter_mixed_endings_uses_current_lf() {
        assert_edit(
            "- A\r\n- bc\n",
            8,
            ListEditIntent::ShiftEnter,
            "- A\r\n- b\n  c\n",
            11,
        );
    }

    #[test]
    fn shift_enter_at_unterminated_eof_inherits_crlf() {
        assert_edit(
            "- A\r\n- bc",
            9,
            ListEditIntent::ShiftEnter,
            "- A\r\n- bc\r\n  ",
            13,
        );
    }

    #[test]
    fn ordered_enter_preserves_crlf_and_next_number() {
        assert_edit(
            "3. A\r\n4. B",
            4,
            ListEditIntent::Enter,
            "3. A\r\n4. \r\n4. B",
            9,
        );
    }

    #[test]
    fn task_enter_preserves_crlf_and_unchecks_marker() {
        assert_edit(
            "- [x] A\r\n",
            7,
            ListEditIntent::Enter,
            "- [x] A\r\n- [ ] \r\n",
            15,
        );
    }

    #[test]
    fn enter_crlf_uses_utf8_byte_offsets() {
        assert_edit(
            "- 日本\r\n",
            8,
            ListEditIntent::Enter,
            "- 日本\r\n- \r\n",
            12,
        );
    }

    #[test]
    fn enter_crlf_continuation_then_exit_preserves_boundaries() {
        let (continued, caret) = replacement("- abc\r\n", 5, ListEditIntent::Enter);
        assert_eq!(continued, "- abc\r\n- \r\n");
        assert_eq!(caret, 9);

        let (exited, caret) = replacement(&continued, caret, ListEditIntent::Enter);
        // 末尾の離脱では区切りを足さないため、直後に入力した文字は
        // 直前の項目の lazy continuation になり得る（独立段落は保証しない）。
        assert_eq!(exited, "- abc\r\n\r\n");
        assert_eq!(caret, 7);

        let mut typed = exited;
        typed.insert(caret, 'x');
        assert_eq!(typed, "- abc\r\nx\r\n");
    }

    #[test]
    fn enter_exit_unterminated_empty_item_preserves_crlf() {
        assert_edit(
            "- abc\r\n- ",
            9,
            ListEditIntent::Enter,
            "- abc\r\n",
            7,
        );
    }

    #[test]
    fn enter_exit_only_empty_crlf_item_preserves_terminator() {
        assert_edit("- \r\n", 2, ListEditIntent::Enter, "\r\n", 0);
    }

    #[test]
    fn enter_exit_mixed_endings_preserves_each_side() {
        assert_edit(
            "3. A\r\n4. \n5. C",
            9,
            ListEditIntent::Enter,
            "3. A\r\n\r\n\n\n5. C",
            8,
        );
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

    /// PR #472 review: a descendant item's own task marker (`[ ]` on
    /// `  - [ ] child`) must not be mistaken for the parent item's marker.
    /// Before the fix, `find_task_marker` recursed into the child subtree and
    /// clamped the parent's body to that child marker's line, leaving the
    /// parent's body range empty even though "A" is real, non-empty content.
    /// That falsely triggered the protected empty-item exit path and absorbed
    /// ordinary edits at the parent's own body-start caret.
    #[test]
    fn parent_task_marker_is_not_confused_with_descendant_task_marker() {
        let text = "- A\n  - [ ] child";
        let source_range = SourceRange::new(0, text.len());
        let parsed = parse_document(Revision(1), source_range, text);
        let projection = build_list_edit_projection(&parsed, text);

        let (_, parent) = projection
            .item_at(SourceOffset(2))
            .expect("expected an item at \"A\"");
        assert_eq!(parent.task, None, "parent item must not be a task item");
        assert_eq!(
            &text[parent.body_range.as_usize()],
            "A",
            "parent body must stay non-empty"
        );

        let (_, child) = projection
            .item_at(SourceOffset(text.find("child").unwrap()))
            .expect("expected an item at \"child\"");
        assert_eq!(child.task, Some(false), "child item must keep its own task marker");

        // Caret byte 3 sits immediately after "A", at the parent's
        // body-start/body-end boundary. Enter must proceed as an ordinary
        // same-level split (not the empty-item exit no-op), and Backspace
        // must fall back to ordinary deletion instead of being absorbed.
        assert_eq!(
            replacement(text, 3, ListEditIntent::Enter),
            ("- A\n- \n  - [ ] child".into(), 6)
        );
        assert!(not_applicable(text, 3, ListEditIntent::Backspace));
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
            replacement("- A\n  - ", 8, ListEditIntent::Backspace),
            ("- A\n- ".into(), 6)
        );
        assert_eq!(
            replacement("- ", 2, ListEditIntent::Backspace),
            ("".into(), 0)
        );
    }

    /// Issue #472: the empty-item exit is only eligible when the caret sits
    /// exactly at the empty body's start. A caret inside the marker prefix
    /// (`-<caret> `) must not be consumed by the list-exit planner for
    /// either intent, so ordinary editing proceeds instead.
    #[test]
    fn empty_item_exit_requires_exact_body_start_caret() {
        assert!(not_applicable("- ", 1, ListEditIntent::Enter));
        assert!(not_applicable("- ", 1, ListEditIntent::Backspace));
    }

    /// Control case for the above: the exact body-start caret (`- <caret>`)
    /// still exits the empty item for both intents.
    #[test]
    fn empty_item_exit_still_applies_at_exact_body_start_caret() {
        assert_eq!(replacement("- ", 2, ListEditIntent::Enter), ("".into(), 0));
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

    /// Issue #767 / E01: exiting the trailing empty top-level item only removes
    /// the marker; no separator line is inserted at the document tail.
    #[test]
    fn backspace_on_trailing_empty_top_level_item_removes_only_the_marker() {
        let text = "- abc\n- def\n- ";
        let (after_exit, caret) = replacement(text, text.len(), ListEditIntent::Backspace);
        assert_eq!(after_exit, "- abc\n- def\n");
        assert_eq!(caret, after_exit.len());
    }

    /// Issue #767: trailing exit cases (document tail, ASCII blank-only tail)
    /// for both Enter and Backspace, with LF / CRLF / ordered markers.
    #[test]
    fn trailing_exit_removes_only_the_marker_without_separator() {
        let cases: &[(&str, usize, &str, usize)] = &[
            ("- abc\n- ", 8, "- abc\n", 6),
            ("- abc\n- \n", 8, "- abc\n\n", 6),
            ("- abc\r\n- ", 9, "- abc\r\n", 7),
            ("- abc\r\n- \r\n", 9, "- abc\r\n\r\n", 7),
            ("3. A\n4. ", 8, "3. A\n", 5),
            ("3. A\n4. \n", 8, "3. A\n\n", 5),
            ("- abc\n- \n\n \t", 8, "- abc\n\n\n \t", 6),
        ];
        for &(text, caret, expected, expected_caret) in cases {
            for intent in [ListEditIntent::Backspace, ListEditIntent::Enter] {
                assert_eq!(
                    replacement(text, caret, intent),
                    (expected.to_string(), expected_caret),
                    "{text:?} {intent:?}"
                );
            }
        }
    }

    /// Issue #767: a nested trailing empty item still just outdents.
    #[test]
    fn nested_trailing_empty_item_still_outdents() {
        for intent in [ListEditIntent::Backspace, ListEditIntent::Enter] {
            assert_eq!(
                replacement("- A\n  - B\n  - ", 14, intent),
                ("- A\n  - B\n- ".to_string(), 12),
                "{intent:?}"
            );
        }
    }

    /// Issue #472: `pulldown_cmark` must look ahead past a trailing blank
    /// line to confirm a list really ends when the next line is unindented,
    /// non-list prose. That lookahead previously left the blank line
    /// attributed to the last (empty) item's own reported range, which made
    /// `exit_is_safe_to_collapse` think the item owned real content past its
    /// opening line and refuse the exit as a no-op — leaving the marker
    /// untouched instead of removing it. An existing blank line already
    /// following the exited item must still be reused, not duplicated.
    #[test]
    fn backspace_exits_trailing_empty_item_already_followed_by_a_blank_line_and_prose() {
        let text = "- abc\n- def\n- \n\nx";
        let (after_exit, caret) = replacement(text, 14, ListEditIntent::Backspace);
        assert_eq!(after_exit, "- abc\n- def\n\n\n\nx");
        assert_eq!(caret, 13);

        let mut typed = after_exit;
        typed.insert_str(caret, "ghi");
        assert_eq!(typed, "- abc\n- def\n\nghi\n\nx");

        let parsed = parse_document(Revision(1), SourceRange::new(0, typed.len()), &typed);
        let paragraphs: Vec<_> = parsed
            .tree
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Paragraph))
            .collect();
        assert_eq!(
            paragraphs.len(),
            2,
            "expected \"ghi\" and \"x\" to be two distinct paragraphs, got {paragraphs:?}"
        );
        for paragraph in &paragraphs {
            assert!(
                !parsed
                    .tree
                    .ancestors(paragraph.0)
                    .skip(1)
                    .any(|ancestor| matches!(
                        parsed.tree.node(ancestor).map(|node| node.kind),
                        Some(NodeKind::ListItem { .. })
                    )),
                "paragraph {paragraph:?} must not be a descendant of a ListItem"
            );
        }
        assert!(
            typed[paragraphs[1].1.source_range.as_usize()].contains('x'),
            "expected the trailing paragraph to still contain \"x\", got {paragraphs:?}"
        );
    }

    #[test]
    fn enter_on_trailing_empty_top_level_item_removes_only_the_marker() {
        let text = "- abc\n- def\n- ";
        assert_eq!(
            replacement(text, text.len(), ListEditIntent::Enter),
            ("- abc\n- def\n".into(), 12)
        );
    }

    #[test]
    fn trailing_empty_top_level_exit_preserves_crlf() {
        let text = "- abc\r\n- def\r\n- ";
        assert_eq!(
            replacement(text, text.len(), ListEditIntent::Backspace),
            ("- abc\r\n- def\r\n".into(), 14)
        );
    }

    /// Issue #460 / E03: when a trailing empty top-level item is preceded by
    /// an already-blank line, that existing blank line already satisfies the
    /// paragraph boundary, so exiting must not insert a second one.
    #[test]
    fn trailing_empty_top_level_exit_after_existing_blank_line_adds_no_extra_blank_line() {
        let text = "- abc\n- def\n\n- ";
        assert_eq!(
            replacement(text, text.len(), ListEditIntent::Backspace),
            ("- abc\n- def\n\n".into(), 13)
        );
        assert_eq!(
            replacement(text, text.len(), ListEditIntent::Enter),
            ("- abc\n- def\n\n".into(), 13)
        );
    }

    /// Issue #460: a preceding line made only of ASCII whitespace (here
    /// three spaces) is still a blank line for boundary purposes, determined
    /// per actual line content rather than by counting line terminators.
    /// Extra coverage alongside the matrix's E04 row, which uses a
    /// space-then-tab blank line instead.
    #[test]
    fn trailing_empty_top_level_exit_after_whitespace_only_blank_line_adds_no_extra_blank_line() {
        let text = "- abc\n- def\n   \n- ";
        assert_eq!(
            replacement(text, text.len(), ListEditIntent::Backspace),
            ("- abc\n- def\n   \n".into(), 16)
        );
    }

    /// Issue #460 / E06: an empty item sitting directly between two other
    /// items (no existing blank line on either side) needs a genuine blank
    /// line on *both* sides of the exit point, not just the single shared
    /// line terminator that a plain deletion would leave. Otherwise typed
    /// text at the caret would be a lazy continuation of `A`'s paragraph on
    /// the left, merging it back into the previous item.
    #[test]
    fn empty_top_level_item_between_siblings_gets_blank_lines_on_both_sides() {
        let (after_exit, caret) = replacement("- A\n- \n- C", 6, ListEditIntent::Backspace);
        assert_eq!(after_exit, "- A\n\n\n\n- C");
        assert_eq!(caret, 5);

        let mut typed = after_exit;
        typed.insert_str(caret, "ghi");
        assert_eq!(typed, "- A\n\nghi\n\n- C");
    }

    /// Issue #460 / E07: an ordered item whose exit is immediately followed
    /// by another ordered item cannot rely on the shared line terminator
    /// either: an ordered marker that does not start at 1 cannot interrupt a
    /// paragraph without a blank line, so without a right separator typed
    /// text would absorb the following item into its own paragraph instead
    /// of leaving it as a list.
    #[test]
    fn empty_top_level_item_between_ordered_siblings_gets_blank_lines_on_both_sides() {
        let text = "3. A\n4. \n5. C";
        let (after_exit, caret) = replacement(text, 8, ListEditIntent::Backspace);
        assert_eq!(after_exit, "3. A\n\n\n\n5. C");
        assert_eq!(caret, 6);

        let mut typed = after_exit;
        typed.insert_str(caret, "ghi");
        assert_eq!(typed, "3. A\n\nghi\n\n5. C");

        let parsed = parse_document(Revision(1), SourceRange::new(0, typed.len()), &typed);
        let list_items: Vec<_> = parsed
            .tree
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .collect();
        assert_eq!(
            list_items.len(),
            2,
            "expected \"3. A\" and \"5. C\" to remain two separate list items, got {list_items:?}"
        );
        let last_item_text = &typed[list_items[1].1.source_range.as_usize()];
        assert!(
            last_item_text.contains('C'),
            "expected the trailing list item to still contain \"C\", got {last_item_text:?}"
        );
        let paragraph = parsed
            .tree
            .iter()
            .find(|(_, node)| matches!(node.kind, NodeKind::Paragraph))
            .expect("expected a paragraph for \"ghi\"");
        assert!(
            !parsed
                .tree
                .ancestors(paragraph.0)
                .skip(1)
                .any(|ancestor| matches!(
                    parsed.tree.node(ancestor).map(|node| node.kind),
                    Some(NodeKind::ListItem { .. })
                )),
            "the paragraph for \"ghi\" must not be a descendant of a ListItem"
        );
    }

    /// Existing blank lines are never duplicated: when the line before the
    /// exit point is already blank, only the right side still needs its own
    /// separator.
    #[test]
    fn existing_multiple_blank_lines_before_exit_are_not_collapsed() {
        assert_eq!(
            replacement("- A\n\n- \n- C", 7, ListEditIntent::Backspace),
            ("- A\n\n\n\n- C".into(), 5)
        );
    }

    /// Symmetric to the previous case: when the line after the exit point
    /// is already blank, only the left side still needs its own separator.
    #[test]
    fn existing_multiple_blank_lines_after_exit_are_not_collapsed() {
        assert_eq!(
            replacement("- A\n- \n\n- C", 6, ListEditIntent::Backspace),
            ("- A\n\n\n\n- C".into(), 5)
        );
    }

    /// Issue #460 / E15: plain prose (not a list marker at all) directly
    /// after the exited item is just as vulnerable to absorption as a
    /// restarting ordered item, since a bare text line can never interrupt a
    /// paragraph without a blank line.
    #[test]
    fn empty_top_level_item_before_plain_prose_gets_a_right_separator() {
        let text = "- A\n- \nplain text";
        let (after_exit, caret) = replacement(text, 6, ListEditIntent::Backspace);
        assert_eq!(after_exit, "- A\n\n\n\nplain text");
        assert_eq!(caret, 5);

        let mut typed = after_exit;
        typed.insert_str(caret, "ghi");
        assert_eq!(typed, "- A\n\nghi\n\nplain text");

        let parsed = parse_document(Revision(1), SourceRange::new(0, typed.len()), &typed);
        let paragraphs: Vec<_> = parsed
            .tree
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Paragraph))
            .collect();
        assert_eq!(
            paragraphs.len(),
            2,
            "expected \"ghi\" and \"plain text\" to be two distinct paragraphs, got {paragraphs:?}"
        );
    }

    /// An item with nothing before it only ever needs a right separator: a
    /// document cannot have a line "before" its own first line.
    #[test]
    fn empty_top_level_item_at_document_start_only_gets_a_right_separator() {
        assert_eq!(
            replacement("- \n5. C", 2, ListEditIntent::Backspace),
            ("\n\n5. C".into(), 0)
        );
    }

    /// Mixed CRLF/LF line endings: each separator is built from its own
    /// side's existing end-of-line bytes, not a hardcoded style.
    #[test]
    fn mixed_line_endings_build_each_separator_from_its_own_side() {
        let text = "3. A\r\n4. \n5. C";
        assert_eq!(
            replacement(text, 9, ListEditIntent::Backspace),
            ("3. A\r\n\r\n\n\n5. C".into(), 8)
        );
    }

    /// Issue #460: offsets into multi-byte UTF-8 content stay in byte
    /// positions, not char positions, on both sides of the exit point. Extra
    /// coverage alongside the matrix's E13 row, which uses `日本🙂` instead.
    #[test]
    fn utf8_content_on_both_sides_uses_byte_offsets() {
        let text = "- \u{3042}\n- \n- \u{3044}";
        let item_byte_len = "- \u{3042}\n".len();
        let caret = item_byte_len + "- ".len();
        let (after_exit, new_caret) = replacement(text, caret, ListEditIntent::Backspace);
        assert_eq!(after_exit, "- \u{3042}\n\n\n\n- \u{3044}");
        assert_eq!(new_caret, item_byte_len + 1);
    }

    fn not_applicable(text: &str, caret: usize, intent: ListEditIntent) -> bool {
        let source_range = SourceRange::new(0, text.len());
        let parsed = parse_document(Revision(1), source_range, text);
        let projection = build_list_edit_projection(&parsed, text);
        matches!(
            plan_list_edit(
                &RopeBuffer::from_text(text),
                &projection,
                SourceSelection::caret(SourceOffset(caret)),
                intent,
            )
            .unwrap(),
            ListEditPlanResult::NotApplicable
        )
    }

    /// Asserts the plan consumes the key as a no-op that leaves the given
    /// caret untouched, rather than falling back to ordinary editing.
    fn no_op_keeps_caret(text: &str, caret: usize, intent: ListEditIntent) -> bool {
        let source_range = SourceRange::new(0, text.len());
        let parsed = parse_document(Revision(1), source_range, text);
        let projection = build_list_edit_projection(&parsed, text);
        let selection = SourceSelection::caret(SourceOffset(caret));
        matches!(
            plan_list_edit(&RopeBuffer::from_text(text), &projection, selection, intent).unwrap(),
            ListEditPlanResult::Handled(MarkdownEditPlan::NoOp { selection_after })
                if selection_after == selection
        )
    }

    /// An item whose opening line is blank but which owns descendant content
    /// (a nested sublist here) is not "empty": removing its marker would
    /// orphan that content. It is still a confirmed list item, so the key is
    /// consumed as a no-op instead of falling back to ordinary editing,
    /// which would otherwise mutate the parent item's protected content.
    #[test]
    fn empty_opening_line_with_nested_content_is_an_exit_no_op() {
        let text = "- \n  - child";
        assert!(no_op_keeps_caret(text, 2, ListEditIntent::Backspace));
        assert!(no_op_keeps_caret(text, 2, ListEditIntent::Enter));
    }

    /// Issue #472: unlike a top-level item, a *nested* empty item that owns
    /// descendants is not protected by `exit_is_safe_to_collapse`'s
    /// opening-line-only guard, because its exit always goes through
    /// `transform_subtree`, which outdents the whole matched subtree —
    /// descendants included — by one level instead of removing only the
    /// opening line. Both Enter and Backspace must therefore outdent the
    /// nested empty item together with its own nested child, keeping the
    /// child's relative structure as a descendant of the now-shallower item.
    #[test]
    fn empty_nested_item_with_descendant_outdents_whole_subtree() {
        let text = "- A\n  - \n    - c";
        let caret = 8;
        for intent in [ListEditIntent::Enter, ListEditIntent::Backspace] {
            let (after_exit, new_caret) = replacement(text, caret, intent);
            assert_eq!(
                after_exit, "- A\n- \n  - c",
                "{intent:?}: unexpected replacement"
            );
            assert_eq!(new_caret, 6, "{intent:?}: unexpected caret");

            let parsed = parse_document(
                Revision(1),
                SourceRange::new(0, after_exit.len()),
                &after_exit,
            );
            // Ancestor ListItems' ranges span their descendants, so every
            // ListItem enclosing "c" matches this filter; pick the one with
            // the smallest range, i.e. the innermost (deepest) item itself.
            let child_item = parsed
                .tree
                .iter()
                .filter(|(_, node)| {
                    matches!(node.kind, NodeKind::ListItem { .. })
                        && after_exit[node.source_range.as_usize()].contains('c')
                })
                .min_by_key(|(_, node)| node.source_range.len_bytes())
                .map(|(id, _)| id)
                .unwrap_or_else(|| {
                    panic!("{intent:?}: expected a ListItem containing \"c\" in {after_exit:?}")
                });
            assert_eq!(
                parsed.tree.list_depth(child_item),
                2,
                "{intent:?}: expected \"c\" to stay a depth-2 nested item in {after_exit:?}"
            );
            let parent_item = parsed
                .tree
                .ancestors(child_item)
                .skip(1)
                .find(|ancestor| {
                    matches!(
                        parsed.tree.node(*ancestor).map(|node| node.kind),
                        Some(NodeKind::ListItem { .. })
                    )
                })
                .unwrap_or_else(|| {
                    panic!("{intent:?}: expected \"c\" to have a ListItem ancestor in {after_exit:?}")
                });
            assert_eq!(
                parsed.tree.list_depth(parent_item),
                1,
                "{intent:?}: expected the emptied former-nested item to now be depth-1 in {after_exit:?}"
            );
            let (_, parent_ordinal) = parsed
                .tree
                .list_item_ordinal(parent_item)
                .unwrap_or_else(|| panic!("{intent:?}: expected the parent item to have a list owner"));
            assert_eq!(
                parent_ordinal, 1,
                "{intent:?}: expected the emptied item to remain \"A\"'s next sibling in {after_exit:?}"
            );
        }
    }

    /// Same contract as `empty_nested_item_with_descendant_outdents_whole_subtree`,
    /// but with bare CR line endings (no LF at all) throughout the source.
    /// `transform_subtree` previously split physical lines with
    /// `str::split_inclusive('\n')`, which treats a CR-only source as a
    /// single unsplit line, so only the subtree's first line was shifted and
    /// the nested child's own indentation was left untouched. Every `\r` here
    /// is one byte, so all offsets match the LF case exactly.
    #[test]
    fn empty_nested_item_with_descendant_outdents_whole_subtree_with_cr_line_endings() {
        let text = "- A\r  - \r    - c";
        let caret = 8;
        for intent in [ListEditIntent::Enter, ListEditIntent::Backspace] {
            let (after_exit, new_caret) = replacement(text, caret, intent);
            assert_eq!(
                after_exit, "- A\r- \r  - c",
                "{intent:?}: unexpected replacement"
            );
            assert_eq!(new_caret, 6, "{intent:?}: unexpected caret");

            let parsed = parse_document(
                Revision(1),
                SourceRange::new(0, after_exit.len()),
                &after_exit,
            );
            let child_item = parsed
                .tree
                .iter()
                .filter(|(_, node)| {
                    matches!(node.kind, NodeKind::ListItem { .. })
                        && after_exit[node.source_range.as_usize()].contains('c')
                })
                .min_by_key(|(_, node)| node.source_range.len_bytes())
                .map(|(id, _)| id)
                .unwrap_or_else(|| {
                    panic!("{intent:?}: expected a ListItem containing \"c\" in {after_exit:?}")
                });
            assert_eq!(
                parsed.tree.list_depth(child_item),
                2,
                "{intent:?}: expected \"c\" to stay a depth-2 nested item in {after_exit:?}"
            );
            let parent_item = parsed
                .tree
                .ancestors(child_item)
                .skip(1)
                .find(|ancestor| {
                    matches!(
                        parsed.tree.node(*ancestor).map(|node| node.kind),
                        Some(NodeKind::ListItem { .. })
                    )
                })
                .unwrap_or_else(|| {
                    panic!("{intent:?}: expected \"c\" to have a ListItem ancestor in {after_exit:?}")
                });
            assert_eq!(
                parsed.tree.list_depth(parent_item),
                1,
                "{intent:?}: expected the emptied former-nested item to now be depth-1 in {after_exit:?}"
            );
            let (_, parent_ordinal) = parsed
                .tree
                .list_item_ordinal(parent_item)
                .unwrap_or_else(|| panic!("{intent:?}: expected the parent item to have a list owner"));
            assert_eq!(
                parent_ordinal, 1,
                "{intent:?}: expected the emptied item to remain \"A\"'s next sibling in {after_exit:?}"
            );
        }
    }

    /// An empty item still inside a blockquote is left alone: its opening
    /// line's bytes include the quote marker (`> `), and this exit operation
    /// does not know how to edit around it without also removing that
    /// marker. It is still a confirmed list item, so the key is consumed as
    /// a no-op instead of falling back to ordinary editing, which would
    /// otherwise remove the quote marker along with the list marker.
    #[test]
    fn empty_item_inside_quote_is_an_exit_no_op() {
        let text = "> - A\n> - \n";
        assert!(no_op_keeps_caret(text, 10, ListEditIntent::Backspace));
    }

    /// A bare `- ` right after a plain paragraph, with no blank line between
    /// them, cannot interrupt that paragraph per CommonMark, so it is not a
    /// real list item — it is ordinary continuation text that only looks
    /// like a marker. The lexical recovery fallback still records a
    /// candidate for it (so Tab/Enter metadata stays available), but the
    /// exit operation must not treat ordinary prose as a list to collapse.
    #[test]
    fn isolated_raw_marker_after_a_plain_paragraph_is_not_an_exit_target() {
        let text = "para\n- ";
        assert!(not_applicable(text, text.len(), ListEditIntent::Backspace));
    }

    /// Chaining two raw recovery candidates at the same top-level indent
    /// inside an unterminated fenced code block (which CommonMark extends to
    /// the end of the document) gives the second candidate a
    /// `previous_sibling` link to the first, purely from lexical chaining
    /// over the raw lines; the formal parser recognizes neither as a list
    /// item, since both are swallowed as code block content. This must not
    /// be trusted as a top-level exit target just because a sibling link
    /// exists; it falls back to ordinary editing instead.
    #[test]
    fn raw_recovery_previous_sibling_at_top_level_is_not_an_exit_target() {
        let text = "```\n- A\n- ";
        assert!(not_applicable(text, text.len(), ListEditIntent::Backspace));
        assert!(not_applicable(text, text.len(), ListEditIntent::Enter));
    }

    /// A body made of non-ASCII whitespace (here U+3000 IDEOGRAPHIC SPACE) is
    /// real content, not an empty item, even though it looks blank.
    #[test]
    fn non_ascii_whitespace_body_is_not_an_exit_target() {
        let text = "- A\n- \u{3000}";
        assert!(not_applicable(text, text.len(), ListEditIntent::Backspace));
    }

    /// Extra structural proof required for a handful of design-spec §8 exit
    /// cases, beyond the shared range/replacement/caret contract and the
    /// `ghi`-stays-outside-any-`ListItem` check every case gets.
    #[derive(Clone, Copy)]
    enum ExitContractExtra {
        None,
        /// The named items, in source order, must still exist as `ListItem`s
        /// and each must still contain its given text.
        RetainsListItems(&'static [&'static str]),
        /// Plain prose immediately after the exited item must stay its own
        /// `Paragraph`, distinct from the one holding the typed `ghi`.
        FollowingProseStaysSeparateParagraph,
    }

    /// Issue #460 design spec §8: an empty top-level item's Enter and
    /// Backspace share one exit contract (`plan_list_edit` reaches the same
    /// branch for both intents before dispatching on `empty`/`on_opening_line`).
    /// Each row fixes the exact source range, replacement bytes, and
    /// post-edit caret the planner must produce for both intents, across the
    /// design's full E01-E16 example matrix.
    struct ExitContractCase {
        label: &'static str,
        text: &'static str,
        caret: usize,
        range: (usize, usize),
        replacement: &'static str,
        caret_after: usize,
        extra: ExitContractExtra,
    }

    #[test]
    fn empty_top_level_exit_contract_matrix() {
        let cases = [
            // E01: the basic case, two preceding sibling items and nothing
            // following; both must still be ListItems after the exit.
            ExitContractCase {
                label: "E01 basic top-level exit",
                text: "- abc\n- def\n- ",
                caret: 14,
                range: (12, 14),
                replacement: "",
                caret_after: 12,
                extra: ExitContractExtra::RetainsListItems(&["abc", "def"]),
            },
            // E02: the item is the entire document, so there is no line
            // before or after it and neither separator is needed.
            ExitContractCase {
                label: "E02 document start and end",
                text: "- ",
                caret: 2,
                range: (0, 2),
                replacement: "",
                caret_after: 0,
                extra: ExitContractExtra::None,
            },
            // E03: an existing blank line before the exit already satisfies
            // the left boundary, so no extra separator is added.
            ExitContractCase {
                label: "E03 existing blank line before",
                text: "- abc\n- def\n\n- ",
                caret: 15,
                range: (13, 15),
                replacement: "",
                caret_after: 13,
                extra: ExitContractExtra::None,
            },
            // E04: a whitespace-only line (a space followed by a tab) still
            // counts as an existing blank line for the same reason.
            ExitContractCase {
                label: "E04 whitespace-only blank line before",
                text: "- abc\n- def\n \t\n- ",
                caret: 17,
                range: (15, 17),
                replacement: "",
                caret_after: 15,
                extra: ExitContractExtra::None,
            },
            // E05: an explicit trailing line terminator after the exited
            // item's own opening line, with nothing following it, must be
            // treated the same as no terminator at all: no right separator.
            ExitContractCase {
                label: "E05 item followed by a trailing EOL",
                text: "- abc\n- def\n- \n",
                caret: 14,
                range: (12, 14),
                replacement: "",
                caret_after: 12,
                extra: ExitContractExtra::None,
            },
            // E06: an empty item between two unordered siblings needs a real
            // blank line on both sides, and both siblings must remain
            // ListItems afterward.
            ExitContractCase {
                label: "E06 between unordered siblings",
                text: "- A\n- \n- C",
                caret: 6,
                range: (4, 6),
                replacement: "\n\n",
                caret_after: 5,
                extra: ExitContractExtra::RetainsListItems(&["A", "C"]),
            },
            // E07: the same shape with ordered markers, where a non-1 ordinal
            // cannot interrupt a paragraph without a blank line either.
            ExitContractCase {
                label: "E07 between ordered siblings",
                text: "3. A\n4. \n5. C",
                caret: 8,
                range: (5, 8),
                replacement: "\n\n",
                caret_after: 6,
                extra: ExitContractExtra::RetainsListItems(&["A", "C"]),
            },
            // E08: existing blank lines are already present on both sides of
            // the exit, so neither separator is added.
            ExitContractCase {
                label: "E08 existing blank lines on both sides",
                text: "- A\n\n- \n\n- C",
                caret: 7,
                range: (5, 7),
                replacement: "",
                caret_after: 5,
                extra: ExitContractExtra::None,
            },
            // E09: CRLF line endings on both preceding lines, with the exit
            // trailing the document.
            ExitContractCase {
                label: "E09 CRLF line endings",
                text: "- abc\r\n- def\r\n- ",
                caret: 16,
                range: (14, 16),
                replacement: "",
                caret_after: 14,
                extra: ExitContractExtra::None,
            },
            // E10: bare CR line endings (no LF at all) on a middle item
            // between two siblings, which still needs a real blank line on
            // both sides.
            ExitContractCase {
                label: "E10 bare CR line endings on a middle item",
                text: "- A\r- \r- C",
                caret: 6,
                range: (4, 6),
                replacement: "\r\r",
                caret_after: 5,
                extra: ExitContractExtra::None,
            },
            // E11: mixed line endings, where each separator is built from its
            // own side's existing end-of-line bytes: LF before, CRLF after.
            ExitContractCase {
                label: "E11 LF before and CRLF after",
                text: "3. A\n4. \r\n5. C",
                caret: 8,
                range: (5, 8),
                replacement: "\n\r\n",
                caret_after: 6,
                extra: ExitContractExtra::None,
            },
            // E12: three leading spaces is the boundary case that still
            // keeps the item at the same (top) level, and the exit must
            // still remove the whole indented opening line.
            ExitContractCase {
                label: "E12 three-space leading indentation",
                text: "   - abc\n   - def\n   - ",
                caret: 23,
                range: (18, 23),
                replacement: "",
                caret_after: 18,
                extra: ExitContractExtra::None,
            },
            // E13: multi-byte UTF-8 content on both sides, including a
            // non-BMP emoji; offsets stay in bytes, not chars.
            ExitContractCase {
                label: "E13 UTF-8 byte offsets",
                text: "- \u{65e5}\u{672c}\n- \n- \u{1f642}",
                caret: 11,
                range: (9, 11),
                replacement: "\n\n",
                caret_after: 10,
                extra: ExitContractExtra::None,
            },
            // E14: a marker whose body is empty because it touches the line
            // ending directly, with no separator space at all after it.
            ExitContractCase {
                label: "E14 no-space marker",
                text: "- abc\n- def\n-\n",
                caret: 13,
                range: (12, 13),
                replacement: "",
                caret_after: 12,
                extra: ExitContractExtra::None,
            },
            // E15: plain prose (not a list marker) directly after the exit;
            // it cannot interrupt a paragraph without a blank line either, so
            // it must stay a separate paragraph from the typed text.
            ExitContractCase {
                label: "E15 before plain prose",
                text: "- A\n- \nplain text",
                caret: 6,
                range: (4, 6),
                replacement: "\n\n",
                caret_after: 5,
                extra: ExitContractExtra::FollowingProseStaysSeparateParagraph,
            },
            // E16: three existing line endings (two blank lines) already
            // precede a trailing empty item; they must not be collapsed, and
            // since the item is last in the document no right separator is
            // needed either.
            ExitContractCase {
                label: "E16 three existing line endings before trailing item",
                text: "- A\n\n\n- ",
                caret: 8,
                range: (6, 8),
                replacement: "",
                caret_after: 6,
                extra: ExitContractExtra::None,
            },
        ];

        for case in &cases {
            for intent in [ListEditIntent::Enter, ListEditIntent::Backspace] {
                let source_range = SourceRange::new(0, case.text.len());
                let parsed = parse_document(Revision(1), source_range, case.text);
                let projection = build_list_edit_projection(&parsed, case.text);
                let result = plan_list_edit(
                    &RopeBuffer::from_text(case.text),
                    &projection,
                    SourceSelection::caret(SourceOffset(case.caret)),
                    intent,
                )
                .unwrap();
                let ListEditPlanResult::Handled(MarkdownEditPlan::Replace {
                    range,
                    replacement,
                    selection_after,
                }) = result
                else {
                    panic!("{}: expected a Replace plan for {intent:?}", case.label);
                };
                assert_eq!(
                    (range.start.0, range.end.0),
                    case.range,
                    "{}: unexpected source range for {intent:?}",
                    case.label
                );
                assert_eq!(
                    replacement, case.replacement,
                    "{}: unexpected replacement for {intent:?}",
                    case.label
                );
                assert_eq!(
                    selection_after,
                    SourceSelection::caret(SourceOffset(case.caret_after)),
                    "{}: unexpected caret for {intent:?}",
                    case.label
                );

                let mut typed = case.text.to_string();
                typed.replace_range(case.range.0..case.range.1, &replacement);
                typed.insert_str(case.caret_after, "ghi");

                let typed_parsed =
                    parse_document(Revision(1), SourceRange::new(0, typed.len()), &typed);
                // Document-tail exits (#767) only remove the marker, so text
                // typed right after may be a lazy continuation of the previous
                // item; the standalone-paragraph guarantee does not apply.
                let is_trailing_exit = ["E01 ", "E05 ", "E09 ", "E12 ", "E14 "]
                    .iter()
                    .any(|prefix| case.label.starts_with(prefix));
                if !is_trailing_exit {
                    let ghi_paragraph = typed_parsed
                        .tree
                        .iter()
                        .find(|(_, node)| {
                            matches!(node.kind, NodeKind::Paragraph)
                                && typed[node.source_range.as_usize()].contains("ghi")
                        })
                        .unwrap_or_else(|| {
                            panic!(
                                "{}: expected a paragraph containing \"ghi\" in {typed:?}",
                                case.label
                            )
                        });
                    assert!(
                        !typed_parsed
                            .tree
                            .ancestors(ghi_paragraph.0)
                            .skip(1)
                            .any(|ancestor| matches!(
                                typed_parsed.tree.node(ancestor).map(|node| node.kind),
                                Some(NodeKind::ListItem { .. })
                            )),
                        "{}: the paragraph for \"ghi\" must not be a descendant of a ListItem in {typed:?}",
                        case.label
                    );
                }

                match case.extra {
                    ExitContractExtra::None => {}
                    ExitContractExtra::RetainsListItems(expected_items) => {
                        let list_items: Vec<_> = typed_parsed
                            .tree
                            .iter()
                            .filter(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                            .collect();
                        assert_eq!(
                            list_items.len(),
                            expected_items.len(),
                            "{}: expected {expected_items:?} to remain ListItems, got {list_items:?} in {typed:?}",
                            case.label
                        );
                        for (item, expected_text) in
                            list_items.iter().zip(expected_items.iter().copied())
                        {
                            assert!(
                                typed[item.1.source_range.as_usize()].contains(expected_text),
                                "{}: expected a ListItem containing {expected_text:?} in {typed:?}",
                                case.label
                            );
                        }
                    }
                    ExitContractExtra::FollowingProseStaysSeparateParagraph => {
                        let paragraphs = typed_parsed
                            .tree
                            .iter()
                            .filter(|(_, node)| matches!(node.kind, NodeKind::Paragraph))
                            .count();
                        assert_eq!(
                            paragraphs, 2,
                            "{}: expected \"ghi\" and the following prose to be two distinct paragraphs in {typed:?}",
                            case.label
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ordered_move_renumbers_only_the_moved_item() {
        let indent = ListEditIntent::Indent;
        let outdent = ListEditIntent::Outdent;
        let cases: &[(&str, usize, ListEditIntent, &str, usize)] = &[
            // basic: new child list starts at 1 under the existing indent rule
            (
                "1. abc\n2. def\n3. ghi",
                18,
                indent,
                "1. abc\n2. def\n   1. ghi",
                21,
            ),
            // existing child list: semantic number, not the source digits
            (
                "1. A\n\n    5. N\n    99. M\n8. B",
                29,
                indent,
                "1. A\n\n    5. N\n    99. M\n   7. B",
                32,
            ),
            // outdent continues the parent's numbering, followers untouched
            ("3. A\n   1. B\n77. C", 12, outdent, "3. A\n4. B\n77. C", 9),
            // two digits to one, continuation line, UTF-8 body
            (
                "9. A\n10. 日本\n    cont",
                12,
                indent,
                "9. A\n   1. 日本\n      cont",
                14,
            ),
            // one digit to two, separate paragraph and descendants kept
            (
                "9. A\n   1. B\n\n      P\n\n      7. C\n9. Z",
                12,
                outdent,
                "9. A\n10. B\n\n    P\n\n    7. C\n9. Z",
                10,
            ),
            ("1) A\n2) B", 9, indent, "1) A\n   1) B", 12),
            // a different delimiter never joins the existing child list
            (
                "1. A\n\n    5) N\n2. B",
                19,
                indent,
                "1. A\n\n    5) N\n   1. B",
                22,
            ),
            // descendant numbers are not touched
            (
                "9. A\n10. B\n\n    7. C\n    42. D",
                10,
                indent,
                "9. A\n   1. B\n\n      7. C\n      42. D",
                12,
            ),
            ("1. A\r\n2. B\r\n", 10, indent, "1. A\r\n   1. B\r\n", 13),
            ("1. A\n2. [ ] B", 13, indent, "1. A\n   1. [ ] B", 16),
            ("9. A\n10. [X] B", 11, indent, "9. A\n   1. [X] B", 13),
            ("9. A\n10. B", 6, indent, "9. A\n   1. B", 9),
            // caret at an unindented marker start follows the marker
            ("1. A\n2. B", 5, indent, "1. A\n   1. B", 8),
            // caret right after the old number lands right after the new number
            ("9. A\n   1. B", 9, outdent, "9. A\n10. B", 7),
            // indents of four or more columns still renumber
            ("10. A\n11. B", 11, indent, "10. A\n    1. B", 14),
            ("1. A\n    1. B", 13, outdent, "1. A\n2. B", 9),
        ];
        for (text, caret, intent, expected, at) in cases {
            assert_eq!(
                replacement(text, *caret, *intent),
                ((*expected).to_owned(), *at),
                "{text:?}"
            );
        }
    }

    #[test]
    fn ordered_move_preserves_structure_and_task_state_after_reparse() {
        let (text, _) = replacement(
            "9. A\n10. [X] B\n\n    P\n\n    7. C",
            11,
            ListEditIntent::Indent,
        );
        let items = reparsed_items(&text);
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].depth, 2);
        assert_eq!(items[1].parent_item, Some(0));
        assert_eq!(items[1].task, Some(true));
        assert_eq!(items[2].depth, 3);
        assert_eq!(items[2].parent_item, Some(1));
        assert_eq!(items[2].list_start, Some(7));

        let (text, _) = replacement(
            "9. A\n   1. B\n\n      P\n\n      7. C\n9. Z",
            12,
            ListEditIntent::Outdent,
        );
        let items = reparsed_items(&text);
        assert_eq!(items.len(), 4);
        assert_eq!(items[1].depth, 1);
        assert_eq!(items[1].ordinal, 1);
        assert_eq!(items[1].list_range, items[0].list_range);
        assert_eq!(items[2].parent_item, Some(1));
    }

    #[test]
    fn ordered_move_keeps_fenced_code_bytes_including_tabs() {
        let (indented, _) = replacement(
            "1. A\n2. B\n   ```\n   all:\n   \tcc -o a a.c\n   ```",
            9,
            ListEditIntent::Indent,
        );
        assert_eq!(
            indented,
            "1. A\n   1. B\n      ```\n      all:\n      \tcc -o a a.c\n      ```"
        );

        let (outdented, _) = replacement(
            "1. A\n   1. B\n      ```\n      all:\n      \tcc -o a a.c\n      ```",
            12,
            ListEditIntent::Outdent,
        );
        assert_eq!(
            outdented,
            "1. A\n2. B\n   ```\n   all:\n   \tcc -o a a.c\n   ```"
        );
    }

    #[test]
    fn ordered_move_ignores_recovered_items_when_continuing_child_numbers() {
        assert_eq!(
            replacement(
                "1. A\n\n       5. code\n2. B",
                25,
                ListEditIntent::Indent
            ),
            ("1. A\n\n       5. code\n   1. B".to_owned(), 28)
        );
    }

    #[test]
    fn ordered_move_keeps_content_start_with_tab_after_marker() {
        let (text, caret) = replacement("9. A\n10.\t  [X] B", 16, ListEditIntent::Indent);
        assert_eq!(text, "9. A\n   1.   [X] B");
        assert_eq!(caret, 18);
        let items = reparsed_items(&text);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].depth, 2);
        assert_eq!(items[1].task, Some(true));
    }

    #[test]
    fn ordered_move_keeps_content_start_of_descendant_with_tab_after_marker() {
        let (indented, _) = replacement(
            "10. A\n11. B\n\n    7.\t  [x] text",
            11,
            ListEditIntent::Indent,
        );
        assert_eq!(indented, "10. A\n    1. B\n\n       7.    [x] text");
        let items = reparsed_items(&indented);
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].task, Some(true));

        let (outdented, _) = replacement(
            "10. A\n    1. B\n\n        7.\t  [x] text",
            13,
            ListEditIntent::Outdent,
        );
        assert_eq!(outdented, "10. A\n11. B\n\n     7.    [x] text");
        let items = reparsed_items(&outdented);
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].task, Some(true));
    }

    #[test]
    fn ordered_move_shifts_tab_structural_indent_by_columns() {
        let (indented, caret) = replacement("10. A\n11. B\n\n\tP", 11, ListEditIntent::Indent);
        assert_eq!(indented, "10. A\n    1. B\n\n       P");
        assert_eq!(caret, 14);
        let items = reparsed_items(&indented);
        assert_eq!(items[1].parent_item, Some(0));

        let (outdented, caret) =
            replacement("10. A\n    1. B\n\n\t\tP", 14, ListEditIntent::Outdent);
        assert_eq!(outdented, "10. A\n11. B\n\n     P");
        assert_eq!(caret, 11);
    }

    #[test]
    fn ordered_move_of_the_first_item_is_a_noop() {
        assert_eq!(
            plan("1. A\n2. B", 4, ListEditIntent::Indent),
            MarkdownEditPlan::NoOp {
                selection_after: SourceSelection::caret(SourceOffset(4)),
            }
        );
    }

    /// `transform_subtree` must shift every physical line of the subtree by
    /// the same column delta while keeping each line's own terminator bytes
    /// untouched, even when those terminators mix CRLF, bare CR, and LF
    /// within the same subtree. Exercised directly against `transform_subtree`
    /// (rather than through `plan_list_edit`) so the terminator bytes in the
    /// source are exact and not an artifact of how the parser happens to
    /// report ranges.
    #[test]
    fn transform_subtree_preserves_mixed_line_endings() {
        let source = "- A\r\n  - B\r    - C\n";
        let subtree_range = SourceRange::new(5, source.len());
        let item = ListEditItem {
            item_range: subtree_range,
            subtree_range,
            list_range: SourceRange::new(0, source.len()),
            marker_range: SourceRange::new(5, 5),
            prefix_range: SourceRange::new(5, 5),
            body_range: SourceRange::new(5, 5),
            marker: ListMarker::Unordered { bullet: b'-' },
            list_start: None,
            ordinal: 1,
            depth: 2,
            task: None,
            marker_indent_columns: 2,
            body_indent_columns: 4,
            previous_sibling: None,
            parent_item: None,
            is_parser_item: true,
            in_quote: false,
        };
        let MarkdownEditPlan::Replace {
            range, replacement, ..
        } = transform_subtree(source, &item, -2, SourceOffset(5))
        else {
            panic!("expected a Replace plan");
        };
        assert_eq!(range, subtree_range);
        assert_eq!(replacement, "- B\r  - C\n");
    }
}

/// The real extent of a parser list item's subtree, derived from its last
/// child rather than trusted from `pulldown_cmark`'s own reported range.
///
/// When a list ends because the next line is unindented, non-list content
/// (rather than another item at the same or a deeper level), `pulldown_cmark`
/// must look ahead past any trailing blank lines to decide that the list
/// really ends there. That lookahead can leave the blank lines attributed to
/// the *last* item's reported range even though nothing in the tree actually
/// owns them, which previously made an otherwise-empty trailing item look
/// like it had content past its own opening line and blocked its exit. An
/// item's real children already cover every byte it actually owns, so using
/// the last child's end (or the opening line's own end, for an item with no
/// children at all) excludes that lookahead-only trailing span.
fn item_subtree_end(tree: &crate::MarkdownTree, item_id: NodeId, opening_line_end: usize) -> usize {
    tree.children(item_id)
        .iter()
        .filter_map(|child| tree.node(*child))
        .map(|child| child.source_range.end.0)
        .max()
        .map_or(opening_line_end, |end| end.max(opening_line_end))
}

/// Plans one task checkbox toggle against the current document.
///
/// `marker` is the real `[ ]` / `[x]` / `[X]` source range a presentation
/// resolved for the clicked checkbox and `expected_checked` the state it drew.
/// Nothing is trusted from that snapshot: the marker is accepted only when
/// `projection` (built from the current revision) still has a formal task
/// item whose own bullet/number marker is immediately followed by exactly
/// these three bytes, with the same checked state. Any disagreement yields
/// `Ok(None)` and the document is left alone.
///
/// The plan replaces only the middle byte (` ` → `x`, `x`/`X` → ` `) and keeps
/// the selection offsets, since the replacement does not change any length.
pub fn plan_task_toggle(
    document: &RopeBuffer,
    projection: &ListEditProjection,
    marker: SourceRange,
    expected_checked: bool,
    selection: SourceSelection,
) -> Result<Option<MarkdownEditPlan>, BufferError> {
    if marker.end.0 != marker.start.0 + 3 {
        return Ok(None);
    }
    let Some((_, item)) = projection.item_at(marker.start) else {
        return Ok(None);
    };
    if !item.is_parser_item
        || item.task != Some(expected_checked)
        || item.marker_range.end != marker.start
        || item.body_range.start.0 < marker.end.0
    {
        return Ok(None);
    }
    let checked = match document.text(marker)?.as_str() {
        "[ ]" => false,
        "[x]" | "[X]" => true,
        _ => return Ok(None),
    };
    if checked != expected_checked {
        return Ok(None);
    }
    Ok(Some(MarkdownEditPlan::Replace {
        range: SourceRange::new(marker.start.0 + 1, marker.start.0 + 2),
        replacement: if checked { " " } else { "x" }.to_owned(),
        selection_after: selection,
    }))
}

/// A `TaskMarker` is always a direct child of its own owning `ListItem`: the
/// parser emits `Event::TaskListMarker` while that item is open (tight) or
/// inside its own empty first paragraph (loose), and `build_tree` normalizes
/// the loose case onto the item, so only `id`'s direct children can ever be its task marker. Recursing into
/// descendants would also match a nested item's own task marker — e.g. for
/// `- A\n  - [ ] child`, the child item's `[ ]` is not `A`'s marker — which
/// would wrongly shrink `A`'s non-empty body down to an empty range.
fn find_task_marker(tree: &crate::MarkdownTree, id: NodeId) -> Option<SourceRange> {
    tree.children(id).iter().find_map(|child| {
        let node = tree.node(*child)?;
        matches!(node.kind, NodeKind::TaskMarker(_)).then_some(node.source_range)
    })
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
    for (content, ending) in split_physical_lines(original) {
        if delta >= 0 {
            replacement.push_str(&" ".repeat(delta as usize));
            replacement.push_str(content);
        } else {
            replacement.push_str(&remove_columns(content, (-delta) as usize));
        }
        replacement.push_str(ending);
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

const MAX_ORDERED_NUMBER: u64 = 999_999_999;

/// Plans an Indent / Outdent of a parser-owned ordered item that rewrites only
/// the moved item's own number. Returns `None` when the item is not an
/// ordered parser item (or its source shape is unexpected), leaving the plain
/// whitespace move to the caller.
fn plan_ordered_move(
    source: &str,
    projection: &ListEditProjection,
    item: &ListEditItem,
    intent: ListEditIntent,
    new_indent: usize,
    selection: SourceSelection,
) -> Option<MarkdownEditPlan> {
    let ListMarker::Ordered { delimiter } = item.marker else {
        return None;
    };
    if !item.is_parser_item || item.in_quote {
        return None;
    }
    let number = match intent {
        ListEditIntent::Indent => {
            let previous_index = item.previous_sibling?;
            let previous = projection.items.get(previous_index)?;
            let last_child = projection
                .items
                .iter()
                .filter(|candidate| {
                    candidate.is_parser_item
                        && candidate.parent_item == Some(previous_index)
                        && candidate.depth == previous.depth + 1
                })
                .max_by_key(|candidate| candidate.item_range.start.0);
            match last_child {
                Some(child)
                    if child.marker == ListMarker::Ordered { delimiter }
                        && source
                            .get(child.subtree_range.end.0..item.subtree_range.start.0)
                            .is_some_and(|between| {
                                between.bytes().all(|byte| byte.is_ascii_whitespace())
                            }) =>
                {
                    child
                        .list_start
                        .unwrap_or(1)
                        .saturating_add(child.ordinal as u64)
                        .saturating_add(1)
                }
                _ => 1,
            }
        }
        ListEditIntent::Outdent => {
            let parent = projection.items.get(item.parent_item?)?;
            match parent.marker {
                ListMarker::Ordered { .. } => parent
                    .list_start
                    .unwrap_or(1)
                    .saturating_add(parent.ordinal as u64)
                    .saturating_add(1),
                ListMarker::Unordered { .. } => 1,
            }
        }
        _ => return None,
    };
    if number > MAX_ORDERED_NUMBER {
        return Some(MarkdownEditPlan::NoOp {
            selection_after: selection,
        });
    }
    transform_ordered_subtree(
        source,
        projection,
        item,
        new_indent,
        number,
        selection.active,
    )
}

fn transform_ordered_subtree(
    source: &str,
    projection: &ListEditProjection,
    item: &ListEditItem,
    new_indent: usize,
    number: u64,
    caret: SourceOffset,
) -> Option<MarkdownEditPlan> {
    let range = item.subtree_range;
    let original = source.get(range.as_usize())?;
    let lines = split_physical_lines(original);
    let (first, first_ending) = *lines.first()?;

    let marker_rel = item.marker_range.start.0.checked_sub(range.start.0)?;
    let old_indent = first.get(..marker_rel)?;
    if !old_indent.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        return None;
    }
    let digits = first[marker_rel..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits == 0 || !matches!(first.as_bytes().get(marker_rel + digits), Some(b'.' | b')')) {
        return None;
    }
    // `first[marker_rel + digits]` is the ASCII delimiter; the run of blanks
    // after it is the marker padding.
    let run_start = marker_rel + digits + 1;
    let run_len = first[run_start..]
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let run = &first[run_start..run_start + run_len];
    let tail = &first[run_start + run_len..];
    // A tab in the padding depends on its absolute column, so it would span a
    // different width after the marker moves. Replace it with spaces covering
    // the same columns so the parser's content start (and task syntax) is kept.
    let new_run = if run.contains('\t') && !tail.is_empty() {
        let run_columns = prefix_columns(&first[..run_start + run_len])
            .saturating_sub(prefix_columns(&first[..run_start]));
        " ".repeat(run_columns)
    } else {
        run.to_owned()
    };
    let delimiter = &first[marker_rel + digits..run_start];
    let new_number = number.to_string();
    let new_first = format!(
        "{}{new_number}{delimiter}{new_run}{tail}",
        " ".repeat(new_indent)
    );

    let old_width = crate::markers::list_item_content_column_at(first, marker_rel)?;
    let new_width = crate::markers::list_item_content_column_at(&new_first, new_indent)?;
    let shift = new_width as isize - old_width as isize;

    let offset = caret.0.saturating_sub(range.start.0);
    let new_marker_end = new_indent + new_number.len();
    let old_marker_end = marker_rel + digits;
    let new_run_start = new_marker_end + 1;
    let old_run_end = run_start + run_len;
    let new_run_end = new_run_start + new_run.len();
    let mut mapped = if offset > first.len() {
        None
    } else if offset == 0 && marker_rel > 0 {
        Some(0)
    } else if offset < marker_rel {
        Some(new_indent.saturating_sub(marker_rel - offset))
    } else if offset == old_marker_end {
        Some(new_marker_end)
    } else if offset < old_marker_end {
        Some((new_indent + (offset - marker_rel)).min(new_marker_end))
    } else if offset <= run_start {
        Some(new_run_start - (run_start - offset))
    } else if offset < old_run_end {
        Some(new_run_start + (offset - run_start).min(new_run.len()))
    } else {
        Some(offset - old_run_end + new_run_end)
    };

    let mut replacement = String::with_capacity(original.len() + new_indent + 8);
    replacement.push_str(&new_first);
    replacement.push_str(first_ending);
    let mut old_pos = first.len() + first_ending.len();
    let mut new_pos = new_first.len() + first_ending.len();
    for (content, ending) in &lines[1..] {
        let blank = content.bytes().all(|byte| matches!(byte, b' ' | b'\t'));
        let line_start_abs = range.start.0 + old_pos;
        let in_code =
            projection.line_in_code_block(line_start_abs, line_start_abs + content.len());
        let (new_content, lead_old, lead_new) = if shift == 0 || blank {
            ((*content).to_owned(), 0, 0)
        } else if in_code {
            // Code lines keep their bytes: only add or drop leading blanks.
            if shift > 0 {
                (
                    format!("{}{content}", " ".repeat(shift as usize)),
                    0,
                    shift as usize,
                )
            } else {
                let kept = remove_columns(content, shift.unsigned_abs());
                let removed = content.len() - kept.len();
                (kept, removed, 0)
            }
        } else {
            reindent_columns(content, shift)
        };
        // A descendant item whose marker is followed by a tab keeps its
        // content start only if the padding is rewritten as spaces covering
        // the same columns (a tab's width depends on its absolute column).
        let mut run_fix = None;
        let new_content = if shift != 0 && !blank && !in_code {
            match fix_descendant_tab_padding(projection, line_start_abs, content, &new_content) {
                Some((fixed, old_run_end, new_run_end)) => {
                    run_fix = Some((old_run_end, new_run_end));
                    fixed
                }
                None => new_content,
            }
        } else {
            new_content
        };
        let old_len = content.len() + ending.len();
        if mapped.is_none() && offset >= old_pos && offset < old_pos + old_len {
            let rel = offset - old_pos;
            let mut new_rel = if rel >= lead_old {
                rel - lead_old + lead_new
            } else {
                lead_new
            };
            if let Some((old_run_end, new_run_end)) = run_fix {
                let shifted_old_end = old_run_end - lead_old + lead_new;
                if rel >= old_run_end {
                    new_rel = new_rel + new_run_end - shifted_old_end;
                } else if new_rel > new_run_end {
                    new_rel = new_run_end;
                }
            }
            mapped = Some(new_pos + new_rel);
        }
        old_pos += old_len;
        new_pos += new_content.len() + ending.len();
        replacement.push_str(&new_content);
        replacement.push_str(ending);
    }
    let mapped = mapped.unwrap_or_else(|| new_pos + offset.saturating_sub(old_pos));
    Some(MarkdownEditPlan::Replace {
        range,
        replacement,
        selection_after: SourceSelection::caret(SourceOffset(range.start.0 + mapped)),
    })
}

/// Splits `text` into physical lines, pairing each line's content with its
/// own line-terminator bytes (`"\n"`, `"\r\n"`, `"\r"`, or `""` for a final
/// line with no terminator). Unlike `str::split_inclusive('\n')`, a bare `\r`
/// not followed by `\n` still ends its line here, so a subtree using only CR
/// terminators (no LF at all) is still split into one entry per line rather
/// than collapsed into a single line.
fn split_physical_lines(text: &str) -> Vec<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut line_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push((&text[line_start..i], &text[i..i + 1]));
                i += 1;
                line_start = i;
            }
            b'\r' => {
                let end = if bytes.get(i + 1) == Some(&b'\n') {
                    i + 2
                } else {
                    i + 1
                };
                lines.push((&text[line_start..i], &text[i..end]));
                i = end;
                line_start = i;
            }
            _ => i += 1,
        }
    }
    if line_start < bytes.len() {
        lines.push((&text[line_start..], ""));
    }
    lines
}

/// For a descendant list item's opening line, rewrites a tab-containing run
/// after the marker as spaces covering the columns it spanned in `old`, so the
/// parser's content start survives the reindent. The item's number is left
/// untouched. Returns the fixed line plus the run end offsets in `old` and the
/// fixed line, or `None` when the line is not such an item line.
fn fix_descendant_tab_padding(
    projection: &ListEditProjection,
    line_start_abs: usize,
    old: &str,
    reindented: &str,
) -> Option<(String, usize, usize)> {
    let lead = old
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let marker_abs = line_start_abs + lead;
    projection
        .items
        .iter()
        .find(|item| item.marker_range.start.0 == marker_abs)?;
    let digits = old[lead..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    let marker_len = if digits > 0 {
        if !matches!(old.as_bytes().get(lead + digits), Some(b'.' | b')')) {
            return None;
        }
        digits + 1
    } else if matches!(old.as_bytes().get(lead), Some(b'-' | b'+' | b'*')) {
        1
    } else {
        return None;
    };
    let run_start = lead + marker_len;
    let run_len = old[run_start..]
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let run_end = run_start + run_len;
    let run = &old[run_start..run_end];
    if !run.contains('\t') || run_end >= old.len() {
        return None;
    }
    let new_lead = reindented
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let new_run_start = new_lead + marker_len;
    let run_columns = prefix_columns(&old[..run_end]).saturating_sub(prefix_columns(&old[..run_start]));
    let fixed = format!(
        "{}{}{}",
        reindented.get(..new_run_start)?,
        " ".repeat(run_columns),
        &old[run_end..]
    );
    Some((fixed, run_end, new_run_start + run_columns))
}

/// Moves a non-code continuation line's structural indent by `shift` display
/// columns, rewriting the leading blanks as spaces so tabs never overshoot the
/// target column. Returns the new line with the old and new leading byte counts.
fn reindent_columns(line: &str, shift: isize) -> (String, usize, usize) {
    let lead = line
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    let target = (columns(line) as isize + shift).max(0) as usize;
    (
        format!("{}{}", " ".repeat(target), &line[lead..]),
        lead,
        target,
    )
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

/// The line's content end (see [`line_end`]) plus its own terminator, i.e.
/// the start of the next physical line, or the source end if there is none.
fn line_end_inclusive(source: &[u8], offset: usize) -> usize {
    let end = line_end(source, offset);
    if end >= source.len() {
        return end;
    }
    if source[end] == b'\r' && source.get(end + 1) == Some(&b'\n') {
        end + 2
    } else {
        end + 1
    }
}
