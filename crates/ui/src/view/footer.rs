//! The one-row footer (Issue #536): the save state of the current document,
//! its character counts, the zoom, the selection-AI entry, and the warnings
//! that used to be squeezed into a two-row strip.
//!
//! Nothing here owns a document rule. The save display is derived from the
//! session on every render; the only state added is the per-document save
//! problem record, which exists because the session forgets a failed write as
//! soon as the next one starts. Character counts read the rope's stored
//! length, never the text.

use super::*;
use gpui_component::menu::PopupMenu;
use hane_session::FilePresence;

/// Editor-column widths (the window minus the sidebar) below which the footer
/// folds more of its controls into one menu. These are starting values: the
/// layout is also stepped down when the count text is too long to fit.
const FOOTER_FULL_MIN_WIDTH: f32 = 640.0;
const FOOTER_COMPACT_MIN_WIDTH: f32 = 440.0;
const FOOTER_NARROW_MIN_WIDTH: f32 = 280.0;
/// Width reserved for everything but the count text in each layout, and the
/// width one character of it is assumed to take. Used only to step the layout
/// down when a long count would not fit, instead of cutting it off.
const FOOTER_FULL_FIXED_WIDTH: f32 = 380.0;
const FOOTER_COMPACT_FIXED_WIDTH: f32 = 340.0;
const FOOTER_COUNT_CHAR_WIDTH: f32 = 12.0;

/// Zoom levels the footer menu offers. The wheel and pinch keep their own
/// continuous range; this is only a shortcut list.
const ZOOM_CHOICES: [f32; 5] = [0.75, 1.0, 1.25, 1.5, 2.0];

const CHAR_COUNT_NOTE: &str = "元のテキストの文字数です。空白・改行・Markdown記号を含みます。結合文字や連結した絵文字は複数文字になる場合があります。";

/// What went wrong with the latest save operation of one document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DocumentSaveProblem {
    Failed(String),
    Conflict,
    SaveAsDialogFailed(String),
}

/// A save problem together with the document instance it was raised for, so
/// it can never be shown for a document that replaced that one.
#[derive(Clone, Debug)]
pub(super) struct ScopedSaveProblem {
    pub(super) instance: DocumentInstance,
    /// The target of the failed request. `None` when no target was chosen
    /// (the Save As dialog itself failed).
    pub(super) target_path: Option<PathBuf>,
    pub(super) problem: DocumentSaveProblem,
}

/// The save state shown at the left of the footer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SaveDisplay {
    Saving,
    Failed,
    Conflict,
    SaveAsFailed,
    Missing,
    Draft,
    Unsaved,
    Saved,
}

impl SaveDisplay {
    pub(super) fn label(self, short: bool) -> &'static str {
        match (self, short) {
            (Self::Saving, false) => "保存処理中…",
            (Self::Saving, true) => "処理中",
            (Self::Failed, false) => "保存できませんでした",
            (Self::Failed, true) => "保存失敗",
            (Self::Conflict, false) => "外部で変更されています",
            (Self::Conflict, true) => "外部変更",
            (Self::SaveAsFailed, false) => "保存先を選べませんでした",
            (Self::SaveAsFailed, true) => "保存先エラー",
            (Self::Missing, false) => "ファイルが見つかりません",
            (Self::Missing, true) => "見つかりません",
            (Self::Draft, _) => "下書き",
            (Self::Unsaved, _) => "未保存",
            (Self::Saved, _) => "保存済み",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Saving => "保存処理中: 書き込みまたはファイル名の変更を実行しています。",
            Self::Failed => {
                "保存できませんでした: 直近の保存に失敗しました。この文書の保存が成功するまで警告は残ります。"
            }
            Self::Conflict => {
                "外部で変更されています: 保存時にディスク上のファイルが変更されていたため書き込んでいません。"
            }
            Self::SaveAsFailed => {
                "保存先を選べませんでした: 名前を付けて保存の保存先を取得できませんでした。"
            }
            Self::Missing => {
                "ファイルが見つかりません: 保存先のファイルが削除または移動されています。保存すると同じ場所に作り直します。"
            }
            Self::Draft => "下書き: 通常のファイルとしてはまだ保存されていません。",
            Self::Unsaved => "未保存: 変更がファイルに保存されていません。",
            Self::Saved => {
                "保存済み: Haneが把握する保存済みの版と現在の文書が一致しています。"
            }
        }
    }
}

