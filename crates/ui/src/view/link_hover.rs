//! Hover emphasis for openable body links (Issue #700).
//!
//! The hovered link is transient view state: it is never part of the document,
//! its history or the saved session, and resolving it never calls
//! `after_input`. Only the link's foreground colour changes, so the layout and
//! the parse/layout caches are untouched by a hover.

use super::*;

/// Identity of the link under the pointer. `link_text_range` is the whole link
/// text, so a link that wraps over several rows is one link and a second link
/// to the same destination is another.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HoveredLink {
    document: DocumentKey,
    link_text_range: SourceRange,
    destination: std::sync::Arc<str>,
}

/// The pointer's last window position inside the body and the link under it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct LinkHover {
    pointer: Option<(f32, f32)>,
    link: Option<HoveredLink>,
}

impl EditorView {
    /// Whether the pointer is busy with something other than reading the text:
    /// a button drag of any kind, IME composition, or UI covering the body.
    fn link_hover_suppressed(&self, dragging: bool) -> bool {
        dragging
            || self.text_selection_drag
            || self.pending_reading_link.is_some()
            || self.sidebar_resize_drag.is_some()
            || self.sidebar_scrollbar_drag.is_some()
            || self.editor_scrollbar_drag.is_some()
            || self.editor().ime().is_some()
            || self.editor_context_menu.is_some()
            || self.file_tab_context_menu.is_some()
            || self.sidebar_context_menu.is_some()
            || self.tab_close_confirm.is_some()
            || self.settings_open
    }

    /// Whether a window-space point lies in the editor body viewport.
    fn link_hover_in_body(&self, window_x: f32, window_y: f32) -> bool {
        let local_y = window_y - self.theme.header_height - self.document_find_reserved_height();
        window_x >= self.main_column_left && local_y >= 0.0 && local_y < self.viewport_height
    }

    /// The link whose text is under a window-space point on one presented row.
    /// The glyph under the pointer decides, not the nearest caret boundary, so
    /// the right half of a link's last glyph still counts.
    fn resolve_link_hover(
        &self,
        line: usize,
        fragment: Range<usize>,
        window_x: f32,
        window_y: f32,
        shaper: &dyn LineShaper,
    ) -> Option<HoveredLink> {
        let hit = self.row_hit_at_window_point(line, fragment, window_x, window_y, shaper)?;
        let glyph_offset = hit.glyph_offset?;
        let (block_id, visual_line) = *self.line_owners.get(&line)?;
        let visual = self.block_cache.get(&block_id)?.lines.get(visual_line)?;
        let link = visual
            .links
            .iter()
            .find(|link| link.contains(glyph_offset))?;
        Some(HoveredLink {
            document: self.document_key(),
            link_text_range: link.link_text_range,
            destination: link.destination.clone(),
        })
    }

    /// Notifies only when the link identity changes, never for movement inside
    /// one link.
    fn set_hovered_link(&mut self, next: Option<HoveredLink>, cx: &mut Context<Self>) {
        if self.link_hover.link != next {
            self.link_hover.link = next;
            cx.notify();
        }
    }

    pub(super) fn clear_link_hover(&mut self, cx: &mut Context<Self>) {
        self.link_hover.pointer = None;
        self.set_hovered_link(None, cx);
    }

    /// Mouse move over one body row.
    pub(super) fn update_link_hover_on_row(
        &mut self,
        line: usize,
        fragment: Range<usize>,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_hover_suppressed(event.dragging()) {
            self.clear_link_hover(cx);
            return;
        }
        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
        self.link_hover.pointer = Some((x, y));
        let shaper = WindowShaper::new(window, self.zoom);
        let next = self.resolve_link_hover(line, fragment, x, y, &shaper);
        self.set_hovered_link(next, cx);
    }

    /// Mouse move anywhere in the window. Rows resolve their own moves; this
    /// only clears the hover once the pointer is outside the body or the
    /// pointer is busy.
    pub(super) fn update_link_hover_on_panel(
        &mut self,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
        if self.link_hover_suppressed(event.dragging()) || !self.link_hover_in_body(x, y) {
            self.clear_link_hover(cx);
        }
    }

    /// Re-resolves the hover against the presentation and layout of the frame
    /// being drawn, so scrolling, editing, re-parsing or a tab switch under a
    /// still pointer never keeps a stale link emphasised. It runs inside
    /// render, so it never notifies. Returns the whole-link range to paint.
    pub(super) fn refresh_link_hover(&mut self, shaper: &dyn LineShaper) -> Option<SourceRange> {
        let resolved = self.link_hover.pointer.and_then(|(x, y)| {
            if self.link_hover_suppressed(false) || !self.link_hover_in_body(x, y) {
                return None;
            }
            let content_y = self.scroll_y + y
                - self.theme.header_height
                - self.document_find_reserved_height();
            let blocks = self
                .line_owners
                .values()
                .map(|(block_id, _)| *block_id)
                .collect::<HashSet<_>>();
            blocks.into_iter().find_map(|block_id| {
                let layout = &self.layout_cache.get(&block_id)?.layout;
                layout.lines.iter().enumerate().find_map(|(row_index, row)| {
                    let top = self.content_y_for_row(block_id, row_index)?;
                    let owned = self
                        .line_owners
                        .get(&(row.line_id as usize))
                        .is_some_and(|(owner, _)| *owner == block_id);
                    (owned && row.height > 0.0 && top <= content_y && content_y < top + row.height)
                        .then(|| {
                            self.resolve_link_hover(
                                row.line_id as usize,
                                row.line_visual_range.clone(),
                                x,
                                y,
                                shaper,
                            )
                        })
                        .flatten()
                })
            })
        });
        self.link_hover.link = resolved;
        self.hovered_link_range()
    }

    /// The whole-link range currently emphasised, if any.
    pub(super) fn hovered_link_range(&self) -> Option<SourceRange> {
        self.link_hover.link.as_ref().map(|link| link.link_text_range)
    }
}
