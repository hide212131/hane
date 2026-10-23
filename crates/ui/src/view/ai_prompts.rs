//! "AIプロンプト" settings page (Issue #449): independent save/edit of the
//! user's selection-action prompts. Deliberately has no `AiServiceHandle`
//! dependency at all — per the implementation spec section 2, prompts must
//! be manageable even with no AI connection configured, while logged out,
//! or with the bundled runtime absent — and never touches AI connection
//! settings, credentials, or `settings_generation`.

use super::EditorView;
use crate::theme::Theme;
use gpui::{
    App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px, rgb,
};
use gpui_component::{Disableable, Sizable};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use hane_ai::{AiPaths, UserPromptDraft, UserPrompts, UserPromptsLoadError, UserPromptsSaveError};
use hane_session::FileStateStore;
use std::path::PathBuf;

/// プロンプト本文Textareaを初期表示から複数行見せるための高さ（px）。
const PROMPT_BODY_HEIGHT: f32 = 220.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaveTarget {
    Category(super::SettingsCategory),
    Close,
}

pub(super) struct AiPromptsPage {
    root: Option<PathBuf>,
    loaded: bool,
    load_error: Option<String>,
    revision: u64,
    baseline: Vec<UserPromptDraft>,
    draft: Vec<UserPromptDraft>,
    selected: usize,
    title_input: Option<Entity<InputState>>,
    body_input: Option<Entity<TextareaState>>,
    message: Option<String>,
    busy: bool,
    leave_prompt: Option<LeaveTarget>,
    route_ready: Option<LeaveTarget>,
    pending_target: Option<LeaveTarget>,
    pending_delete: Option<usize>,
    load_generation: u64,
    save_generation: u64,
}

impl Default for AiPromptsPage {
    fn default() -> Self {
        Self {
            root: FileStateStore::from_environment()
                .ok()
                .map(|store| store.root().to_path_buf()),
            loaded: false,
            load_error: None,
            revision: 0,
            baseline: Vec::new(),
            draft: Vec::new(),
            selected: 0,
            title_input: None,
            body_input: None,
            message: None,
            busy: false,
            leave_prompt: None,
            route_ready: None,
            pending_target: None,
            pending_delete: None,
            load_generation: 0,
            save_generation: 0,
        }
    }
}

/// Shared with `ai_selection`'s read-only menu listing, which must read the
/// exact same persisted file this settings page edits/saves.
pub(super) fn store_for(root: &std::path::Path) -> hane_ai::UserPromptsStore {
    let paths = AiPaths::new(root);
    hane_ai::UserPromptsStore::new(paths.user_prompts_path(), paths.user_prompts_lock_path())
}

impl AiPromptsPage {
    pub(super) fn begin_settings_session(&mut self, cx: &mut Context<EditorView>) {
        self.leave_prompt = None;
        self.route_ready = None;
        self.pending_target = None;
        self.pending_delete = None;
        self.message = None;
        self.reload(cx);
    }

    pub(super) fn close_settings(&mut self) {
        self.title_input = None;
        self.body_input = None;
        self.leave_prompt = None;
        self.pending_target = None;
        self.pending_delete = None;
    }