/// The facts the save display is derived from, read from the current document.
pub(super) struct SaveFacts<'a> {
    pub(super) in_flight: bool,
    pub(super) problem: Option<&'a DocumentSaveProblem>,
    pub(super) missing: bool,
    pub(super) has_path: bool,
    pub(super) is_draft: bool,
    pub(super) dirty: bool,
}

/// Picks the display in a fixed priority. The autosave setting is never
/// consulted: whether a document is saved is a fact about the document.
pub(super) fn derive_save_display(facts: &SaveFacts<'_>) -> SaveDisplay {
    if facts.in_flight {
        return SaveDisplay::Saving;
    }
    match facts.problem {
        Some(DocumentSaveProblem::Failed(_)) => return SaveDisplay::Failed,
        Some(DocumentSaveProblem::Conflict) => return SaveDisplay::Conflict,
        Some(DocumentSaveProblem::SaveAsDialogFailed(_)) => return SaveDisplay::SaveAsFailed,
        None => {}
    }
    if facts.missing {
        return SaveDisplay::Missing;
    }
    if !facts.has_path {
        return if facts.is_draft {
            SaveDisplay::Draft
        } else {
            SaveDisplay::Unsaved
        };
    }
    if facts.dirty {
        SaveDisplay::Unsaved
    } else {
        SaveDisplay::Saved
    }
}

/// How much of the footer stays visible; the rest moves into one menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FooterLayout {
    /// Everything shown.
    Full,
    /// One count field; zoom and settings in "その他".
    Compact,
    /// Save state, AI and "その他".
    Narrow,
    /// A single "状態と操作" entry.
    Single,
}

/// Chooses the layout for the editor column's width. A count text longer than
/// the layout can hold steps the layout down rather than being cut off.
pub(super) fn footer_layout_for_width(
    width: f32,
    full_count_chars: usize,
    compact_count_chars: usize,
) -> FooterLayout {
    let full_needed = FOOTER_FULL_FIXED_WIDTH + FOOTER_COUNT_CHAR_WIDTH * full_count_chars as f32;
    let compact_needed =
        FOOTER_COMPACT_FIXED_WIDTH + FOOTER_COUNT_CHAR_WIDTH * compact_count_chars as f32;
    if width >= FOOTER_FULL_MIN_WIDTH && width >= full_needed {
        FooterLayout::Full
    } else if width >= FOOTER_COMPACT_MIN_WIDTH && width >= compact_needed {
        FooterLayout::Compact
    } else if width >= FOOTER_NARROW_MIN_WIDTH {
        FooterLayout::Narrow
    } else {
        FooterLayout::Single
    }
}

/// `1234567` as `1,234,567`.
pub(super) fn format_count(value: usize) -> String {
    // Groups of three digits from the right; the digits are ASCII, so every
    // chunk is valid UTF-8.
    value
        .to_string()
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(",")
}

fn zoom_percent(zoom: f32) -> i32 {
    (zoom * 100.0).round() as i32
}

fn save_problem_text(label: &str, scoped: &ScopedSaveProblem) -> String {
    let target = scoped
        .target_path
        .as_ref()
        .map(|path| format!("（保存先: {}）", path.display()))
        .unwrap_or_default();
    match &scoped.problem {
        DocumentSaveProblem::Failed(error) => {
            format!("{label}: 保存できませんでした{target}: {error}")
        }
        DocumentSaveProblem::Conflict => format!(
            "{label}: 外部で変更されています{target}。ファイルがディスク上で変更されたため保存していません。名前を付けて保存するか、もう一度保存すると上書きします。"
        ),
        DocumentSaveProblem::SaveAsDialogFailed(error) => {
            format!("{label}: 保存先を選べませんでした: {error}")
        }
    }
}

