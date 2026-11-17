//! Plain-text mode (Issue #737): the source is shown and edited as it is, one
//! physical line per row, with no marker hiding, block conversion or
//! disclosure. It is an axis of its own, independent of `EditorMode` (whether
//! the body can be changed), and one app-wide value stored in
//! `Settings::plain_text_mode`. Sessions, editors and the save path never see
//! it; only presentation, heights and a few editing shortcuts do.

use super::reading_mode::MODE_SWITCH_BLOCKED_BY_COMPOSITION;
use super::*;
use gpui_component::Icon;

/// What Tab inserts, and Shift+Tab removes up to, in plain-text mode.
const PLAIN_INDENT: &str = "    ";

/// One line's change in a block indent / outdent: `removed` bytes at `start`
/// are dropped and `inserted` bytes are added there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineEdit {
    start: usize,
    removed: usize,
    inserted: usize,
}

/// Where `offset` lands once `edits` (ascending, non-overlapping) are applied:
/// shifted by what was added or removed at or before it, never left of the
/// line start an edit removed text from.
fn shift_offset(offset: usize, edits: &[LineEdit]) -> usize {
    let mut added = 0;
    let mut removed = 0;
    for edit in edits {
        if edit.start > offset {
            break;
        }
        added += edit.inserted;
        removed += edit.removed.min(offset - edit.start);
    }
    offset + added - removed
}

/// Leading bytes Shift+Tab removes from one line: one tab, or up to
/// [`PLAIN_INDENT`]'s width in spaces.
fn outdent_width(line: &str) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    line.bytes()
        .take(PLAIN_INDENT.len())
        .take_while(|byte| *byte == b' ')
        .count()
}

impl EditorView {
    pub(crate) fn plain_text(&self) -> bool {
        self.settings.plain_text_mode
    }

    pub(crate) fn toggle_plain_text(&mut self, cx: &mut Context<Self>) -> bool {
        self.set_plain_text(!self.plain_text(), cx)
    }

    /// Switches plain-text mode on or off. Returns whether the mode is now the
    /// requested one. Only the display changes: the document, its history, the
    /// selection and the tabs are untouched, and the scroll position is carried
    /// by the same source-row anchor the reading-mode switch uses.
    pub(crate) fn set_plain_text(&mut self, plain: bool, cx: &mut Context<Self>) -> bool {
        if self.plain_text() == plain {
            return true;
        }
        if self.editor_mode_switch_blocked() {
            // The marked text is neither committed nor dropped here; the
            // platform ends it, after which the switch is available again.
            self.status = Some(MODE_SWITCH_BLOCKED_BY_COMPOSITION.to_owned());
            cx.notify();
            return false;
        }
        // A drag or autoscroll belongs to the geometry of the old display.
        self.cancel_text_selection_autoscroll();
        // A Paste started under the old display may not finish under the new
        // one: plain-text mode never converts clipboard HTML.
        self.invalidate_clipboard_job();
        self.settings.plain_text_mode = plain;
        self.rebuild_presentation_for_mode_change();
        // A failed write keeps the switch for this run, like the theme.
        self.store_settings();
        cx.notify();
        true
    }

    /// Whether the plain-text display colors Markdown syntax (Issue #738). An
    /// axis of its own: the value is kept while the Markdown display is shown.
    pub(crate) fn syntax_highlight(&self) -> bool {
        self.settings.syntax_highlight
    }

    /// Turns syntax coloring on or off. Returns whether it is now the requested
    /// value. Only `style_runs` of the plain display change, never the text,
    /// the selection or the history. While the Markdown display is shown the
    /// value is just stored: nothing visible depends on it.
    pub(crate) fn set_syntax_highlight(&mut self, on: bool, cx: &mut Context<Self>) -> bool {
        if self.syntax_highlight() == on {
            return true;
        }
        if self.editor_mode_switch_blocked() {
            // The marked text is neither committed nor dropped here; the
            // platform ends it, after which the switch is available again.
            self.status = Some(MODE_SWITCH_BLOCKED_BY_COMPOSITION.to_owned());
            cx.notify();
            return false;
        }
        self.settings.syntax_highlight = on;
        if self.plain_text() {
            // Bold changes where lines wrap, so the layouts and measured
            // heights go with the cached presentations.
            self.cancel_text_selection_autoscroll();
            self.rebuild_presentation_for_mode_change();
        }
        // A failed write keeps the change for this run, like the theme.
        self.store_settings();
        cx.notify();
        true
    }

