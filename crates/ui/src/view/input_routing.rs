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
}
