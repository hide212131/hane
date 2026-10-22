use crate::view::EditorView;
use crate::view::input_routing::InputTarget;
use gpui::{App, ClipboardItem, Context, InteractiveElement, KeyBinding, Window, actions};
use hane_editor::{EditorCommand, ForwardStop};
use hane_markdown::ListEditIntent;

/// Where a forward word/paragraph move stops: the end of the word on macOS and
/// the start of the next one elsewhere, matching each OS's own text fields.
const FORWARD_STOP: ForwardStop = if cfg!(target_os = "macos") {
    ForwardStop::End
} else {
    ForwardStop::NextStart
};

/// Whether a body-only key added for Issue #494 may act on the document now.
/// It must not leak into the search/find/AI surfaces or the sidebar filter and
/// rename, and must not commit a running IME composition (every
/// `Editor::dispatch` commits it).
fn body_key_ready(view: &EditorView) -> bool {
    !keys_claimed_elsewhere(view)
        && matches!(view.input_target(), InputTarget::Body)
        && view.editor().ime().is_none()
}

/// Runs a body line/document move or selection unless an IME composition is
/// running: `Editor::dispatch` would commit it, so the composition, selection
/// and text are left as they are until the IME ends it.
fn dispatch_body_move(view: &mut EditorView, command: EditorCommand, cx: &mut Context<EditorView>) {
    if view.editor().ime().is_none() {
        view.dispatch(command, cx);
    }
}

/// The search input/results, the document-find input and the AI popup own the
/// keyboard before the body, filter or rename do.
fn keys_claimed_elsewhere(view: &EditorView) -> bool {
    view.content_search_input_is_focused()
        || view.content_search_results_focused()
        || view.document_find_input_is_focused()
        || view.ai_selection_blocks_editor_keys()
}

/// `command_actions!` leaves an action's key empty when each platform binds it
/// itself; this is the named test for that convention.
fn defers_to_platform_bindings(key: &str) -> bool {
    key.is_empty()
}

macro_rules! command_actions {
    ($(
        $action:ident ($key:expr) => $handler:ident |$view:ident, $window:ident, $cx:ident| $body:block
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
                    if self.settings_open() && stringify!($action) != "CancelComposition" {
                        return;
                    }
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
            // An empty key means the action is bound per platform by
            // `register_platform_key_bindings`.
            $(
                if !defers_to_platform_bindings($key) {
                    cx.bind_keys([KeyBinding::new($key, $action, Some("HaneEditor"))]);
                }
            )+
        }
    };
}

/// Registers this platform's key bindings. Most bindings use `secondary-`,
/// which gpui resolves to `cmd` on macOS and `ctrl` elsewhere, so one binding
/// list covers both without touching macOS behavior. The line, document, word,
/// paragraph and page keys differ per OS (Issue #494) and follow each OS's own
/// text-editing conventions; they are bound in
/// `register_secondary_platform_key_bindings`. Emacs-style Control+letter
/// aliases are intentionally absent (use Karabiner/Keyhac for those).
pub fn register_key_bindings(cx: &mut App) {
    register_core_key_bindings(cx);
    register_secondary_platform_key_bindings(cx);
    register_document_find_macos_key_bindings(cx);
    register_ai_selection_key_binding(cx);
}