    /// Whether Tab / Shift+Tab may change the text as plain indentation now.
    fn plain_indent_allowed(&self) -> bool {
        self.plain_text()
            && self.body_editable()
            && self.editor().ime().is_none()
            && !self.late_input_guard_armed()
    }

    /// Tab in plain-text mode: four spaces at the caret, or four spaces at the
    /// start of every line a selection touches (one undo step).
    pub(crate) fn plain_indent(&mut self, cx: &mut Context<Self>) {
        if !self.plain_indent_allowed() {
            return;
        }
        if self.editor().selection().range().is_empty() {
            self.insert_text(PLAIN_INDENT, cx);
            return;
        }
        self.change_selected_lines(
            |line| {
                // Blank lines stay blank rather than gaining trailing spaces.
                if line.trim_end_matches(['\r', '\n']).is_empty() {
                    (0, "")
                } else {
                    (0, PLAIN_INDENT)
                }
            },
            cx,
        );
    }

    /// Shift+Tab in plain-text mode: removes up to four spaces (or one tab)
    /// from the start of the caret line or of every selected line.
    pub(crate) fn plain_outdent(&mut self, cx: &mut Context<Self>) {
        if !self.plain_indent_allowed() {
            return;
        }
        self.change_selected_lines(|line| (outdent_width(line), ""), cx);
    }

