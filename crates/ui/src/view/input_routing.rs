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

    #[gpui::test]
    fn platform_target_matrix_preserves_entrypoint_contracts(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("body\n", "Untitled", cx));
        for bits in 0..8u8 {
            let filter = bits & 1 != 0;
            let input = bits & 2 != 0;
            let results = bits & 4 != 0;
            view.update(cx, |view, _| {
                view.sidebar_filter_focused = filter;
                view.content_search.input_focused = input;
                view.content_search.results_focused = results;
                let broad = filter || input || results;
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
}
