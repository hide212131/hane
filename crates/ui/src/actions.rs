use crate::view::EditorView;
use gpui::{App, ClipboardItem, Context, InteractiveElement, KeyBinding, Window, actions};
use hane_editor::EditorCommand;
use hane_markdown::ListEditIntent;

macro_rules! command_actions {
    ($(
        $action:ident ($key:literal) => $handler:ident |$view:ident, $window:ident, $cx:ident| $body:block
    ),+ $(,)?) => {
        actions!(hane_editor, [$($action),+]);

        impl EditorView {
            $(
                fn $handler(
                    &mut self,
                    _: &$action,
                    window: &mut Window,
                    cx: &mut Context<Self>,
                ) {
                    let $view = self;
                    let $window = window;
                    let $cx = cx;
                    $body
                }
            )+
        }

        pub(crate) fn install_action_listeners(
            root: gpui::Div,
            cx: &mut Context<EditorView>,
        ) -> gpui::Div {
            let root = root;
            $(let root = root.on_action(cx.listener(EditorView::$handler));)+
            root
        }

        fn register_core_key_bindings(cx: &mut App) {
            cx.bind_keys([
                $(KeyBinding::new($key, $action, Some("HaneEditor"))),+
            ]);
        }
    };
}

/// Registers this platform's key bindings. Most bindings use `secondary-`,
/// which gpui resolves to `cmd` on macOS and `ctrl` elsewhere, so one binding
/// list covers both without touching macOS behavior. A few Windows/Linux
/// conventions (`ctrl-y` for redo, `ctrl-home`/`ctrl-end` for document
/// bounds) have no macOS equivalent and are layered on top instead.
pub fn register_key_bindings(cx: &mut App) {
    register_core_key_bindings(cx);
    register_secondary_platform_key_bindings(cx);
}

#[cfg(not(target_os = "macos"))]
fn register_secondary_platform_key_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-y", Redo, Some("HaneEditor")),
        KeyBinding::new("ctrl-home", DocumentStart, Some("HaneEditor")),
        KeyBinding::new("ctrl-end", DocumentEnd, Some("HaneEditor")),
        KeyBinding::new("ctrl-shift-home", SelectDocumentStart, Some("HaneEditor")),
        KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, Some("HaneEditor")),
    ]);
}

#[cfg(target_os = "macos")]
fn register_secondary_platform_key_bindings(_cx: &mut App) {}

