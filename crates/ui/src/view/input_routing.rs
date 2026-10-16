use super::*;

/// RF3-A P1 (Issue #304): the single destination a normal editing-key
/// action resolves to. The sidebar filter and an active inline rename take
/// priority over the document body, matching the dispatch order the 14
/// actions in `actions.rs` already use.
#[derive(Clone, Copy, Debug)]
pub(crate) enum InputTarget {
    SidebarFilter,
    InlineRename,
    Body,
}

impl EditorView {
    /// Resolves the input destination for the 14 normal editing-key actions
    /// in `actions.rs` (Backspace, Delete, Left, Right, SelectLeft,
    /// SelectRight, SelectAll, Home, End, SelectHome, SelectEnd, Copy, Cut,
    /// Paste), and, since RF3-A P2 (Issue #304), for the special-key actions
    /// Enter/Shift+Enter, Tab/Shift+Tab, Up/Down, Shift+Up/Shift+Down,
    /// Escape, NextFileTab/PrevFileTab, DocumentFindNext/DocumentFindPrevious,
    /// the body-only document navigation actions
    /// DocumentStart/DocumentEnd/SelectDocumentStart/SelectDocumentEnd, and
    /// Undo/Redo, after each of those actions' own unrelated short-circuits
    /// (content search, document find, AI selection, settings, menus, ...)
    /// have already run. Every one of those actions calls this once per
    /// dispatch and routes to exactly one of the existing body `Editor`,
    /// sidebar filter, or inline rename handlers below; the document
    /// navigation and Undo/Redo actions are body-only and no-op for
    /// `SidebarFilter`/`InlineRename`.
    pub(crate) fn input_target(&self) -> InputTarget {
        if self.sidebar_filter_is_focused() {
            InputTarget::SidebarFilter
        } else if self.inline_rename_active() {
            InputTarget::InlineRename
        } else {
            InputTarget::Body
        }
    }

    /// The broad sidebar-input condition: the raw sidebar filter flag, or the
    /// content-search input or results. It is a compatibility condition shared
    /// by the key routing, the platform text callbacks and the body
    /// `InputCapture` registration; it does not mean the filter string may be
    /// edited (the filter helpers check the raw flag themselves).
    pub(crate) fn sidebar_filter_is_focused(&self) -> bool {
        self.sidebar_filter_focused
            || self.content_search_input_is_focused()
            || self.content_search_results_focused()
    }

    /// RF3-A P3 (Issue #304): destination of the platform text callbacks
    /// `text_for_range`, `marked_text_range`, `unmark_text`,
    /// `replace_text_in_range`, `replace_and_mark_text_in_range`,
    /// `bounds_for_range` and `character_index_for_point`. Rename first, then
    /// the broad sidebar condition, then the body.
    pub(crate) fn input_target_for_text_callback(&self) -> InputTarget {
        if self.inline_rename_active() {
            InputTarget::InlineRename
        } else if self.sidebar_filter_is_focused() {
            InputTarget::SidebarFilter
        } else {
            InputTarget::Body
        }
    }

    /// RF3-A P3 (Issue #304): the marked range, in UTF-16, of the surface the
    /// text callbacks currently reach (the same answer as `marked_text_range`).
    pub(crate) fn composition_marked_range(&self) -> Option<Range<usize>> {
        match self.input_target_for_text_callback() {
            InputTarget::InlineRename => self.inline_rename_marked_range(),
            InputTarget::SidebarFilter => self.sidebar_filter_marked_range(),
            InputTarget::Body => self
                .editor()
                .ime()
                .and_then(|ime| self.editor().source_range_to_utf16(ime.marked_range).ok()),
        }
    }

    /// RF3-A P3 (Issue #304): destination of `selected_text_range`. Rename
    /// first, then only the raw sidebar filter flag (not the content-search
    /// flags), then the body.
    pub(crate) fn input_target_for_selection_query(&self) -> InputTarget {
        if self.inline_rename_active() {
            InputTarget::InlineRename
        } else if self.sidebar_filter_focused {
            InputTarget::SidebarFilter
        } else {
            InputTarget::Body
        }
    }

