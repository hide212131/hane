//! The document body's right-click menu: copy, cut, paste and "リンクを挿入".
//!
//! A right press never goes through the left-button row handler, so it moves no
//! caret, opens no link and toggles no checkbox. The menu captures the document
//! instance, revision, directional selection and the clipboard URL when it
//! opens. Every item re-checks all of that right before acting, so a menu left
//! over from a tab switch, edit, reload or IME start does nothing.
//!
//! "リンクを挿入" is planned by `hane_markdown::plan_link_insertion` and applied
//! through the shared `Editor::replace_range_recorded` and
//! `EditorView::after_input`; this module adds no replacement or history API.

use super::*;
use crate::view::input_routing::InputTarget;
use hane_markdown::{
    LinkInsertionPlan, LinkInsertionRefusal, clipboard_http_url, plan_link_insertion,
};

/// State of the open body menu.
#[derive(Clone, Debug)]
pub(super) struct EditorContextMenu {
    pub(super) position: gpui::Point<Pixels>,
    pub(super) id: SessionId,
    pub(super) instance: DocumentInstance,
    pub(super) revision: Revision,
    pub(super) selection: Selection,
    /// Whether the clipboard held text when the menu opened.
    pub(super) has_clipboard_text: bool,
    /// The HTTP(S) URL read from the clipboard when the menu opened. The
    /// clipboard is never read again for the link item.
    pub(super) link_url: Option<String>,
    /// Whether the link item applied to the selection when the menu opened.
    pub(super) link_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditorMenuAction {
    Copy,
    Cut,
    Paste,
    InsertLink,
}

impl EditorView {
    /// Right press on the document body. Deliberately separate from
    /// `on_editor_mouse_down` / `on_row_mouse_down`, which are left-button
    /// handlers that move the caret, follow links and toggle checkboxes.
    pub(super) fn on_editor_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.open_editor_context_menu(event.position, cx);
    }

    /// Whether the body may own a context menu now: not while settings, a
    /// modal, the AI popup, or another input (sidebar filter, rename, search,
    /// find) owns the keyboard.
    fn editor_context_menu_may_open(&self) -> bool {
        !self.settings_open()
            && self.tab_close_confirm.is_none()
            && self.save_as_dialogs_open == 0
            && !self.ai_selection_blocks_editor_keys()
            && !self.content_search_input_is_focused()
            && !self.content_search_results_focused()
            && !self.document_find_input_is_focused()
            && matches!(self.input_target(), InputTarget::Body)
    }

    fn open_editor_context_menu(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if !self.editor_context_menu_may_open() {
            return;
        }
        let id = self.sessions.active_id();
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        let session = self.sessions.active();
        let revision = session.revision();
        let selection = session.editor().selection();
        let clipboard_text = cx.read_from_clipboard().and_then(|item| item.text());
        let has_clipboard_text = clipboard_text.as_deref().is_some_and(|text| !text.is_empty());
        let link_url = clipboard_text.as_deref().and_then(clipboard_http_url);
        let link_available = self.body_editable()
            && self.editor().ime().is_none()
            && link_url
                .as_deref()
                .is_some_and(|url| self.plan_link_for(selection, url).is_ok());
        self.file_tab_context_menu = None;
        self.sidebar_context_menu = None;
        self.editor_context_menu = Some(EditorContextMenu {
            position,
            id,
            instance,
            revision,
            selection,
            has_clipboard_text,
            link_url,
            link_available,
        });
        cx.notify();
    }

    /// The link plan for `selection` against the current document text.
    fn plan_link_for(
        &self,
        selection: Selection,
        url: &str,
    ) -> Result<LinkInsertionPlan, LinkInsertionRefusal> {
        let range = selection.range();
        if range.is_empty() {
            return Err(LinkInsertionRefusal::EmptySelection);
        }
        let document = self.editor().document();
        let source = document
            .text(SourceRange::new(0, document.len_bytes().0))
            .map_err(|_| LinkInsertionRefusal::OutOfRange)?;
        plan_link_insertion(&source, range, url)
    }

    /// Whether `menu` still describes the live state: the same document
    /// instance is active with the same revision and directional selection, no
    /// IME composition is running, and nothing else owns the keyboard.
    pub(super) fn editor_context_menu_is_current(&self, menu: &EditorContextMenu) -> bool {
        self.sessions.active_id() == menu.id
            && self.document_instance(menu.id) == Some(menu.instance)
            && self.sessions.get(menu.id).is_some_and(|session| {
                session.revision() == menu.revision
                    && session.editor().selection() == menu.selection
                    && session.editor().ime().is_none()
            })
            && self.editor_context_menu_may_open()
    }