    fn reload(&mut self, cx: &mut Context<EditorView>) {
        let Some(root) = self.root.clone() else {
            self.load_error = Some("保存先を利用できません。".to_owned());
            self.loaded = true;
            return;
        };
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        self.busy = true;
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { store_for(&root).load() })
                .await;
            let _ = view.update(cx, move |view, cx| {
                view.ai_prompts.apply_load(generation, outcome);
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_load(&mut self, generation: u64, outcome: Result<Option<UserPrompts>, UserPromptsLoadError>) {
        if generation != self.load_generation {
            return;
        }
        self.busy = false;
        let first_load = !self.loaded;
        match outcome {
            Ok(None) => {
                self.revision = 0;
                self.baseline = vec![hane_ai::initial_default_draft()];
                self.draft = self.baseline.clone();
                self.selected = 0;
                self.load_error = None;
                self.message = None;
                self.title_input = None;
                self.body_input = None;
                self.loaded = true;
            }
            Ok(Some(prompts)) => {
                self.revision = prompts.revision;
                self.baseline = prompts
                    .prompts
                    .into_iter()
                    .map(|p| UserPromptDraft {
                        id: Some(p.id),
                        title: p.title,
                        prompt: p.prompt,
                    })
                    .collect();
                self.draft = self.baseline.clone();
                self.selected = 0;
                self.load_error = None;
                self.message = None;
                self.title_input = None;
                self.body_input = None;
                self.loaded = true;
            }
            Err(error) => {
                let text = load_error_message(&error);
                if first_load {
                    self.load_error = Some(text);
                    self.loaded = true;
                } else {
                    self.message = Some(text);
                }
            }
        }
    }

    fn flush_selected_from_inputs(&mut self, cx: &App) {
        if let (Some(title_input), Some(body_input)) = (&self.title_input, &self.body_input) {
            let title = value(title_input, cx);
            let prompt = textarea_value(body_input, cx);
            if let Some(entry) = self.draft.get_mut(self.selected) {
                entry.title = title;
                entry.prompt = prompt;
            }
        }
    }

    fn dirty_draft(&self, cx: &App) -> Vec<UserPromptDraft> {
        let mut draft = self.draft.clone();
        if let (Some(title_input), Some(body_input)) = (&self.title_input, &self.body_input)
            && let Some(entry) = draft.get_mut(self.selected)
        {
            entry.title = value(title_input, cx);
            entry.prompt = textarea_value(body_input, cx);
        }
        draft
    }

    pub(super) fn is_dirty(&self, cx: &App) -> bool {
        self.loaded && self.dirty_draft(cx) != self.baseline
    }

    /// Makes the form report unsaved changes (restart-check tests only).
    #[cfg(test)]
    pub(super) fn force_dirty_for_test(&mut self) {
        self.loaded = true;
        self.baseline = Vec::new();
        self.draft = vec![hane_ai::initial_default_draft()];
    }

    fn sync_inputs_from_selected(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        let Some(entry) = self.draft.get(self.selected) else {
            self.title_input = None;
            self.body_input = None;
            return;
        };
        let (title, prompt) = (entry.title.clone(), entry.prompt.clone());
        match (&self.title_input, &self.body_input) {
            (Some(title_input), Some(body_input)) => {
                set_value(title_input, &title, window, cx);
                set_textarea_value(body_input, &prompt, window, cx);
            }
            _ => {
                self.title_input = Some(cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(&title)
                        .placeholder("タイトル")
                }));
                self.body_input = Some(cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .default_value(&prompt)
                        .placeholder("保存した指示として、選択範囲のAIメニューから実行する内容")
                }));
            }
        }
    }

    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        if !self.loaded || self.load_error.is_some() {
            return;
        }
        if self.title_input.is_none() {
            self.sync_inputs_from_selected(window, cx);
        }
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<EditorView>) {
        if index == self.selected || index >= self.draft.len() {
            return;
        }
        self.flush_selected_from_inputs(cx);
        self.selected = index;
        self.title_input = None;
        self.body_input = None;
        self.sync_inputs_from_selected(window, cx);
        self.message = None;
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        if self.draft.len() >= hane_ai::MAX_PROMPT_COUNT {
            self.message = Some(format!(
                "保存できる指示は最大{}件です。",
                hane_ai::MAX_PROMPT_COUNT
            ));
            return;
        }
        self.flush_selected_from_inputs(cx);
        self.draft.push(UserPromptDraft {
            id: None,
            title: String::new(),
            prompt: String::new(),
        });
        self.selected = self.draft.len() - 1;
        self.title_input = None;
        self.body_input = None;
        self.sync_inputs_from_selected(window, cx);
        self.message = None;
        if let Some(title_input) = &self.title_input {
            window.focus(&title_input.read(cx).focus_handle(cx), cx);
        }
    }

    fn request_delete(&mut self, index: usize) {
        self.pending_delete = Some(index);
    }

    fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<EditorView>) {
        // A save in flight already sent a snapshot of `self.draft` in its
        // current order; removing an entry now would shift every later
        // entry's position and make `apply_save`'s positional id
        // reconciliation assign the wrong id to the wrong entry. Leave
        // `pending_delete` set so the confirmation stays open and the user
        // can retry once the save completes.
        if self.busy {
            return;
        }
        let Some(index) = self.pending_delete.take() else {
            return;
        };
        if index >= self.draft.len() {
            return;
        }
        self.flush_selected_from_inputs(cx);
        self.draft.remove(index);
        self.selected = self.selected.min(self.draft.len().saturating_sub(1));
        self.title_input = None;
        self.body_input = None;
        if !self.draft.is_empty() {
            self.sync_inputs_from_selected(window, cx);
        }
        self.message = None;
    }

    pub(super) fn revert(&mut self, cx: &mut Context<EditorView>) {
        self.message = None;
        self.pending_delete = None;
        self.reload(cx);
    }

    fn save(&mut self, cx: &mut Context<EditorView>, target: Option<LeaveTarget>) {
        self.flush_selected_from_inputs(cx);
        self.message = None;
        let Some(root) = self.root.clone() else {
            self.message = Some("保存先を利用できません。".to_owned());
            return;
        };
        if self.draft.len() > hane_ai::MAX_PROMPT_COUNT {
            self.message = Some(format!(
                "保存できる指示は最大{}件です。",
                hane_ai::MAX_PROMPT_COUNT
            ));
            return;
        }
        for entry in &self.draft {
            if entry.title.trim().is_empty() {
                self.message = Some("タイトルを入力してください。".to_owned());
                return;
            }
            if entry.title.contains('\n') || entry.title.contains('\r') {
                self.message = Some("タイトルに改行は使えません。".to_owned());
                return;
            }
            if entry.title.chars().count() > hane_ai::MAX_PROMPT_TITLE_CHARS {
                self.message = Some(format!(
                    "タイトルは{}文字以内にしてください。",
                    hane_ai::MAX_PROMPT_TITLE_CHARS
                ));
                return;
            }
            if entry.prompt.trim().is_empty() {
                self.message = Some("プロンプト本文を入力してください。".to_owned());
                return;
            }
            if entry.prompt.len() > hane_ai::MAX_PROMPT_BODY_BYTES {
                self.message = Some(format!(
                    "プロンプト本文は{}バイト以内にしてください。",
                    hane_ai::MAX_PROMPT_BODY_BYTES
                ));
                return;
            }
        }
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let expected_revision = self.revision;
        let drafts = self.draft.clone();
        self.busy = true;
        self.pending_target = target;
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { store_for(&root).save(expected_revision, drafts) })
                .await;
            let _ = view.update(cx, move |view, cx| {
                view.ai_prompts.apply_save(generation, cx, outcome);
                cx.notify();
            });
        })
        .detach();
    }

    /// Title/body edits, adding a row, and switching the selected row are
    /// never blocked while a save is in flight (only deleting is, in
    /// `confirm_delete`); the user can keep working while the previous
    /// snapshot is still being persisted. On success this must not discard
    /// whatever that continued editing produced: it only adopts the ids the
    /// store just assigned, using the fact that nothing can have shifted
    /// `self.draft`'s existing positions in the meantime, so they still line
    /// up with the snapshot that was actually sent.
    fn apply_save(
        &mut self,
        generation: u64,
        cx: &App,
        outcome: Result<UserPrompts, UserPromptsSaveError>,
    ) {
        if generation != self.save_generation {
            return;
        }
        self.busy = false;
        match outcome {
            Ok(saved) => {
                self.flush_selected_from_inputs(cx);

                self.revision = saved.revision;
                self.baseline = saved
                    .prompts
                    .into_iter()
                    .map(|p| UserPromptDraft {
                        id: Some(p.id),
                        title: p.title,
                        prompt: p.prompt,
                    })
                    .collect();
                for (entry, saved_entry) in self.draft.iter_mut().zip(self.baseline.iter()) {
                    if entry.id.is_none() {
                        entry.id = saved_entry.id;
                    }
                }
                self.selected = self.selected.min(self.draft.len().saturating_sub(1));
                self.message = None;
                // Saving is not permission to leave: edits made while the
                // save was in flight are still unsaved, so ask again and
                // keep the held drop instead of closing the settings.
                if let Some(target) = self.pending_target.take() {
                    if self.is_dirty(cx) {
                        self.leave_prompt = Some(target);
                    } else {
                        self.route_ready = Some(target);
                    }
                }
            }
            Err(error) => {
                self.pending_target = None;
                self.message = Some(save_error_message(&error));
            }
        }
    }

    /// Drops a save-then-leave intent that a newer decision superseded.
    /// An already generated ready route is superseded as well.
    pub(super) fn cancel_pending_leave(&mut self) {
        self.pending_target = None;
        self.route_ready = None;
    }

    pub(super) fn confirm_leave(&mut self, target: LeaveTarget) {
        self.pending_target = None;
        self.route_ready = None;
        self.leave_prompt = Some(target);
    }

    /// The last synchronous boundary before actually leaving: edits made
    /// after the save completed (and before the route is consumed) turn the
    /// route back into a leave confirmation instead of discarding them.
    pub(super) fn take_ready_route(&mut self, cx: &App) -> Option<LeaveTarget> {
        let target = self.route_ready.take()?;
        if self.is_dirty(cx) {
            self.leave_prompt = Some(target);
            return None;
        }
        Some(target)
    }

    pub(super) fn input_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.title_input.as_ref().is_some_and(|input| {
            input.read(cx).focus_handle(cx).is_focused(window)
        }) || self.body_input.as_ref().is_some_and(|input| {
            input.read(cx).focus_handle(cx).is_focused(window)
        })
    }

    pub(super) fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<EditorView>,
        theme: Theme,
    ) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        let dirty = self.is_dirty(cx);

        let mut body = div()
            .id("ai-prompts-page-content")
            .w_full()
            .max_w(px(948.0))
            .px(px(38.0))
            .py(px(29.0))
            .flex()
            .flex_col()
            .gap_4()
            .text_size(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(27.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("AIプロンプト"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(theme.quote_foreground))
                            .child("保存した指示は、文章を選択したときのAIメニューに表示されます。"),
                    ),
            );

        if let Some(error) = &self.load_error {
            let view = cx.entity();
            let retry = Button::new("ai-prompts-reload")
                .label("再読み込み")
                .small()
                .on_click(move |_, _, app| {
                    view.update(app, |view, cx| view.ai_prompts.revert(cx));
                });
            return body
                .child(
                    div()
                        .id("ai-prompts-load-error")
                        .text_color(rgb(0xb42318))
                        .child(error.clone()),
                )
                .child(retry)
                .into_any_element();
        }

        if !self.loaded {
            return body.child(div().child("読み込んでいます…")).into_any_element();
        }

        body = body.child(self.editor_section(cx, theme));

        if let Some(message) = &self.message {
            body = body.child(
                div()
                    .id("ai-prompts-message")
                    .text_color(rgb(0xb42318))
                    .child(message.clone()),
            );
        }
        if let Some(target) = self.leave_prompt {
            body = body.child(self.leave_confirmation(cx, target));
        }
        if self.pending_delete.is_some() {
            body = body.child(self.delete_confirmation(cx));
        }

        let busy = self.busy;
        let view = cx.entity();
        let save = Button::new("ai-prompts-save")
            .label("保存")
            .primary()
            .disabled(!dirty || busy)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.ai_prompts.save(cx, None));
            });
        let view = cx.entity();
        let revert = Button::new("ai-prompts-revert")
            .label("元に戻す")
            .disabled(busy)
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.ai_prompts.revert(cx));
            });
        let selected = self.selected;
        let view = cx.entity();
        let delete = Button::new("ai-prompts-delete")
            .label("削除")
            .disabled(busy || self.draft.is_empty())
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_prompts.request_delete(selected);
                    cx.notify();
                });
            });

        let savebar = div()
            .id("ai-prompts-savebar")
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px(px(16.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(rgb(theme.table_border))
            .child(save)
            .child(revert)
            .child(delete);

        let scroll = div()
            .id("ai-prompts-scroll-area")
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .child(body);
        div()
            .id("ai-prompts-page")
            .size_full()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(scroll)
            .child(savebar)
            .into_any_element()
    }

    fn editor_section(&mut self, cx: &mut Context<EditorView>, theme: Theme) -> impl IntoElement {
        let mut list = div()
            .id("ai-prompts-list")
            .w(px(220.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll();
        for (index, entry) in self.draft.iter().enumerate() {
            let selected = index == self.selected;
            let label = if entry.title.trim().is_empty() {
                "（タイトル未入力）".to_owned()
            } else {
                entry.title.clone()
            };
            let view = cx.entity();
            let row = div()
                .id(("ai-prompt-row", index))
                .w_full()
                .px(px(10.0))
                .py(px(7.0))
                .rounded_sm()
                .cursor_pointer()
                .bg(rgb(if selected {
                    theme.sidebar_active_background
                } else {
                    theme.code_background
                }))
                .child(label)
                .on_click(move |_, window, app| {
                    view.update(app, |view, cx| {
                        view.ai_prompts.select(index, window, cx);
                        cx.notify();
                    });
                });
            list = list.child(row);
        }
        let view = cx.entity();
        let add = Button::new("ai-prompts-add")
            .label("追加")
            .small()
            .ghost()
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_prompts.add(window, cx);
                    cx.notify();
                });
            });
        let list_column = div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(220.0))
            .flex_none()
            .child(list)
            .child(add);

        let detail = if self.draft.is_empty() {
            div()
                .flex_1()
                .min_w(px(0.0))
                .child("「追加」から最初の指示を作成してください。")
                .into_any_element()
        } else {
            let title_input = self.title_input.clone();
            let body_input = self.body_input.clone();
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap_2()
                .child("タイトル")
                .children(title_input.map(|input| Input::new(&input).aria_label("prompt title")))
                .child("プロンプト")
                .child(
                    div()
                        .id("ai-prompt-body")
                        .w_full()
                        .h(px(PROMPT_BODY_HEIGHT))
                        .flex_none()
                        .overflow_y_scroll()
                        .children(
                            body_input.map(|input| Textarea::new(&input).aria_label("prompt body")),
                        ),
                )
                .into_any_element()
        };
        div().w_full().flex().gap_4().child(list_column).child(detail)
    }

    fn leave_confirmation(
        &mut self,
        cx: &mut Context<EditorView>,
        target: LeaveTarget,
    ) -> impl IntoElement {
        let view = cx.entity();
        let save = Button::new("ai-prompts-save-and-leave")
            .label("保存して移動")
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| view.ai_prompts.save(cx, Some(target)));
            });
        let view = cx.entity();
        let discard = Button::new("ai-prompts-discard-and-leave")
            .label("破棄して移動")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_prompts.leave_prompt = None;
                    view.ai_prompts.draft = view.ai_prompts.baseline.clone();
                    view.ai_prompts.selected = 0;
                    view.ai_prompts.title_input = None;
                    view.ai_prompts.body_input = None;
                    match target {
                        LeaveTarget::Category(category) => {
                            view.select_settings_category(category, window, cx)
                        }
                        LeaveTarget::Close => view.close_settings(window, cx),
                    }
                })
            });
        let view = cx.entity();
        let stay = Button::new("ai-prompts-continue-editing")
            .label("編集を続ける")
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_prompts.leave_prompt = None;
                    view.ai_prompts.cancel_pending_leave();
                    view.cancel_pending_settings_drop();
                    cx.notify();
                })
            });
        div()
            .id("ai-prompts-leave-confirmation")
            .flex()
            .flex_col()
            .gap_2()
            .child("AIプロンプトに未保存の変更があります。保存して移動するか、変更を破棄してください。")
            .child(div().flex().gap_2().child(save).child(discard).child(stay))
    }

    fn delete_confirmation(&mut self, cx: &mut Context<EditorView>) -> impl IntoElement {
        let view = cx.entity();
        let confirm = Button::new("ai-prompts-confirm-delete")
            .label("削除する")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.ai_prompts.confirm_delete(window, cx);
                    cx.notify();
                });
            });
        let view = cx.entity();
        let cancel = Button::new("ai-prompts-cancel-delete")
            .label("取り消す")
            .ghost()
            .on_click(move |_, _, app| {
                view.update(app, |view, cx| {
                    view.ai_prompts.cancel_delete();
                    cx.notify();
                });
            });
        div()
            .id("ai-prompts-delete-confirmation")
            .flex()
            .flex_col()
            .gap_2()
            .child("この指示を削除しますか？「保存」を押すまで確定しません。")
            .child(div().flex().gap_2().child(confirm).child(cancel))
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

