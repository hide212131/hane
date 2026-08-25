use crate::view::EditorView;
use gpui::{App, Context, InteractiveElement, KeyBinding, Window, actions};
use hane_editor::EditorCommand;

macro_rules! command_actions {
    ($(
        $action:ident ($key:literal) => $handler:ident |$view:ident, $cx:ident| $body:block
    ),+ $(,)?) => {
        actions!(hane_editor, [$($action),+]);

        impl EditorView {
            $(
                fn $handler(
                    &mut self,
                    _: &$action,
                    _: &mut Window,
                    cx: &mut Context<Self>,
                ) {
                    let $view = self;
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
    Backspace ("backspace") => backspace |view, cx| { view.dispatch(EditorCommand::Backspace, cx); },
    Delete ("delete") => delete |view, cx| { view.dispatch(EditorCommand::Delete, cx); },
    Left ("left") => left |view, cx| { view.dispatch(EditorCommand::MoveLeft { extend: false }, cx); },
    Right ("right") => right |view, cx| { view.dispatch(EditorCommand::MoveRight { extend: false }, cx); },
    Up ("up") => up |view, cx| { view.dispatch(EditorCommand::MoveUp { extend: false }, cx); },
    Down ("down") => down |view, cx| { view.dispatch(EditorCommand::MoveDown { extend: false }, cx); },
    SelectLeft ("shift-left") => select_left |view, cx| { view.dispatch(EditorCommand::MoveLeft { extend: true }, cx); },
    SelectRight ("shift-right") => select_right |view, cx| { view.dispatch(EditorCommand::MoveRight { extend: true }, cx); },
    SelectUp ("shift-up") => select_up |view, cx| { view.dispatch(EditorCommand::MoveUp { extend: true }, cx); },
    SelectDown ("shift-down") => select_down |view, cx| { view.dispatch(EditorCommand::MoveDown { extend: true }, cx); },
    SelectAll ("cmd-a") => select_all |view, cx| { view.dispatch(EditorCommand::SelectAll, cx); },
    Home ("home") => home |view, cx| { view.dispatch(EditorCommand::MoveToStart { extend: false }, cx); },
    End ("end") => end |view, cx| { view.dispatch(EditorCommand::MoveToEnd { extend: false }, cx); },
    CancelComposition ("escape") => cancel_composition |view, cx| { view.perform_cancel_composition(cx); },
}