#[cfg(not(target_os = "macos"))]
fn register_secondary_platform_key_bindings(cx: &mut App) {
    let ctx = Some("HaneEditor");
    cx.bind_keys([
        KeyBinding::new("ctrl-y", Redo, ctx),
        // Line edges.
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        // Document edges.
        KeyBinding::new("ctrl-home", DocumentStart, ctx),
        KeyBinding::new("ctrl-end", DocumentEnd, ctx),
        KeyBinding::new("ctrl-shift-home", SelectDocumentStart, ctx),
        KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, ctx),
        // Words.
        KeyBinding::new("ctrl-left", MoveWordLeft, ctx),
        KeyBinding::new("ctrl-right", MoveWordRight, ctx),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
        KeyBinding::new("ctrl-backspace", DeleteWordBackward, ctx),
        KeyBinding::new("ctrl-delete", DeleteWordForward, ctx),
        // Paragraphs (source lines).
        KeyBinding::new("ctrl-up", MoveParagraphUp, ctx),
        KeyBinding::new("ctrl-down", MoveParagraphDown, ctx),
        KeyBinding::new("ctrl-shift-up", SelectParagraphUp, ctx),
        KeyBinding::new("ctrl-shift-down", SelectParagraphDown, ctx),
        // Pages move the caret.
        KeyBinding::new("pageup", MovePageUp, ctx),
        KeyBinding::new("pagedown", MovePageDown, ctx),
        KeyBinding::new("shift-pageup", SelectPageUp, ctx),
        KeyBinding::new("shift-pagedown", SelectPageDown, ctx),
    ]);
}

#[cfg(target_os = "macos")]
fn register_secondary_platform_key_bindings(cx: &mut App) {
    let ctx = Some("HaneEditor");
    cx.bind_keys([
        // Line edges.
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("cmd-shift-left", SelectHome, ctx),
        KeyBinding::new("cmd-shift-right", SelectEnd, ctx),
        // Document edges.
        KeyBinding::new("cmd-up", DocumentStart, ctx),
        KeyBinding::new("cmd-down", DocumentEnd, ctx),
        KeyBinding::new("cmd-shift-up", SelectDocumentStart, ctx),
        KeyBinding::new("cmd-shift-down", SelectDocumentEnd, ctx),
        KeyBinding::new("shift-home", SelectDocumentStart, ctx),
        KeyBinding::new("shift-end", SelectDocumentEnd, ctx),
        // Words.
        KeyBinding::new("alt-left", MoveWordLeft, ctx),
        KeyBinding::new("alt-right", MoveWordRight, ctx),
        KeyBinding::new("alt-shift-left", SelectWordLeft, ctx),
        KeyBinding::new("alt-shift-right", SelectWordRight, ctx),
        KeyBinding::new("alt-backspace", DeleteWordBackward, ctx),
        KeyBinding::new("alt-delete", DeleteWordForward, ctx),
        // Paragraphs (source lines).
        KeyBinding::new("alt-up", MoveParagraphUp, ctx),
        KeyBinding::new("alt-down", MoveParagraphDown, ctx),
        KeyBinding::new("alt-shift-up", SelectParagraphUp, ctx),
        KeyBinding::new("alt-shift-down", SelectParagraphDown, ctx),
        // Option+PageUp/Down moves the caret; plain Home/End/PageUp/PageDown
        // and Control+L only scroll.
        KeyBinding::new("alt-pageup", MovePageUp, ctx),
        KeyBinding::new("alt-pagedown", MovePageDown, ctx),
        KeyBinding::new("home", ScrollDocumentStart, ctx),
        KeyBinding::new("end", ScrollDocumentEnd, ctx),
        KeyBinding::new("pageup", ScrollPageUp, ctx),
        KeyBinding::new("pagedown", ScrollPageDown, ctx),
        KeyBinding::new("ctrl-l", CenterCursorLine, ctx),
    ]);
}

/// `DocumentFindNext`/`DocumentFindPrevious` already get `f3`/`shift-f3` from
/// `command_actions!` below, the same on every platform. macOS additionally
/// accepts the platform-native Cmd+G/Cmd+Shift+G for the same two actions —
/// bound literally to `cmd` rather than `secondary`, since `secondary-g`
/// would resolve to Ctrl+G on Windows/Linux and collide with that
/// platform's own "go to line" convention, which Cmd+G has no equivalent
/// conflict with on macOS.
#[cfg(target_os = "macos")]
fn register_document_find_macos_key_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-g", DocumentFindNext, Some("HaneEditor")),
        KeyBinding::new("cmd-shift-g", DocumentFindPrevious, Some("HaneEditor")),
    ]);
}