fn set_value(input: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut Context<EditorView>) {
    input.update(cx, |state, cx| state.set_value(value, window, cx));
}

fn textarea_value(input: &Entity<TextareaState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

fn set_textarea_value(
    input: &Entity<TextareaState>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<EditorView>,
) {
    input.update(cx, |state, cx| state.set_value(value, window, cx));
}

fn load_error_message(error: &UserPromptsLoadError) -> String {
    match error {
        UserPromptsLoadError::Io(_) => {
            "保存済みの指示を読み込めませんでした。ファイルが壊れているか権限を確認してください。"
                .to_owned()
        }
        UserPromptsLoadError::UnsupportedSchemaVersion { .. } => {
            "保存済みの指示は新しいHaneで作成されたため、このバージョンでは読み込めません。"
                .to_owned()
        }
    }
}

fn save_error_message(error: &UserPromptsSaveError) -> String {
    match error {
        UserPromptsSaveError::Io(_) => "保存に失敗しました。もう一度お試しください。".to_owned(),
        UserPromptsSaveError::Busy => {
            "別の保存処理が進行中です。少し待って再試行してください。".to_owned()
        }
        UserPromptsSaveError::RevisionConflict { .. } => {
            "他の画面でプロンプトが更新されています。「元に戻す」で最新の内容を読み込んでから編集し直してください。"
                .to_owned()
        }
        UserPromptsSaveError::TooManyPrompts { max } => {
            format!("保存できる指示は最大{max}件です。")
        }
        UserPromptsSaveError::InvalidId { .. } | UserPromptsSaveError::DuplicateId { .. } => {
            "内部状態が不整合です。「元に戻す」で最新の内容を読み込んでから編集し直してください。"
                .to_owned()
        }
        UserPromptsSaveError::TitleEmpty => "タイトルを入力してください。".to_owned(),
        UserPromptsSaveError::TitleContainsNewline => "タイトルに改行は使えません。".to_owned(),
        UserPromptsSaveError::TitleTooLong { max_chars } => {
            format!("タイトルは{max_chars}文字以内にしてください。")
        }
        UserPromptsSaveError::PromptEmpty => "プロンプト本文を入力してください。".to_owned(),
        UserPromptsSaveError::PromptTooLarge { max_bytes } => {
            format!("プロンプト本文は{max_bytes}バイト以内にしてください。")
        }
        UserPromptsSaveError::IdCounterOverflow | UserPromptsSaveError::RevisionOverflow => {
            "内部状態の上限に達しました。アプリを再起動してください。".to_owned()
        }
        UserPromptsSaveError::PersistedDurabilityUnconfirmed(_) => {
            "保存は反映された可能性がありますが確認できませんでした。「元に戻す」で再読み込みしてください。"
                .to_owned()
        }
    }
}

impl EditorView {
    pub(super) fn ai_prompts_render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ai_prompts.render(window, cx, self.theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{ExternalDropTarget, SettingsCategory};
    use gpui::px;

    // Regression for a saved draft resetting `selected` to the first prompt
    // and losing track of the one actually being edited.
    #[gpui::test]
    fn saving_a_non_first_prompt_keeps_it_selected(cx: &mut gpui::TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "hane-ai-prompts-save-selection-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));

        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::Prompts;
            view.ai_prompts.root = Some(root.clone());
            view.ai_prompts.loaded = true;
            view.ai_prompts.load_error = None;
            view.ai_prompts.revision = 0;
            view.ai_prompts.baseline = vec![
                UserPromptDraft {
                    id: None,
                    title: "一番目".to_owned(),
                    prompt: "最初の指示".to_owned(),
                },
                UserPromptDraft {
                    id: None,
                    title: "二番目".to_owned(),
                    prompt: "二番目の指示".to_owned(),
                },
                UserPromptDraft {
                    id: None,
                    title: "三番目".to_owned(),
                    prompt: "三番目の指示".to_owned(),
                },
            ];
            view.ai_prompts.draft = view.ai_prompts.baseline.clone();
            view.ai_prompts.selected = 1;
            cx.notify();
        });
        cx.run_until_parked();

        let selected_title_before = view.read_with(cx, |view, app| {
            value(view.ai_prompts.title_input.as_ref().unwrap(), app)
        });
        assert_eq!(selected_title_before, "二番目");

        view.update(cx, |view, cx| {
            view.ai_prompts.save(cx, None);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            assert_eq!(view.ai_prompts.message, None, "save must not fail");
            assert_eq!(
                view.ai_prompts.selected, 1,
                "selected must stay on the edited prompt, not reset to the first one"
            );
            assert_eq!(view.ai_prompts.draft.len(), 3);
            assert_eq!(
                value(
                    view.ai_prompts
                        .title_input
                        .as_ref()
                        .expect("inputs stay populated after save"),
                    app
                ),
                "二番目",
                "the input fields must stay in sync with the still-selected prompt"
            );
        });

        let _ = std::fs::remove_dir_all(&root);
    }

    // Saving an emptied list (e.g. after deleting the only prompt) must leave
    // `selected` in a valid state instead of an out-of-range index.
    #[gpui::test]
    fn saving_an_emptied_draft_list_keeps_selected_valid(cx: &mut gpui::TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "hane-ai-prompts-save-selection-empty-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));

        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::Prompts;
            view.ai_prompts.root = Some(root.clone());
            view.ai_prompts.loaded = true;
            view.ai_prompts.load_error = None;
            view.ai_prompts.revision = 0;
            view.ai_prompts.baseline = vec![UserPromptDraft {
                id: None,
                title: "唯一の指示".to_owned(),
                prompt: "唯一の指示の内容".to_owned(),
            }];
            view.ai_prompts.draft = view.ai_prompts.baseline.clone();
            view.ai_prompts.selected = 0;
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.ai_prompts.request_delete(0);
                view.ai_prompts.confirm_delete(window, cx);
            });
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.ai_prompts.draft.is_empty());
            assert_eq!(view.ai_prompts.selected, 0);
        });

        view.update(cx, |view, cx| {
            view.ai_prompts.save(cx, None);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.ai_prompts.message, None,
                "saving an emptied list must not fail"
            );
            assert!(view.ai_prompts.draft.is_empty());
            assert_eq!(view.ai_prompts.selected, 0);
            assert!(view.ai_prompts.title_input.is_none());
            assert!(view.ai_prompts.body_input.is_none());
        });

        let _ = std::fs::remove_dir_all(&root);
    }

    // Regression for #450: picking another category while the prompts
    // draft is dirty must route to the clicked category after the leave is
    // confirmed, not unconditionally back to "一般".
    #[gpui::test]
    fn switching_category_from_a_dirty_ai_prompts_draft_targets_the_clicked_category(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!(
            "hane-ai-prompts-switch-category-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));

        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::Prompts;
            view.ai_prompts.root = Some(root.clone());
            view.ai_prompts.loaded = true;
            view.ai_prompts.load_error = None;
            view.ai_prompts.revision = 0;
            view.ai_prompts.baseline = vec![UserPromptDraft {
                id: None,
                title: "指示".to_owned(),
                prompt: "指示の内容".to_owned(),
            }];
            view.ai_prompts.draft = vec![UserPromptDraft {
                id: None,
                title: "編集中の指示".to_owned(),
                prompt: "指示の内容".to_owned(),
            }];
            view.ai_prompts.selected = 0;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, app| view.ai_prompts.is_dirty(app)));

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.request_settings_category(SettingsCategory::Connection, window, cx)
            });
        });

        assert_eq!(
            view.read_with(cx, |view, _| view.settings_category),
            SettingsCategory::Prompts,
            "the category must not switch until the leave is confirmed"
        );
        assert_eq!(
            view.read_with(cx, |view, _| view.ai_prompts.leave_prompt),
            Some(LeaveTarget::Category(SettingsCategory::Connection))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn dirty_prompts_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        name: &str,
        valid_title: bool,
    ) -> (
        gpui::Entity<EditorView>,
        std::path::PathBuf,
        &'a mut gpui::VisualTestContext,
    ) {
        let root = std::env::temp_dir().join(format!("hane-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));
        let title = if valid_title { "編集中の指示" } else { "" };
        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::Prompts;
            view.ai_prompts.root = Some(root.clone());
            view.ai_prompts.loaded = true;
            view.ai_prompts.load_error = None;
            view.ai_prompts.revision = 0;
            view.ai_prompts.baseline = vec![UserPromptDraft {
                id: None,
                title: "指示".to_owned(),
                prompt: "指示の内容".to_owned(),
            }];
            view.ai_prompts.draft = vec![UserPromptDraft {
                id: None,
                title: title.to_owned(),
                prompt: "指示の内容".to_owned(),
            }];
            view.ai_prompts.selected = 0;
            cx.notify();
        });
        cx.run_until_parked();
        (view, root, cx)
    }

    fn drop_two(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext) {
        let paths = vec![
            std::path::PathBuf::from("/tmp/hane-drop-a.md"),
            std::path::PathBuf::from("/tmp/hane-drop-b.md"),
        ];
        cx.update(|window, app| {
            view.update(app, |view, cx| view.handle_external_drop(&paths, window, cx));
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_drop_behind_dirty_prompts_is_applied_once_after_discard(cx: &mut gpui::TestAppContext) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-drop-discard", true);
        drop_two(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.settings_open);
            assert_eq!(view.ai_prompts.leave_prompt, Some(LeaveTarget::Close));
            assert_eq!(view.status, None);
        });

        // Same steps as the 「破棄して移動」 button.
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.ai_prompts.leave_prompt = None;
                view.ai_prompts.draft = view.ai_prompts.baseline.clone();
                view.close_settings(window, cx);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.settings_open);
            assert_eq!(view.status.as_deref(), Some("Drop one item at a time"));
            assert!(view.pending_settings_drop.is_none());
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn continue_editing_cancels_the_held_drop(cx: &mut gpui::TestAppContext) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-drop-continue", true);
        drop_two(&view, cx);
        // Same steps as the 「編集を続ける」 button.
        view.update(cx, |view, cx| {
            view.ai_prompts.leave_prompt = None;
            view.cancel_pending_settings_drop();
            cx.notify();
        });
        cx.update(|window, app| {
            view.update(app, |view, cx| view.close_settings(window, cx));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.settings_open);
            assert_eq!(view.status, None, "the cancelled drop must not apply");
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_drop_behind_dirty_prompts_is_applied_after_save_and_not_on_validation_failure(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-drop-save-fail", false);
        drop_two(&view, cx);
        view.update(cx, |view, cx| {
            view.ai_prompts.save(cx, Some(LeaveTarget::Close));
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.settings_open, "validation failure keeps settings open");
            assert_eq!(view.status, None);
        });

        // `save` syncs the draft from the on-screen inputs first, so the
        // title must be entered through the real input, not the draft.
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                set_value(
                    view.ai_prompts.title_input.as_ref().unwrap(),
                    "保存できる指示",
                    window,
                    cx,
                );
                view.ai_prompts.save(cx, Some(LeaveTarget::Close));
                cx.notify();
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.settings_open);
            assert_eq!(view.status.as_deref(), Some("Drop one item at a time"));
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_later_drop_replaces_the_held_one_and_empty_is_ignored(cx: &mut gpui::TestAppContext) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-drop-replace", true);
        drop_two(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.handle_external_drop(&[], window, cx));
        });
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.pending_settings_drop,
                Some(ExternalDropTarget::TooMany),
                "an empty drop must not touch the held request"
            );
        });
        let single = vec![std::path::PathBuf::from("/tmp/hane-drop-c.md")];
        cx.update(|window, app| {
            view.update(app, |view, cx| view.handle_external_drop(&single, window, cx));
        });
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.pending_settings_drop,
                Some(ExternalDropTarget::Open(single[0].clone()))
            );
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn edits_made_during_a_save_then_leave_keep_settings_and_the_held_drop(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-drop-save-edit", true);
        drop_two(&view, cx);
        // Start 「保存して移動」, then keep typing before the save completes.
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.ai_prompts.save(cx, Some(LeaveTarget::Close));
                assert!(view.ai_prompts.busy, "the save must be in flight here");
                set_value(
                    view.ai_prompts.title_input.as_ref().unwrap(),
                    "保存後に続けた編集",
                    window,
                    cx,
                );
                cx.notify();
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, app| {
            assert!(view.settings_open, "unsaved continued edits keep settings open");
            assert_eq!(view.status, None, "the held drop must not be applied yet");
            assert_eq!(
                view.pending_settings_drop,
                Some(ExternalDropTarget::TooMany),
                "the held drop must survive"
            );
            assert_eq!(view.ai_prompts.leave_prompt, Some(LeaveTarget::Close));
            assert!(view.ai_prompts.is_dirty(app));
            assert_eq!(
                value(view.ai_prompts.title_input.as_ref().unwrap(), app),
                "保存後に続けた編集"
            );
        });

        // Saving the latest edit now allows the leave, applying the drop once.
        view.update(cx, |view, cx| {
            view.ai_prompts.save(cx, Some(LeaveTarget::Close));
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.settings_open);
            assert_eq!(view.status.as_deref(), Some("Drop one item at a time"));
            assert!(view.pending_settings_drop.is_none());
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn continue_editing_during_a_save_then_leave_does_not_close_later(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-drop-save-stay", true);
        drop_two(&view, cx);
        cx.update(|_, app| {
            view.update(app, |view, cx| {
                view.ai_prompts.save(cx, Some(LeaveTarget::Close));
                // Same steps as the 「編集を続ける」 button.
                view.ai_prompts.leave_prompt = None;
                view.ai_prompts.cancel_pending_leave();
                view.cancel_pending_settings_drop();
                cx.notify();
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.settings_open, "a stale save must not close the settings");
            assert!(view.ai_prompts.leave_prompt.is_none());
            assert_eq!(view.status, None);
        });

        // An ordinary leave later must not revive the cancelled drop.
        cx.update(|window, app| {
            view.update(app, |view, cx| view.close_settings(window, cx));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.settings_open);
            assert_eq!(view.status, None, "the cancelled drop must not apply");
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn an_edit_between_save_completion_and_route_consumption_keeps_settings(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-ready-route-edit", true);
        drop_two(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                // The save completed cleanly: the route is ready but unconsumed.
                view.ai_prompts.baseline = view.ai_prompts.dirty_draft(cx);
                view.ai_prompts.route_ready = Some(LeaveTarget::Close);
                // A further edit lands before the route is consumed.
                set_value(
                    view.ai_prompts.title_input.as_ref().unwrap(),
                    "経路消費前の編集",
                    window,
                    cx,
                );
                assert!(view.ai_prompts.take_ready_route(cx).is_none());
                assert_eq!(view.ai_prompts.leave_prompt, Some(LeaveTarget::Close));
                assert!(view.settings_open);
                assert_eq!(
                    view.pending_settings_drop,
                    Some(ExternalDropTarget::TooMany)
                );
                assert_eq!(
                    value(view.ai_prompts.title_input.as_ref().unwrap(), cx),
                    "経路消費前の編集"
                );
            });
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_superseding_intent_clears_an_unconsumed_ready_route(cx: &mut gpui::TestAppContext) {
        let (view, root, cx) = dirty_prompts_view(cx, "prompts-ready-route-cancel", true);
        view.update(cx, |view, cx| {
            view.ai_prompts.baseline = view.ai_prompts.dirty_draft(cx);
            view.ai_prompts.route_ready = Some(LeaveTarget::Close);
            // Same steps as the 「編集を続ける」 button.
            view.ai_prompts.cancel_pending_leave();
            assert!(view.ai_prompts.take_ready_route(cx).is_none());
            assert!(view.ai_prompts.leave_prompt.is_none());

            view.ai_prompts.route_ready = Some(LeaveTarget::Close);
            view.ai_prompts.confirm_leave(LeaveTarget::Category(SettingsCategory::General));
            assert!(view.ai_prompts.take_ready_route(cx).is_none());
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    // Regression for #450: editing the selected prompt's title/body,
    // switching to a different row, and adding a new row must all survive a
    // save that was already in flight when they happened, instead of being
    // silently replaced by the (now stale) snapshot that save sent.
    #[gpui::test]
    fn concurrent_edits_while_a_save_is_in_flight_are_not_lost(cx: &mut gpui::TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "hane-ai-prompts-save-edit-race-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));

        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::Prompts;
            view.ai_prompts.root = Some(root.clone());
            view.ai_prompts.loaded = true;
            view.ai_prompts.load_error = None;
            view.ai_prompts.revision = 0;
            view.ai_prompts.baseline = vec![
                UserPromptDraft {
                    id: None,
                    title: "一番目".to_owned(),
                    prompt: "最初の指示".to_owned(),
                },
                UserPromptDraft {
                    id: None,
                    title: "二番目".to_owned(),
                    prompt: "二番目の指示".to_owned(),
                },
            ];
            view.ai_prompts.draft = view.ai_prompts.baseline.clone();
            view.ai_prompts.selected = 0;
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                // Sends a snapshot of the two baseline entries and leaves
                // the background save pending (it is not driven until the
                // `run_until_parked` below).
                view.ai_prompts.save(cx, None);
                assert!(view.ai_prompts.busy, "the save must be in flight here");

                // Switch to the second row and edit it while the save above
                // is still pending.
                view.ai_prompts.select(1, window, cx);
                set_value(
                    view.ai_prompts.title_input.as_ref().unwrap(),
                    "二番目・編集後",
                    window,
                    cx,
                );
                set_textarea_value(
                    view.ai_prompts.body_input.as_ref().unwrap(),
                    "二番目の指示・編集後",
                    window,
                    cx,
                );

                // Add a brand-new third row and type into it too, still
                // while the same save is pending.
                view.ai_prompts.add(window, cx);
                set_value(
                    view.ai_prompts.title_input.as_ref().unwrap(),
                    "三番目（保存中に追加）",
                    window,
                    cx,
                );
                set_textarea_value(
                    view.ai_prompts.body_input.as_ref().unwrap(),
                    "三番目の指示",
                    window,
                    cx,
                );
            });
        });

        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            assert_eq!(view.ai_prompts.message, None, "save must not fail");
            assert_eq!(view.ai_prompts.revision, 1);
            assert_eq!(
                view.ai_prompts.draft,
                vec![
                    UserPromptDraft {
                        id: Some(1),
                        title: "一番目".to_owned(),
                        prompt: "最初の指示".to_owned(),
                    },
                    UserPromptDraft {
                        id: Some(2),
                        title: "二番目・編集後".to_owned(),
                        prompt: "二番目の指示・編集後".to_owned(),
                    },
                    UserPromptDraft {
                        id: None,
                        title: "三番目（保存中に追加）".to_owned(),
                        prompt: "三番目の指示".to_owned(),
                    },
                ],
                "the title/body edit and the added row made during the save must survive"
            );
            assert_eq!(
                view.ai_prompts.selected, 2,
                "the row switched to during the save must stay selected"
            );
            assert_eq!(
                value(view.ai_prompts.title_input.as_ref().unwrap(), app),
                "三番目（保存中に追加）",
                "the input must still show what was being typed, not be reset"
            );
        });

        let _ = std::fs::remove_dir_all(&root);
    }

    // Regression for #450: deleting a row while a save is in flight would
    // shift later entries' positions and make the success handler assign
    // the wrong newly minted id to the wrong row. Deleting must be ignored
    // until the save completes, and must work normally afterward.
    #[gpui::test]
    fn deleting_while_a_save_is_in_flight_is_ignored_until_it_completes(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!(
            "hane-ai-prompts-save-delete-race-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("document body\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(640.0), px(420.0)));

        view.update(cx, |view, cx| {
            view.settings_open = true;
            view.settings_category = SettingsCategory::Prompts;
            view.ai_prompts.root = Some(root.clone());
            view.ai_prompts.loaded = true;
            view.ai_prompts.load_error = None;
            view.ai_prompts.revision = 0;
            view.ai_prompts.baseline = vec![
                UserPromptDraft {
                    id: None,
                    title: "一番目".to_owned(),
                    prompt: "最初の指示".to_owned(),
                },
                UserPromptDraft {
                    id: None,
                    title: "二番目".to_owned(),
                    prompt: "二番目の指示".to_owned(),
                },
                UserPromptDraft {
                    id: None,
                    title: "三番目".to_owned(),
                    prompt: "三番目の指示".to_owned(),
                },
            ];
            view.ai_prompts.draft = view.ai_prompts.baseline.clone();
            view.ai_prompts.selected = 0;
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.ai_prompts.save(cx, None);
                assert!(view.ai_prompts.busy, "the save must be in flight here");

                view.ai_prompts.request_delete(1);
                view.ai_prompts.confirm_delete(window, cx);
                assert_eq!(
                    view.ai_prompts.draft.len(),
                    3,
                    "deleting must be ignored while the save is in flight"
                );
                assert_eq!(
                    view.ai_prompts.pending_delete,
                    Some(1),
                    "the confirmation must stay pending so the user can retry"
                );
            });
        });

        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_prompts.message, None, "save must not fail");
            assert_eq!(view.ai_prompts.draft.len(), 3);
        });

        // Once the save has completed, deleting works normally again.
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.ai_prompts.confirm_delete(window, cx);
            });
        });

        view.read_with(cx, |view, _| {
            assert_eq!(view.ai_prompts.draft.len(), 2);
            assert_eq!(view.ai_prompts.pending_delete, None);
        });

        let _ = std::fs::remove_dir_all(&root);
    }
}
