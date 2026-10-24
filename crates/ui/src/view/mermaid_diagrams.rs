//! Mermaid diagrams in the editor view (Issue #587).
//!
//! A fenced code block whose language is `mermaid` is shown as one picture while
//! nothing edits it. This file is the connection between the pure parts
//! (`crate::mermaid`, `hane_presentation`) and the view: it reads the code blocks
//! that reach the viewport, starts and finishes background renders, tells
//! presentation when a block may be shown as a diagram, and turns a click on the
//! picture into an edit position.
//!
//! The Markdown text is never touched. Whether a block is a picture or source is
//! decided every frame from the same editor state that opens Markdown markers
//! (caret, selection, composition, the find bar's current match), so drawing,
//! cache checks, movement and hit testing cannot disagree.

use super::*;
use crate::line::lines_are_disclosed;
use crate::mermaid::{
    BLOCK_READ_SLACK_BYTES, BLOCK_READ_SLACK_LINES, DiagramPaint, Finished, JobControl,
    MAX_INPUT_BYTES, MAX_INPUT_LINES, MermaidFailure, Palette, Recognition, Recognized, RenderJob,
    RenderedDiagram, classify_fence, head_mentions_mermaid, render_diagram,
};
use crate::theme::DARK_THEME;
use gpui::Div;
use hane_markdown::{Confidence, mermaid_fence};
use hane_presentation::{
    BlockKind, DiagramPlan, LineContext, block_line_context, diagram_display_size,
    diagram_height, diagram_owner_line, diagram_row_height,
};

/// How much of a block's start is read to decide whether it could be a Mermaid
/// fence at all.
const HEAD_BYTES: usize = 4 * 1024;

/// The first lines of a block, and the range they cover. Never more than
/// [`HEAD_BYTES`], so a huge block is looked at without being copied.
fn block_head(document: &RopeBuffer, span: &Range<usize>) -> Option<(SourceRange, String)> {
    let first = document.line_range(LineId(span.start)).ok()?;
    if first.end.0 - first.start.0 > HEAD_BYTES {
        return None;
    }
    let mut end = first.end;
    for line in span.start + 1..(span.start + BLOCK_READ_SLACK_LINES).min(span.end) {
        let Ok(range) = document.line_range(LineId(line)) else {
            break;
        };
        if range.end.0 - first.start.0 > HEAD_BYTES {
            break;
        }
        end = range.end;
    }
    let range = SourceRange::new(first.start.0, end.0);
    let text = document.text(range).ok()?;
    Some((range, text))
}

/// The physical lines a source range covers.
fn construct_lines(document: &RopeBuffer, range: SourceRange) -> Option<Range<usize>> {
    if range.is_empty() {
        return None;
    }
    let first = document.line_for_offset(range.start).ok()?.0;
    let last = document
        .line_for_offset(SourceOffset(range.end.0 - 1))
        .ok()?
        .0;
    Some(first..last + 1)
}

/// Whether a cached presentation shows the diagram `plan` describes, or shows
/// none when there is no plan.
pub(super) fn diagram_plan_matches(cached: &VisualBlock, plan: Option<&DiagramPlan>) -> bool {
    match (diagram_owner_line(cached), diagram_height(cached), plan) {
        (None, _, None) => true,
        (Some(owner), Some(height), Some(plan)) => owner == plan.lines.start && height == plan.height,
        _ => false,
    }
}

impl EditorView {
    fn mermaid_dark(&self) -> bool {
        self.theme == DARK_THEME
    }