#[cfg(not(target_os = "macos"))]
fn register_document_find_macos_key_bindings(_cx: &mut App) {}

actions!(hane_editor, [OpenAiSelection]);

/// Windows Rewrite uses Alt+I, an app-specific key. macOS has no AI key:
/// Control+L now centers the caret line (Issue #494), so the footer "AIに指示"
/// button is the macOS entry (and is shown on both platforms). Bound only to
/// "HaneEditor", like every other body-editing action, so the shortcut never
/// fires while a settings field, the sidebar filter, or an inline rename has
/// focus.
#[cfg(target_os = "macos")]
fn register_ai_selection_key_binding(_cx: &mut App) {}

#[cfg(not(target_os = "macos"))]
fn register_ai_selection_key_binding(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("alt-i", OpenAiSelection, Some("HaneEditor"))]);
}

impl EditorView {
    pub(crate) fn open_ai_selection_action(
        &mut self,
        _: &OpenAiSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_ai_selection(window, cx);
    }
}

command_actions! {
    Open ("secondary-o") => open_action |view, _window, cx| {
        if !view.inline_rename_active() { view.prompt_open(cx); }
    },
    OpenFolder ("secondary-shift-o") => open_folder_action |view, _window, cx| {
        if !view.inline_rename_active() { view.prompt_open_work_folder(cx); }
    },
    ContentSearch ("secondary-shift-f") => content_search_action |view, window, cx| {
        view.open_content_search(window, cx);
    },
    DocumentFind ("secondary-f") => document_find_action |view, window, cx| {
        view.open_document_find(window, cx);
    },
    // Bound once more (to Cmd+G/Cmd+Shift+G on macOS) by
    // `register_document_find_macos_key_bindings`; this macro only declares
    // each action's own primary key binding.
    DocumentFindNext ("f3") => document_find_next_action |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() {
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => {
                // While the find input itself has an active IME composition, F3
                // (and the macOS Cmd+G bound to this same action) must not move
                // the current match out from under it.
                if !view.document_find_input_is_composing(_window, cx) {
                    view.document_find_next(cx);
                }
            }
        }
    },
    DocumentFindPrevious ("shift-f3") => document_find_previous_action |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() {
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => {
                // Same as `DocumentFindNext` above, for Shift+F3 and macOS
                // Cmd+Shift+G.
                if !view.document_find_input_is_composing(_window, cx) {
                    view.document_find_previous(cx);
                }
            }
        }
    },
    Save ("secondary-s") => save |view, _window, cx| {
        if !view.inline_rename_active() { view.save_or_prompt(cx); }
    },
    SaveAs ("secondary-shift-s") => save_as |view, _window, cx| {
        if !view.inline_rename_active() { view.prompt_save_as(cx); }
    },
    ToggleAutosave ("secondary-alt-a") => toggle_autosave_action |view, _window, cx| { view.toggle_autosave(cx); },
    NextFileTab ("ctrl-tab") => next_file_tab_action |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() {
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => {
                if view.editor().ime().is_none() {
                    view.next_file_tab(cx);
                }
            }
        }
    },
    PrevFileTab ("ctrl-shift-tab") => prev_file_tab_action |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() {
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => {
                if view.editor().ime().is_none() {
                    view.prev_file_tab(cx);
                }
            }
        }
    },
    CloseFileTab ("secondary-w") => close_file_tab_action |view, _window, cx| {
        if keys_claimed_elsewhere(view) {
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => {
                if view.editor().ime().is_none() {
                    view.close_active_file_tab(cx);
                }
            }
        }
    },
    ResetZoom ("secondary-0") => reset_zoom_action |view, _window, cx| { view.reset_zoom(cx); },
    Rename ("f2") => rename |view, window, cx| { view.begin_inline_rename_from_selection(window, cx); },
    Newline ("enter") => newline |view, window, cx| {
        if view.content_search_results_focused() {
            view.handle_content_search_enter(window, cx);
            return;
        }
        if view.content_search_input_is_focused() || view.document_find_input_is_focused() {
            return;
        }
        if view.ai_selection_blocks_editor_keys() {
            view.ai_selection_handle_enter(window, cx);
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter => {
                if view.sidebar_filter_has_composition() {
                    view.commit_sidebar_filter_composition(cx);
                }
            }
            InputTarget::InlineRename => {
                if view.inline_rename_has_composition() {
                    view.commit_inline_rename_composition(cx);
                } else {
                    view.confirm_inline_rename(cx);
                }
            }
            InputTarget::Body => {
                if view.editor().ime().is_none()
                    && !view.late_input_guard_armed()
                    && !view.apply_list_edit_intent(ListEditIntent::Enter, cx)
                {
                    view.insert_newline(cx);
                }
            }
        }
    },
    ShiftNewline ("shift-enter") => shift_newline |view, _window, cx| {
        // The find input's own `InputEvent::PressEnter { shift: true }`
        // subscription (see `document_find.rs`) already drives Previous for
        // Shift+Enter in this field after this action propagates to it;
        // navigating again here would move the current match twice.
        if view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() {
            return;
        }
        if view.content_search_input_is_focused() || view.content_search_results_focused() {
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter => {
                if view.sidebar_filter_has_composition() {
                    view.commit_sidebar_filter_composition(cx);
                }
            }
            InputTarget::InlineRename => {
                if view.inline_rename_has_composition() {
                    view.commit_inline_rename_composition(cx);
                } else {
                    view.confirm_inline_rename(cx);
                }
            }
            InputTarget::Body => {
                if view.editor().ime().is_none()
                    && !view.apply_list_edit_intent(ListEditIntent::ShiftEnter, cx)
                {
                    view.insert_newline(cx);
                }
            }
        }
    },
    Backspace ("backspace") => backspace |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.backspace_sidebar_filter(cx),
            InputTarget::InlineRename => view.backspace_inline_rename(cx),
            InputTarget::Body => {
                if !view.apply_list_edit_intent(ListEditIntent::Backspace, cx) {
                    view.dispatch(EditorCommand::Backspace, cx);
                }
            }
        }
    },
    Indent ("tab") => indent |view, window, cx| {
        if view.ai_selection_handle_tab(true, window, cx) {
            return;
        }
        if view.document_find_input_is_focused() {
            return;
        }
        if view.content_search_input_is_focused() {
            view.focus_content_search_results(window, cx);
            return;
        }
        if view.content_search_results_focused() {
            view.focus_content_search_input(window, cx);
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => {
                view.apply_list_edit_intent(ListEditIntent::Indent, cx);
            }
        }
    },
    Outdent ("shift-tab") => outdent |view, window, cx| {
        if view.ai_selection_handle_tab(false, window, cx) {
            return;
        }
        if view.document_find_input_is_focused() {
            return;
        }
        if view.content_search_input_is_focused() || view.content_search_results_focused() {
            view.focus_content_search_input(window, cx);
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => {
                view.apply_list_edit_intent(ListEditIntent::Outdent, cx);
            }
        }
    },
    Delete ("delete") => delete |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.delete_sidebar_filter(cx),
            InputTarget::InlineRename => view.delete_inline_rename(cx),
            InputTarget::Body => view.dispatch(EditorCommand::Delete, cx),
        }
    },
    Left ("left") => left |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.move_sidebar_filter_left(false, cx),
            InputTarget::InlineRename => view.move_inline_rename_left(false, cx),
            InputTarget::Body => view.dispatch(EditorCommand::MoveLeft { extend: false }, cx),
        }
    },
    Right ("right") => right |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.move_sidebar_filter_right(false, cx),
            InputTarget::InlineRename => view.move_inline_rename_right(false, cx),
            InputTarget::Body => view.dispatch(EditorCommand::MoveRight { extend: false }, cx),
        }
    },
    Up ("up") => up |view, window, cx| {
        if view.ai_selection_handle_vertical_arrow(false, cx) || view.document_find_input_is_focused() {
            return;
        } else if view.content_search_results_focused() {
            view.move_content_search_selection(false, cx);
        } else if view.content_search_input_is_focused() {
            return;
        } else {
            match view.input_target() {
                InputTarget::SidebarFilter | InputTarget::InlineRename => {}
                InputTarget::Body => view.move_vertical(false, false, window, cx),
            }
        }
    },
    Down ("down") => down |view, window, cx| {
        if view.ai_selection_handle_vertical_arrow(true, cx) || view.document_find_input_is_focused() {
            return;
        } else if view.content_search_results_focused() {
            view.move_content_search_selection(true, cx);
        } else if view.content_search_input_is_focused() {
            view.focus_content_search_results(window, cx);
        } else {
            match view.input_target() {
                InputTarget::SidebarFilter | InputTarget::InlineRename => {}
                InputTarget::Body => view.move_vertical(true, false, window, cx),
            }
        }
    },
    SelectLeft ("shift-left") => select_left |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.select_sidebar_filter_left(cx),
            InputTarget::InlineRename => view.select_inline_rename_left(cx),
            InputTarget::Body => view.dispatch(EditorCommand::MoveLeft { extend: true }, cx),
        }
    },
    SelectRight ("shift-right") => select_right |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.select_sidebar_filter_right(cx),
            InputTarget::InlineRename => view.select_inline_rename_right(cx),
            InputTarget::Body => view.dispatch(EditorCommand::MoveRight { extend: true }, cx),
        }
    },
    SelectUp ("shift-up") => select_up |view, window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => view.move_vertical(false, true, window, cx),
        }
    },
    SelectDown ("shift-down") => select_down |view, window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => view.move_vertical(true, true, window, cx),
        }
    },
    SelectAll ("secondary-a") => select_all |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.select_all_sidebar_filter(cx),
            InputTarget::InlineRename => view.select_all_inline_rename(cx),
            InputTarget::Body => view.dispatch(EditorCommand::SelectAll, cx),
        }
    },
    // Line, document and the Issue #494 word/paragraph/page actions below have
    // an empty key: they are bound per platform in
    // `register_secondary_platform_key_bindings`.
    Home ("") => home |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.move_sidebar_filter_home(cx),
            InputTarget::InlineRename => view.move_inline_rename_home(cx),
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToLineStart { extend: false }, cx),
        }
    },
    End ("") => end |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.move_sidebar_filter_end(cx),
            InputTarget::InlineRename => view.move_inline_rename_end(cx),
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToLineEnd { extend: false }, cx),
        }
    },
    SelectHome ("") => select_home |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.select_sidebar_filter_home(cx),
            InputTarget::InlineRename => view.select_inline_rename_home(cx),
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToLineStart { extend: true }, cx),
        }
    },
    SelectEnd ("") => select_end |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.select_sidebar_filter_end(cx),
            InputTarget::InlineRename => view.select_inline_rename_end(cx),
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToLineEnd { extend: true }, cx),
        }
    },
    DocumentStart ("") => document_start |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToStart { extend: false }, cx),
        }
    },
    DocumentEnd ("") => document_end |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToEnd { extend: false }, cx),
        }
    },
    // macOS Shift+Home/End also lands here; a one-line filter/rename field has
    // only one "document", so its start/end is the field's start/end.
    SelectDocumentStart ("") => select_document_start |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.select_sidebar_filter_home(cx),
            InputTarget::InlineRename => view.select_inline_rename_home(cx),
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToStart { extend: true }, cx),
        }
    },
    SelectDocumentEnd ("") => select_document_end |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.select_sidebar_filter_end(cx),
            InputTarget::InlineRename => view.select_inline_rename_end(cx),
            InputTarget::Body => dispatch_body_move(view, EditorCommand::MoveToEnd { extend: true }, cx),
        }
    },
    MoveWordLeft ("") => move_word_left |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveWord { forward: false, stop: FORWARD_STOP, extend: false }, cx);
        }
    },
    MoveWordRight ("") => move_word_right |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveWord { forward: true, stop: FORWARD_STOP, extend: false }, cx);
        }
    },
    SelectWordLeft ("") => select_word_left |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveWord { forward: false, stop: FORWARD_STOP, extend: true }, cx);
        }
    },
    SelectWordRight ("") => select_word_right |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveWord { forward: true, stop: FORWARD_STOP, extend: true }, cx);
        }
    },
    DeleteWordBackward ("") => delete_word_backward |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::DeleteWord { forward: false, stop: FORWARD_STOP }, cx);
        }
    },
    DeleteWordForward ("") => delete_word_forward |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::DeleteWord { forward: true, stop: FORWARD_STOP }, cx);
        }
    },
    MoveParagraphUp ("") => move_paragraph_up |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveParagraph { forward: false, stop: FORWARD_STOP, extend: false }, cx);
        }
    },
    MoveParagraphDown ("") => move_paragraph_down |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveParagraph { forward: true, stop: FORWARD_STOP, extend: false }, cx);
        }
    },
    SelectParagraphUp ("") => select_paragraph_up |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveParagraph { forward: false, stop: FORWARD_STOP, extend: true }, cx);
        }
    },
    SelectParagraphDown ("") => select_paragraph_down |view, _window, cx| {
        if body_key_ready(view) {
            view.dispatch(EditorCommand::MoveParagraph { forward: true, stop: FORWARD_STOP, extend: true }, cx);
        }
    },
    MovePageUp ("") => move_page_up |view, window, cx| {
        if body_key_ready(view) { view.move_page(false, false, window, cx); }
    },
    MovePageDown ("") => move_page_down |view, window, cx| {
        if body_key_ready(view) { view.move_page(true, false, window, cx); }
    },
    SelectPageUp ("") => select_page_up |view, window, cx| {
        if body_key_ready(view) { view.move_page(false, true, window, cx); }
    },
    SelectPageDown ("") => select_page_down |view, window, cx| {
        if body_key_ready(view) { view.move_page(true, true, window, cx); }
    },
    // macOS Home/End/PageUp/PageDown/Control+L only scroll. Home/End keep
    // moving within a filter or rename field, which has nothing to scroll.
    ScrollDocumentStart ("") => scroll_document_start |view, _window, cx| {
        if keys_claimed_elsewhere(view) { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.move_sidebar_filter_home(cx),
            InputTarget::InlineRename => view.move_inline_rename_home(cx),
            InputTarget::Body => view.scroll_to_document_edge(false, cx),
        }
    },
    ScrollDocumentEnd ("") => scroll_document_end |view, _window, cx| {
        if keys_claimed_elsewhere(view) { return; }
        match view.input_target() {
            InputTarget::SidebarFilter => view.move_sidebar_filter_end(cx),
            InputTarget::InlineRename => view.move_inline_rename_end(cx),
            InputTarget::Body => view.scroll_to_document_edge(true, cx),
        }
    },
    ScrollPageUp ("") => scroll_page_up |view, _window, cx| {
        if !keys_claimed_elsewhere(view) && matches!(view.input_target(), InputTarget::Body) {
            view.scroll_page(false, cx);
        }
    },
    ScrollPageDown ("") => scroll_page_down |view, _window, cx| {
        if !keys_claimed_elsewhere(view) && matches!(view.input_target(), InputTarget::Body) {
            view.scroll_page(true, cx);
        }
    },
    CenterCursorLine ("") => center_cursor_line_action |view, window, cx| {
        if !keys_claimed_elsewhere(view) && matches!(view.input_target(), InputTarget::Body) {
            view.center_cursor_line(window, cx);
        }
    },
    Undo ("secondary-z") => undo |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => view.dispatch(EditorCommand::Undo, cx),
        }
    },
    Redo ("secondary-shift-z") => redo |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        match view.input_target() {
            InputTarget::SidebarFilter | InputTarget::InlineRename => {}
            InputTarget::Body => view.dispatch(EditorCommand::Redo, cx),
        }
    },
    Copy ("secondary-c") => copy |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        let text = match view.input_target() {
            InputTarget::SidebarFilter => view.selected_sidebar_filter_text(),
            InputTarget::InlineRename => view.selected_inline_rename_text(),
            InputTarget::Body => view.editor().selected_text().ok(),
        };
        if let Some(text) = text && !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    },
    Cut ("secondary-x") => cut |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        let target = view.input_target();
        // A read-only body must not even reach the clipboard: Cut would write
        // the selection out before the delete is refused.
        if matches!(target, InputTarget::Body) && !view.body_editable() {
            return;
        }
        let text = match target {
            InputTarget::SidebarFilter => view.selected_sidebar_filter_text(),
            InputTarget::InlineRename => view.selected_inline_rename_text(),
            InputTarget::Body => view.editor().selected_text().ok(),
        };
        if let Some(text) = text && !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            match target {
                InputTarget::SidebarFilter => view.delete_sidebar_filter(cx),
                InputTarget::InlineRename => view.delete_inline_rename(cx),
                InputTarget::Body => view.dispatch(EditorCommand::Backspace, cx),
            }
        }
    },
    Paste ("secondary-v") => paste |view, _window, cx| {
        if view.content_search_input_is_focused() || view.content_search_results_focused() || view.document_find_input_is_focused() || view.ai_selection_blocks_editor_keys() { return; }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            match view.input_target() {
                InputTarget::SidebarFilter => {
                    view.replace_sidebar_filter_text(None, &text, cx);
                }
                InputTarget::InlineRename => {
                    view.replace_inline_rename_text(None, &text, cx);
                }
                InputTarget::Body => view.dispatch(EditorCommand::Insert(&text), cx),
            }
        }
    },
    CancelComposition ("escape") => cancel_composition |view, window, cx| {
        if view.settings_open() {
            view.handle_settings_escape(window, cx);
            return;
        }
        if view.ai_selection_should_leave_on_escape() {
            view.leave_ai_selection(window, cx);
            return;
        }
        if view.ai_selection_blocks_editor_keys() {
            // Reaching here means the popup is open but this Escape was not
            // meant to leave it (`ai_selection_should_leave_on_escape` above
            // was false) — i.e. it only cancelled the popup's own pinned
            // input's in-progress IME composition, which already happened
            // through the normal key-dispatch path above this action. Never
            // fall through to `perform_cancel_composition` below: that is
            // the main document body's own composition-cancel, and its
            // `after_input` unconditionally invalidates a still-open popup
            // via `AiSelectionState::invalidate_for_edit`, which would close
            // this popup on the very Escape that was only supposed to
            // cancel its composition.
            return;
        }
        if view.dismiss_file_tab_context_menu(cx) {
            return;
        }
        if view.dismiss_sidebar_context_menu(cx) {
            return;
        }
        if view.dismiss_tab_close_confirm(cx) {
            return;
        }
        if view.content_search_should_leave_on_escape() {
            view.leave_content_search(window, cx);
            return;
        }
        if view.document_find_should_leave_on_escape() {
            view.leave_document_find(window, cx);
            return;
        }
        match view.input_target() {
            InputTarget::SidebarFilter => {
                if view.sidebar_filter_has_composition() {
                    view.cancel_sidebar_filter_composition(cx);
                } else {
                    view.blur_sidebar_filter(cx);
                }
            }
            InputTarget::InlineRename => {
                if view.inline_rename_has_composition() {
                    view.cancel_inline_rename_composition(cx);
                } else {
                    view.cancel_inline_rename(cx);
                }
            }
            InputTarget::Body => {
                view.perform_cancel_composition(cx);
            }
        }
    },
}