/// What a footer menu shows. Built from the current values each time the
/// footer renders, so a menu opened later is never older than the frame.
#[derive(Clone)]
struct MenuSpec {
    detail_lines: Vec<String>,
    info_lines: Vec<String>,
    zoom: f32,
    show_zoom: bool,
    show_settings: bool,
    show_ai: bool,
    ai_enabled: bool,
}

fn text_item(text: String, max_width: f32) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .max_w(px(max_width))
            .px(px(5.0))
            .py(px(3.0))
            .whitespace_normal()
            .child(text.clone())
    })
}

/// Fills `menu` from `spec`. The menu is bounded to the window and scrolls,
/// so a long error or path can always be read in full.
fn build_menu(
    menu: PopupMenu,
    spec: &MenuSpec,
    view: &gpui::Entity<EditorView>,
    window: &Window,
) -> PopupMenu {
    let viewport = window.viewport_size();
    let width = (f32::from(viewport.width) - 24.0).clamp(160.0, 380.0);
    let height = (f32::from(viewport.height) - 80.0).max(120.0);
    let item_width = width - 16.0;
    let mut menu = menu.max_w(px(width)).max_h(px(height)).scrollable(true);
    for line in &spec.detail_lines {
        menu = menu.item(text_item(line.clone(), item_width));
    }
    for line in &spec.info_lines {
        menu = menu.item(text_item(line.clone(), item_width));
    }
    if spec.show_zoom {
        for value in ZOOM_CHOICES {
            let percent = zoom_percent(value);
            // Checked only when this is the actual zoom, not when the
            // rounded percentage merely reads the same.
            let checked = (spec.zoom - value).abs() < 0.0001;
            let zoom_view = view.clone();
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .debug_selector(move || format!("footer-zoom-{percent}"))
                        .px(px(5.0))
                        .py(px(3.0))
                        .child(format!("倍率 {percent}%"))
                })
                .checked(checked)
                .on_click(move |_, window, app| {
                    zoom_view.update(app, |view, cx| {
                        view.apply_footer_zoom(value, cx);
                        if view.footer_menu_returns_focus_to_body() {
                            window.focus(&view.focus_handle, cx);
                        }
                    });
                }),
            );
        }
    }
    if spec.show_ai {
        let ai_view = view.clone();
        let enabled = spec.ai_enabled;
        let mut item = PopupMenuItem::element(move |_, _| {
            div()
                .debug_selector(|| "footer-menu-ai".to_owned())
                .px(px(5.0))
                .py(px(3.0))
                .when(!enabled, |element| element.opacity(0.5))
                .child("AIに指示")
        });
        if enabled {
            item = item.on_click(move |_, window, app| {
                ai_view.update(app, |view, cx| view.open_ai_selection(window, cx));
            });
        }
        menu = menu.item(item);
    }
    if spec.show_settings {
        let settings_view = view.clone();
        menu = menu.item(
            PopupMenuItem::element(move |_, _| {
                div()
                    .debug_selector(|| "footer-menu-settings".to_owned())
                    .px(px(5.0))
                    .py(px(3.0))
                    .child("設定")
            })
            .on_click(move |_, window, app| {
                settings_view.update(app, |view, cx| view.open_settings(window, cx));
            }),
        );
    }
    menu
}

impl EditorView {
    /// Records the latest save problem of one document. The caller has
    /// already matched the result to its document instance.
    pub(super) fn record_save_problem(
        &mut self,
        id: SessionId,
        instance: DocumentInstance,
        target_path: Option<PathBuf>,
        problem: DocumentSaveProblem,
    ) {
        self.save_problems.insert(
            id,
            ScopedSaveProblem {
                instance,
                target_path,
                problem,
            },
        );
    }

    /// The problem recorded for `id`, if it still belongs to the document
    /// that id names now.
    fn valid_save_problem(&self, id: SessionId) -> Option<&ScopedSaveProblem> {
        let scoped = self.save_problems.get(&id)?;
        (self.document_instance(id) == Some(scoped.instance)).then_some(scoped)
    }

    /// The save state of the active document. Another tab's problem or
    /// in-flight write never changes it.
    pub(super) fn active_save_display(&self) -> SaveDisplay {
        let session = self.sessions.active();
        let id = session.id();
        let problem = self.valid_save_problem(id).map(|scoped| &scoped.problem);
        derive_save_display(&SaveFacts {
            in_flight: session.save_in_flight(),
            problem,
            missing: session.presence() == FilePresence::Missing,
            has_path: session.path().is_some(),
            is_draft: self.work_folder_drafts.contains_key(&id),
            dirty: session.is_dirty(),
        })
    }