    /// RF3-A P3 (Issue #304): whether the body `InputCapture` registers its
    /// platform input handler. Unchanged from the previous inline condition:
    /// no rename, no broad sidebar focus and no AI selection key block.
    pub(crate) fn body_input_capture_enabled(&self) -> bool {
        !self.inline_rename_active()
            && !self.sidebar_filter_is_focused()
            && !self.ai_selection_blocks_editor_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::EntityInputHandler;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[gpui::test]
    fn platform_target_matrix_preserves_entrypoint_contracts(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        // The content-search focus flags are private to `content_search`, so
        // the fixture drives them through its existing focus operations. The
        // results-focused flag needs real hits and is not reachable here.
        for bits in 0..4u8 {
            let filter = bits & 1 != 0;
            let input = bits & 2 != 0;
            cx.update(|window, app| {
                view.update(app, |view, cx| {
                    if input {
                        view.focus_content_search_input(window, cx);
                    } else {
                        view.blur_content_search_focus(cx);
                    }
                });
            });
            view.update(cx, |view, _| {
                view.sidebar_filter_focused = filter;
                let broad = filter || input;
                let text = view.input_target_for_text_callback();
                let selection = view.input_target_for_selection_query();
                assert!(
                    matches!(
                        (broad, text),
                        (true, InputTarget::SidebarFilter) | (false, InputTarget::Body)
                    ),
                    "text callbacks use the broad condition: {bits:03b}"
                );
                assert!(
                    matches!(
                        (filter, selection),
                        (true, InputTarget::SidebarFilter) | (false, InputTarget::Body)
                    ),
                    "selection query uses only the raw filter flag: {bits:03b}"
                );
                assert_eq!(view.body_input_capture_enabled(), !broad);
            });
        }
    }

    // RF3-A P3 contract tests (Issue #304, design spec §9 R02 / R06 / R11 /
    // R12). Everything below drives the existing focus operations
    // (`focus_sidebar_filter`, `begin_inline_rename`, `open_content_search`,
    // `focus_content_search_*`, `open_ai_selection`, `open_settings`,
    // `open_document_find`, real clicks and keystrokes) and the real
    // `EntityInputHandler` callbacks. No private focus flag is written, and
    // the expectations are spelled out per case instead of being derived
    // from the resolvers under test.

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    /// UTF-16 offsets: `A`=0, `😀`=1..3, `e`=3, U+0301=4, `Z`=5.
    /// Byte offsets: `A`=0..1, `😀`=1..5, `e`=5..6, U+0301=6..8, `Z`=8..9.
    const TEXT: &str = "A😀e\u{301}Z";
    const REPLACED: &str = "A羽e\u{301}Z";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Surface {
        Body,
        Filter,
        Rename,
    }

    #[derive(Clone, Copy)]
    struct FocusState {
        rename: bool,
        filter: bool,
        input: bool,
        results: bool,
    }

    fn fixture_root(label: &str) -> PathBuf {
        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-304-p3-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Rename-me.md"), "needle in the file").unwrap();
        root
    }

    fn rename_path(root: &Path) -> PathBuf {
        root.join("Rename-me.md")
    }

    fn open_routing_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        body: &str,
        root: &Path,
    ) -> (TestView, &'a mut TestCx) {
        cx.update(crate::actions::register_key_bindings);
        let work_folder = OsWorkFolderScanner.scan(root).unwrap();
        let body = body.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&body, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            cx.notify();
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn click_body(cx: &mut TestCx) {
        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_click(point, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn click_filter(cx: &mut TestCx) {
        let point = cx
            .debug_bounds("sidebar-filter")
            .expect("sidebar filter painted")
            .center();
        cx.simulate_click(point, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn focus_body(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                window.focus(&view.focus_handle, cx);
            });
        });
        cx.run_until_parked();
    }

    fn filter_mouse_down() -> gpui::MouseDownEvent {
        gpui::MouseDownEvent {
            position: point(px(0.0), px(0.0)),
            modifiers: gpui::Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        }
    }

    fn focus_filter(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.focus_sidebar_filter(&filter_mouse_down(), window, cx);
            });
        });
        cx.run_until_parked();
    }

    fn begin_rename(view: &TestView, cx: &mut TestCx, path: &Path) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.begin_inline_rename(path.to_path_buf(), InlineRenameKind::File, window, cx);
            });
        });
        cx.run_until_parked();
    }

    fn focus_search_input(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.focus_content_search_input(window, cx));
        });
        cx.run_until_parked();
    }

    fn focus_search_results(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.focus_content_search_results(window, cx));
        });
        cx.run_until_parked();
    }

    /// Opens content search, types a query through the real search input and
    /// lets the debounce, the background workers and the delivery poll run
    /// until the single fixture file's hit is displayed, then focuses the
    /// results list. Fails (does not skip) when no result ever appears.
    fn search_until_results_are_focused(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));
        cx.simulate_input("needle");
        for _ in 0..300 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
            // Only changes state once a hit is displayed.
            focus_search_results(view, cx);
            if view.read_with(cx, |view, _| view.content_search_results_focused()) {
                return;
            }
        }
        panic!("content search never displayed a result that could be focused");
    }

    /// Reaches `state` from any earlier state using only focus operations. A
    /// body click first clears rename, filter and both search focus flags
    /// without leaving the search mode or dropping the displayed results.
    fn enter_state(view: &TestView, cx: &mut TestCx, state: FocusState, root: &Path) {
        click_body(cx);
        if state.results {
            focus_search_results(view, cx);
        } else if state.input {
            focus_search_input(view, cx);
        }
        if state.filter {
            focus_filter(view, cx);
        }
        if state.rename {
            begin_rename(view, cx, &rename_path(root));
        }
    }

    fn query_selection(view: &TestView, cx: &mut TestCx) -> Option<(Range<usize>, bool)> {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.selected_text_range(false, window, cx))
        })
        .map(|selection| (selection.range, selection.reversed))
    }

    fn query_text(
        view: &TestView,
        cx: &mut TestCx,
        range: Range<usize>,
    ) -> (Option<String>, Option<Range<usize>>) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                let mut actual = None;
                let text = view.text_for_range(range, &mut actual, window, cx);
                (text, actual)
            })
        })
    }

    fn query_marked(view: &TestView, cx: &mut TestCx) -> Option<Range<usize>> {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.marked_text_range(window, cx))
        })
    }

    fn replace(view: &TestView, cx: &mut TestCx, range: Option<Range<usize>>, text: &str) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.replace_text_in_range(range, text, window, cx);
            });
        });
    }

    fn mark(
        view: &TestView,
        cx: &mut TestCx,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.replace_and_mark_text_in_range(range, text, selected, window, cx);
            });
        });
    }

    fn unmark(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.unmark_text(window, cx));
        });
    }

    fn target_name(target: InputTarget) -> &'static str {
        match target {
            InputTarget::SidebarFilter => "filter",
            InputTarget::InlineRename => "rename",
            InputTarget::Body => "body",
        }
    }

    fn surface_text(view: &TestView, cx: &mut TestCx, surface: Surface) -> String {
        view.read_with(cx, |view, _| match surface {
            Surface::Body => view.editor().document().full_text(),
            Surface::Filter => view.sidebar_filter.clone(),
            Surface::Rename => view
                .inline_rename
                .as_ref()
                .expect("rename is active")
                .text
                .clone(),
        })
    }

    /// The surface's selection in bytes.
    fn surface_selection(view: &TestView, cx: &mut TestCx, surface: Surface) -> Range<usize> {
        view.read_with(cx, |view, _| match surface {
            Surface::Body => {
                let range = view.editor().selection().range();
                range.start.0..range.end.0
            }
            Surface::Filter => view.sidebar_filter_selected_range.clone(),
            Surface::Rename => view
                .inline_rename
                .as_ref()
                .expect("rename is active")
                .selected_range
                .clone(),
        })
    }

    fn assert_other_surfaces_untouched(view: &TestView, cx: &mut TestCx, surface: Surface) {
        view.read_with(cx, |view, _| {
            if surface != Surface::Body {
                assert_eq!(view.editor().document().full_text(), "", "body leaked");
            }
            if surface != Surface::Filter {
                assert_eq!(view.sidebar_filter, "", "filter leaked");
            }
            if surface != Surface::Rename {
                assert!(view.inline_rename.is_none(), "rename leaked");
            }
        });
    }

    /// Focuses `surface` through its existing operation and types `text`
    /// through the real callback (replacing the surface's initial selection).
    fn prepare_surface<'a>(
        cx: &'a mut gpui::TestAppContext,
        surface: Surface,
        root: &Path,
        text: &str,
    ) -> (TestView, &'a mut TestCx) {
        let (view, cx) = open_routing_view(cx, "", root);
        match surface {
            Surface::Body => focus_body(&view, cx),
            Surface::Filter => click_filter(cx),
            Surface::Rename => begin_rename(&view, cx, &rename_path(root)),
        }
        replace(&view, cx, None, text);
        (view, cx)
    }

    /// Selects `😀` (byte 1..5, UTF-16 1..3) with the caret after it.
    fn select_emoji_forward(cx: &mut TestCx) {
        cx.simulate_keystrokes("home right shift-right");
        cx.run_until_parked();
    }

    /// Selects `😀` with the caret before it (a reversed selection).
    fn select_emoji_reversed(cx: &mut TestCx) {
        cx.simulate_keystrokes("home right right shift-left");
        cx.run_until_parked();
    }

    // ----- R02 -----

    #[gpui::test]
    fn platform_target_matrix_covers_rename_filter_search_input_and_results(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = fixture_root("matrix");
        let (view, cx) = open_routing_view(cx, "body-text\n", &root);

        // Three distinguishable texts: body "bod", filter "ren", rename "Ren".
        focus_filter(&view, cx);
        replace(&view, cx, None, "ren");
        click_body(cx);
        search_until_results_are_focused(&view, cx);

        // The input and results flags are set exclusively by the existing
        // focus operations, so the four combinations with both set are
        // covered by the synthetic matrix in `content_search` (which owns the
        // private flags); the twelve reachable ones are driven through real
        // focus operations below.
        focus_search_input(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.content_search_input_is_focused());
            assert!(!view.content_search_results_focused());
        });
        focus_search_results(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(!view.content_search_input_is_focused());
            assert!(view.content_search_results_focused());
        });

        for bits in 0..16u8 {
            let state = FocusState {
                rename: bits & 1 != 0,
                filter: bits & 2 != 0,
                input: bits & 4 != 0,
                results: bits & 8 != 0,
            };
            if state.input && state.results {
                // Unreachable through the focus operations (asserted above).
                continue;
            }
            enter_state(&view, cx, state, &root);
            let label = format!(
                "rename={} filter={} input={} results={}",
                state.rename, state.filter, state.input, state.results
            );
            let broad = state.filter || state.input || state.results;

            let expected_text_target = if state.rename {
                "rename"
            } else if broad {
                "filter"
            } else {
                "body"
            };
            let expected_selection_target = if state.rename {
                "rename"
            } else if state.filter {
                "filter"
            } else {
                "body"
            };
            let (text_target, selection_target, capture, rename_active, flags) =
                view.read_with(cx, |view, _| {
                    (
                        target_name(view.input_target_for_text_callback()),
                        target_name(view.input_target_for_selection_query()),
                        view.body_input_capture_enabled(),
                        view.inline_rename_active(),
                        (
                            view.sidebar_filter_focused,
                            view.content_search_input_is_focused(),
                            view.content_search_results_focused(),
                        ),
                    )
                });
            assert_eq!(rename_active, state.rename, "{label}");
            assert_eq!(
                flags,
                (state.filter, state.input, state.results),
                "{label}: the focus operations reached the requested flags"
            );
            assert_eq!(text_target, expected_text_target, "{label}");
            assert_eq!(selection_target, expected_selection_target, "{label}");
            assert_eq!(capture, !(state.rename || broad), "{label}");

            // The callbacks really reach the surface the resolvers name.
            let expected_text = match expected_text_target {
                "rename" => "Ren",
                "filter" => "ren",
                _ => "bod",
            };
            assert_eq!(
                query_text(&view, cx, 0..3),
                (Some(expected_text.to_owned()), Some(0..3)),
                "{label}: text callback"
            );
            let body_selection = view.read_with(cx, |view, _| {
                let range = view.editor().selection().range();
                (
                    range.start.0..range.end.0,
                    view.editor().selection().is_reversed(),
                )
            });
            let expected_selection = match expected_selection_target {
                "rename" => (0..9, false),
                "filter" => (0..3, false),
                _ => body_selection,
            };
            assert_eq!(
                query_selection(&view, cx),
                Some(expected_selection),
                "{label}: selection query"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    // ----- R06 -----

    fn utf16_range_contract(cx: &mut gpui::TestAppContext, surface: Surface, label: &str) {
        let root = fixture_root(label);
        let (view, cx) = prepare_surface(cx, surface, &root, TEXT);
        assert_eq!(surface_text(&view, cx, surface), TEXT);

        select_emoji_forward(cx);
        assert_eq!(surface_selection(&view, cx, surface), 1..5);
        assert_eq!(query_selection(&view, cx), Some((1..3, false)));
        assert_eq!(
            query_text(&view, cx, 1..3),
            (Some("😀".to_owned()), Some(1..3))
        );
        assert_eq!(
            query_text(&view, cx, 3..5),
            (Some("e\u{301}".to_owned()), Some(3..5))
        );

        replace(&view, cx, Some(1..3), "羽");
        assert_eq!(surface_text(&view, cx, surface), REPLACED);
        assert_eq!(surface_selection(&view, cx, surface), 4..4);
        assert_eq!(query_selection(&view, cx), Some((2..2, false)));
        assert_other_surfaces_untouched(&view, cx, surface);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn reversed_selection_contract(cx: &mut gpui::TestAppContext, surface: Surface, label: &str) {
        let root = fixture_root(label);
        let (view, cx) = prepare_surface(cx, surface, &root, TEXT);

        select_emoji_reversed(cx);
        assert_eq!(surface_selection(&view, cx, surface), 1..5);
        assert_eq!(query_selection(&view, cx), Some((1..3, true)));

        // A replacement without a range replaces the reversed selection.
        replace(&view, cx, None, "羽");
        assert_eq!(surface_text(&view, cx, surface), REPLACED);
        assert_eq!(surface_selection(&view, cx, surface), 4..4);
        assert_eq!(query_selection(&view, cx), Some((2..2, false)));
        assert_other_surfaces_untouched(&view, cx, surface);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn composition_contract(cx: &mut gpui::TestAppContext, surface: Surface, label: &str) {
        let root = fixture_root(label);
        let (view, cx) = prepare_surface(cx, surface, &root, "ab");
        assert_eq!(surface_text(&view, cx, surface), "ab");
        assert_eq!(query_marked(&view, cx), None);

        mark(&view, cx, None, "か", None);
        assert_eq!(surface_text(&view, cx, surface), "abか");
        assert_eq!(query_marked(&view, cx), Some(2..3));
        assert_eq!(
            query_text(&view, cx, 2..3),
            (Some("か".to_owned()), Some(2..3))
        );

        // Commit through `unmark_text` keeps the composed text.
        unmark(&view, cx);
        assert_eq!(surface_text(&view, cx, surface), "abか");
        assert_eq!(query_marked(&view, cx), None);

        // Cancel (Escape) restores the pre-composition text.
        mark(&view, cx, None, "き", None);
        assert_eq!(surface_text(&view, cx, surface), "abかき");
        assert_eq!(query_marked(&view, cx), Some(3..4));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(surface_text(&view, cx, surface), "abか");
        assert_eq!(query_marked(&view, cx), None);

        // Commit through a final replacement of the marked text.
        mark(&view, cx, None, "さ", None);
        assert_eq!(query_marked(&view, cx), Some(3..4));
        replace(&view, cx, None, "さ");
        assert_eq!(surface_text(&view, cx, surface), "abかさ");
        assert_eq!(query_marked(&view, cx), None);
        assert_other_surfaces_untouched(&view, cx, surface);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn body_utf16_range_replacement_maps_to_the_emoji_bytes(cx: &mut gpui::TestAppContext) {
        utf16_range_contract(cx, Surface::Body, "range-body");
    }

    #[gpui::test]
    fn filter_utf16_range_replacement_maps_to_the_emoji_bytes(cx: &mut gpui::TestAppContext) {
        utf16_range_contract(cx, Surface::Filter, "range-filter");
    }

    #[gpui::test]
    fn rename_utf16_range_replacement_maps_to_the_emoji_bytes(cx: &mut gpui::TestAppContext) {
        utf16_range_contract(cx, Surface::Rename, "range-rename");
    }

    #[gpui::test]
    fn body_reversed_selection_reports_and_is_replaced(cx: &mut gpui::TestAppContext) {
        reversed_selection_contract(cx, Surface::Body, "reversed-body");
    }

    #[gpui::test]
    fn filter_reversed_selection_reports_and_is_replaced(cx: &mut gpui::TestAppContext) {
        reversed_selection_contract(cx, Surface::Filter, "reversed-filter");
    }

    #[gpui::test]
    fn rename_reversed_selection_reports_and_is_replaced(cx: &mut gpui::TestAppContext) {
        reversed_selection_contract(cx, Surface::Rename, "reversed-rename");
    }

    #[gpui::test]
    fn body_composition_commit_and_cancel(cx: &mut gpui::TestAppContext) {
        composition_contract(cx, Surface::Body, "ime-body");
    }

    #[gpui::test]
    fn filter_composition_commit_and_cancel(cx: &mut gpui::TestAppContext) {
        composition_contract(cx, Surface::Filter, "ime-filter");
    }

    #[gpui::test]
    fn rename_composition_commit_and_cancel(cx: &mut gpui::TestAppContext) {
        composition_contract(cx, Surface::Rename, "ime-rename");
    }

    // ----- IME cancel over a selection (Issue #304 P3 Windows report) -----

    /// Selects the emoji, starts a composition over it through the real
    /// callback, and returns the pre-composition `(text, selection)`.
    fn start_composition_over_selected_emoji(
        view: &TestView,
        cx: &mut TestCx,
        surface: Surface,
    ) -> (String, Range<usize>) {
        select_emoji_forward(cx);
        let text = surface_text(view, cx, surface);
        let selection = surface_selection(view, cx, surface);
        assert_eq!(text, TEXT);
        assert_eq!(selection, 1..5);

        mark(view, cx, None, "か", None);
        assert!(
            query_marked(view, cx).is_some(),
            "the composition replaced the selection and is marked"
        );
        assert_ne!(surface_text(view, cx, surface), TEXT);
        (text, selection)
    }

    fn assert_cancel_restored(
        view: &TestView,
        cx: &mut TestCx,
        surface: Surface,
        original: &(String, Range<usize>),
    ) {
        assert_eq!(
            surface_text(view, cx, surface),
            original.0,
            "the text selected before the composition must survive the cancel"
        );
        assert_eq!(surface_selection(view, cx, surface), original.1);
        assert_eq!(query_marked(view, cx), None);
        assert_other_surfaces_untouched(view, cx, surface);
    }

    /// Escape delivered through the real key dispatch cancels the composition.
    fn escape_cancel_over_selection_contract(
        cx: &mut gpui::TestAppContext,
        surface: Surface,
        label: &str,
    ) {
        let root = fixture_root(label);
        let (view, cx) = prepare_surface(cx, surface, &root, TEXT);
        let original = start_composition_over_selected_emoji(&view, cx, surface);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_cancel_restored(&view, cx, surface, &original);

        if surface == Surface::Rename {
            // The first Escape only cancelled the composition.
            assert!(view.read_with(cx, |view, _| view.inline_rename_active()));
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            assert!(!view.read_with(cx, |view, _| view.inline_rename_active()));
            assert!(rename_path(&root).exists(), "the file name is unchanged");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The Windows IME reports a cancelled composition as an empty
    /// `replace_text_in_range(None, "")` (`WM_IME_COMPOSITION`, `lparam == 0`)
    /// instead of an Escape key event. The pre-composition text must survive.
    fn platform_cancel_over_selection_contract(
        cx: &mut gpui::TestAppContext,
        surface: Surface,
        label: &str,
    ) {
        let root = fixture_root(label);
        let (view, cx) = prepare_surface(cx, surface, &root, TEXT);
        let original = start_composition_over_selected_emoji(&view, cx, surface);

        replace(&view, cx, None, "");
        cx.run_until_parked();
        assert_cancel_restored(&view, cx, surface, &original);
        if surface == Surface::Rename {
            assert!(view.read_with(cx, |view, _| view.inline_rename_active()));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// macOS reverts the composition to its reading with the first Escape and
    /// cancels it with the next: an empty mark (or an empty replacement of
    /// exactly the marked range) followed by `unmark_text`. The selected text
    /// that the composition replaced must survive.
    fn mac_cancel_over_selection_contract(
        cx: &mut gpui::TestAppContext,
        surface: Surface,
        label: &str,
        replace_marked_range: bool,
    ) {
        let root = fixture_root(label);
        let (view, cx) = prepare_surface(cx, surface, &root, TEXT);
        let original = start_composition_over_selected_emoji(&view, cx, surface);

        // First Escape: the composition reverts to the reading.
        mark(&view, cx, None, "にほんご", None);
        assert!(query_marked(&view, cx).is_some());

        // Second Escape: the composition is cancelled.
        if replace_marked_range {
            let marked = query_marked(&view, cx);
            replace(&view, cx, marked, "");
        } else {
            let marked = query_marked(&view, cx);
            mark(&view, cx, marked, "", None);
        }
        unmark(&view, cx);
        cx.run_until_parked();
        assert_cancel_restored(&view, cx, surface, &original);
        if surface == Surface::Rename {
            assert!(view.read_with(cx, |view, _| view.inline_rename_active()));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn body_mac_empty_mark_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        mac_cancel_over_selection_contract(cx, Surface::Body, "cancel-mac-mark-body", false);
    }

    #[gpui::test]
    fn filter_mac_empty_mark_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        mac_cancel_over_selection_contract(cx, Surface::Filter, "cancel-mac-mark-filter", false);
    }

    #[gpui::test]
    fn rename_mac_empty_mark_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        mac_cancel_over_selection_contract(cx, Surface::Rename, "cancel-mac-mark-rename", false);
    }

    #[gpui::test]
    fn body_mac_marked_range_replace_cancel_keeps_the_selected_text(
        cx: &mut gpui::TestAppContext,
    ) {
        mac_cancel_over_selection_contract(cx, Surface::Body, "cancel-mac-rep-body", true);
    }

    #[gpui::test]
    fn filter_mac_marked_range_replace_cancel_keeps_the_selected_text(
        cx: &mut gpui::TestAppContext,
    ) {
        mac_cancel_over_selection_contract(cx, Surface::Filter, "cancel-mac-rep-filter", true);
    }

    #[gpui::test]
    fn rename_mac_marked_range_replace_cancel_keeps_the_selected_text(
        cx: &mut gpui::TestAppContext,
    ) {
        mac_cancel_over_selection_contract(cx, Surface::Rename, "cancel-mac-rep-rename", true);
    }

    /// Selects the whole original text, starts a composition over it through
    /// the real callback, and returns the pre-composition `(text, selection)`.
    fn start_composition_over_full_selection(
        view: &TestView,
        cx: &mut TestCx,
        surface: Surface,
    ) -> (String, Range<usize>) {
        cx.simulate_keystrokes("home shift-end");
        cx.run_until_parked();
        let text = surface_text(view, cx, surface);
        let selection = surface_selection(view, cx, surface);
        assert_eq!(text, TEXT);
        assert_eq!(selection, 0..TEXT.len());

        mark(view, cx, None, "か", None);
        assert!(
            query_marked(view, cx).is_some(),
            "the composition replaced the whole selection and is marked"
        );
        assert_ne!(surface_text(view, cx, surface), TEXT);
        (text, selection)
    }

    /// Same macOS cancellation callback order as
    /// `mac_cancel_over_selection_contract`, with the whole text selected.
    fn mac_cancel_over_full_selection_contract(
        cx: &mut gpui::TestAppContext,
        surface: Surface,
        label: &str,
        replace_marked_range: bool,
    ) {
        let root = fixture_root(label);
        let (view, cx) = prepare_surface(cx, surface, &root, TEXT);
        let original = start_composition_over_full_selection(&view, cx, surface);

        // First Escape: the composition reverts to the reading.
        mark(&view, cx, None, "にほんご", None);
        assert!(query_marked(&view, cx).is_some());

        // Second Escape: the composition is cancelled.
        let marked = query_marked(&view, cx);
        if replace_marked_range {
            replace(&view, cx, marked, "");
        } else {
            mark(&view, cx, marked, "", None);
        }
        unmark(&view, cx);
        cx.run_until_parked();
        assert_cancel_restored(&view, cx, surface, &original);
        if surface == Surface::Rename {
            assert!(view.read_with(cx, |view, _| view.inline_rename_active()));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn body_mac_empty_mark_cancel_keeps_the_fully_selected_text(cx: &mut gpui::TestAppContext) {
        mac_cancel_over_full_selection_contract(cx, Surface::Body, "cancel-full-mark-body", false);
    }

    #[gpui::test]
    fn filter_mac_empty_mark_cancel_keeps_the_fully_selected_text(cx: &mut gpui::TestAppContext) {
        mac_cancel_over_full_selection_contract(
            cx,
            Surface::Filter,
            "cancel-full-mark-filter",
            false,
        );
    }

    #[gpui::test]
    fn rename_mac_empty_mark_cancel_keeps_the_fully_selected_text(cx: &mut gpui::TestAppContext) {
        mac_cancel_over_full_selection_contract(
            cx,
            Surface::Rename,
            "cancel-full-mark-rename",
            false,
        );
    }

    #[gpui::test]
    fn body_mac_marked_range_replace_cancel_keeps_the_fully_selected_text(
        cx: &mut gpui::TestAppContext,
    ) {
        mac_cancel_over_full_selection_contract(cx, Surface::Body, "cancel-full-rep-body", true);
    }

    #[gpui::test]
    fn filter_mac_marked_range_replace_cancel_keeps_the_fully_selected_text(
        cx: &mut gpui::TestAppContext,
    ) {
        mac_cancel_over_full_selection_contract(
            cx,
            Surface::Filter,
            "cancel-full-rep-filter",
            true,
        );
    }

    #[gpui::test]
    fn rename_mac_marked_range_replace_cancel_keeps_the_fully_selected_text(
        cx: &mut gpui::TestAppContext,
    ) {
        mac_cancel_over_full_selection_contract(
            cx,
            Surface::Rename,
            "cancel-full-rep-rename",
            true,
        );
    }

    #[gpui::test]
    fn body_escape_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        escape_cancel_over_selection_contract(cx, Surface::Body, "cancel-esc-body");
    }

    #[gpui::test]
    fn filter_escape_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        escape_cancel_over_selection_contract(cx, Surface::Filter, "cancel-esc-filter");
    }

    #[gpui::test]
    fn rename_escape_cancel_keeps_the_selected_text_then_closes(cx: &mut gpui::TestAppContext) {
        escape_cancel_over_selection_contract(cx, Surface::Rename, "cancel-esc-rename");
    }

    #[gpui::test]
    fn body_platform_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        platform_cancel_over_selection_contract(cx, Surface::Body, "cancel-plat-body");
    }

    #[gpui::test]
    fn filter_platform_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        platform_cancel_over_selection_contract(cx, Surface::Filter, "cancel-plat-filter");
    }

    #[gpui::test]
    fn rename_platform_cancel_keeps_the_selected_text(cx: &mut gpui::TestAppContext) {
        platform_cancel_over_selection_contract(cx, Surface::Rename, "cancel-plat-rename");
    }

    // ----- R11 -----

    fn body_text(view: &TestView, cx: &mut TestCx) -> String {
        surface_text(view, cx, Surface::Body)
    }

    fn capture_enabled(view: &TestView, cx: &mut TestCx) -> bool {
        view.read_with(cx, |view, _| view.body_input_capture_enabled())
    }

    #[gpui::test]
    fn body_input_capture_is_only_registered_when_no_other_handler_owns_input(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = fixture_root("capture");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        // Body focused: registered, and typed text reaches the body.
        click_body(cx);
        assert!(capture_enabled(&view, cx));
        cx.simulate_input("q");
        assert_eq!(body_text(&view, cx).len(), "body\n".len() + 1);
        let body_after_typing = body_text(&view, cx);

        // Files filter focused: not registered; text goes to the filter.
        click_filter(cx);
        assert!(!capture_enabled(&view, cx));
        cx.simulate_input("e");
        assert_eq!(body_text(&view, cx), body_after_typing);
        assert_eq!(view.read_with(cx, |view, _| view.sidebar_filter.clone()), "e");

        // Inline rename: not registered; text goes to the rename field.
        click_body(cx);
        begin_rename(&view, cx, &rename_path(&root));
        assert!(!capture_enabled(&view, cx));
        cx.simulate_input("x");
        assert_eq!(body_text(&view, cx), body_after_typing);
        assert_eq!(surface_text(&view, cx, Surface::Rename), "x");

        // Document find: the current condition still registers the body
        // capture, but the find input owns real focus so it keeps the text.
        click_body(cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_find_input_is_focused()));
        assert!(capture_enabled(&view, cx));
        cx.simulate_input("w");
        assert_eq!(body_text(&view, cx), body_after_typing);

        // Selection AI popup: not registered, so its input is not stolen.
        click_body(cx);
        assert!(capture_enabled(&view, cx));
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(2),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_ai_selection(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.ai_selection_blocks_editor_keys()));
        assert!(!capture_enabled(&view, cx));
        cx.simulate_input("v");
        assert_eq!(body_text(&view, cx), body_after_typing);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.leave_ai_selection(window, cx));
        });
        cx.run_until_parked();
        assert!(capture_enabled(&view, cx));

        // Content search input and results: not registered.
        click_body(cx);
        search_until_results_are_focused(&view, cx);
        assert!(!capture_enabled(&view, cx));
        assert_eq!(body_text(&view, cx), body_after_typing);
        focus_search_input(&view, cx);
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));
        assert!(!capture_enabled(&view, cx));
        click_body(cx);
        assert!(capture_enabled(&view, cx));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Asserts the caret rectangle the short fields (filter / rename) report
    /// to the IME for the currently selected emoji.
    fn assert_short_field_caret(view: &TestView, cx: &mut TestCx, reversed: bool) {
        if reversed {
            select_emoji_reversed(cx);
        } else {
            select_emoji_forward(cx);
        }
        let bounds = Bounds {
            origin: point(px(10.0), px(20.0)),
            size: gpui::size(px(200.0), px(18.0)),
        };
        let (actual, caret_x, other_x) = cx.update(|window, app| {
            view.update(app, |view, cx| {
                let actual = view
                    .bounds_for_range(0..0, bounds, window, cx)
                    .expect("short field reports bounds");
                let state = view.text_input_render_state().expect("render state");
                assert_eq!(state.selection_reversed, reversed);
                let line = shape_inline_rename_line(&state, window);
                // Reversed selections put the caret at the start (byte 1),
                // forward selections at the end (byte 5).
                let (caret, other) = if reversed { (1, 5) } else { (5, 1) };
                (
                    actual,
                    bounds.left() + line.x_for_index(caret),
                    bounds.left() + line.x_for_index(other),
                )
            })
        });
        assert_ne!(caret_x, other_x, "the two ends must be distinguishable");
        assert_eq!(actual.origin.x, caret_x, "reversed={reversed}");
        assert_eq!(actual.origin.y, bounds.top());
        assert_eq!(actual.size.width, px(1.0));
        assert_eq!(actual.size.height, bounds.size.height);
    }

    #[gpui::test]
    fn short_field_caret_bounds_follow_the_reversed_flag(cx: &mut gpui::TestAppContext) {
        let root = fixture_root("bounds");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        click_filter(cx);
        replace(&view, cx, None, TEXT);
        assert_short_field_caret(&view, cx, false);
        assert_short_field_caret(&view, cx, true);

        click_body(cx);
        begin_rename(&view, cx, &rename_path(&root));
        replace(&view, cx, None, TEXT);
        assert_short_field_caret(&view, cx, false);
        assert_short_field_caret(&view, cx, true);

        // With only the content-search input focused the callbacks still use
        // the broad filter route, but the raw filter flag is unset, so the
        // filter's render state has no selection and the caret is at index 0.
        click_body(cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));
        let bounds = Bounds {
            origin: point(px(10.0), px(20.0)),
            size: gpui::size(px(200.0), px(18.0)),
        };
        let (actual, expected_x) = cx.update(|window, app| {
            view.update(app, |view, cx| {
                let actual = view
                    .bounds_for_range(0..0, bounds, window, cx)
                    .expect("filter route reports bounds");
                let state = view.text_input_render_state().expect("render state");
                let line = shape_inline_rename_line(&state, window);
                (actual, bounds.left() + line.x_for_index(0))
            })
        });
        assert_eq!(actual.origin.x, expected_x);
        assert_eq!(expected_x, bounds.left());
        std::fs::remove_dir_all(root).unwrap();
    }

    // ----- R12 -----

    fn snapshot(view: &TestView, cx: &mut TestCx) -> (String, String, Option<String>) {
        view.read_with(cx, |view, _| {
            (
                view.editor().document().full_text(),
                view.sidebar_filter.clone(),
                view.inline_rename.as_ref().map(|rename| rename.text.clone()),
            )
        })
    }

    #[gpui::test]
    fn input_follows_body_filter_rename_body_without_leaking(cx: &mut gpui::TestAppContext) {
        let root = fixture_root("flow-rename");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        click_body(cx);
        cx.simulate_input("q");
        let (body, filter, rename) = snapshot(&view, cx);
        assert_eq!((body.len(), body.contains('q')), (6, true));
        assert_eq!((filter.as_str(), rename), ("", None));

        // "e" keeps the renamed file visible in the filtered tree.
        click_filter(cx);
        cx.simulate_input("e");
        assert_eq!(
            snapshot(&view, cx),
            (body.clone(), "e".to_owned(), None),
            "filter input must not reach the body"
        );

        begin_rename(&view, cx, &rename_path(&root));
        cx.simulate_input("x");
        assert_eq!(
            snapshot(&view, cx),
            (body.clone(), "e".to_owned(), Some("x".to_owned())),
            "rename wins over the still-focused filter and nothing else changes"
        );

        click_body(cx);
        view.read_with(cx, |view, _| {
            assert!(!view.inline_rename_active());
            assert!(!view.sidebar_filter_focused);
            assert_eq!(target_name(view.input_target_for_text_callback()), "body");
            assert_eq!(
                target_name(view.input_target_for_selection_query()),
                "body"
            );
            assert!(view.body_input_capture_enabled());
        });
        cx.simulate_input("y");
        let (after_body, after_filter, after_rename) = snapshot(&view, cx);
        assert_eq!(after_body.len(), body.len() + 1);
        assert!(after_body.contains('y'));
        assert_eq!((after_filter.as_str(), after_rename), ("e", None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn input_follows_search_input_results_body_without_leaking(cx: &mut gpui::TestAppContext) {
        let root = fixture_root("flow-search");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        search_until_results_are_focused(&view, cx);
        assert_eq!(snapshot(&view, cx), ("body\n".to_owned(), String::new(), None));

        // Results are focused: neither typing nor the platform callbacks may
        // reach the body or the (unfocused) filter.
        view.read_with(cx, |view, _| {
            assert!(view.content_search_results_focused());
            assert!(!view.body_input_capture_enabled());
        });
        cx.simulate_input("q");
        replace(&view, cx, None, "q");
        mark(&view, cx, None, "か", None);
        assert_eq!(snapshot(&view, cx), ("body\n".to_owned(), String::new(), None));

        focus_search_input(&view, cx);
        assert_eq!(snapshot(&view, cx), ("body\n".to_owned(), String::new(), None));
        view.read_with(cx, |view, _| {
            assert!(view.content_search_input_is_focused());
            assert!(!view.body_input_capture_enabled());
        });

        click_body(cx);
        view.read_with(cx, |view, _| {
            assert!(!view.content_search_input_is_focused());
            assert!(!view.content_search_results_focused());
            assert!(view.body_input_capture_enabled());
            assert_eq!(target_name(view.input_target_for_text_callback()), "body");
        });
        cx.simulate_input("y");
        let (body, filter, rename) = snapshot(&view, cx);
        assert_eq!((body.len(), body.contains('y')), (6, true));
        assert_eq!((filter.as_str(), rename), ("", None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn input_follows_document_find_then_body_without_leaking(cx: &mut gpui::TestAppContext) {
        let root = fixture_root("flow-find");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        click_body(cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_find_input_is_focused()));
        cx.simulate_input("q");
        assert_eq!(snapshot(&view, cx), ("body\n".to_owned(), String::new(), None));

        click_body(cx);
        assert!(!view.read_with(cx, |view, _| view.document_find_input_is_focused()));
        cx.simulate_input("y");
        let (body, filter, rename) = snapshot(&view, cx);
        assert_eq!((body.len(), body.contains('y')), (6, true));
        assert_eq!((filter.as_str(), rename), ("", None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn input_follows_selection_ai_show_and_cancel_without_leaking(cx: &mut gpui::TestAppContext) {
        let root = fixture_root("flow-ai");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        click_body(cx);
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(4),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_ai_selection(window, cx));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.ai_selection_blocks_editor_keys());
            assert!(!view.body_input_capture_enabled());
        });
        cx.simulate_input("q");
        assert_eq!(snapshot(&view, cx), ("body\n".to_owned(), String::new(), None));
        assert_eq!(
            surface_selection(&view, cx, Surface::Body),
            0..4,
            "typing into the popup must not replace the document selection"
        );

        cx.update(|window, app| {
            view.update(app, |view, cx| view.leave_ai_selection(window, cx));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.ai_selection_blocks_editor_keys());
            assert!(view.body_input_capture_enabled());
        });
        // Cancelling hands focus back to the body, which replaces the
        // still-selected text.
        cx.simulate_input("y");
        assert_eq!(snapshot(&view, cx), ("y\n".to_owned(), String::new(), None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn input_follows_settings_show_and_close_without_leaking(cx: &mut gpui::TestAppContext) {
        let root = fixture_root("flow-settings");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        focus_body(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_settings(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.settings_open()));
        cx.simulate_input("q");
        assert_eq!(snapshot(&view, cx), ("body\n".to_owned(), String::new(), None));

        cx.update(|window, app| {
            view.update(app, |view, cx| view.close_settings(window, cx));
        });
        cx.run_until_parked();
        assert!(!view.read_with(cx, |view, _| view.settings_open()));
        cx.simulate_input("y");
        assert_eq!(
            snapshot(&view, cx),
            ("ybody\n".to_owned(), String::new(), None)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn pending_rename_refuses_focus_changes_and_keeps_input_on_the_rename(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = fixture_root("flow-pending");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        begin_rename(&view, cx, &rename_path(&root));
        replace(&view, cx, None, "Renamed");
        // No `run_until_parked` until the checks are done: the background
        // rename stays in flight.
        cx.update(|_, app| {
            view.update(app, |view, cx| view.confirm_inline_rename(cx));
        });
        assert!(view.read_with(cx, |view, _| {
            view.inline_rename.as_ref().is_some_and(|rename| rename.pending)
        }));

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.focus_sidebar_filter(&filter_mouse_down(), window, cx);
                view.open_content_search(window, cx);
            });
        });
        view.read_with(cx, |view, _| {
            assert!(!view.sidebar_filter_focused, "filter focus is refused");
            assert!(!view.content_search_input_is_focused());
            assert!(view.inline_rename_active());
            assert_eq!(target_name(view.input_target_for_text_callback()), "rename");
            assert_eq!(
                target_name(view.input_target_for_selection_query()),
                "rename"
            );
        });

        // Callbacks during the pending rename change nothing anywhere.
        replace(&view, cx, None, "q");
        mark(&view, cx, None, "か", None);
        assert_eq!(
            snapshot(&view, cx),
            ("body\n".to_owned(), String::new(), Some("Renamed".to_owned()))
        );

        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.inline_rename_active());
            assert_eq!(target_name(view.input_target_for_text_callback()), "body");
        });
        assert!(root.join("Renamed.md").exists());
        assert!(!rename_path(&root).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn input_received_while_rename_is_pending_never_reaches_the_body_afterwards(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = fixture_root("flow-pending-late");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        begin_rename(&view, cx, &rename_path(&root));
        replace(&view, cx, None, "Renamed");
        cx.update(|_, app| {
            view.update(app, |view, cx| view.confirm_inline_rename(cx));
        });
        assert!(view.read_with(cx, |view, _| {
            view.inline_rename.as_ref().is_some_and(|rename| rename.pending)
        }));

        // A real key-down while the background rename is in flight. Dispatch
        // it directly: `simulate_input` parks the executors, which would
        // finish the rename before the pending state could be asserted. The
        // platform's text callback for it is still held back.
        cx.update(|window, app| {
            window.dispatch_keystroke(gpui::Keystroke::parse("q").unwrap(), app);
        });
        assert!(view.read_with(cx, |view, _| {
            view.inline_rename.as_ref().is_some_and(|rename| rename.pending)
        }));
        assert_eq!(
            snapshot(&view, cx),
            ("body\n".to_owned(), String::new(), Some("Renamed".to_owned()))
        );

        cx.run_until_parked();
        assert!(!view.read_with(cx, |view, _| view.inline_rename_active()));
        assert!(root.join("Renamed.md").exists());

        // The platform delivers the held text only after the rename finished.
        replace(&view, cx, None, "q");
        mark(&view, cx, None, "q", None);
        unmark(&view, cx);
        assert_eq!(
            snapshot(&view, cx),
            ("body\n".to_owned(), String::new(), None),
            "text received during the pending rename must not reach the body"
        );

        // The next keystroke ends the guard and ordinary typing works again.
        click_body(cx);
        cx.simulate_input("y");
        let (body, filter, rename) = snapshot(&view, cx);
        assert!(body.contains('y'));
        assert!(!body.contains('q'));
        assert_eq!((filter.as_str(), rename), ("", None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn ime_confirming_enter_after_rename_keeps_the_late_text_guard(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = fixture_root("flow-pending-late-enter");
        let (view, cx) = open_routing_view(cx, "body\n", &root);

        begin_rename(&view, cx, &rename_path(&root));
        replace(&view, cx, None, "Renamed");
        cx.update(|_, app| {
            view.update(app, |view, cx| view.confirm_inline_rename(cx));
        });

        // IME key-down while the rename is pending; its text callback is held.
        cx.update(|window, app| {
            window.dispatch_keystroke(gpui::Keystroke::parse("q").unwrap(), app);
        });
        cx.run_until_parked();
        assert!(!view.read_with(cx, |view, _| view.inline_rename_active()));

        // The Enter that confirms the composition arrives after the rename.
        cx.update(|window, app| {
            window.dispatch_keystroke(gpui::Keystroke::parse("enter").unwrap(), app);
        });

        // The delayed platform text callbacks still must not reach anything.
        replace(&view, cx, None, "q");
        mark(&view, cx, None, "q", None);
        unmark(&view, cx);
        assert_eq!(
            snapshot(&view, cx),
            ("body\n".to_owned(), String::new(), None),
            "late text after the confirming Enter must not reach the body"
        );

        // A new non-Enter keystroke ends the guard and typing works again.
        click_body(cx);
        cx.simulate_input("y");
        let (body, filter, rename) = snapshot(&view, cx);
        assert!(body.contains('y'));
        assert!(!body.contains('q'));
        assert_eq!((filter.as_str(), rename), ("", None));
        std::fs::remove_dir_all(root).unwrap();
    }
}
