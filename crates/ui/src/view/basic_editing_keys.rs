//! Issue #494: tests for the OS-standard basic editing keys and the footer AI
//! button. Registered keys are sent through the real keymap and the reached
//! position is checked by inserting text there.

#[cfg(test)]
mod tests {
    use super::super::*;
    use gpui::MouseButton;

    const MAC: bool = cfg!(target_os = "macos");

    const WORD_LEFT: &str = if MAC { "alt-left" } else { "ctrl-left" };
    const WORD_RIGHT: &str = if MAC { "alt-right" } else { "ctrl-right" };
    const SELECT_WORD_RIGHT: &str = if MAC {
        "alt-shift-right"
    } else {
        "ctrl-shift-right"
    };
    const DELETE_WORD_BACK: &str = if MAC { "alt-backspace" } else { "ctrl-backspace" };
    const DELETE_WORD_FORWARD: &str = if MAC { "alt-delete" } else { "ctrl-delete" };
    const PARAGRAPH_UP: &str = if MAC { "alt-up" } else { "ctrl-up" };
    const PARAGRAPH_DOWN: &str = if MAC { "alt-down" } else { "ctrl-down" };
    const SELECT_PARAGRAPH_DOWN: &str = if MAC {
        "alt-shift-down"
    } else {
        "ctrl-shift-down"
    };
    const DOCUMENT_START: &str = if MAC { "cmd-up" } else { "ctrl-home" };
    const DOCUMENT_END: &str = if MAC { "cmd-down" } else { "ctrl-end" };
    const LINE_START: &str = if MAC { "cmd-left" } else { "home" };
    const LINE_END: &str = if MAC { "cmd-right" } else { "end" };
    const PAGE_MOVE_DOWN: &str = if MAC { "alt-pagedown" } else { "pagedown" };

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    fn open_view<'a>(cx: &'a mut gpui::TestAppContext, text: &str) -> (TestView, &'a mut TestCx) {
        cx.update(crate::actions::register_key_bindings);
        let text = text.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&text, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        // Focus the body the way a click into the document does.
        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
        (view, cx)
    }

    fn set_selection(view: &TestView, cx: &mut TestCx, anchor: usize, active: usize) {
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(anchor),
                    active: SourceOffset(active),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn selection(view: &TestView, cx: &mut TestCx) -> Selection {
        view.read_with(cx, |view, _| view.editor().selection())
    }

    fn text(view: &TestView, cx: &mut TestCx) -> String {
        view.read_with(cx, |view, _| view.editor().document().full_text())
    }

    fn press(cx: &mut TestCx, keys: &str) {
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
    }

    /// Where the caret is, proven by typing there rather than by reading the
    /// selection back.
    fn caret_by_insertion(view: &TestView, cx: &mut TestCx) -> usize {
        view.update(cx, |view, cx| view.insert_text("\u{2022}", cx));
        let text = text(view, cx);
        let at = text.find('\u{2022}').expect("marker inserted");
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        at
    }

    #[gpui::test]
    fn word_keys_reach_the_os_specific_offsets(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "one two\n");
        set_selection(&view, cx, 0, 0);
        let (first, second) = if MAC { (3, 7) } else { (4, 7) };
        press(cx, WORD_RIGHT);
        assert_eq!(caret_by_insertion(&view, cx), first);
        assert_eq!(text(&view, cx), "one two\n");
        press(cx, WORD_RIGHT);
        assert_eq!(caret_by_insertion(&view, cx), second);

        press(cx, WORD_LEFT);
        assert_eq!(caret_by_insertion(&view, cx), 4);
        press(cx, WORD_LEFT);
        assert_eq!(caret_by_insertion(&view, cx), 0);

        press(cx, SELECT_WORD_RIGHT);
        assert_eq!(
            selection(&view, cx),
            Selection {
                anchor: SourceOffset(0),
                active: SourceOffset(first)
            }
        );
    }

    #[gpui::test]
    fn paragraph_keys_follow_source_lines_and_leave_document_keys_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "one\ntwo\nlast");
        set_selection(&view, cx, 0, 0);
        let expected: &[usize] = if MAC { &[3, 7, 12] } else { &[4, 8, 12] };
        for want in expected {
            press(cx, PARAGRAPH_DOWN);
            assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(*want)));
        }
        set_selection(&view, cx, 9, 9);
        for want in [8, 4, 0] {
            press(cx, PARAGRAPH_UP);
            assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(want)));
        }

        // Ctrl+Up/Down are not document moves any more: from the middle of the
        // text they stop at a paragraph edge, while the document keys go to 0.
        set_selection(&view, cx, 9, 9);
        press(cx, PARAGRAPH_UP);
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(8)));
        press(cx, DOCUMENT_START);
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(0)));
        press(cx, DOCUMENT_END);
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(12)));

        set_selection(&view, cx, 1, 1);
        press(cx, SELECT_PARAGRAPH_DOWN);
        assert_eq!(selection(&view, cx).anchor, SourceOffset(1));
        assert_eq!(selection(&view, cx).active, SourceOffset(if MAC { 3 } else { 4 }));
    }

    #[gpui::test]
    fn line_keys_keep_the_source_line_meaning(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "abc\ndef\n");
        set_selection(&view, cx, 5, 5);
        press(cx, LINE_END);
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(7)));
        press(cx, LINE_START);
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(4)));
    }

    #[gpui::test]
    fn select_all_replaces_the_document_on_both_platforms(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "one two\n");
        press(cx, "secondary-a");
        view.update(cx, |view, cx| view.insert_text("Z", cx));
        assert_eq!(text(&view, cx), "Z");
    }

    #[gpui::test]
    fn word_delete_keys_are_one_undo_step_and_leave_the_clipboard_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "one two");
        cx.update(|_, app| app.write_to_clipboard(gpui::ClipboardItem::new_string("keep".into())));
        set_selection(&view, cx, 7, 7);
        press(cx, DELETE_WORD_BACK);
        assert_eq!(text(&view, cx), "one ");
        press(cx, "secondary-z");
        assert_eq!(text(&view, cx), "one two");
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(7)));

        set_selection(&view, cx, 0, 0);
        press(cx, DELETE_WORD_FORWARD);
        assert_eq!(text(&view, cx), if MAC { " two" } else { "two" });
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("keep".to_owned())
        );

        // A selection is deleted as is, and Undo restores it.
        press(cx, "secondary-z");
        set_selection(&view, cx, 4, 1);
        press(cx, DELETE_WORD_BACK);
        assert_eq!(text(&view, cx), "otwo");
        press(cx, "secondary-z");
        assert_eq!(text(&view, cx), "one two");
        assert_eq!(
            selection(&view, cx),
            Selection {
                anchor: SourceOffset(4),
                active: SourceOffset(1)
            }
        );
    }

    #[gpui::test]
    fn shift_line_selection_then_delete_edits_a_line(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "abc\ndef\n");
        set_selection(&view, cx, 4, 4);
        press(cx, if MAC { "cmd-shift-right" } else { "shift-end" });
        assert_eq!(
            selection(&view, cx),
            Selection {
                anchor: SourceOffset(4),
                active: SourceOffset(7)
            }
        );
        press(cx, "backspace");
        assert_eq!(text(&view, cx), "abc\n\n");
    }

    #[gpui::test]
    fn new_keys_do_not_commit_an_ime_composition(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "abc");
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(1)))
                .unwrap();
            view.editor_mut()
                .replace_and_mark_text(None, "に", Some(1..1))
                .unwrap();
            cx.notify();
        });
        let before = text(&view, cx);
        for key in [
            WORD_RIGHT,
            WORD_LEFT,
            PARAGRAPH_DOWN,
            DELETE_WORD_BACK,
            DELETE_WORD_FORWARD,
            PAGE_MOVE_DOWN,
        ] {
            press(cx, key);
            view.read_with(cx, |view, _| {
                assert!(view.editor().ime().is_some(), "{key} committed the composition");
                assert_eq!(view.editor().document().full_text(), before, "{key}");
            });
        }
    }

    #[gpui::test]
    fn new_keys_do_not_leak_into_the_body_from_the_sidebar_filter(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "one two\n");
        set_selection(&view, cx, 4, 4);
        view.update(cx, |view, cx| {
            view.sidebar_filter_focused = true;
            view.sidebar_keyboard_focus = true;
            view.sidebar_filter = "abc def".to_owned();
            view.sidebar_filter_selected_range = 7..7;
            cx.notify();
        });
        cx.run_until_parked();
        for key in [
            WORD_LEFT,
            DELETE_WORD_BACK,
            PARAGRAPH_UP,
            DOCUMENT_START,
            PAGE_MOVE_DOWN,
        ] {
            press(cx, key);
        }
        assert_eq!(text(&view, cx), "one two\n");
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(4)));
    }

    #[gpui::test]
    fn emacs_style_control_letters_are_not_bound(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "one two\n");
        set_selection(&view, cx, 3, 3);
        let keys: &[&str] = if MAC {
            &[
                "ctrl-a", "ctrl-e", "ctrl-b", "ctrl-f", "ctrl-p", "ctrl-n", "ctrl-k", "ctrl-y",
                "ctrl-h", "ctrl-d", "cmd-backspace", "cmd-delete",
            ]
        } else {
            &[
                "ctrl-e",
                "ctrl-b",
                "ctrl-p",
                "ctrl-n",
                "ctrl-k",
                "ctrl-h",
                "ctrl-d",
                "ctrl-shift-backspace",
                "ctrl-shift-delete",
            ]
        };
        for key in keys {
            press(cx, key);
            assert_eq!(text(&view, cx), "one two\n", "{key}");
            assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(3)), "{key}");
        }
    }

    fn long_document() -> String {
        (0..200).map(|index| format!("line {index}\n")).collect()
    }

    #[gpui::test]
    fn page_move_travels_about_a_viewport_and_stops_at_the_document_end(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, &long_document());
        set_selection(&view, cx, 0, 0);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.move_page(true, false, window, cx));
        });
        cx.run_until_parked();
        let moved_to = selection(&view, cx);
        assert_eq!(moved_to.anchor, moved_to.active, "plain page move collapses");
        view.read_with(cx, |view, _| {
            let page = (view.viewport_height - view.line_height()).max(view.line_height());
            let active = view.editor().selection().active;
            let (top, _) = view.source_offset_row_bounds(active).expect("row bounds");
            let (start, _) = view
                .source_offset_row_bounds(SourceOffset(0))
                .expect("row bounds");
            assert!(
                top - start >= page * 0.8,
                "moved {} for a page of {page}",
                top - start
            );
            let line = view.editor().document().line_for_offset(active).unwrap().0;
            assert!(line > 1 && line < 150, "line {line}");
        });

        // Shift keeps the anchor.
        set_selection(&view, cx, 0, 0);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.move_page(true, true, window, cx));
        });
        assert_eq!(selection(&view, cx).anchor, SourceOffset(0));
        assert!(selection(&view, cx).active.0 > 0);

        // The end of the document stops the move instead of looping.
        let end = long_document().len();
        set_selection(&view, cx, end, end);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.move_page(true, false, window, cx));
        });
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(end)));

        // A viewport smaller than a row still moves at least one row.
        set_selection(&view, cx, 0, 0);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.viewport_height = 1.0;
                view.move_page(true, false, window, cx);
            });
        });
        assert!(selection(&view, cx).active.0 > 0);
    }

    #[gpui::test]
    fn scroll_only_operations_keep_text_selection_and_history(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, &long_document());
        set_selection(&view, cx, 10, 40);
        let before_text = text(&view, cx);
        let before_selection = selection(&view, cx);

        view.update(cx, |view, cx| view.scroll_to_document_edge(true, cx));
        cx.run_until_parked();
        let at_end = view.read_with(cx, |view, _| view.scroll_y);
        assert!(at_end > 0.0);
        view.update(cx, |view, cx| view.scroll_page(false, cx));
        cx.run_until_parked();
        let paged = view.read_with(cx, |view, _| view.scroll_y);
        assert!(paged < at_end, "page up scrolls back up and stays there");
        view.update(cx, |view, cx| view.scroll_to_document_edge(false, cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.scroll_y), 0.0);

        view.read_with(cx, |view, _| {
            assert!(!view.editor().can_undo());
            assert!(!view.pending_caret_visibility_after_layout);
        });
        assert_eq!(text(&view, cx), before_text);
        assert_eq!(selection(&view, cx), before_selection);
    }

    #[gpui::test]
    fn center_cursor_line_puts_the_caret_row_in_the_middle(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, &long_document());
        let caret = long_document().find("line 100").unwrap();
        set_selection(&view, cx, caret, caret);
        view.update(cx, |view, cx| {
            view.center_cursor_line(cx);
            let (top, height) = view
                .source_offset_row_bounds(SourceOffset(caret))
                .expect("row bounds");
            let middle = view.scroll_y + view.viewport_height / 2.0;
            assert!((top + height / 2.0 - middle).abs() < 1.0);
            assert!(!view.pending_caret_visibility_after_layout);
        });
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(caret)));
        assert!(!view.read_with(cx, |view, _| view.editor().can_undo()));
    }

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn mac_home_end_and_page_keys_only_scroll(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, &long_document());
        set_selection(&view, cx, 10, 40);
        press(cx, "end");
        assert!(view.read_with(cx, |view, _| view.scroll_y) > 0.0);
        press(cx, "pageup");
        press(cx, "home");
        assert_eq!(view.read_with(cx, |view, _| view.scroll_y), 0.0);
        press(cx, "pagedown");
        assert!(view.read_with(cx, |view, _| view.scroll_y) > 0.0);
        assert_eq!(
            selection(&view, cx),
            Selection {
                anchor: SourceOffset(10),
                active: SourceOffset(40)
            }
        );
        assert_eq!(text(&view, cx), long_document());
        press(cx, "shift-end");
        assert_eq!(
            selection(&view, cx),
            Selection {
                anchor: SourceOffset(10),
                active: SourceOffset(long_document().len())
            }
        );
    }

    #[gpui::test]
    fn footer_ai_button_opens_the_popup_over_the_existing_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "one two three\n");
        set_selection(&view, cx, 4, 7);
        let button = cx
            .debug_bounds("footer-ai-selection")
            .expect("the footer shows the AI button")
            .center();
        cx.simulate_mouse_down(button, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(button, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_selection.open);
            assert_eq!(view.ai_selection.original_text, "two");
            assert_eq!(view.editor().document().full_text(), "one two three\n");
        });
        assert_eq!(
            selection(&view, cx),
            Selection {
                anchor: SourceOffset(4),
                active: SourceOffset(7)
            }
        );
    }

    #[gpui::test]
    fn footer_ai_button_without_a_selection_only_shows_guidance(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "one two\n");
        set_selection(&view, cx, 2, 2);
        let button = cx
            .debug_bounds("footer-ai-selection")
            .expect("the footer shows the AI button")
            .center();
        cx.simulate_mouse_down(button, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(button, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(!view.ai_selection.open));
        assert_eq!(selection(&view, cx), Selection::caret(SourceOffset(2)));
    }
}