    /// Every unresolved warning, in full: the active document's save problem
    /// first, then those of other tabs (named), then the draft-recovery
    /// warning. A re-save in progress does not remove a problem.
    pub(super) fn footer_warnings(&self) -> Vec<String> {
        let active = self.sessions.active_id();
        let mut warnings = Vec::new();
        if let Some(scoped) = self.valid_save_problem(active) {
            warnings.push(save_problem_text(&self.sessions.active().label(), scoped));
        }
        for session in self.sessions() {
            if session.id() == active {
                continue;
            }
            if let Some(scoped) = self.valid_save_problem(session.id()) {
                warnings.push(save_problem_text(&session.label(), scoped));
            }
        }
        if let Some(warning) = self.draft_recovery_warning.as_ref() {
            warnings.push(warning.clone());
        }
        warnings
    }

    /// Whole-document and selected character counts, as Unicode scalar
    /// values. Both come from the rope's stored lengths; no text is copied or
    /// walked. The selection count is absent for a caret, while an IME
    /// composition owns the selection, and if the range cannot be sliced.
    pub(super) fn footer_char_counts(&self) -> (usize, Option<usize>) {
        let editor = self.editor();
        let document = editor.document();
        let total = document.len_chars().0;
        if editor.ime().is_some() {
            return (total, None);
        }
        let range = editor.selection().range();
        if range.start == range.end {
            return (total, None);
        }
        let selected = document
            .slice(range)
            .ok()
            .map(|slice| slice.len_chars().0);
        (total, selected)
    }

    /// Whether a footer menu choice should hand the focus back to the body.
    /// A menu opened from the sidebar filter, the find field or a rename
    /// keeps that field's own focus and input-target state.
    pub(super) fn footer_menu_returns_focus_to_body(&self) -> bool {
        !(self.sidebar_filter_is_focused()
            || self.document_find_input_is_focused()
            || self.inline_rename_active())
    }

    /// Applies a zoom chosen in the footer menu through the existing zoom
    /// path. The text, selection, history and dirty state are untouched.
    pub(super) fn apply_footer_zoom(&mut self, value: f32, cx: &mut Context<Self>) {
        // A coast still running belongs to the scroll that started it.
        self.scroll_inertia = None;
        let interrupted_wheel = self.wheel_zoom_animation.is_some();
        let previous_zoom = self.zoom;
        if value == 1.0 {
            self.reset_zoom(cx);
        } else {
            self.set_zoom(value, self.viewport_height / 2.0, cx);
        }
        if interrupted_wheel && self.zoom == previous_zoom {
            // The wheel animation kept offscreen heights from an older zoom;
            // landing on the painted zoom must still settle them once.
            self.rebuild_height_estimates();
            cx.notify();
        }
    }

