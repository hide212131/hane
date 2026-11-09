//! Accessibility nodes for the visible body links (Issue #704, Phase 0).
//!
//! A thin element wraps the editor body and is exposed as the document root.
//! While accessibility is active the view collects the links whose text is in
//! the viewport from the frame's own layout, and the element publishes them as
//! synthetic `Link` leaves with an explicit `Click` action. Nothing here is
//! document state: the collected records live for one frame, and while
//! accessibility is inactive none of the collection, label, hash or handler
//! work runs.
//!
//! An AX/UIA click is never turned into mouse input. It is checked against the
//! current document and content generation and then reaches the same
//! `cx.open_url` the mouse path ends in, without touching selection, caret,
//! focus, the document or its history.

use super::*;
use gpui::{A11ySubtreeBuilder, AnyElement, Element, ElementId, GlobalElementId, LayoutId};

#[cfg(test)]
thread_local! {
    /// How many times the a11y-only collector, label builder and handler
    /// registration ran on this thread; read by the inactive-cost tests.
    pub(super) static COLLECTOR_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static LABEL_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static HANDLER_REGISTRATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count(counter: &'static std::thread::LocalKey<std::cell::Cell<usize>>) {
    counter.with(|calls| calls.set(calls.get() + 1));
}

const DOCUMENT_ELEMENT_ID: &str = "editor-document";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum A11yNodeKind {
    Link,
}

/// What a synthetic node's id is derived from, together with the document
/// root's id. A content generation that changes with the text makes every id of
/// the old content unreachable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct A11yNodeKey {
    pub kind: A11yNodeKind,
    pub content_generation: u64,
    pub link_start: usize,
    pub link_end: usize,
}

/// A visible link as published for one frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct A11yLink {
    pub key: A11yNodeKey,
    pub link_text_range: SourceRange,
    pub destination: Arc<str>,
    pub label: String,
    /// Part of the link is outside the viewport.
    pub partial: bool,
    /// Logical window bounds times the window's scale factor, the unit GPUI
    /// uses for its own nodes.
    pub bounds: accesskit::Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct A11yDocumentFrame {
    pub document: DocumentKey,
    pub content_generation: u64,
    pub name: String,
    pub description: &'static str,
    pub links: Vec<A11yLink>,
}

/// What a click on a published link is checked against before it opens.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct A11yLinkRequest {
    pub document: DocumentKey,
    pub content_generation: u64,
    pub link_text_range: SourceRange,
    pub destination: Arc<str>,
}

impl A11yDocumentFrame {
    pub(super) fn request_for(&self, link: &A11yLink) -> A11yLinkRequest {
        A11yLinkRequest {
            document: self.document,
            content_generation: self.content_generation,
            link_text_range: link.link_text_range,
            destination: link.destination.clone(),
        }
    }
}

/// The content generation, advanced lazily: only frames drawn while
/// accessibility is active compare the stamp, so nothing tracks edits while it
/// is inactive, and a change made meanwhile still yields a new generation on
/// the next active frame. The counter never repeats within the window's life.
#[derive(Debug, Default)]
pub(super) struct A11yState {
    content_generation: u64,
    stamp: Option<(DocumentKey, Revision)>,
}