    pub(crate) fn dismiss_editor_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.editor_context_menu.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Mouse press outside the menu. A left (or other non-right) press only
    /// closes the menu and is not passed on, so it cannot move the selection,
    /// follow a link or toggle a checkbox behind it. A right press is passed
    /// on so it can open the body's or another surface's own menu.
    fn close_editor_context_menu(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor_context_menu.take().is_some() {
            if event.button != MouseButton::Right {
                cx.stop_propagation();
            }
            cx.notify();
        }
    }

    pub(super) fn editor_context_menu_element(
        &self,
        menu: &EditorContextMenu,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let editable = self.body_editable();
        let has_selection = !menu.selection.range().is_empty();
        let show_link = menu.link_available && editable && self.editor().ime().is_none();

        let copy_menu = menu.clone();
        let copy_item = self.file_tab_context_menu_item(
            "editor-context-copy",
            "コピー",
            has_selection,
            cx,
            move |view, _, window, cx| {
                view.run_editor_menu_action(&copy_menu, EditorMenuAction::Copy, window, cx);
            },
        );
        let cut_menu = menu.clone();
        let cut_item = self.file_tab_context_menu_item(
            "editor-context-cut",
            "切り取り",
            has_selection && editable,
            cx,
            move |view, _, window, cx| {
                view.run_editor_menu_action(&cut_menu, EditorMenuAction::Cut, window, cx);
            },
        );
        let paste_menu = menu.clone();
        let paste_item = self.file_tab_context_menu_item(
            "editor-context-paste",
            "貼り付け",
            menu.has_clipboard_text && editable,
            cx,
            move |view, _, window, cx| {
                view.run_editor_menu_action(&paste_menu, EditorMenuAction::Paste, window, cx);
            },
        );

        let menu_element = div()
            .id("editor-context-menu")
            .debug_selector(|| "editor-context-menu".to_owned())
            // Occludes its own bounds so a press on padding or a disabled row
            // does not reach the document behind the menu.
            .occlude()
            .min_w(px(180.0))
            .flex()
            .flex_col()
            .p_1()
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.header_foreground))
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .on_mouse_down_out(cx.listener(Self::close_editor_context_menu))
            .child(copy_item)
            .child(cut_item)
            .child(paste_item);
        if !show_link {
            return menu_element;
        }
        let link_menu = menu.clone();
        let link_item = self.file_tab_context_menu_item(
            "editor-context-insert-link",
            "リンクを挿入",
            true,
            cx,
            move |view, _, window, cx| {
                view.run_editor_menu_action(&link_menu, EditorMenuAction::InsertLink, window, cx);
            },
        );
        menu_element
            .child(
                div()
                    .w_full()
                    .h(px(1.0))
                    .my_1()
                    .bg(rgb(self.theme.quote_foreground)),
            )
            .child(link_item)
    }

    /// Runs one menu item. The menu closes first; a menu that is no longer
    /// current does nothing at all.
    fn run_editor_menu_action(
        &mut self,
        menu: &EditorContextMenu,
        action: EditorMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.editor_context_menu_is_current(menu);
        self.editor_context_menu = None;
        if !current {
            cx.notify();
            return;
        }
        window.focus(&self.focus_handle, cx);
        match action {
            // The existing actions keep their own routing and guards (read-only
            // body, other inputs), so the menu and the keyboard cannot diverge.
            EditorMenuAction::Copy => {
                window.dispatch_action(Box::new(crate::actions::Copy), cx);
            }
            EditorMenuAction::Cut => {
                window.dispatch_action(Box::new(crate::actions::Cut), cx);
            }
            EditorMenuAction::Paste => {
                window.dispatch_action(Box::new(crate::actions::Paste), cx);
            }
            EditorMenuAction::InsertLink => self.insert_link_from_menu(menu, cx),
        }
        cx.notify();
    }

    /// Replaces the menu's selection with `[selection](url)` as one undoable
    /// transaction. Every guard is checked again here; any failure leaves the
    /// document, history, selection and dirty state untouched.
    fn insert_link_from_menu(&mut self, menu: &EditorContextMenu, cx: &mut Context<Self>) {
        if !self.body_editable()
            || self.editor().ime().is_some()
            || !self.editor_context_menu_is_current(menu)
        {
            return;
        }
        let Some(url) = menu.link_url.as_deref() else {
            return;
        };
        let plan = match self.plan_link_for(menu.selection, url) {
            Ok(plan) => plan,
            Err(refusal) => {
                self.status = Some(refusal.message().to_owned());
                return;
            }
        };
        // No composition is running (checked above), so the commit inside
        // `replace_range_recorded` cannot finalise or cancel an IME session.
        match self.editor_mut().replace_range_recorded(
            plan.range,
            &plan.replacement,
            Selection::caret(plan.caret_after),
        ) {
            Ok(_) => {
                self.status = None;
                self.after_input(cx);
            }
            Err(error) => self.report_error("link insertion", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/hane";

    fn open_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        text: &str,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        let text = text.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&text, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        (view, cx)
    }

    fn select(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        selection: Selection,
    ) {
        view.update(cx, |view, cx| {
            view.editor_mut().set_selection(selection).unwrap();
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn right_click_row(cx: &mut gpui::VisualTestContext) {
        let bounds = cx.debug_bounds("row-0-0").expect("row painted");
        let point = bounds.center();
        cx.simulate_mouse_down(point, MouseButton::Right, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Right, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn close_menu(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext) {
        view.update(cx, |view, cx| view.dismiss_editor_context_menu(cx));
        cx.run_until_parked();
    }

    /// Runs the item callback the way a delayed click would, with the snapshot
    /// the menu was opened with.
    fn run_item(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        menu: &EditorContextMenu,
        action: EditorMenuAction,
    ) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.run_editor_menu_action(menu, action, window, cx);
            });
        });
        cx.run_until_parked();
    }

    fn click_item(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let point = cx.debug_bounds(selector).expect("item painted").center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn text_of(view: &gpui::Entity<EditorView>, cx: &gpui::VisualTestContext) -> String {
        view.read_with(cx, |view, _| view.editor().document().full_text())
    }

    fn menu_is_open(view: &gpui::Entity<EditorView>, cx: &gpui::VisualTestContext) -> bool {
        view.read_with(cx, |view, _| view.editor_context_menu.is_some())
    }

    /// A1 / A3 / A6 / A9: the item appears for a URL on the clipboard, keeps the
    /// (reversed) selection while open, never opens the browser, links only the
    /// selection, and is one separate undo step that restores the selection.
    #[gpui::test]
    fn insert_link_replaces_only_the_selection_as_one_undo_step(cx: &mut gpui::TestAppContext) {
        let text = "前 Haneの紹介 後\n";
        let (view, cx) = open_view(cx, text);
        let start = text.find("Hane").unwrap();
        let end = start + "Haneの紹介".len();
        let reversed = Selection {
            anchor: SourceOffset(end),
            active: SourceOffset(start),
        };
        select(&view, cx, reversed);
        cx.write_to_clipboard(ClipboardItem::new_string(format!("{URL}\n")));

        right_click_row(cx);
        assert!(menu_is_open(&view, cx));
        view.read_with(cx, |view, _| {
            // The right press kept the selection and edited nothing.
            assert_eq!(view.editor().selection(), reversed);
            assert_eq!(view.editor().document().full_text(), text);
        });
        assert!(cx.debug_bounds("editor-context-insert-link").is_some());

        // The clipboard changing while the menu is open does not change the
        // snapshot URL the item uses.
        cx.write_to_clipboard(ClipboardItem::new_string("https://other.example/".to_owned()));
        click_item(cx, "editor-context-insert-link");

        let linked = format!("前 [Haneの紹介]({URL}) 後\n");
        assert_eq!(text_of(&view, cx), linked);
        assert!(cx.opened_url().is_none());
        assert!(!menu_is_open(&view, cx));
        let caret = SourceOffset(start + format!("[Haneの紹介]({URL})").len());
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection::caret(caret));
            assert!(view.editor().can_undo());
        });

        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(text_of(&view, cx), text);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), reversed);
            assert!(!view.editor().can_undo());
        });
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Redo, cx));
        assert_eq!(text_of(&view, cx), linked);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection::caret(caret));
        });
    }

    /// A2: the item only appears for an HTTP(S) URL (and a non-empty
    /// selection); the rest of the menu is still there.
    #[gpui::test]
    fn link_item_is_shown_only_for_a_valid_url_and_selection(cx: &mut gpui::TestAppContext) {
        let text = "label text\n";
        let (view, cx) = open_view(cx, text);
        select(
            &view,
            cx,
            Selection {
                anchor: SourceOffset(0),
                active: SourceOffset(5),
            },
        );
        for clipboard in [
            "www.example.com",
            "javascript:alert(1)",
            "https://a.example/ https://b.example/",
            "",
        ] {
            cx.write_to_clipboard(ClipboardItem::new_string(clipboard.to_owned()));
            right_click_row(cx);
            assert!(menu_is_open(&view, cx), "{clipboard:?}");
            assert!(cx.debug_bounds("editor-context-copy").is_some());
            assert!(
                cx.debug_bounds("editor-context-insert-link").is_none(),
                "{clipboard:?}"
            );
            close_menu(&view, cx);
        }

        // No selection: no link item either.
        select(&view, cx, Selection::caret(SourceOffset(2)));
        cx.write_to_clipboard(ClipboardItem::new_string(URL.to_owned()));
        right_click_row(cx);
        assert!(menu_is_open(&view, cx));
        assert!(cx.debug_bounds("editor-context-insert-link").is_none());
        assert_eq!(text_of(&view, cx), text);
    }

    /// A5: a selection that cannot become a link offers no item and edits
    /// nothing.
    #[gpui::test]
    fn link_item_is_hidden_for_an_unsupported_selection(cx: &mut gpui::TestAppContext) {
        let text = "[a](https://x.example/) b\n";
        let (view, cx) = open_view(cx, text);
        select(
            &view,
            cx,
            Selection {
                anchor: SourceOffset(1),
                active: SourceOffset(2),
            },
        );
        cx.write_to_clipboard(ClipboardItem::new_string(URL.to_owned()));
        right_click_row(cx);
        assert!(menu_is_open(&view, cx));
        assert!(cx.debug_bounds("editor-context-insert-link").is_none());
        assert_eq!(text_of(&view, cx), text);
    }

    /// A7: a menu whose selection, revision or document changed is dropped, and
    /// a delayed item callback for it does nothing.
    #[gpui::test]
    fn a_stale_menu_never_edits(cx: &mut gpui::TestAppContext) {
        let text = "label text\n";
        let (view, cx) = open_view(cx, text);
        let selection = Selection {
            anchor: SourceOffset(0),
            active: SourceOffset(5),
        };
        select(&view, cx, selection);
        cx.write_to_clipboard(ClipboardItem::new_string(URL.to_owned()));
        right_click_row(cx);
        let menu = view
            .read_with(cx, |view, _| view.editor_context_menu.clone())
            .expect("menu open");

        // Selection change: the menu is dropped, and a delayed item callback
        // for it edits nothing.
        select(&view, cx, Selection::caret(SourceOffset(3)));
        assert!(!menu_is_open(&view, cx));
        run_item(&view, cx, &menu, EditorMenuAction::InsertLink);
        assert_eq!(text_of(&view, cx), text);
        view.read_with(cx, |view, _| {
            assert!(!view.editor().can_undo());
            assert_eq!(view.editor().selection(), Selection::caret(SourceOffset(3)));
        });

        // Document edit after the selection was restored: same result.
        select(&view, cx, selection);
        right_click_row(cx);
        let menu = view
            .read_with(cx, |view, _| view.editor_context_menu.clone())
            .expect("menu open");
        view.update(cx, |view, cx| {
            view.editor_mut().set_selection(Selection::caret(SourceOffset(0))).unwrap();
            view.insert_text("x", cx);
            view.editor_mut().set_selection(selection).unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        let edited = text_of(&view, cx);
        run_item(&view, cx, &menu, EditorMenuAction::InsertLink);
        assert_eq!(text_of(&view, cx), edited);
    }

    /// A8: reading mode offers copy but no link insertion.
    #[gpui::test]
    fn reading_mode_has_no_link_item(cx: &mut gpui::TestAppContext) {
        let text = "label text\n";
        let (view, cx) = open_view(cx, text);
        select(
            &view,
            cx,
            Selection {
                anchor: SourceOffset(0),
                active: SourceOffset(5),
            },
        );
        view.update(cx, |view, cx| {
            view.editor_mode = EditorMode::Reading;
            cx.notify();
        });
        cx.run_until_parked();
        cx.write_to_clipboard(ClipboardItem::new_string(URL.to_owned()));
        right_click_row(cx);
        assert!(menu_is_open(&view, cx));
        assert!(cx.debug_bounds("editor-context-insert-link").is_none());
        assert_eq!(text_of(&view, cx), text);
    }

    /// A9: Escape-equivalent dismissal and an outside press both close the menu
    /// without moving the selection.
    #[gpui::test]
    fn dismissing_the_menu_keeps_the_selection(cx: &mut gpui::TestAppContext) {
        let text = "label text\n";
        let (view, cx) = open_view(cx, text);
        let selection = Selection {
            anchor: SourceOffset(0),
            active: SourceOffset(5),
        };
        select(&view, cx, selection);
        cx.write_to_clipboard(ClipboardItem::new_string(URL.to_owned()));

        right_click_row(cx);
        assert!(view.update(cx, |view, cx| view.dismiss_editor_context_menu(cx)));
        assert!(!menu_is_open(&view, cx));
        assert!(!view.update(cx, |view, cx| view.dismiss_editor_context_menu(cx)));

        right_click_row(cx);
        let menu_bounds = cx
            .debug_bounds("editor-context-menu")
            .expect("menu painted");
        // A left press on the row far from the menu closes it and is not
        // delivered to the row, so the selection stays.
        let outside = gpui::point(px(900.0), px(700.0));
        assert!(!menu_bounds.contains(&outside));
        cx.simulate_mouse_down(outside, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(outside, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(!menu_is_open(&view, cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), selection);
            assert_eq!(view.editor().document().full_text(), text);
        });
    }
}