    /// The footer row. `column_width` is the editor column (window minus
    /// sidebar), which decides how much is folded into menus.
    pub(super) fn footer_element(
        &self,
        column_width: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ime_active = self.editor().ime().is_some();
        let display = self.active_save_display();
        let warnings = self.footer_warnings();
        let notice = self.status.clone();
        let (total, selected) = self.footer_char_counts();
        let ai_enabled = self.ai_selection_entry_available();
        let show_settings = self.work_folder.is_none();
        let zoom = self.zoom;

        let full_count = match selected {
            Some(selected) => format!(
                "{}文字・選択{}文字",
                format_count(total),
                format_count(selected)
            ),
            None => format!("{}文字", format_count(total)),
        };
        let compact_count = match selected {
            Some(selected) => format!("選択{}文字", format_count(selected)),
            None => format!("{}文字", format_count(total)),
        };
        let layout = footer_layout_for_width(
            column_width,
            full_count.chars().count(),
            compact_count.chars().count(),
        );

        let short = !matches!(layout, FooterLayout::Full | FooterLayout::Compact);
        let warning_mark = if warnings.is_empty() { "" } else { "⚠ " };

        let mut detail_lines = vec![display.description().to_owned()];
        detail_lines.extend(warnings.iter().cloned());
        if let Some(notice) = &notice {
            detail_lines.push(format!("通知: {notice}"));
        }
        detail_lines.push(CHAR_COUNT_NOTE.to_owned());

        // Counts that no longer fit on the row, in the order they are read.
        let mut info_lines = Vec::new();
        match layout {
            FooterLayout::Compact => {
                if selected.is_some() {
                    info_lines.push(format!("全体 {}文字", format_count(total)));
                }
            }
            FooterLayout::Narrow | FooterLayout::Single => {
                info_lines.push(format!("全体 {}文字", format_count(total)));
                if let Some(selected) = selected {
                    info_lines.push(format!("選択 {}文字", format_count(selected)));
                }
            }
            FooterLayout::Full => {}
        }

        let view = cx.entity();
        let mut left: Vec<gpui::AnyElement> = Vec::new();
        let mut right: Vec<gpui::AnyElement> = Vec::new();

        if layout == FooterLayout::Single {
            let spec = MenuSpec {
                detail_lines,
                info_lines,
                zoom,
                show_zoom: true,
                show_settings,
                show_ai: true,
                ai_enabled,
            };
            let menu_view = view.clone();
            left.push(
                div()
                    .debug_selector(|| "footer-single-entry".to_owned())
                    .flex_none()
                    .child(
                        Button::new("footer-single-entry-button")
                            .label(format!("{warning_mark}{} · 状態と操作", display.label(true)))
                            .xsmall()
                            .ghost()
                            .disabled(ime_active)
                            .dropdown_menu(move |menu, window, _| {
                                build_menu(menu, &spec, &menu_view, window)
                            }),
                    )
                    .into_any_element(),
            );
        } else {
            let details_spec = MenuSpec {
                detail_lines,
                info_lines: Vec::new(),
                zoom,
                show_zoom: false,
                show_settings: false,
                show_ai: false,
                ai_enabled,
            };
            let details_view = view.clone();
            left.push(
                div()
                    .debug_selector(|| "footer-save-state".to_owned())
                    .flex_none()
                    .child(
                        Button::new("footer-save-state-button")
                            .label(format!("{warning_mark}{}", display.label(short)))
                            .xsmall()
                            .ghost()
                            .tooltip("保存状態と通知の詳細")
                            .disabled(ime_active)
                            .dropdown_menu(move |menu, window, _| {
                                build_menu(menu, &details_spec, &details_view, window)
                            }),
                    )
                    .into_any_element(),
            );

            let ai_view = view.clone();
            let ai_button = div()
                .debug_selector(|| "footer-ai-selection".to_owned())
                .flex_none()
                .child(
                    Button::new("footer-ai-selection-button")
                        .label("AIに指示")
                        .xsmall()
                        .ghost()
                        .tooltip("選択した文章に指示する")
                        .disabled(!ai_enabled)
                        .on_click(move |_, window, app| {
                            ai_view.update(app, |view, cx| view.open_ai_selection(window, cx));
                        }),
                )
                .into_any_element();

            match layout {
                FooterLayout::Full => {
                    right.push(
                        div()
                            .debug_selector(|| "footer-char-count".to_owned())
                            .flex_none()
                            .child(full_count)
                            .into_any_element(),
                    );
                    let zoom_spec = MenuSpec {
                        detail_lines: Vec::new(),
                        info_lines: Vec::new(),
                        zoom,
                        show_zoom: true,
                        show_settings: false,
                        show_ai: false,
                        ai_enabled,
                    };
                    let zoom_view = view.clone();
                    right.push(
                        div()
                            .debug_selector(|| "footer-zoom".to_owned())
                            .flex_none()
                            .child(
                                Button::new("footer-zoom-button")
                                    .label(format!("{}%", zoom_percent(zoom)))
                                    .xsmall()
                                    .ghost()
                                    .tooltip("表示倍率")
                                    .disabled(ime_active)
                                    .dropdown_menu(move |menu, window, _| {
                                        build_menu(menu, &zoom_spec, &zoom_view, window)
                                    }),
                            )
                            .into_any_element(),
                    );
                    right.push(ai_button);
                    if show_settings {
                        let settings_view = view.clone();
                        right.push(
                            div()
                                .debug_selector(|| "footer-settings".to_owned())
                                .flex_none()
                                .child(
                                    Button::new("footer-settings-button")
                                        .label("設定")
                                        .xsmall()
                                        .ghost()
                                        .on_click(move |_, window, app| {
                                            settings_view.update(app, |view, cx| {
                                                view.open_settings(window, cx);
                                            });
                                        }),
                                )
                                .into_any_element(),
                        );
                    }
                }
                FooterLayout::Compact | FooterLayout::Narrow => {
                    if layout == FooterLayout::Compact {
                        right.push(
                            div()
                                .debug_selector(|| "footer-char-count".to_owned())
                                .flex_none()
                                .child(compact_count)
                                .into_any_element(),
                        );
                    }
                    right.push(ai_button);
                    let mut more_info = info_lines;
                    more_info.push(CHAR_COUNT_NOTE.to_owned());
                    let more_spec = MenuSpec {
                        detail_lines: Vec::new(),
                        info_lines: more_info,
                        zoom,
                        show_zoom: true,
                        show_settings,
                        show_ai: false,
                        ai_enabled,
                    };
                    let more_view = view.clone();
                    right.push(
                        div()
                            .debug_selector(|| "footer-more".to_owned())
                            .flex_none()
                            .child(
                                Button::new("footer-more-button")
                                    .label("その他")
                                    .xsmall()
                                    .ghost()
                                    .tooltip("文字数・倍率・設定")
                                    .disabled(ime_active)
                                    .dropdown_menu(move |menu, window, _| {
                                        build_menu(menu, &more_spec, &more_view, window)
                                    }),
                            )
                            .into_any_element(),
                    );
                }
                FooterLayout::Single => {}
            }
        }

        // The most recent operation message fits the row only when there is
        // room; its full text is always in the details menu.
        let middle = match (&notice, layout) {
            (Some(notice), FooterLayout::Full | FooterLayout::Compact) => div()
                .debug_selector(|| "footer-notice".to_owned())
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .child(notice.clone()),
            _ => div().flex_1().min_w(px(0.0)),
        };

        div()
            .id("editor-footer")
            .debug_selector(|| "editor-footer".to_owned())
            .h(px(self.theme.footer_height))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .overflow_hidden()
            .text_size(px(11.0))
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .children(left)
            .child(middle)
            .children(right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_session::FileIdentity;

    fn facts() -> SaveFacts<'static> {
        SaveFacts {
            in_flight: false,
            problem: None,
            missing: false,
            has_path: true,
            is_draft: false,
            dirty: false,
        }
    }

    #[test]
    fn save_display_follows_the_documents_own_facts_in_priority_order() {
        assert_eq!(derive_save_display(&facts()), SaveDisplay::Saved);
        assert_eq!(
            derive_save_display(&SaveFacts {
                dirty: true,
                ..facts()
            }),
            SaveDisplay::Unsaved
        );
        // An empty untitled document is never "saved".
        assert_eq!(
            derive_save_display(&SaveFacts {
                has_path: false,
                ..facts()
            }),
            SaveDisplay::Unsaved
        );
        assert_eq!(
            derive_save_display(&SaveFacts {
                has_path: false,
                is_draft: true,
                ..facts()
            }),
            SaveDisplay::Draft
        );
        assert_eq!(
            derive_save_display(&SaveFacts {
                missing: true,
                dirty: true,
                ..facts()
            }),
            SaveDisplay::Missing
        );
        let failed = DocumentSaveProblem::Failed("disk full".to_owned());
        assert_eq!(
            derive_save_display(&SaveFacts {
                problem: Some(&failed),
                missing: true,
                ..facts()
            }),
            SaveDisplay::Failed
        );
        assert_eq!(
            derive_save_display(&SaveFacts {
                problem: Some(&DocumentSaveProblem::Conflict),
                ..facts()
            }),
            SaveDisplay::Conflict
        );
        let dialog = DocumentSaveProblem::SaveAsDialogFailed("denied".to_owned());
        assert_eq!(
            derive_save_display(&SaveFacts {
                problem: Some(&dialog),
                ..facts()
            }),
            SaveDisplay::SaveAsFailed
        );
        // A write in progress wins over an earlier problem for the display.
        assert_eq!(
            derive_save_display(&SaveFacts {
                in_flight: true,
                problem: Some(&failed),
                ..facts()
            }),
            SaveDisplay::Saving
        );
    }

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(2_430), "2,430");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn layout_folds_controls_as_the_editor_column_narrows() {
        assert_eq!(footer_layout_for_width(740.0, 7, 7), FooterLayout::Full);
        assert_eq!(footer_layout_for_width(640.0, 7, 7), FooterLayout::Full);
        assert_eq!(footer_layout_for_width(639.0, 7, 7), FooterLayout::Compact);
        assert_eq!(footer_layout_for_width(440.0, 7, 7), FooterLayout::Compact);
        assert_eq!(footer_layout_for_width(439.0, 7, 7), FooterLayout::Narrow);
        assert_eq!(footer_layout_for_width(280.0, 7, 7), FooterLayout::Narrow);
        assert_eq!(footer_layout_for_width(279.0, 7, 7), FooterLayout::Single);
        // A very long count steps the layout down instead of being cut off.
        assert_eq!(footer_layout_for_width(700.0, 40, 12), FooterLayout::Compact);
        assert_eq!(footer_layout_for_width(500.0, 40, 40), FooterLayout::Narrow);
    }