command_actions! {
    Open ("secondary-o") => open_action |view, _window, cx| {
        if !view.inline_rename_active() { view.prompt_open(cx); }
    },
    OpenFolder ("secondary-shift-o") => open_folder_action |view, _window, cx| {
        if !view.inline_rename_active() { view.prompt_open_work_folder(cx); }
    },
    Save ("secondary-s") => save |view, _window, cx| {
        if !view.inline_rename_active() { view.save_or_prompt(cx); }
    },
    SaveAs ("secondary-shift-s") => save_as |view, _window, cx| {
        if !view.inline_rename_active() { view.prompt_save_as(cx); }
    },
    ToggleAutosave ("secondary-alt-a") => toggle_autosave_action |view, _window, cx| { view.toggle_autosave(cx); },
    ResetZoom ("secondary-0") => reset_zoom_action |view, _window, cx| { view.reset_zoom(cx); },
    Rename ("f2") => rename |view, window, cx| { view.begin_inline_rename_from_selection(window, cx); },
    Newline ("enter") => newline |view, _window, cx| {
        if view.sidebar_filter_is_focused() {
            if view.sidebar_filter_has_composition() {
                view.commit_sidebar_filter_composition(cx);
            }
            return;
        } else if view.inline_rename_active() {
            if view.inline_rename_has_composition() {
                view.commit_inline_rename_composition(cx);
            } else {
                view.confirm_inline_rename(cx);
            }
        } else if view.editor().ime().is_none()
            && !view.apply_list_edit_intent(ListEditIntent::Enter, cx)
        {
            view.insert_newline(cx);
        }
    },
    ShiftNewline ("shift-enter") => shift_newline |view, _window, cx| {
        if view.sidebar_filter_is_focused() {
            if view.sidebar_filter_has_composition() {
                view.commit_sidebar_filter_composition(cx);
            }
            return;
        } else if view.inline_rename_active() {
            if view.inline_rename_has_composition() {
                view.commit_inline_rename_composition(cx);
            } else {
                view.confirm_inline_rename(cx);
            }
        } else if view.editor().ime().is_none()
            && !view.apply_list_edit_intent(ListEditIntent::ShiftEnter, cx)
        {
            view.insert_newline(cx);
        }
    },
    Backspace ("backspace") => backspace |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.backspace_sidebar_filter(cx); } else if view.inline_rename_active() { view.backspace_inline_rename(cx); } else if !view.apply_list_edit_intent(ListEditIntent::Backspace, cx) { view.dispatch(EditorCommand::Backspace, cx); }
    },
    Indent ("tab") => indent |view, _window, cx| {
        if !view.sidebar_filter_is_focused() && !view.inline_rename_active() {
            view.apply_list_edit_intent(ListEditIntent::Indent, cx);
        }
    },
    Outdent ("shift-tab") => outdent |view, _window, cx| {
        if !view.sidebar_filter_is_focused() && !view.inline_rename_active() {
            view.apply_list_edit_intent(ListEditIntent::Outdent, cx);
        }
    },
    Delete ("delete") => delete |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.delete_sidebar_filter(cx); } else if view.inline_rename_active() { view.delete_inline_rename(cx); } else { view.dispatch(EditorCommand::Delete, cx); }
    },
    Left ("left") => left |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.move_sidebar_filter_left(false, cx); } else if view.inline_rename_active() { view.move_inline_rename_left(false, cx); } else { view.dispatch(EditorCommand::MoveLeft { extend: false }, cx); }
    },
    Right ("right") => right |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.move_sidebar_filter_right(false, cx); } else if view.inline_rename_active() { view.move_inline_rename_right(false, cx); } else { view.dispatch(EditorCommand::MoveRight { extend: false }, cx); }
    },
    Up ("up") => up |view, window, cx| {
        if !view.sidebar_filter_is_focused() && !view.inline_rename_active() { view.move_vertical(false, false, window, cx); }
    },
    Down ("down") => down |view, window, cx| {
        if !view.sidebar_filter_is_focused() && !view.inline_rename_active() { view.move_vertical(true, false, window, cx); }
    },
    SelectLeft ("shift-left") => select_left |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.select_sidebar_filter_left(cx); } else if view.inline_rename_active() { view.select_inline_rename_left(cx); } else { view.dispatch(EditorCommand::MoveLeft { extend: true }, cx); }
    },
    SelectRight ("shift-right") => select_right |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.select_sidebar_filter_right(cx); } else if view.inline_rename_active() { view.select_inline_rename_right(cx); } else { view.dispatch(EditorCommand::MoveRight { extend: true }, cx); }
    },
    SelectUp ("shift-up") => select_up |view, window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.move_vertical(false, true, window, cx);
    },
    SelectDown ("shift-down") => select_down |view, window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.move_vertical(true, true, window, cx);
    },
    SelectAll ("secondary-a") => select_all |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.select_all_sidebar_filter(cx); } else if view.inline_rename_active() { view.select_all_inline_rename(cx); } else { view.dispatch(EditorCommand::SelectAll, cx); }
    },
    Home ("home") => home |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.move_sidebar_filter_home(cx); } else if view.inline_rename_active() { view.move_inline_rename_home(cx); } else { view.dispatch(EditorCommand::MoveToLineStart { extend: false }, cx); }
    },
    End ("end") => end |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.move_sidebar_filter_end(cx); } else if view.inline_rename_active() { view.move_inline_rename_end(cx); } else { view.dispatch(EditorCommand::MoveToLineEnd { extend: false }, cx); }
    },
    SelectHome ("shift-home") => select_home |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.select_sidebar_filter_home(cx); return; }
        if view.inline_rename_active() { view.select_inline_rename_home(cx); return; }
        view.dispatch(EditorCommand::MoveToLineStart { extend: true }, cx);
    },
    SelectEnd ("shift-end") => select_end |view, _window, cx| {
        if view.sidebar_filter_is_focused() { view.select_sidebar_filter_end(cx); return; }
        if view.inline_rename_active() { view.select_inline_rename_end(cx); return; }
        view.dispatch(EditorCommand::MoveToLineEnd { extend: true }, cx);
    },
    DocumentStart ("secondary-up") => document_start |view, _window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.dispatch(EditorCommand::MoveToStart { extend: false }, cx);
    },
    DocumentEnd ("secondary-down") => document_end |view, _window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.dispatch(EditorCommand::MoveToEnd { extend: false }, cx);
    },
    SelectDocumentStart ("secondary-shift-up") => select_document_start |view, _window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.dispatch(EditorCommand::MoveToStart { extend: true }, cx);
    },
    SelectDocumentEnd ("secondary-shift-down") => select_document_end |view, _window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.dispatch(EditorCommand::MoveToEnd { extend: true }, cx);
    },
    Undo ("secondary-z") => undo |view, _window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.dispatch(EditorCommand::Undo, cx);
    },
    Redo ("secondary-shift-z") => redo |view, _window, cx| {
        if view.sidebar_filter_is_focused() || view.inline_rename_active() { return; }
        view.dispatch(EditorCommand::Redo, cx);
    },
    Copy ("secondary-c") => copy |view, _window, cx| {
        let text = if view.sidebar_filter_is_focused() {
            view.selected_sidebar_filter_text()
        } else if view.inline_rename_active() {
            view.selected_inline_rename_text()
        } else {
            view.editor().selected_text().ok()
        };
        if let Some(text) = text && !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    },
    Cut ("secondary-x") => cut |view, _window, cx| {
        let text = if view.sidebar_filter_is_focused() {
            view.selected_sidebar_filter_text()
        } else if view.inline_rename_active() {
            view.selected_inline_rename_text()
        } else {
            view.editor().selected_text().ok()
        };
        if let Some(text) = text && !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            if view.sidebar_filter_is_focused() {
                view.delete_sidebar_filter(cx);
            } else if view.inline_rename_active() {
                view.delete_inline_rename(cx);
            } else {
                view.dispatch(EditorCommand::Backspace, cx);
            }
        }
    },
    Paste ("secondary-v") => paste |view, _window, cx| {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            if view.sidebar_filter_is_focused() {
                view.replace_sidebar_filter_text(None, &text, cx);
            } else if view.inline_rename_active() {
                view.replace_inline_rename_text(None, &text, cx);
            } else {
                view.dispatch(EditorCommand::Insert(&text), cx);
            }
        }
    },
    CancelComposition ("escape") => cancel_composition |view, _window, cx| {
        if view.sidebar_filter_is_focused() {
            if view.sidebar_filter_has_composition() {
                view.cancel_sidebar_filter_composition(cx);
            } else {
                view.blur_sidebar_filter(cx);
            }
        } else if view.inline_rename_active() {
            if view.inline_rename_has_composition() {
                view.cancel_inline_rename_composition(cx);
            } else {
                view.cancel_inline_rename(cx);
            }
        } else {
            view.perform_cancel_composition(cx);
        }
    },
}