    /// Hands the window the pictures that were dropped since the last frame, so
    /// its copy is freed together with ours.
    pub(super) fn begin_mermaid_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for retired in self.mermaid.begin_frame() {
            let (image, _reservation) = retired.into_parts();
            cx.drop_image(image, Some(&mut *window));
        }
    }

    /// Reads the code blocks that reach the viewport and starts the renders they
    /// need. Reading is remembered per block and block revision, so an
    /// unchanged block costs a lookup. A diagram being edited is not rendered:
    /// the picture is made when the edit position leaves it, and the source
    /// stays on screen until it is ready.
    pub(super) fn schedule_mermaid(&mut self, blocks: &[IndexedBlock], cx: &mut Context<Self>) {
        let dark = self.mermaid_dark();
        // Blocks whose own entry already matches, and blocks that still need one.
        let mut claimed = HashSet::new();
        let mut wanted = Vec::new();
        for block in blocks {
            if block_line_context(block.kind) != LineContext::FencedCode {
                continue;
            }
            if self
                .mermaid
                .current(block.id, block.revision, block.source_range)
                .is_none()
            {
                let recognized = self.recognize_mermaid_block(block);
                self.mermaid.remember(block.id, recognized);
            }
            self.mermaid.touch(block.id);
            if block.confidence != Confidence::Formal {
                continue;
            }
            let Some((code, lines)) = self
                .mermaid
                .current(block.id, block.revision, block.source_range)
                .and_then(|known| match &known.recognition {
                    Recognition::Diagram(code) => Some((Arc::clone(code), known.lines.clone())),
                    _ => None,
                })
            else {
                continue;
            };
            if self.mermaid.has_entry(block.id, &code, dark) {
                claimed.insert(block.id);
            } else {
                wanted.push((block.id, code, lines));
            }
        }
        for (id, code, lines) in wanted {
            if lines_are_disclosed(
                self.editor(),
                &lines,
                self.document_find_current_match(),
                self.disclosure_policy(),
            ) {
                continue;
            }
            // A finished picture for the same text under another id (ids move
            // when a block is added above) is taken over instead of redrawn.
            if self.mermaid.adopt(id, &code, dark, &claimed) {
                claimed.insert(id);
                continue;
            }
            self.start_mermaid_job(id, code, dark, cx);
        }
    }

    /// What reading one block says about Mermaid. The parser decides whether it
    /// is a fenced `mermaid` block; a block too large to copy is only looked at
    /// from its start.
    fn recognize_mermaid_block(&self, block: &IndexedBlock) -> Recognized {
        let document = self.editor().document();
        let span = block_line_span(document, block).unwrap_or(0..0);
        let known = |lines: Range<usize>, recognition: Recognition| Recognized {
            revision: block.revision,
            source_range: block.source_range,
            lines,
            recognition,
        };
        let Some((head_range, head)) =
            block_head(document, &span).filter(|(_, head)| head_mentions_mermaid(head))
        else {
            return known(span, Recognition::Plain);
        };
        let bytes = block
            .source_range
            .end
            .0
            .saturating_sub(block.source_range.start.0);
        if bytes > MAX_INPUT_BYTES + BLOCK_READ_SLACK_BYTES
            || span.len() > MAX_INPUT_LINES + BLOCK_READ_SLACK_LINES
        {
            let recognition = if mermaid_fence(head_range, &head).is_some() {
                Recognition::Rejected(MermaidFailure::TooLarge)
            } else {
                Recognition::Plain
            };
            return known(span, recognition);
        }
        let Ok(text) = document.text(block.source_range) else {
            return known(span, Recognition::Plain);
        };
        let fence = mermaid_fence(block.source_range, &text);
        let lines = fence
            .as_ref()
            .and_then(|fence| construct_lines(document, fence.construct_range))
            .unwrap_or(span);
        known(lines, classify_fence(fence.as_ref()))
    }

    /// Starts rendering one diagram off the UI thread, stopping whatever the
    /// block had before. Nothing starts while every slot is busy; the next
    /// frame asks again once a job has finished.
    fn start_mermaid_job(
        &mut self,
        id: BlockId,
        code: Arc<str>,
        dark: bool,
        cx: &mut Context<Self>,
    ) {
        let control = JobControl::new();
        let Some(token) = self.mermaid.begin(id, Arc::clone(&code), dark, control.clone()) else {
            return;
        };
        let job = RenderJob {
            code,
            palette: Palette::from_theme(&self.theme, dark),
            control,
            budget: self.mermaid.budget(),
            svg_renderer: cx.svg_renderer(),
        };
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { render_diagram(&job) })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_mermaid_job(id, token, result, cx));
        })
        .detach();
    }

    /// Takes a finished job's result. A result whose entry was replaced or
    /// removed in the meantime (another edit, a tab switch, a reload, a deleted
    /// block, a theme change) is dropped; the slot is freed either way.
    fn finish_mermaid_job(
        &mut self,
        id: BlockId,
        token: u64,
        result: Result<RenderedDiagram, MermaidFailure>,
        cx: &mut Context<Self>,
    ) {
        self.mermaid.job_ended();
        if self.mermaid.finish(id, token, result) == Finished::Updated {
            // Presentation, rows and the measured height all depended on the old
            // answer; the next frame builds and measures them again.
            self.block_cache.remove(&id);
            self.layout_cache.remove(&id);
        }
        cx.notify();
    }

    /// The plan for showing `block` as a diagram right now, or `None` when its
    /// source has to be shown: no formal block, no finished picture for the
    /// exact current text and palette, or an edit position, selection,
    /// composition or find match on the construct.
    pub(super) fn diagram_plan_for(&self, block: &IndexedBlock) -> Option<DiagramPlan> {
        if block.confidence != Confidence::Formal {
            return None;
        }
        let known = self
            .mermaid
            .current(block.id, block.revision, block.source_range)?;
        let Recognition::Diagram(code) = &known.recognition else {
            return None;
        };
        let ready = self.mermaid.ready(block.id, code, self.mermaid_dark())?;
        if lines_are_disclosed(
            self.editor(),
            &known.lines,
            self.document_find_current_match(),
            self.disclosure_policy(),
        ) {
            return None;
        }
        let (width, height) = diagram_display_size(ready.natural(), self.content_width, self.zoom);
        if !(width > 0.0 && height > 0.0) {
            return None;
        }
        Some(DiagramPlan {
            lines: known.lines.clone(),
            height: diagram_row_height(height, self.zoom),
        })
    }

    /// The picture for a presented block's owning row, if it shows one.
    pub(super) fn diagram_paint(&self, visual: &VisualBlock) -> Option<DiagramPaint> {
        diagram_owner_line(visual)?;
        self.mermaid.paint(visual.id)
    }

    /// The short reason a block's source is shown instead of a picture, placed
    /// at the top right of the block.
    pub(super) fn mermaid_note_element(&self, visual: &VisualBlock) -> Option<Div> {
        let note = self
            .mermaid
            .note(visual.id, visual.source_range, self.mermaid_dark())?;
        Some(
            div()
                .absolute()
                .top(px(2.0 * self.zoom))
                .right(px(self.theme.line_horizontal_padding))
                .text_size(px(11.0 * self.zoom))
                .text_color(rgb(self.theme.quote_foreground))
                .child(note),
        )
    }

    /// A press on the row that owns a diagram. Editing mode puts the edit
    /// position at the start of the diagram's text, which opens the whole fenced
    /// block as source; reading mode ignores the press. Returns whether the row
    /// was a diagram, so the press is not also read as an edit of some other
    /// line.
    pub(super) fn click_diagram_row(&mut self, line: usize, cx: &mut Context<Self>) -> bool {
        let target = self
            .line_owners
            .get(&line)
            .copied()
            .and_then(|(id, at)| self.block_cache.get(&id)?.lines.get(at))
            .filter(|row| row.kind == BlockKind::Diagram)
            .map(|row| row.source_range.end);
        let Some(target) = target else {
            return false;
        };
        if !self.body_editable() {
            return true;
        }
        match self.editor_mut().set_selection(Selection::caret(target)) {
            Ok(()) => {
                self.status = None;
                self.after_input(cx);
            }
            Err(error) => self.report_error("diagram selection", error),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    /// Lines: 0 intro, 1 blank, 2 fence, 3-4 body, 5 closing fence, 6 blank, 7 outro.
    const DOC: &str =
        "前の段落\n\n```mermaid\nflowchart TD\n  A[開始] --> B[終了]\n```\n\n後の段落\n";

    fn body_start() -> usize {
        DOC.find("flowchart").expect("body")
    }

    /// Lets the formal parse, the render jobs and the frames they ask for run.
    fn settle(cx: &mut TestCx) {
        for _ in 0..4 {
            cx.run_until_parked();
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
        }
        cx.run_until_parked();
    }

    fn open<'a>(cx: &'a mut gpui::TestAppContext, source: &str) -> (TestView, &'a mut TestCx) {
        cx.update(crate::actions::register_key_bindings);
        let source = source.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&source, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(900.0), px(700.0)));
        settle(cx);
        (view, cx)
    }

    fn block_at(view: &TestView, cx: &mut TestCx, offset: usize) -> IndexedBlock {
        view.read_with(cx, |view, _| {
            view.current_index()
                .expect("a formal block index")
                .block_at(SourceOffset(offset))
                .expect("the fenced block")
        })
    }

    /// The block of the first fence in `source`.
    fn fence_block(view: &TestView, cx: &mut TestCx, source: &str) -> IndexedBlock {
        let at = source
            .find("```mermaid")
            .or_else(|| source.find("~~~"))
            .unwrap_or(0);
        block_at(view, cx, at)
    }

    fn presented(view: &TestView, cx: &mut TestCx, id: BlockId) -> Option<VisualBlock> {
        view.read_with(cx, |view, _| view.block_cache.get(&id).cloned())
    }

    fn is_diagram(view: &TestView, cx: &mut TestCx, id: BlockId) -> bool {
        presented(view, cx, id).is_some_and(|visual| diagram_owner_line(&visual).is_some())
    }

    fn jobs_started(view: &TestView, cx: &mut TestCx) -> u64 {
        view.read_with(cx, |view, _| view.mermaid.jobs_started())
    }

    fn text(view: &TestView, cx: &mut TestCx) -> String {
        view.read_with(cx, |view, _| {
            let document = view.editor().document();
            document
                .text(SourceRange::new(0, document.len_bytes().0))
                .expect("document text")
        })
    }

    fn select(view: &TestView, cx: &mut TestCx, offset: usize) {
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(offset)))
                .expect("selection");
            view.after_input(cx);
        });
        settle(cx);
    }

    #[gpui::test]
    fn a_mermaid_fence_is_one_diagram_and_the_text_is_untouched(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open(cx, DOC);
        let block = fence_block(&view, cx, DOC);
        let visual = presented(&view, cx, block.id).expect("presented");
        assert_eq!(diagram_owner_line(&visual), Some(2));
        let heights = visual.lines.iter().map(VisualLine::height).collect::<Vec<_>>();
        assert_eq!(heights.len(), 5, "every physical line stays in the block");
        assert!(heights[0] > 0.0);
        assert_eq!(&heights[1..4], &[0.0, 0.0, 0.0]);
        assert!(heights[4] > 0.0, "the blank line after the fence is not folded");
        // The block's laid-out height is the one the height index holds.
        let (layout_height, indexed_height) = view.read_with(cx, |view, _| {
            (
                view.layout_cache.get(&block.id).map(|entry| entry.layout.height()),
                view.heights.height(block.ordinal),
            )
        });
        assert_eq!(layout_height, indexed_height);
        assert!((layout_height.unwrap_or_default() - visual.height()).abs() < 0.01);
        // Showing the picture changed nothing about the document.
        assert_eq!(text(&view, cx), DOC);
        let (dirty, undo) = view.read_with(cx, |view, _| {
            (view.sessions.active().is_dirty(), view.editor().can_undo())
        });
        assert!(!dirty && !undo);
        assert_eq!(jobs_started(&view, cx), 1);
    }

    #[gpui::test]
    fn clicking_the_picture_opens_the_whole_source_and_leaving_restores_the_picture(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open(cx, DOC);
        let block = fence_block(&view, cx, DOC);
        assert!(is_diagram(&view, cx, block.id));

        let handled = view.update(cx, |view, cx| view.click_diagram_row(2, cx));
        assert!(handled);
        settle(cx);
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        assert_eq!(selection, Selection::caret(SourceOffset(body_start())));
        assert!(!is_diagram(&view, cx, block.id), "the source is shown");
        assert_eq!(text(&view, cx), DOC);

        // The same press on a row that is not a diagram is not handled.
        let other = view.update(cx, |view, cx| view.click_diagram_row(0, cx));
        assert!(!other);

        select(&view, cx, 0);
        assert!(is_diagram(&view, cx, block.id));
        assert_eq!(jobs_started(&view, cx), 1, "an unchanged diagram is reused");
        assert_eq!(text(&view, cx), DOC);
        let (dirty, undo) = view.read_with(cx, |view, _| {
            (view.sessions.active().is_dirty(), view.editor().can_undo())
        });
        assert!(!dirty && !undo);
    }

    #[gpui::test]
    fn any_edit_position_selection_or_composition_on_the_construct_shows_the_source(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open(cx, DOC);
        let block = fence_block(&view, cx, DOC);
        let fence = DOC.find("```mermaid").unwrap();
        let closing = DOC.rfind("```").unwrap();
        for offset in [fence, fence + 3, body_start() + 4, closing, closing + 2] {
            select(&view, cx, offset);
            assert!(!is_diagram(&view, cx, block.id), "caret at {offset}");
            select(&view, cx, 0);
            assert!(is_diagram(&view, cx, block.id), "caret back at 0");
        }
        // The blank line after the closing fence is outside the construct.
        select(&view, cx, DOC.find("\n\n後").unwrap() + 1);
        assert!(is_diagram(&view, cx, block.id));
        // A selection that merely touches the construct opens it.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(fence + 1),
                })
                .expect("selection");
            view.after_input(cx);
        });
        settle(cx);
        assert!(!is_diagram(&view, cx, block.id));
        select(&view, cx, 0);
        assert!(is_diagram(&view, cx, block.id));
        assert_eq!(jobs_started(&view, cx), 1);
    }

    #[gpui::test]
    fn an_edited_diagram_is_drawn_again_only_after_the_edit_position_leaves(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open(cx, DOC);
        let block = fence_block(&view, cx, DOC);
        select(&view, cx, DOC.find("終了").unwrap() + "終".len());
        assert!(!is_diagram(&view, cx, block.id));
        view.update(cx, |view, cx| view.insert_text("x", cx));
        settle(cx);
        assert_eq!(
            jobs_started(&view, cx),
            1,
            "nothing is rendered while the diagram is being edited"
        );
        // Away from the diagram: the old picture is never shown for new text.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(0)))
                .expect("selection");
            view.after_input(cx);
            let block = view
                .current_index()
                .and_then(|index| index.block_at(SourceOffset(DOC.find("```mermaid").unwrap())))
                .expect("the fenced block");
            assert!(view.diagram_plan_for(&block).is_none());
        });
        settle(cx);
        assert_eq!(jobs_started(&view, cx), 2);
        let block = fence_block(&view, cx, DOC);
        assert!(is_diagram(&view, cx, block.id));
        let edited = text(&view, cx);
        assert!(edited.contains("終x了"));
        assert_eq!(edited.len(), DOC.len() + 1);
    }

    #[gpui::test]
    fn reading_mode_shows_the_picture_with_the_caret_inside_and_ignores_presses(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open(cx, DOC);
        let block = fence_block(&view, cx, DOC);
        select(&view, cx, body_start() + 2);
        assert!(!is_diagram(&view, cx, block.id));
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.set_editor_mode(EditorMode::Reading, window, cx));
            });
        });
        settle(cx);
        assert!(is_diagram(&view, cx, block.id), "reading mode shows diagrams");
        let before = view.read_with(cx, |view, _| view.editor().selection());
        let handled = view.update(cx, |view, cx| view.click_diagram_row(2, cx));
        assert!(handled, "the press belongs to the diagram");
        settle(cx);
        assert_eq!(view.read_with(cx, |view, _| view.editor().selection()), before);
        assert!(is_diagram(&view, cx, block.id));
        assert_eq!(text(&view, cx), DOC);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.set_editor_mode(EditorMode::Editing, window, cx));
            });
        });
        settle(cx);
        assert!(!is_diagram(&view, cx, block.id), "the caret is inside again");
    }

    #[gpui::test]
    fn blocks_that_cannot_be_drawn_stay_source_with_a_reason_and_are_not_retried(
        cx: &mut gpui::TestAppContext,
    ) {
        for (source, failure) in [
            (
                "```mermaid\npie title x\n  \"a\": 1\n```\n",
                MermaidFailure::Unsupported,
            ),
            (
                "```mermaid\nflowchart TD\n  A --> B\n",
                MermaidFailure::Unclosed,
            ),
        ] {
            let (view, cx) = open(cx, source);
            let block = fence_block(&view, cx, source);
            assert!(!is_diagram(&view, cx, block.id), "{source:?}");
            let note = view.read_with(cx, |view, _| {
                view.mermaid.note(block.id, block.source_range, view.mermaid_dark())
            });
            assert_eq!(note, Some(failure.reason()), "{source:?}");
            assert_eq!(jobs_started(&view, cx), 0, "{source:?}");
            for _ in 0..3 {
                view.update(cx, |_, cx| cx.notify());
                settle(cx);
            }
            assert_eq!(jobs_started(&view, cx), 0, "{source:?}");
            assert_eq!(text(&view, cx), source);
        }
    }

    #[gpui::test]
    fn a_description_merman_rejects_stays_source_and_is_rendered_once(
        cx: &mut gpui::TestAppContext,
    ) {
        // The caret starts at offset 0, so a leading paragraph keeps it off the fence.
        let source = "前の段落\n\n```mermaid\nflowchart TD\n  A[unclosed --> B\n```\n";
        let (view, cx) = open(cx, source);
        let block = fence_block(&view, cx, source);
        assert!(!is_diagram(&view, cx, block.id));
        assert_eq!(jobs_started(&view, cx), 1);
        let note = view.read_with(cx, |view, _| {
            view.mermaid.note(block.id, block.source_range, view.mermaid_dark())
        });
        assert!(note.is_some(), "a reason is shown beside the source");
        for _ in 0..3 {
            view.update(cx, |_, cx| cx.notify());
            settle(cx);
        }
        assert_eq!(jobs_started(&view, cx), 1, "the same failure is not retried");
        assert_eq!(text(&view, cx), source);
    }

    #[gpui::test]
    fn other_code_and_nested_fences_are_never_read_as_diagrams(cx: &mut gpui::TestAppContext) {
        for source in [
            "```rust\nfn main() {}\n```\n",
            "> ```mermaid\n> flowchart TD\n>   A --> B\n> ```\n",
            "- item\n\n  ```mermaid\n  flowchart TD\n    A --> B\n  ```\n",
        ] {
            let (view, cx) = open(cx, source);
            assert_eq!(jobs_started(&view, cx), 0, "{source:?}");
            let any_diagram = view.read_with(cx, |view, _| {
                view.block_cache
                    .values()
                    .any(|visual| diagram_owner_line(visual).is_some())
            });
            assert!(!any_diagram, "{source:?}");
        }
    }

    #[gpui::test]
    fn a_tilde_fence_and_a_second_diagram_are_both_drawn(cx: &mut gpui::TestAppContext) {
        // The caret starts at offset 0, so a leading paragraph keeps it off the fences.
        let source = "前の段落\n\n~~~ Mermaid\nflowchart TD\n  A --> B\n~~~\n\n```mermaid\nsequenceDiagram\n  太郎->>花子: こんにちは\n```\n";
        let (view, cx) = open(cx, source);
        let first = block_at(&view, cx, source.find("~~~").unwrap());
        let second = block_at(&view, cx, source.find("```mermaid").unwrap());
        assert_ne!(first.id, second.id);
        assert!(is_diagram(&view, cx, first.id));
        assert!(is_diagram(&view, cx, second.id));
        assert_eq!(jobs_started(&view, cx), 2);
    }

    #[gpui::test]
    fn the_picture_follows_the_text_column_width_without_rendering_again(
        cx: &mut gpui::TestAppContext,
    ) {
        // The caret starts at offset 0, so a leading paragraph keeps it off the fence.
        let source = "前の段落\n\n```mermaid\nflowchart LR\n  A[とても長いラベルの開始] --> B[とても長いラベルの途中] --> C[とても長いラベルの終了]\n```\n";
        let (view, cx) = open(cx, source);
        let block = fence_block(&view, cx, source);
        let wide = presented(&view, cx, block.id)
            .and_then(|visual| diagram_height(&visual))
            .expect("diagram height");
        cx.simulate_resize(gpui::size(px(240.0), px(700.0)));
        settle(cx);
        let narrow = presented(&view, cx, block.id)
            .and_then(|visual| diagram_height(&visual))
            .expect("diagram height");
        assert!(narrow < wide, "{narrow} < {wide}: the picture scales to the column");
        assert_eq!(jobs_started(&view, cx), 1, "a width change never renders again");
        let expected = view.read_with(cx, |view, _| view.layout_cache.get(&block.id).map(|e| e.layout.height()));
        let indexed = view.read_with(cx, |view, _| view.heights.height(block.ordinal));
        assert_eq!(expected, indexed);
    }
}
