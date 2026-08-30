use crate::view::EditorView;
use gpui::{App, ClipboardItem, Context, InteractiveElement, KeyBinding, Window, actions};
use hane_editor::EditorCommand;

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

        pub fn register_key_bindings(cx: &mut App) {
            cx.bind_keys([
                $(KeyBinding::new($key, $action, Some("HaneEditor"))),+
            ]);
        }
    };
}

command_actions! {
    Open ("cmd-o") => open_action |view, _window, cx| { view.prompt_open(cx); },
    OpenFolder ("cmd-shift-o") => open_folder_action |view, _window, cx| { view.prompt_open_work_folder(cx); },
    Save ("cmd-s") => save |view, _window, cx| { view.save_or_prompt(cx); },
    SaveAs ("cmd-shift-s") => save_as |view, _window, cx| { view.prompt_save_as(cx); },
    ToggleAutosave ("cmd-alt-a") => toggle_autosave_action |view, _window, cx| { view.toggle_autosave(cx); },
    Newline ("enter") => newline |view, _window, cx| {
        if view.editor().ime().is_none() {
            view.dispatch(EditorCommand::Insert("\n"), cx);
        }
    },
    ShiftNewline ("shift-enter") => shift_newline |view, _window, cx| {
        if view.editor().ime().is_none() {
            view.dispatch(EditorCommand::Insert("\n"), cx);
        }
    },
    Backspace ("backspace") => backspace |view, _window, cx| { view.dispatch(EditorCommand::Backspace, cx); },
    Delete ("delete") => delete |view, _window, cx| { view.dispatch(EditorCommand::Delete, cx); },
    Left ("left") => left |view, _window, cx| { view.dispatch(EditorCommand::MoveLeft { extend: false }, cx); },
    Right ("right") => right |view, _window, cx| { view.dispatch(EditorCommand::MoveRight { extend: false }, cx); },
    Up ("up") => up |view, window, cx| { view.move_vertical(false, false, window, cx); },
    Down ("down") => down |view, window, cx| { view.move_vertical(true, false, window, cx); },
    SelectLeft ("shift-left") => select_left |view, _window, cx| { view.dispatch(EditorCommand::MoveLeft { extend: true }, cx); },
    SelectRight ("shift-right") => select_right |view, _window, cx| { view.dispatch(EditorCommand::MoveRight { extend: true }, cx); },
    SelectUp ("shift-up") => select_up |view, window, cx| { view.move_vertical(false, true, window, cx); },
    SelectDown ("shift-down") => select_down |view, window, cx| { view.move_vertical(true, true, window, cx); },
    SelectAll ("cmd-a") => select_all |view, _window, cx| { view.dispatch(EditorCommand::SelectAll, cx); },
    Home ("home") => home |view, _window, cx| { view.dispatch(EditorCommand::MoveToLineStart { extend: false }, cx); },
    End ("end") => end |view, _window, cx| { view.dispatch(EditorCommand::MoveToLineEnd { extend: false }, cx); },
    SelectHome ("shift-home") => select_home |view, _window, cx| { view.dispatch(EditorCommand::MoveToLineStart { extend: true }, cx); },
    SelectEnd ("shift-end") => select_end |view, _window, cx| { view.dispatch(EditorCommand::MoveToLineEnd { extend: true }, cx); },
    DocumentStart ("cmd-up") => document_start |view, _window, cx| { view.dispatch(EditorCommand::MoveToStart { extend: false }, cx); },
    DocumentEnd ("cmd-down") => document_end |view, _window, cx| { view.dispatch(EditorCommand::MoveToEnd { extend: false }, cx); },
    SelectDocumentStart ("cmd-shift-up") => select_document_start |view, _window, cx| { view.dispatch(EditorCommand::MoveToStart { extend: true }, cx); },
    SelectDocumentEnd ("cmd-shift-down") => select_document_end |view, _window, cx| { view.dispatch(EditorCommand::MoveToEnd { extend: true }, cx); },
    Undo ("cmd-z") => undo |view, _window, cx| { view.dispatch(EditorCommand::Undo, cx); },
    Redo ("cmd-shift-z") => redo |view, _window, cx| { view.dispatch(EditorCommand::Redo, cx); },
    Copy ("cmd-c") => copy |view, _window, cx| {
        if let Ok(text) = view.editor().selected_text() && !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    },
    Cut ("cmd-x") => cut |view, _window, cx| {
        if let Ok(text) = view.editor().selected_text() && !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            view.dispatch(EditorCommand::Backspace, cx);
        }
    },
    Paste ("cmd-v") => paste |view, _window, cx| {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            view.dispatch(EditorCommand::Insert(&text), cx);
        }
    },
    CancelComposition ("escape") => cancel_composition |view, _window, cx| { view.perform_cancel_composition(cx); },
}