    fn open_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        text: &str,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        let text = text.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&text, "Untitled", cx));
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui::test]
    fn footer_is_one_32px_row_with_the_expected_controls_at_each_width(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_view, cx) = open_view(cx, "body\n");
        for (width, zoom, count, more, ai, single) in [
            (740.0, true, true, false, true, false),
            (640.0, true, true, false, true, false),
            (639.0, false, true, true, true, false),
            (440.0, false, true, true, true, false),
            (439.0, false, false, true, true, false),
            (280.0, false, false, true, true, false),
            (279.0, false, false, false, false, true),
        ] {
            cx.simulate_resize(gpui::size(px(width), px(400.0)));
            cx.run_until_parked();
            let footer = cx.debug_bounds("editor-footer").expect("footer rendered");
            assert_eq!(footer.size.height, px(32.0), "width {width}");
            assert!(cx.debug_bounds("footer-save-state").is_some() != single, "width {width}");
            assert_eq!(cx.debug_bounds("footer-zoom").is_some(), zoom, "width {width}");
            assert_eq!(cx.debug_bounds("footer-char-count").is_some(), count, "width {width}");
            assert_eq!(cx.debug_bounds("footer-more").is_some(), more, "width {width}");
            assert_eq!(cx.debug_bounds("footer-ai-selection").is_some(), ai, "width {width}");
            assert_eq!(
                cx.debug_bounds("footer-single-entry").is_some(),
                single,
                "width {width}"
            );
        }
    }

    #[gpui::test]
    fn counts_use_unicode_scalar_values_for_the_whole_text_and_either_selection_direction(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "Aあ😀\ne\u{301}");
        view.update(cx, |view, _| {
            let end = SourceOffset(view.editor().document().len_bytes().0);
            // "Aあ😀" (3) + LF (1) + "e" and a combining accent (2).
            assert_eq!(view.footer_char_counts(), (6, None));
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: end,
                })
                .unwrap();
            assert_eq!(view.footer_char_counts(), (6, Some(6)));
            view.editor_mut()
                .set_selection(Selection {
                    anchor: end,
                    active: SourceOffset(0),
                })
                .unwrap();
            assert_eq!(view.footer_char_counts(), (6, Some(6)));
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(1),
                    active: SourceOffset(4),
                })
                .unwrap();
            assert_eq!(view.footer_char_counts(), (6, Some(1)));
        });
    }

    #[gpui::test]
    fn counts_follow_edits_and_hide_the_selection_count_during_composition(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "ab\n");
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(1)))
                .unwrap();
            view.editor_mut().insert_text("あ").unwrap();
            view.after_input(cx);
            assert_eq!(view.footer_char_counts(), (4, None));
            view.editor_mut()
                .replace_and_mark_text(None, "に", Some(1..1))
                .unwrap();
            // The composing character is part of the document's text, but its
            // internal selection is not an ordinary text selection.
            assert_eq!(view.footer_char_counts(), (5, None));
            assert!(!view.ai_selection_entry_available());
        });
    }

    #[gpui::test]
    fn a_save_problem_stays_with_its_document_and_does_not_rewrite_the_current_display(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "one\n");
        view.update(cx, |view, _| {
            let first = view.sessions.active_id();
            let second = view.sessions.open_untitled("two\n", "Second.md");
            assert!(view.sessions.activate(first));
            let instance = view.document_instance(second).unwrap();
            view.record_save_problem(
                second,
                instance,
                Some(PathBuf::from("Second.md")),
                DocumentSaveProblem::Failed("denied".to_owned()),
            );

            // The other tab's failure is reachable, with its name, but the
            // current document's own display is untouched.
            assert_ne!(view.active_save_display(), SaveDisplay::Failed);
            let warnings = view.footer_warnings();
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("denied"));

            // Moving between tabs does not resolve the problem.
            assert!(view.sessions.activate(second));
            assert_eq!(view.active_save_display(), SaveDisplay::Failed);
            assert!(view.sessions.activate(first));
            assert_eq!(view.footer_warnings().len(), 1);
        });
    }

    #[gpui::test]
    fn a_save_problem_recorded_for_a_replaced_document_is_not_shown(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "one\n");
        view.update(cx, |view, _| {
            let id = view.sessions.active_id();
            let instance = view.document_instance(id).unwrap();
            view.record_save_problem(id, instance, None, DocumentSaveProblem::Conflict);
            assert_eq!(view.active_save_display(), SaveDisplay::Conflict);

            view.sessions
                .get_mut(id)
                .unwrap()
                .adopt(LoadedFile {
                    document: RopeBuffer::from_text("replacement\n"),
                    identity: FileIdentity::lexical(PathBuf::from("replacement.md")),
                    stamp: None,
                });
            assert!(view.footer_warnings().is_empty());
            assert_ne!(view.active_save_display(), SaveDisplay::Conflict);
        });
    }

    #[gpui::test]
    fn editing_keeps_one_problem_per_document_until_a_valid_save_clears_it(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-footer-problem-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("note.md");
        std::fs::write(&path, "before").unwrap();
        let loaded = OsFileService.load(&path).unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.run_until_parked();

        // A Save As whose target cannot be written (a directory) fails.
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            let _ = view.sessions.active_mut().editor_mut().insert_text("x");
            view.save_session(id, SaveIntent::To(root.clone()), cx);
        });
        cx.run_until_parked();
        view.update(cx, |view, _| {
            assert_eq!(view.footer_warnings().len(), 1);
            assert_ne!(view.active_save_display(), SaveDisplay::Saved);
            // Editing again neither clears nor duplicates the problem.
            let _ = view.sessions.active_mut().editor_mut().insert_text("y");
            assert_eq!(view.footer_warnings().len(), 1);
        });

        // A valid save of the same document resolves it.
        view.update(cx, |view, cx| view.save_current(cx));
        cx.run_until_parked();
        view.update(cx, |view, _| {
            assert!(view.footer_warnings().is_empty());
        });

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[gpui::test]
    fn choosing_a_zoom_keeps_the_text_selection_history_and_dirty_state(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "one two\n");
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(3),
                })
                .unwrap();
            let text = view.editor().document().full_text();
            let selection = view.editor().selection();
            let revision = view.editor().document().revision();
            let dirty = view.sessions.active().is_dirty();

            view.apply_footer_zoom(1.5, cx);
            assert_eq!(view.zoom, 1.5);
            assert_eq!(zoom_percent(view.zoom), 150);
            view.apply_footer_zoom(1.0, cx);
            assert_eq!(view.zoom, 1.0);

            assert_eq!(view.editor().document().full_text(), text);
            assert_eq!(view.editor().selection(), selection);
            assert_eq!(view.editor().document().revision(), revision);
            assert_eq!(view.sessions.active().is_dirty(), dirty);
        });
    }
}