#[cfg(test)]
impl A11yState {
    pub(super) fn content_generation_for_test(&self) -> u64 {
        self.content_generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LogicalRect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl LogicalRect {
    fn intersect(self, other: Self) -> Option<Self> {
        let rect = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (rect.x0 < rect.x1 && rect.y0 < rect.y1).then_some(rect)
    }

    fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// The visible part of one link on one row.
struct LinkFragment {
    link_text_range: SourceRange,
    destination: Arc<str>,
    /// Document-content position of the row and x of the piece, for reading
    /// the pieces of a wrapped link in order.
    order: (f32, f32),
    text: String,
    rect: LogicalRect,
    clipped: bool,
}

/// Label from the link's displayed text, never its Markdown or destination.
fn link_label(pieces: &[&str]) -> String {
    #[cfg(test)]
    count(&LABEL_CALLS);
    pieces.concat().trim().to_owned()
}

impl EditorView {
    /// The frame to publish, or `None` while accessibility is inactive. This
    /// is the only entry point: everything a11y-specific sits behind the check.
    pub(super) fn a11y_document_frame(&mut self, window: &Window) -> Option<A11yDocumentFrame> {
        if !window.is_a11y_active() {
            return None;
        }
        let shaper = WindowShaper::new(window, self.zoom);
        Some(self.build_a11y_frame(
            &shaper,
            f64::from(window.scale_factor()),
            f32::from(window.viewport_size().width),
        ))
    }

    pub(super) fn build_a11y_frame(
        &mut self,
        shaper: &dyn LineShaper,
        scale: f64,
        window_width: f32,
    ) -> A11yDocumentFrame {
        let content_generation = self.a11y_content_generation();
        let links = self.collect_a11y_links(shaper, content_generation, scale, window_width);
        A11yDocumentFrame {
            document: self.document_key(),
            content_generation,
            name: self.sessions.active().file().short_label(),
            description: match self.editor_mode() {
                EditorMode::Editing => "編集モード。表示範囲内のリンクのみ公開しています",
                EditorMode::Reading => "閲覧モード。表示範囲内のリンクのみ公開しています",
            },
            links,
        }
    }

    fn a11y_content_stamp(&self) -> (DocumentKey, Revision) {
        (self.document_key(), self.sessions.active().revision())
    }

    fn a11y_content_generation(&mut self) -> u64 {
        let stamp = self.a11y_content_stamp();
        if self.a11y.stamp != Some(stamp) {
            self.a11y.stamp = Some(stamp);
            self.a11y.content_generation += 1;
        }
        self.a11y.content_generation
    }

    /// Visible links from this frame's layout. Rows that do not reach the
    /// viewport are skipped before any geometry is measured, and a link with no
    /// visible rectangle is not published.
    fn collect_a11y_links(
        &self,
        shaper: &dyn LineShaper,
        content_generation: u64,
        scale: f64,
        window_width: f32,
    ) -> Vec<A11yLink> {
        #[cfg(test)]
        count(&COLLECTOR_CALLS);
        let body_top = self.theme.header_height + self.document_find_reserved_height();
        let viewport = LogicalRect {
            x0: self.main_column_left,
            y0: body_top,
            x1: window_width,
            y1: body_top + self.viewport_height,
        };
        let text_left = self.main_column_left + self.theme.line_horizontal_padding;
        let blocks = self
            .line_owners
            .values()
            .map(|(block_id, _)| *block_id)
            .collect::<HashSet<_>>();
        let mut fragments = Vec::new();
        for block_id in blocks {
            let (Some(layout), Some(visual)) = (
                self.layout_cache.get(&block_id).map(|entry| &entry.layout),
                self.block_cache.get(&block_id),
            ) else {
                continue;
            };
            for (row_index, row) in layout.lines.iter().enumerate() {
                let owned = self
                    .line_owners
                    .get(&(row.line_id as usize))
                    .is_some_and(|(owner, _)| *owner == block_id);
                if !owned || row.height <= 0.0 {
                    continue;
                }
                let Some(line) = visual.lines.get(row.line) else {
                    continue;
                };
                if line.links.is_empty() {
                    continue;
                }
                let Some(row_content_y) = self.content_y_for_row(block_id, row_index) else {
                    continue;
                };
                let row_top = body_top + row_content_y - self.scroll_y;
                if row_top + row.height <= viewport.y0 || row_top >= viewport.y1 {
                    continue;
                }
                for link in &line.links {
                    let Some(visual_range) =
                        layout.visual_range_on_row(visual, row_index, link.source_range)
                    else {
                        continue;
                    };
                    let Some(span) =
                        layout.visual_span_on_row(visual, row_index, visual_range.clone(), shaper)
                    else {
                        continue;
                    };
                    let full = LogicalRect {
                        x0: text_left + span.x0,
                        y0: row_top + span.y,
                        x1: text_left + span.x1,
                        y1: row_top + span.y + span.height,
                    };
                    let Some(visible) = full.intersect(viewport) else {
                        continue;
                    };
                    let Some(text) = line.visual_text.get(visual_range) else {
                        continue;
                    };
                    fragments.push(LinkFragment {
                        link_text_range: link.link_text_range,
                        destination: link.destination.clone(),
                        order: (row_content_y, span.x0),
                        text: text.to_owned(),
                        rect: visible,
                        clipped: visible != full,
                    });
                }
            }
        }
        fragments.sort_by(|a, b| {
            (a.link_text_range.start.0, a.link_text_range.end.0)
                .cmp(&(b.link_text_range.start.0, b.link_text_range.end.0))
                .then(a.order.0.total_cmp(&b.order.0))
                .then(a.order.1.total_cmp(&b.order.1))
        });
        let mut links = Vec::new();
        let mut rest = fragments.as_slice();
        while let Some(first) = rest.first() {
            let group_len = rest
                .iter()
                .take_while(|fragment| fragment.link_text_range == first.link_text_range)
                .count();
            let (group, tail) = rest.split_at(group_len);
            rest = tail;
            let label = link_label(&group.iter().map(|f| f.text.as_str()).collect::<Vec<_>>());
            if label.is_empty() {
                continue;
            }
            let rect = group
                .iter()
                .skip(1)
                .fold(first.rect, |rect, fragment| rect.union(fragment.rect));
            links.push(A11yLink {
                key: A11yNodeKey {
                    kind: A11yNodeKind::Link,
                    content_generation,
                    link_start: first.link_text_range.start.0,
                    link_end: first.link_text_range.end.0,
                },
                link_text_range: first.link_text_range,
                destination: first.destination.clone(),
                label,
                partial: group.iter().any(|fragment| fragment.clipped),
                bounds: accesskit::Rect {
                    x0: f64::from(rect.x0) * scale,
                    y0: f64::from(rect.y0) * scale,
                    x1: f64::from(rect.x1) * scale,
                    y1: f64::from(rect.y1) * scale,
                },
            });
        }
        links
    }

    /// Whether the presentation drawn last still has exactly this link.
    fn a11y_link_is_current(&self, request: &A11yLinkRequest) -> bool {
        let blocks = self
            .line_owners
            .values()
            .map(|(block_id, _)| *block_id)
            .collect::<HashSet<_>>();
        blocks.into_iter().any(|block_id| {
            self.block_cache.get(&block_id).is_some_and(|visual| {
                visual.lines.iter().any(|line| {
                    line.links.iter().any(|link| {
                        link.link_text_range == request.link_text_range
                            && link.destination == request.destination
                    })
                })
            })
        })
    }

    /// Opens the link a screen reader asked for, if the request still names a
    /// current link of the current document content. Anything else does
    /// nothing at all. Returns whether the link was opened.
    pub(super) fn activate_a11y_link(
        &mut self,
        request: &A11yLinkRequest,
        cx: &mut Context<Self>,
    ) -> bool {
        let content_is_current = request.document == self.document_key()
            && self.a11y.content_generation == request.content_generation
            && self.a11y.stamp == Some(self.a11y_content_stamp());
        if !content_is_current
            || self.settings_open
            || self.tab_close_confirm.is_some()
            || !self.a11y_link_is_current(request)
        {
            return false;
        }
        cx.open_url(&request.destination);
        true
    }
}

/// The editor body as the document root. Layout, prepaint and paint belong to
/// the wrapped body; this only names it and adds the link nodes.
pub(super) struct A11yDocument {
    child: AnyElement,
    frame: Option<A11yDocumentFrame>,
    view: gpui::WeakEntity<EditorView>,
}

pub(super) trait A11yDocumentExt: IntoElement + Sized {
    fn a11y_document(
        self,
        frame: Option<A11yDocumentFrame>,
        view: gpui::WeakEntity<EditorView>,
    ) -> A11yDocument {
        A11yDocument {
            child: self.into_any_element(),
            frame,
            view,
        }
    }
}

impl<T: IntoElement> A11yDocumentExt for T {}

impl IntoElement for A11yDocument {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for A11yDocument {
    type RequestLayoutState = ();
    /// The nodes pushed this frame, so paint registers a handler for exactly
    /// the ids the tree contains.
    type PrepaintState = Vec<(accesskit::NodeId, A11yLinkRequest)>;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::from(DOCUMENT_ELEMENT_ID))
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn a11y_role(&self) -> Option<accesskit::Role> {
        Some(accesskit::Role::Document)
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        if let Some(frame) = &self.frame {
            node.set_label(frame.name.clone());
            node.set_description(frame.description.to_owned());
        }
    }

    fn a11y_synthetic_children(
        &mut self,
        prepaint: &mut Self::PrepaintState,
        builder: &mut A11ySubtreeBuilder<'_>,
    ) {
        let Some(frame) = &self.frame else {
            return;
        };
        for link in &frame.links {
            let id = builder.synthetic_node_id(link.key);
            let mut node = accesskit::Node::new(accesskit::Role::Link);
            node.set_label(link.label.clone());
            if link.partial {
                node.set_description("リンクの一部のみ表示されています".to_owned());
            }
            node.set_bounds(link.bounds);
            node.add_action(accesskit::Action::Click);
            if builder.push_child(id, node) {
                prepaint.push((id, frame.request_for(link)));
            }
        }
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
        Vec::new()
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
        // Listeners are cleared at the start of every frame, so they are
        // registered again here for the ids this frame published.
        for (node_id, request) in prepaint.iter() {
            #[cfg(test)]
            count(&HANDLER_REGISTRATIONS);
            let view = self.view.clone();
            let request = request.clone();
            window.on_a11y_action(
                *node_id,
                accesskit::Action::Click,
                move |_, _, cx| {
                    let _ = view.update(cx, |view, cx| view.activate_a11y_link(&request, cx));
                },
            );
        }
    }
}