    /// Rewrites the lines the selection touches as one undoable replacement and
    /// puts both selection ends back on the same characters.
    ///
    /// `plan` says, for one line (with its line ending), how many leading bytes
    /// to remove and what to put at its start instead. A selection that ends
    /// at the very start of a line does not touch that line, and nothing is
    /// replaced when no line changes.
    fn change_selected_lines(
        &mut self,
        plan: impl Fn(&str) -> (usize, &'static str),
        cx: &mut Context<Self>,
    ) {
        let selection = self.editor().selection();
        let range = selection.range();
        let document = self.editor().document();
        let Ok(first) = document.line_for_offset(range.start) else {
            return;
        };
        let Ok(mut last) = document.line_for_offset(range.end) else {
            return;
        };
        if last.0 > first.0
            && document
                .line_range(last)
                .is_ok_and(|line| line.start == range.end)
        {
            last = LineId(last.0 - 1);
        }
        let mut edits = Vec::new();
        let mut replacement = String::new();
        let (Ok(first_range), Ok(last_range)) =
            (document.line_range(first), document.line_range(last))
        else {
            return;
        };
        for line in first.0..=last.0 {
            let Ok(line_range) = document.line_range(LineId(line)) else {
                return;
            };
            let Ok(text) = document.text(line_range) else {
                return;
            };
            let (removed, inserted) = plan(&text);
            if removed > 0 || !inserted.is_empty() {
                edits.push(LineEdit {
                    start: line_range.start.0,
                    removed,
                    inserted: inserted.len(),
                });
            }
            replacement.push_str(inserted);
            replacement.push_str(&text[removed..]);
        }
        if edits.is_empty() {
            return;
        }
        let selection_after = Selection {
            anchor: SourceOffset(shift_offset(selection.anchor.0, &edits)),
            active: SourceOffset(shift_offset(selection.active.0, &edits)),
        };
        let replaced = SourceRange::new(first_range.start.0, last_range.end.0);
        match self
            .editor_mut()
            .replace_range_recorded(replaced, &replacement, selection_after)
        {
            Ok(_) => self.status = None,
            Err(error) => self.report_error("indent", error),
        }
        self.after_input(cx);
    }

    /// The tab-row switch for plain-text mode: three text lines while the
    /// Markdown display is shown, the Markdown mark while plain text is.
    /// Built like [`Self::reading_mode_button`], so it keeps its size, swallows
    /// the press (a disabled button would otherwise still move focus and end a
    /// composition) and shows its tooltip through a `HoverCard`.
    pub(super) fn plain_text_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (icon, label) = if self.plain_text() {
            (icons::ICON_MARKDOWN_MODE, "Markdown表示に戻す")
        } else {
            (icons::ICON_PLAIN_TEXT_MODE, "プレーンテキスト表示に切り替え")
        };
        let blocked = self.editor_mode_switch_blocked();
        let tooltip = if blocked {
            MODE_SWITCH_BLOCKED_BY_COMPOSITION
        } else {
            label
        };
        let view = cx.entity();
        let button = Button::new("plain-text-toggle")
            .ghost()
            .with_size(px(24.0))
            .icon(Icon::empty().path(icon))
            .accessibility_label(label)
            .disabled(blocked)
            .on_click(move |_, _window, app| {
                view.update(app, |view, cx| {
                    view.toggle_plain_text(cx);
                });
            });
        let wrapper = div()
            .id("plain-text-toggle-wrapper")
            .debug_selector(|| "plain-text-toggle".to_owned())
            .flex_none()
            .px_1()
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(button);
        HoverCard::new("plain-text-toggle-tooltip")
            .anchor(Anchor::TopCenter)
            .trigger(wrapper)
            .content(move |_, _, _| {
                div()
                    .px_2()
                    .py_1()
                    .debug_selector(move || format!("plain-text-toggle-tooltip:{tooltip}"))
                    .child(tooltip)
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    const MIXED: &str = "# Heading\n\n**bold** `code` [t](https://e.com)\n\n- [ ] task\n- item\n\n> quote\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n![alt](a.png)\n";

    fn settle(cx: &mut TestCx) {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
    }

    /// A window over an in-memory state store, so toggling never writes the
    /// real `settings.conf`.
    fn open_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        source: &str,
        stores: StateStores,
    ) -> (TestView, &'a mut TestCx) {
        let source = source.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled(&source, "Untitled"),
                Arc::new(OsFileService),
                stores,
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
        settle(cx);
        (view, cx)
    }

    fn set_plain(view: &TestView, cx: &mut TestCx, plain: bool) -> bool {
        let result = view.update(cx, |view, cx| view.set_plain_text(plain, cx));
        settle(cx);
        result
    }

    fn set_highlight(view: &TestView, cx: &mut TestCx, on: bool) -> bool {
        let result = view.update(cx, |view, cx| view.set_syntax_highlight(on, cx));
        settle(cx);
        result
    }

    /// Every cached line's text and runs, in document order.
    fn cached_lines(
        view: &TestView,
        cx: &mut TestCx,
    ) -> Vec<(u64, String, Vec<hane_presentation::StyleRun>)> {
        view.read_with(cx, |view, _| {
            let mut lines = view
                .block_cache
                .values()
                .flat_map(|block| &block.lines)
                .map(|line| (line.line_id, line.visual_text.clone(), line.style_runs.clone()))
                .collect::<Vec<_>>();
            lines.sort_by_key(|(line_id, _, _)| *line_id);
            lines
        })
    }

    fn runs_of(
        view: &TestView,
        cx: &mut TestCx,
        text: &str,
    ) -> Option<Vec<hane_presentation::StyleRun>> {
        cached_lines(view, cx)
            .into_iter()
            .find(|(_, line, _)| line == text)
            .map(|(_, _, runs)| runs)
    }

    fn has_tone(runs: &[hane_presentation::StyleRun], tone: hane_presentation::SyntaxTone) -> bool {
        runs.iter()
            .any(|run| run.kind == StyleKind::Syntax(tone))
    }

    fn text_of(view: &TestView, cx: &mut TestCx) -> String {
        view.read_with(cx, |view, _| {
            let document = view.editor().document();
            document
                .text(SourceRange::new(0, document.len_bytes().0))
                .unwrap()
        })
    }

    fn snapshot(view: &TestView, cx: &mut TestCx) -> (Revision, usize, bool, bool) {
        view.read_with(cx, |view, _| {
            (
                view.editor().document().revision(),
                view.editor().document().len_bytes().0,
                view.sessions.active().is_dirty(),
                view.editor().can_undo(),
            )
        })
    }

    fn select(view: &TestView, cx: &mut TestCx, anchor: usize, active: usize) {
        view.update(cx, |view, _| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(anchor),
                    active: SourceOffset(active),
                })
                .unwrap();
        });
    }

    fn selection_of(view: &TestView, cx: &mut TestCx) -> (usize, usize) {
        view.read_with(cx, |view, _| {
            let selection = view.editor().selection();
            (selection.anchor.0, selection.active.0)
        })
    }

    fn press_enter(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| {
            if !view.apply_list_edit_intent(ListEditIntent::Enter, cx) {
                view.insert_newline(cx);
            }
        });
    }

    fn press_shift_enter(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| {
            if !view.apply_list_edit_intent(ListEditIntent::ShiftEnter, cx) {
                view.insert_newline(cx);
            }
        });
    }

    fn press_backspace(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| {
            if !view.apply_list_edit_intent(ListEditIntent::Backspace, cx) {
                view.dispatch(EditorCommand::Backspace, cx);
            }
        });
    }

    fn press_tab(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| {
            if !view.apply_list_edit_intent(ListEditIntent::Indent, cx) {
                view.plain_indent(cx);
            }
        });
    }

    fn press_shift_tab(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| {
            if !view.apply_list_edit_intent(ListEditIntent::Outdent, cx) {
                view.plain_outdent(cx);
            }
        });
    }

    fn undo(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
    }

    #[test]
    fn outdent_width_takes_a_tab_or_up_to_four_spaces() {
        assert_eq!(outdent_width("  a"), 2);
        assert_eq!(outdent_width("    a"), 4);
        assert_eq!(outdent_width("      a"), 4);
        assert_eq!(outdent_width("\ta"), 1);
        assert_eq!(outdent_width("a"), 0);
    }

    #[test]
    fn shift_offset_follows_the_same_character() {
        let indent = [
            LineEdit { start: 0, removed: 0, inserted: 4 },
            LineEdit { start: 10, removed: 0, inserted: 4 },
        ];
        assert_eq!(shift_offset(0, &indent), 4);
        assert_eq!(shift_offset(5, &indent), 9);
        assert_eq!(shift_offset(10, &indent), 18);
        let outdent = [LineEdit { start: 10, removed: 4, inserted: 0 }];
        assert_eq!(shift_offset(9, &outdent), 9);
        assert_eq!(shift_offset(12, &outdent), 10);
        assert_eq!(shift_offset(20, &outdent), 16);
    }

    #[gpui::test]
    fn plain_toggle_preserves_document_snapshot(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        view.update(cx, |view, cx| view.insert_text("x", cx));
        let before = snapshot(&view, cx);
        let text = text_of(&view, cx);
        for plain in [true, false, true] {
            assert!(set_plain(&view, cx, plain));
            assert_eq!(snapshot(&view, cx), before);
            assert_eq!(text_of(&view, cx), text);
        }
    }

    #[gpui::test]
    fn plain_toggle_preserves_caret_and_selection_offsets(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        // Inside `**bold**`, inside the table, on the fence row, and a
        // selection spanning several blocks.
        let bold = MIXED.find("bold").unwrap() + 2;
        let cell = MIXED.find("| 1 |").unwrap() + 3;
        let fence = MIXED.find("```rust").unwrap() + 1;
        for (anchor, active) in [
            (bold, bold),
            (cell, cell),
            (fence, fence),
            (3, MIXED.len() - 4),
            (MIXED.len() - 4, 3),
        ] {
            select(&view, cx, anchor, active);
            for plain in [true, false, true] {
                assert!(set_plain(&view, cx, plain));
                assert_eq!(selection_of(&view, cx), (anchor, active));
            }
            assert!(set_plain(&view, cx, false));
        }
    }

    #[gpui::test]
    fn plain_toggle_does_not_enter_undo_history(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "abc", StateStores::memory());
        assert!(!snapshot(&view, cx).3);
        assert!(set_plain(&view, cx, true));
        assert!(!snapshot(&view, cx).3, "a toggle alone is not undoable");
        select(&view, cx, 3, 3);
        view.update(cx, |view, cx| view.insert_text("d", cx));
        assert!(set_plain(&view, cx, false));
        assert!(snapshot(&view, cx).3);
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "abc");
    }

    #[gpui::test]
    fn plain_toggle_is_refused_while_composing(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "hello\n", StateStores::memory());
        view.update(cx, |view, _| {
            view.editor_mut()
                .replace_and_mark_text(None, "あ", Some(1..1))
                .unwrap();
        });
        assert!(!set_plain(&view, cx, true));
        view.read_with(cx, |view, _| {
            assert!(!view.plain_text());
            assert!(view.editor().ime().is_some(), "the marked text is kept");
            assert_eq!(
                view.status.as_deref(),
                Some(MODE_SWITCH_BLOCKED_BY_COMPOSITION)
            );
        });
    }

    #[gpui::test]
    fn plain_mode_survives_reading_round_trip(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        for mode in [EditorMode::Reading, EditorMode::Editing] {
            cx.update(|window, app| {
                view.update(app, |view, cx| {
                    assert!(view.set_editor_mode(mode, window, cx));
                });
            });
            settle(cx);
            view.read_with(cx, |view, _| {
                assert!(view.plain_text());
                assert!(view.disclosure_policy().is_plain());
            });
        }
    }

    #[gpui::test]
    fn plain_toggle_persists_setting(cx: &mut gpui::TestAppContext) {
        let stores = StateStores::memory();
        let (view, cx) = open_view(cx, "a\n", stores.clone());
        assert!(!stores.settings().load().plain_text_mode);
        assert!(set_plain(&view, cx, true));
        assert!(stores.settings().load().plain_text_mode);
        assert!(set_plain(&view, cx, false));
        assert!(!stores.settings().load().plain_text_mode);
    }

    #[gpui::test]
    fn plain_rows_show_source_verbatim(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        // Coloring only adds runs; with it off the rows carry none.
        assert!(set_highlight(&view, cx, false));
        view.read_with(cx, |view, _| {
            let document = view.editor().document();
            let mut rows = 0;
            for visual in view.block_cache.values() {
                for line in &visual.lines {
                    let range = document.line_range(LineId(line.line_id as usize)).unwrap();
                    let source = document.text(range).unwrap();
                    assert_eq!(line.visual_text, source.trim_end_matches(['\r', '\n']));
                    assert!(line.style_runs.is_empty());
                    rows += 1;
                }
            }
            assert!(rows > 0, "the window drew some rows");
        });
    }

    #[gpui::test]
    fn plain_table_block_uses_text_layout(cx: &mut gpui::TestAppContext) {
        let source = "| a | b |\n| --- | --- |\n| 1 | 2 |\n";
        let (view, cx) = open_view(cx, source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        view.read_with(cx, |view, _| {
            let visual = view.block_cache.values().next().expect("table drawn");
            assert_eq!(visual.kind, BlockKind::Unsupported);
            assert!(visual.table_projection.is_none());
            let texts = visual
                .lines
                .iter()
                .map(|line| line.visual_text.as_str())
                .collect::<Vec<_>>();
            assert_eq!(&texts[..3], ["| a | b |", "| --- | --- |", "| 1 | 2 |"]);
        });
    }

    #[gpui::test]
    fn plain_height_index_matches_line_count(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        view.read_with(cx, |view, _| {
            let rows = MIXED.matches('\n').count() + 1;
            let total = view.item_heights().into_iter().sum::<f32>();
            let expected = rows as f32 * view.line_height();
            assert!((total - expected).abs() < 0.01, "{total} vs {expected}");
        });
    }

    #[gpui::test]
    fn plain_enter_in_list_inserts_bare_newline(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "- a", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 3, 3);
        press_enter(&view, cx);
        assert_eq!(text_of(&view, cx), "- a\n");
    }

    #[gpui::test]
    fn plain_enter_on_empty_list_item_keeps_marker(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "- ", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 2, 2);
        press_enter(&view, cx);
        assert_eq!(text_of(&view, cx), "- \n");
    }

    #[gpui::test]
    fn plain_shift_enter_inserts_bare_newline(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "1. a", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 4, 4);
        press_shift_enter(&view, cx);
        assert_eq!(text_of(&view, cx), "1. a\n");
    }

    #[gpui::test]
    fn plain_backspace_after_marker_deletes_one_grapheme(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "- a", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 2, 2);
        press_backspace(&view, cx);
        assert_eq!(text_of(&view, cx), "-a");
    }

    #[gpui::test]
    fn plain_tab_inserts_four_spaces(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "- a", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 3, 3);
        press_tab(&view, cx);
        assert_eq!(text_of(&view, cx), "- a    ");
        assert_eq!(selection_of(&view, cx), (7, 7));
    }

    #[gpui::test]
    fn tab_outside_plain_mode_still_does_nothing_outside_a_list(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "plain", StateStores::memory());
        select(&view, cx, 5, 5);
        let before = snapshot(&view, cx);
        press_tab(&view, cx);
        press_shift_tab(&view, cx);
        assert_eq!(snapshot(&view, cx), before);
    }

    #[gpui::test]
    fn plain_indent_selected_lines_is_one_undo_step(cx: &mut gpui::TestAppContext) {
        let source = "one\ntwo\nthree";
        let (view, cx) = open_view(cx, source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 0, source.len());
        press_tab(&view, cx);
        assert_eq!(text_of(&view, cx), "    one\n    two\n    three");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), source);
    }

    #[gpui::test]
    fn plain_outdent_removes_up_to_four_spaces(cx: &mut gpui::TestAppContext) {
        let source = "  a\n    b\n      c\n\td";
        let (view, cx) = open_view(cx, source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 0, source.len());
        press_shift_tab(&view, cx);
        assert_eq!(text_of(&view, cx), "a\nb\n  c\nd");
    }

    #[gpui::test]
    fn plain_outdent_without_indent_is_noop(cx: &mut gpui::TestAppContext) {
        let source = "a\nb";
        let (view, cx) = open_view(cx, source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 0, source.len());
        let before = snapshot(&view, cx);
        press_shift_tab(&view, cx);
        assert_eq!(snapshot(&view, cx), before);
        assert_eq!(text_of(&view, cx), source);
    }

    #[gpui::test]
    fn plain_indent_keeps_selection_on_same_text(cx: &mut gpui::TestAppContext) {
        let source = "ab\ncd\nef";
        let (view, cx) = open_view(cx, source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 1, 7);
        press_tab(&view, cx);
        let text = text_of(&view, cx);
        let (anchor, active) = selection_of(&view, cx);
        assert_eq!(&text[anchor..active], "b\n    cd\n    e");
    }

    #[gpui::test]
    fn plain_indent_respects_crlf(cx: &mut gpui::TestAppContext) {
        let source = "a\r\nb\r\nc";
        let (view, cx) = open_view(cx, source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 0, 4);
        press_tab(&view, cx);
        assert_eq!(text_of(&view, cx), "    a\r\n    b\r\nc");
    }

    #[gpui::test]
    fn syntax_highlight_toggle_preserves_document_snapshot(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        view.update(cx, |view, cx| view.insert_text("x", cx));
        assert!(set_plain(&view, cx, true));
        let before = snapshot(&view, cx);
        let text = text_of(&view, cx);
        let bold = text.find("bold").unwrap() + 2;
        let fence = text.find("```rust").unwrap() + 1;
        for (anchor, active) in [(bold, bold), (3, text.len() - 4), (fence, fence)] {
            select(&view, cx, anchor, active);
            for on in [false, true, false, true] {
                assert!(set_highlight(&view, cx, on));
                assert_eq!(snapshot(&view, cx), before);
                assert_eq!(text_of(&view, cx), text);
                assert_eq!(selection_of(&view, cx), (anchor, active));
            }
        }
    }

    #[gpui::test]
    fn syntax_highlight_toggle_rebuilds_plain_presentation(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "# Heading\n\ntext\n", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        let colored = runs_of(&view, cx, "# Heading").expect("heading row drawn");
        assert!(!colored.is_empty());
        assert!(set_highlight(&view, cx, false));
        assert!(runs_of(&view, cx, "# Heading").expect("redrawn").is_empty());
        assert!(set_highlight(&view, cx, true));
        assert_eq!(runs_of(&view, cx, "# Heading").expect("redrawn"), colored);
    }

    #[gpui::test]
    fn syntax_highlight_toggle_in_projection_mode_changes_nothing_visible(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        let blocks = |view: &TestView, cx: &mut TestCx| {
            view.read_with(cx, |view, _| {
                let mut blocks = view.block_cache.values().cloned().collect::<Vec<_>>();
                blocks.sort_by_key(|block| block.source_range.start.0);
                blocks
            })
        };
        let before = blocks(&view, cx);
        assert!(!before.is_empty());
        assert!(set_highlight(&view, cx, false));
        assert_eq!(blocks(&view, cx), before);
        view.read_with(cx, |view, _| assert!(!view.syntax_highlight()));
    }

    #[gpui::test]
    fn projection_mode_never_emits_syntax_tone(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        view.read_with(cx, |view, _| assert!(view.syntax_highlight()));
        for mode in [EditorMode::Editing, EditorMode::Reading] {
            cx.update(|window, app| {
                view.update(app, |view, cx| {
                    assert!(view.set_editor_mode(mode, window, cx));
                });
            });
            settle(cx);
            let lines = cached_lines(&view, cx);
            assert!(!lines.is_empty());
            for (_, text, runs) in lines {
                assert!(
                    runs.iter()
                        .all(|run| !matches!(run.kind, StyleKind::Syntax(_))),
                    "{text:?}"
                );
            }
        }
    }

    #[gpui::test]
    fn syntax_highlight_toggle_is_refused_while_composing(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "hello\n", StateStores::memory());
        view.update(cx, |view, _| {
            view.editor_mut()
                .replace_and_mark_text(None, "あ", Some(1..1))
                .unwrap();
        });
        assert!(!set_highlight(&view, cx, false));
        view.read_with(cx, |view, _| {
            assert!(view.syntax_highlight());
            assert!(view.editor().ime().is_some(), "the marked text is kept");
            assert_eq!(
                view.status.as_deref(),
                Some(MODE_SWITCH_BLOCKED_BY_COMPOSITION)
            );
        });
    }

    /// Presents the block at `offset` the way a frame does and reports whether
    /// the cache was reused, with the lines' runs.
    fn present_at(
        view: &TestView,
        cx: &mut TestCx,
        offset: usize,
    ) -> (bool, Vec<Vec<hane_presentation::StyleRun>>) {
        view.update(cx, |view, _| {
            let indexed = view.block_at_offset(SourceOffset(offset)).unwrap();
            let span = block_line_span(view.editor().document(), &indexed).unwrap();
            let (visual, reused) = view.cached_block(&indexed, &span).unwrap();
            (
                reused,
                visual.lines.iter().map(|line| line.style_runs.clone()).collect(),
            )
        })
    }

    #[gpui::test]
    fn plain_highlight_edit_reparses_only_edited_block(cx: &mut gpui::TestAppContext) {
        let source = "# one\n\n# two\n";
        let second = source.find("# two").unwrap();
        let (view, cx) = open_view(cx, source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        present_at(&view, cx, 0);
        let (_, second_runs) = present_at(&view, cx, second);
        assert!(second_runs.iter().any(|runs| !runs.is_empty()));

        select(&view, cx, 5, 5);
        view.update(cx, |view, cx| view.insert_text("x", cx));

        let (_, first_runs) = present_at(&view, cx, 0);
        let heading_ends: Vec<usize> = first_runs
            .iter()
            .flatten()
            .filter(|run| run.kind == StyleKind::Syntax(hane_presentation::SyntaxTone::Heading))
            .map(|run| run.visual_range.end.0)
            .collect();
        assert!(
            heading_ends.contains(&6),
            "the edited block reflects the new text: {heading_ends:?}"
        );
        assert!(!heading_ends.contains(&5));
        let (second_reused, second_after) = present_at(&view, cx, second + 1);
        assert!(second_reused, "the other block is reused");
        assert_eq!(second_after, second_runs);
    }

    #[gpui::test]
    fn plain_highlight_block_cache_is_reused_across_frames(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "# one\n\ntext\n", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        let (_, first) = present_at(&view, cx, 0);
        let (reused, second) = present_at(&view, cx, 0);
        assert!(reused);
        assert_eq!(first, second);
    }

    #[gpui::test]
    fn plain_highlight_typing_keeps_caret_offset(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "**a", StateStores::memory());
        assert!(set_plain(&view, cx, true));
        select(&view, cx, 3, 3);
        let (_, runs) = present_at(&view, cx, 0);
        assert!(runs.iter().all(|runs| runs.iter().all(|run| run.kind != StyleKind::Bold)));
        for expected in [4, 5] {
            view.update(cx, |view, cx| view.insert_text("*", cx));
            assert_eq!(selection_of(&view, cx), (expected, expected));
        }
        assert_eq!(text_of(&view, cx), "**a**");
        let (_, runs) = present_at(&view, cx, 0);
        assert!(runs[0].iter().any(|run| run.kind == StyleKind::Bold));
    }

    #[gpui::test]
    fn plain_highlight_over_budget_block_uses_window_parse(cx: &mut gpui::TestAppContext) {
        let source = "- a\n".repeat(4_097);
        let (view, cx) = open_view(cx, &source, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        view.update(cx, |view, _| {
            let indexed = view.block_at_offset(SourceOffset(0)).unwrap();
            let span = block_line_span(view.editor().document(), &indexed).unwrap();
            assert!(!crate::line::block_fits_sync_join_budget(&indexed, &span));
            let (visual, _) = view.cached_block(&indexed, &(0..20)).unwrap();
            assert!(!visual.lines.is_empty());
            for line in &visual.lines {
                assert_eq!(line.visual_text, "- a");
                assert!(has_tone(&line.style_runs, hane_presentation::SyntaxTone::Structure));
            }
        });
    }

    #[gpui::test]
    fn plain_highlight_survives_theme_change(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, MIXED, StateStores::memory());
        assert!(set_plain(&view, cx, true));
        let before = cached_lines(&view, cx);
        assert!(before.iter().any(|(_, _, runs)| !runs.is_empty()));
        for preference in [ThemePreference::Dark, ThemePreference::Light] {
            cx.update(|window, app| {
                view.update(app, |view, cx| view.set_theme(preference, window, cx));
            });
            settle(cx);
            assert_eq!(cached_lines(&view, cx), before, "{preference:?}");
        }
    }

    #[gpui::test]
    fn settings_checkbox_reflects_syntax_highlight(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let stores = StateStores::memory();
        let (view, cx) = open_view(cx, "body\n", stores.clone());
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_settings(window, cx));
        });
        settle(cx);
        assert!(view.read_with(cx, |view, _| view.settings_open));
        // The checkbox itself has no debug selector, so aim at it through its
        // card: it is the last row, inset by the card's border and padding.
        // Plain-text mode stays off, as the checkbox is usable without it.
        let plain_before = view.read_with(cx, |view, _| view.plain_text());
        for _ in 0..2 {
            let before = view.read_with(cx, |view, _| view.syntax_highlight());
            let card = cx
                .debug_bounds("settings-plain-text-card")
                .expect("the plain-text settings card is laid out");
            let target = gpui::point(card.left() + px(25.0), card.bottom() - px(24.0));
            cx.simulate_click(target, gpui::Modifiers::none());
            settle(cx);
            let expected = !before;
            view.read_with(cx, |view, _| {
                assert_eq!(view.syntax_highlight(), expected);
                assert_eq!(
                    view.plain_text(),
                    plain_before,
                    "the click must hit the syntax-highlight checkbox only"
                );
            });
            assert_eq!(stores.settings().load().syntax_highlight, expected);
        }
    }
}
