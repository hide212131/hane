use super::*;
use hane_session::{DryingInkSpeed, DryingInkStrength, PaperTextureStrength};

impl EditorView {
    pub(crate) fn toggle_autosave(&mut self, cx: &mut Context<Self>) {
        self.set_autosave(!self.settings.autosave, cx);
    }

    /// Applies and stores the autosave setting. Turning it on arms the
    /// existing debounce for the active document; turning it off only stops
    /// later autosave requests, a write that already started is not canceled.
    pub(crate) fn set_autosave(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.settings.autosave == enabled {
            return;
        }
        self.settings.autosave = enabled;
        self.status = Some(if enabled {
            "自動保存をオンにしました".to_owned()
        } else {
            "自動保存をオフにしました".to_owned()
        });
        self.store_settings();
        self.schedule_autosave(cx);
        cx.notify();
    }

    pub(crate) fn settings_open(&self) -> bool {
        self.settings_open
    }

    pub fn settings_is_open(&self) -> bool {
        self.settings_open
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor().ime().is_some() {
            self.status = Some("入力変換を確定または取り消してから設定を開いてください".to_owned());
            cx.notify();
            return;
        }
        if self.sidebar_filter_has_composition() || self.inline_rename_has_composition() {
            self.status =
                Some("未確定文字を確定または取り消してから設定を開いてください".to_owned());
            cx.notify();
            return;
        }
        if !self.cancel_inline_rename(cx) {
            return;
        }
        crate::init_components(cx);
        self.ai_selection.reset();
        self.blur_sidebar_filter(cx);
        self.settings_open = true;
        self.settings_category = SettingsCategory::General;
        self.ai_settings.begin_settings_session();
        self.ai_prompts.begin_settings_session(cx);
        self.network_settings
            .begin_settings_session(&self.settings.network);
        self.settings_error = None;
        self.default_folder_error = None;
        self.file_context_menu_generation = self.file_context_menu_generation.wrapping_add(1);
        let generation = self.file_context_menu_generation;
        self.file_context_menu_busy = true;
        self.file_context_menu_state = FileContextMenuState::NotChecked;
        cx.spawn(async move |view, cx| {
            let state = cx
                .background_executor()
                .spawn(async { context_menu::current_file_context_menu_state() })
                .await;
            let _ = view.update(cx, |view, cx| {
                if view.settings_open && view.file_context_menu_generation == generation {
                    view.file_context_menu_busy = false;
                    view.file_context_menu_state = state;
                    cx.notify();
                }
            });
        })
        .detach();
        // Local re-validation of an already prepared update only; this never
        // communicates, and a check or download still needs a button press.
        self.restore_app_update_local(cx);
        window.focus(&self.settings_focus_handle, cx);
        cx.notify();
    }

    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_open {
            return;
        }
        self.settings_open = false;
        self.settings_category = SettingsCategory::General;
        self.ai_settings.close_settings();
        self.ai_prompts.close_settings();
        self.network_settings.close_settings(&self.settings.network);
        self.file_context_menu_busy = false;
        self.file_context_menu_generation = self.file_context_menu_generation.wrapping_add(1);
        window.focus(&self.focus_handle, cx);
        cx.notify();
        if let Some(target) = self.pending_settings_drop.take() {
            let protected = std::mem::take(&mut self.pending_settings_drop_protected);
            self.apply_external_drop(target, protected, cx);
        }
    }

    pub(crate) fn handle_settings_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.settings_category {
            SettingsCategory::Connection if self.ai_settings.input_has_focus(window, cx) => {
                // A settings input owns Escape, including while an IME
                // composition is active. The visible navigation remains
                // available to leave.
                return;
            }
            SettingsCategory::Prompts if self.ai_prompts.input_has_focus(window, cx) => return,
            SettingsCategory::Network if self.network_settings.input_has_focus(window, cx) => {
                return;
            }
            _ => {}
        }
        self.request_leave_settings(window, cx);
    }

    pub(crate) fn select_settings_category(
        &mut self,
        category: SettingsCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if category == SettingsCategory::Connection && self.settings_category != category {
            self.ai_settings.activate(cx);
        }
        self.settings_category = category;
        self.pending_settings_drop = None;
        // Moving on settles any save-then-leave intent that is still in
        // flight; a late save completion must not route again.
        self.ai_settings.cancel_pending_leave();
        self.ai_prompts.cancel_pending_leave();
        self.network_settings.cancel_pending_leave();
        window.focus(&self.settings_focus_handle, cx);
        cx.notify();
    }

    /// Ordinary leave request (back button, Escape): any held drop belongs to
    /// an earlier request and is cancelled.
    fn request_leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_settings_drop = None;
        self.leave_settings(window, cx);
    }

    pub(crate) fn cancel_pending_settings_drop(&mut self) {
        self.pending_settings_drop = None;
    }

    pub(super) fn leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.settings_category {
            SettingsCategory::Connection if self.ai_settings.is_dirty(cx) => {
                self.ai_settings
                    .confirm_leave(ai_settings::LeaveTarget::Close);
                cx.notify();
            }
            SettingsCategory::Prompts if self.ai_prompts.is_dirty(cx) => {
                self.ai_prompts.confirm_leave(ai_prompts::LeaveTarget::Close);
                cx.notify();
            }
            SettingsCategory::Network if self.network_settings.is_busy() => {
                self.status =
                    Some("ネットワーク設定を保存しています。完了してから移動してください".to_owned());
                cx.notify();
            }
            SettingsCategory::Network if self.network_settings.is_dirty(cx) => {
                self.network_settings
                    .confirm_leave(network_settings::LeaveTarget::Close);
                cx.notify();
            }
            _ => self.close_settings(window, cx),
        }
    }

    pub(super) fn request_settings_category(
        &mut self,
        category: SettingsCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_category != category {
            self.pending_settings_drop = None;
            match self.settings_category {
                SettingsCategory::Connection if self.ai_settings.is_dirty(cx) => {
                    self.ai_settings
                        .confirm_leave(ai_settings::LeaveTarget::Category(category));
                    cx.notify();
                    return;
                }
                SettingsCategory::Prompts if self.ai_prompts.is_dirty(cx) => {
                    self.ai_prompts
                        .confirm_leave(ai_prompts::LeaveTarget::Category(category));
                    cx.notify();
                    return;
                }
                SettingsCategory::Network if self.network_settings.is_busy() => {
                    self.status = Some(
                        "ネットワーク設定を保存しています。完了してから移動してください".to_owned(),
                    );
                    cx.notify();
                    return;
                }
                SettingsCategory::Network if self.network_settings.is_dirty(cx) => {
                    self.network_settings
                        .confirm_leave(network_settings::LeaveTarget::Category(category));
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        self.select_settings_category(category, window, cx);
    }

    fn set_file_context_menu(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.file_context_menu_busy {
            return;
        }
        if matches!(
            self.file_context_menu_state,
            FileContextMenuState::Unsupported | FileContextMenuState::Conflict
        ) {
            return;
        }
        self.file_context_menu_busy = true;
        self.file_context_menu_generation = self.file_context_menu_generation.wrapping_add(1);
        let generation = self.file_context_menu_generation;
        self.settings_error = None;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let (result, state) = cx
                .background_executor()
                .spawn(async move {
                    let result = context_menu::set_file_context_menu(enabled);
                    let state = context_menu::current_file_context_menu_state();
                    (result, state)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                if !view.settings_open || view.file_context_menu_generation != generation {
                    return;
                }
                view.file_context_menu_busy = false;
                view.file_context_menu_state = state;
                if let Err(error) = result {
                    view.settings_error = Some(error.to_string());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn file_context_menu_status(&self) -> &'static str {
        match &self.file_context_menu_state {
            FileContextMenuState::NotChecked => "状態を確認しています…",
            FileContextMenuState::Unsupported => "この設定はWindowsで利用できます。",
            FileContextMenuState::Unregistered => "未登録",
            FileContextMenuState::Registered => "登録済み",
            FileContextMenuState::Stale => {
                "Explorer拡張が未導入、または以前のHane.exeを指しています。ONにして更新できます。"
            }
            FileContextMenuState::Conflict => "別の登録内容があるため変更していません。",
            FileContextMenuState::Unknown(_) => {
                "登録状態を確認できません。もう一度お試しください。"
            }
        }
    }

    pub(super) fn settings_screen_element(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if let Some(target) = self.ai_settings.take_ready_route(cx) {
            match target {
                ai_settings::LeaveTarget::Category(category) => {
                    self.select_settings_category(category, window, cx)
                }
                ai_settings::LeaveTarget::Close => {
                    self.close_settings(window, cx);
                }
            }
        }
        if let Some(target) = self.ai_prompts.take_ready_route(cx) {
            match target {
                ai_prompts::LeaveTarget::Category(category) => {
                    self.select_settings_category(category, window, cx)
                }
                ai_prompts::LeaveTarget::Close => {
                    self.close_settings(window, cx);
                }
            }
        }
        if let Some(target) = self.network_settings.take_ready_route(cx) {
            match target {
                network_settings::LeaveTarget::Category(category) => {
                    self.select_settings_category(category, window, cx)
                }
                network_settings::LeaveTarget::Close => {
                    self.close_settings(window, cx);
                }
            }
        }
        let body = match self.settings_category {
            SettingsCategory::General => self.general_settings_content(cx).into_any_element(),
            SettingsCategory::Network => {
                self.network_settings_render(window, cx).into_any_element()
            }
            SettingsCategory::Connection => self.ai_settings_render(window, cx).into_any_element(),
            SettingsCategory::Prompts => self.ai_prompts_render(window, cx).into_any_element(),
        };
        let narrow = settings_layout_is_narrow(
            f32::from(window.viewport_size().width),
            self.sidebar_width,
        );
        self.settings_screen_shell(body, narrow, cx)
    }

    // Build the page before allocating the settings shell's style temporaries.
    // Both frames must not be live together on Windows' 1 MiB UI thread stack.
    #[inline(never)]
    fn settings_screen_shell(
        &self,
        body: gpui::AnyElement,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let view = cx.entity();
        let back = Button::new("settings-back")
            .icon(IconName::ArrowLeft)
            .label("アプリに戻る")
            .ghost()
            .tooltip("アプリに戻る")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.request_leave_settings(window, cx));
            });

        let view = cx.entity();
        let general_tab = Button::new("settings-category-general")
            .label("一般")
            .ghost()
            .selected(self.settings_category == SettingsCategory::General)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.request_settings_category(SettingsCategory::General, window, cx)
                });
            });
        let view = cx.entity();
        let ai_tab = Button::new("settings-category-ai")
            .label("AI")
            .ghost()
            .selected(self.settings_category == SettingsCategory::Connection)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.request_settings_category(SettingsCategory::Connection, window, cx)
                });
            });
        let view = cx.entity();
        let ai_prompts_tab = Button::new("settings-category-ai-prompts")
            .label("AIプロンプト")
            .ghost()
            .selected(self.settings_category == SettingsCategory::Prompts)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.request_settings_category(SettingsCategory::Prompts, window, cx)
                });
            });

        let view = cx.entity();
        let network_tab = Button::new("settings-category-network")
            .label("ネットワーク")
            .ghost()
            .selected(self.settings_category == SettingsCategory::Network)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.request_settings_category(SettingsCategory::Network, window, cx)
                });
            });

        let content = div()
            .id("settings-content")
            .flex_1()
            .min_w(px(0.0))
            .h_full();
        let content = if matches!(
            self.settings_category,
            SettingsCategory::General | SettingsCategory::Network
        ) {
            content.overflow_y_scroll().child(body)
        } else {
            content
                .flex()
                .flex_col()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(body)
        };

        let root = div()
            .size_full()
            .flex()
            .bg(rgb(self.theme.editor_background))
            .text_color(rgb(self.theme.foreground))
            .key_context("HaneEditor")
            .track_focus(&self.settings_focus_handle);
        let root = if narrow { root.flex_col() } else { root.flex_row() };
        let sidebar = div()
            .id("settings-sidebar")
            .debug_selector(|| "settings-sidebar".to_owned())
            .flex_none()
            .flex()
            .gap_4()
            .px(px(16.0))
            .py(px(16.0))
            .bg(rgb(self.theme.sidebar_background));
        // In a narrow window the navigation becomes a wrapping row above the
        // content, so the content keeps the full width instead of being
        // squeezed out of view beside a fixed-width column.
        let sidebar = if narrow {
            sidebar.w_full().flex_row().flex_wrap()
        } else {
            sidebar.w(px(self.sidebar_width)).h_full().flex_col()
        };
        let sidebar = sidebar
            .child(back)
            .child(general_tab)
            .child(ai_tab)
            .child(ai_prompts_tab)
            .child(network_tab);
        let content = if narrow {
            content.h_auto().min_h(px(0.0))
        } else {
            content
        };
        install_action_listeners(root.child(sidebar).child(content), cx)
    }

    fn general_settings_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_sort = self.settings.sidebar_sort;
        let sidebar_sort_view = cx.entity();
        let sidebar_sort_buttons = WorkFolderSortOrder::all().map(|order| {
            let view = sidebar_sort_view.clone();
            Button::new(format!("settings-sidebar-sort-{}", order.as_str()))
                .label(order.label())
                .ghost()
                .selected(sidebar_sort == order)
                .on_click(move |_, _window, app| {
                    view.update(app, |view, cx| {
                        view.set_sidebar_sort(order, cx);
                    });
                })
        });
        let unsupported = matches!(
            self.file_context_menu_state,
            FileContextMenuState::Unsupported
        );
        let checked = self.file_context_menu_state.is_checked();
        let busy = self.file_context_menu_busy;
        let view = cx.entity();
        let checkbox = Checkbox::new("settings-file-context-menu")
            .checked(checked)
            .disabled(unsupported || busy)
            .label("ファイルを右クリックでHaneで開けるよう登録")
            .on_click(move |checked, _window, app| {
                view.update(app, |view, cx| view.set_file_context_menu(*checked, cx));
            });
        let error = self.settings_error.as_ref().map(|error| {
            div()
                .id("settings-file-context-menu-error")
                .text_color(rgb(0xb42318))
                .child(format!("登録に失敗しました: {error}"))
        });
        div()
            .id("general-settings-content")
            .w_full()
            .max_w(px(760.0))
            .px(px(32.0))
            .py(px(28.0))
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("一般"),
            )
            .child(self.display_and_save_settings(cx))
            .child(self.default_folder_card(cx))
            .children(self.app_update_card(cx))
            .child(
                div()
                    .pt(px(16.0))
                    .border_t_1()
                    .border_color(rgb(self.theme.sidebar_active_background))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Windowsとの連携"),
                    )
                    .child(
                        div()
                            .id("settings-file-context-menu-card")
                            .w_full()
                            .px(px(16.0))
                            .py(px(14.0))
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(self.theme.sidebar_active_background))
                            .bg(rgb(self.theme.code_background))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(checkbox)
                            .child(
                                div()
                                    .pl(px(28.0))
                                    .text_color(rgb(self.theme.quote_foreground))
                                    .child("エクスプローラーのファイルの右クリックメニューに「Haneで開く」を追加します。既定のアプリは変更しません。Windows 11では署名済みのExplorer拡張パッケージが必要です。"),
                            )
                            .child(
                                div()
                                    .pl(px(28.0))
                                    .text_color(rgb(self.theme.quote_foreground))
                                    .child(if busy {
                                        "反映しています…"
                                    } else {
                                        self.file_context_menu_status()
                                    }),
                            )
                            .children(error),
                    ),
            )
            .child(
                div()
                    .pt(px(16.0))
                    .border_t_1()
                    .border_color(rgb(self.theme.sidebar_active_background))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("サイドバー"),
                    )
                    .child(
                        div()
                            .id("settings-sidebar-sort-card")
                            .w_full()
                            .px(px(16.0))
                            .py(px(14.0))
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(self.theme.sidebar_active_background))
                            .bg(rgb(self.theme.code_background))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(self.theme.quote_foreground))
                                    .child("ファイル・フォルダーの並び順"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_wrap()
                                    .gap_2()
                                    .children(sidebar_sort_buttons),
                            ),
                    ),
            )
    }

    /// The "表示" and "保存" sections of the general settings: the theme and
    /// autosave controls that used to live in the footer. Both apply and are
    /// stored the moment they change, through `set_theme` / `set_autosave`.
    #[inline(never)]
    fn display_and_save_settings(&self, cx: &mut Context<Self>) -> gpui::Div {
        let current_theme = self.settings.theme;
        let theme_view = cx.entity();
        let theme_buttons = [
            (ThemePreference::System, "システムに合わせる", "system"),
            (ThemePreference::Light, "ライト", "light"),
            (ThemePreference::Dark, "ダーク", "dark"),
        ]
        .map(|(preference, label, key)| {
            let view = theme_view.clone();
            Button::new(format!("settings-theme-{key}"))
                .label(label)
                .ghost()
                .selected(current_theme == preference)
                .on_click(move |_, window, app| {
                    view.update(app, |view, cx| view.set_theme(preference, window, cx));
                })
        });
        let autosave_view = cx.entity();
        let autosave = Checkbox::new("settings-autosave")
            .checked(self.settings.autosave)
            .label("変更を自動保存する")
            .on_click(move |checked, _window, app| {
                autosave_view.update(app, |view, cx| view.set_autosave(*checked, cx));
            });
        let plain_text_view = cx.entity();
        let plain_text = Checkbox::new("settings-plain-text-mode")
            .checked(self.plain_text())
            .label("プレーンテキストで表示・編集する（Markdownの記号を隠さない）")
            .on_click(move |checked, _window, app| {
                plain_text_view.update(app, |view, cx| {
                    view.set_plain_text(*checked, cx);
                });
            });
        // Independent of the plain-text switch, so it stays usable while the
        // Markdown display is shown.
        let syntax_highlight_view = cx.entity();
        let syntax_highlight = Checkbox::new("settings-syntax-highlight")
            .checked(self.syntax_highlight())
            .label("プレーンテキスト表示で Markdown 記法を色分けする")
            .on_click(move |checked, _window, app| {
                syntax_highlight_view.update(app, |view, cx| {
                    view.set_syntax_highlight(*checked, cx);
                });
            });
        let paper_texture_view = cx.entity();
        let paper_texture = Checkbox::new("settings-paper-texture")
            .checked(self.paper_texture_enabled())
            .label("背景に紙・壁の質感を敷く")
            .on_click(move |checked, _window, app| {
                paper_texture_view.update(app, |view, cx| {
                    view.set_paper_texture(*checked, cx);
                });
            });
        // The strength only matters while the texture is on.
        let paper_texture_on = self.paper_texture_enabled();
        let current_texture_strength = self.paper_texture_strength();
        let texture_strength_view = cx.entity();
        let texture_strength_buttons = [
            (PaperTextureStrength::Weak, "弱"),
            (PaperTextureStrength::Standard, "標準"),
            (PaperTextureStrength::Strong, "強"),
        ]
        .map(|(strength, label)| {
            let view = texture_strength_view.clone();
            Button::new(format!("settings-paper-texture-strength-{}", strength.as_str()))
                .label(label)
                .ghost()
                .disabled(!paper_texture_on)
                .selected(current_texture_strength == strength)
                .on_click(move |_, _window, app| {
                    view.update(app, |view, cx| {
                        view.set_paper_texture_strength(strength, cx);
                    });
                })
        });
        let drying_ink_view = cx.entity();
        let drying_ink = Checkbox::new("settings-drying-ink")
            .checked(self.drying_ink_enabled())
            .label("確定した文字を、乾くまで少し濃く表示する")
            .on_click(move |checked, _window, app| {
                drying_ink_view.update(app, |view, cx| {
                    view.set_drying_ink(*checked, cx);
                });
            });
        // The band and the tuning only matter while the master switch is on.
        let drying_ink_on = self.drying_ink_enabled();
        let band_view = cx.entity();
        let drying_ink_band = Checkbox::new("settings-drying-ink-band")
            .checked(self.drying_ink_band_enabled())
            .disabled(!drying_ink_on)
            .label("句点などで文を区切ったとき、直前の文字の背後に一瞬だけ帯を出す")
            .on_click(move |checked, _window, app| {
                band_view.update(app, |view, cx| {
                    view.set_drying_ink_band(*checked, cx);
                });
            });
        let soak_view = cx.entity();
        let drying_ink_soak = Checkbox::new("settings-drying-ink-soak")
            .checked(self.drying_ink_soak_enabled())
            .disabled(!drying_ink_on)
            .label("確定した文字の背後を、乾くまで紙が濡れたように少し沈ませる（質感と組み合わせると見えやすい）")
            .on_click(move |checked, _window, app| {
                soak_view.update(app, |view, cx| {
                    view.set_drying_ink_soak(*checked, cx);
                });
            });
        let current_speed = self.drying_ink_speed();
        let speed_view = cx.entity();
        let speed_buttons = [
            (DryingInkSpeed::Fast, "速い（20秒）"),
            (DryingInkSpeed::Standard, "標準（60秒）"),
            (DryingInkSpeed::Slow, "ゆっくり（180秒）"),
        ]
        .map(|(speed, label)| {
            let view = speed_view.clone();
            Button::new(format!("settings-drying-ink-speed-{}", speed.as_str()))
                .label(label)
                .ghost()
                .disabled(!drying_ink_on)
                .selected(current_speed == speed)
                .on_click(move |_, _window, app| {
                    view.update(app, |view, cx| view.set_drying_ink_speed(speed, cx));
                })
        });
        let current_strength = self.drying_ink_strength();
        let strength_view = cx.entity();
        let strength_buttons = [
            (DryingInkStrength::Weak, "弱"),
            (DryingInkStrength::Standard, "標準"),
            (DryingInkStrength::Strong, "強"),
        ]
        .map(|(strength, label)| {
            let view = strength_view.clone();
            Button::new(format!("settings-drying-ink-strength-{}", strength.as_str()))
                .label(label)
                .ghost()
                .disabled(!drying_ink_on)
                .selected(current_strength == strength)
                .on_click(move |_, _window, app| {
                    view.update(app, |view, cx| view.set_drying_ink_strength(strength, cx));
                })
        });
        // Offered only where the link exists (the app attached a controller).
        let link_section = self.whisparrot_link_available().then(|| {
            let link_view = cx.entity();
            let link = Checkbox::new("settings-whisparrot-link")
                .checked(self.whisparrot_link_enabled())
                .label("whisparrot とのローカル連携を許可する")
                .on_click(move |checked, _window, app| {
                    link_view.update(app, |view, cx| {
                        view.set_whisparrot_link(*checked, cx);
                    });
                });
            (link, self.theme.quote_foreground)
        });
        let store_error = self.settings_store_error.as_ref().map(|error| {
            div()
                .id("settings-store-error")
                .debug_selector(|| "settings-store-error".to_owned())
                .text_color(rgb(0xb42318))
                .child(format!(
                    "設定を保存できませんでした: {error}。この起動中は反映されていますが、次回起動時に戻る可能性があります。"
                ))
        });
        let section_title = |title: &'static str| {
            div()
                .text_size(px(14.0))
                .font_weight(gpui::FontWeight::BOLD)
                .child(title)
        };
        let card = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(|| id.to_owned())
                .w_full()
                .px(px(16.0))
                .py(px(14.0))
                .rounded_sm()
                .border_1()
                .border_color(rgb(self.theme.sidebar_active_background))
                .bg(rgb(self.theme.code_background))
                .flex()
                .flex_col()
                .gap_2()
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_title("表示"))
                    .child(
                        card("settings-theme-card")
                            .child(
                                div()
                                    .text_color(rgb(self.theme.quote_foreground))
                                    .child("テーマ"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_wrap()
                                    .gap_2()
                                    .children(theme_buttons),
                            ),
                    )
                    .child(
                        card("settings-paper-texture-card")
                            .child(paper_texture)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .when(!paper_texture_on, |element| element.opacity(0.5))
                                    .child(
                                        div()
                                            .text_color(rgb(self.theme.quote_foreground))
                                            .child("質感の強さ"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .flex_wrap()
                                            .gap_2()
                                            .children(texture_strength_buttons),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(rgb(self.theme.quote_foreground))
                                    .child("本文の背景だけに敷きます。サイドバーやタブには敷きません。"),
                            ),
                    )
                    .child(
                        card("settings-plain-text-card")
                            .child(plain_text)
                            .child(syntax_highlight),
                    )
                    .child(
                        card("settings-drying-ink-card")
                            .child(drying_ink)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .when(!drying_ink_on, |element| element.opacity(0.5))
                                    .child(drying_ink_band)
                                    .child(drying_ink_soak)
                                    .child(
                                        div()
                                            .text_color(rgb(self.theme.quote_foreground))
                                            .child("乾く速さ"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .flex_wrap()
                                            .gap_2()
                                            .children(speed_buttons),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(self.theme.quote_foreground))
                                            .child("濃さ"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .flex_wrap()
                                            .gap_2()
                                            .children(strength_buttons),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(rgb(self.theme.quote_foreground))
                                    .child("文書には保存されません。タブの切り替えや再起動で消えます。"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_title("保存"))
                    .child(card("settings-autosave-card").child(autosave).children(store_error)),
            )
            .children(link_section.map(|(link, note_color)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_title("連携"))
                    .child(card("settings-whisparrot-link-card").child(link).child(
                        div().text_color(rgb(note_color)).child(
                            "音声入力アプリ whisparrot が、開いている文書のカーソル付近の文章・\
                             タイトル・Front Matter を読み取り、認識結果を文書へ挿入・差し替えできるようになります。\
                             通信はこのパソコン内の専用の接続に限られ、既定ではオフです。",
                        ),
                    ))
            }))
    }

    /// The "デフォルトのフォルダ" card: the saved default folder (or 未設定)
    /// with one button that opens the folder dialog. A long path is
    /// ellipsized on one line and shown in full on hover.
    #[inline(never)]
    fn default_folder_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let picking = self.default_folder_picking;
        let saved = self
            .settings
            .default_folder
            .as_ref()
            .map(|path| path.display().to_string());
        let button_label = if saved.is_some() { "変更…" } else { "選択…" };
        let button = Button::new("settings-default-folder-button")
            .label(button_label)
            .ghost()
            .disabled(picking)
            .on_click(move |_, _window, app| {
                view.update(app, |view, cx| view.prompt_settings_default_folder(cx));
            });
        let value = match saved {
            Some(full_path) => {
                let trigger = div()
                    .debug_selector(|| "settings-default-folder-value".to_owned())
                    .w_full()
                    .min_w_0()
                    .truncate()
                    .child(full_path.clone());
                HoverCard::new("settings-default-folder-path")
                    .anchor(Anchor::TopCenter)
                    .trigger(trigger)
                    .content(move |_, _, _| {
                        div()
                            .max_w(px(560.0))
                            .whitespace_normal()
                            .child(full_path.clone())
                    })
                    .into_any_element()
            }
            None => div()
                .debug_selector(|| "settings-default-folder-value".to_owned())
                .w_full()
                .min_w_0()
                .truncate()
                .child("未設定")
                .into_any_element(),
        };
        let error = self.default_folder_error.as_ref().map(|error| {
            div()
                .id("settings-default-folder-error")
                .debug_selector(|| "settings-default-folder-error".to_owned())
                .text_color(rgb(0xb42318))
                .child(format!("フォルダを選択できませんでした（{error}）。"))
        });
        div()
            .id("settings-default-folder-card")
            .debug_selector(|| "settings-default-folder-card".to_owned())
            .w_full()
            .px(px(16.0))
            .py(px(14.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.sidebar_active_background))
            .bg(rgb(self.theme.code_background))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(rgb(self.theme.quote_foreground))
                    .child("デフォルトのフォルダ"),
            )
            .child(
                div()
                    .text_color(rgb(self.theme.quote_foreground))
                    .child("次回の通常起動時に、このフォルダを開きます。"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(120.0))
                            .overflow_hidden()
                            .child(value),
                    )
                    .child(
                        div()
                            .flex_none()
                            .debug_selector(|| "settings-default-folder-button".to_owned())
                            .child(button),
                    ),
            )
            .children(error)
    }

    /// Opens the folder-only, single-choice dialog for the default folder.
    /// It never switches the current work folder; the result is handled by
    /// `finish_settings_default_folder_prompt`.
    pub(crate) fn prompt_settings_default_folder(&mut self, cx: &mut Context<Self>) {
        if self.default_folder_picking {
            return;
        }
        self.default_folder_picking = true;
        self.default_folder_error = None;
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("デフォルトのフォルダを選択".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = match receiver.await {
                Ok(Ok(paths)) => Ok(paths),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = view.update(cx, |view, cx| {
                view.finish_settings_default_folder_prompt(result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Applies the dialog result: a chosen folder becomes the saved default
    /// (stored through `store_settings`, the settings screen stays open and
    /// the work folder is untouched); cancel and an empty result write
    /// nothing; a dialog failure is shown inside the card.
    pub(crate) fn finish_settings_default_folder_prompt(
        &mut self,
        result: Result<Option<Vec<PathBuf>>, String>,
        cx: &mut Context<Self>,
    ) {
        self.default_folder_picking = false;
        match result {
            Ok(paths) => {
                if let Some(path) = paths.and_then(|paths| paths.into_iter().next()) {
                    self.default_folder_error = None;
                    self.settings.default_folder = Some(path);
                    self.store_settings();
                }
            }
            Err(error) => self.default_folder_error = Some(error),
        }
        cx.notify();
    }

    /// Applies and stores the chosen theme preference. Choosing the current
    /// value again changes nothing, so no cache is rebuilt for it.
    pub(crate) fn set_theme(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.theme == preference {
            return;
        }
        self.settings.theme = preference;
        self.theme = resolve_theme(self.settings.theme, window.appearance());
        sync_component_theme(self.settings.theme, window.appearance(), window, cx);
        self.block_cache.clear();
        self.layout_cache.clear();
        self.heights = HeightIndex::new(self.item_heights());
        self.store_settings();
        cx.notify();
    }

    /// Changes how the work-folder sidebar orders files and folders,
    /// re-sorting the already-scanned tree in place (no rescan) so the
    /// change is visible immediately, and persists the choice the same way
    /// `set_theme`/`set_autosave` do. Selection, the active session,
    /// expanded folders, and any unsaved content are all addressed by path
    /// rather than by position in the sidebar list, so re-sorting the tree
    /// here never disturbs any of them.
    pub(crate) fn set_sidebar_sort(&mut self, order: WorkFolderSortOrder, cx: &mut Context<Self>) {
        if self.settings.sidebar_sort == order {
            return;
        }
        self.settings.sidebar_sort = order;
        if let Some(folder) = self.work_folder.as_mut() {
            folder.set_sort_order(order);
        }
        self.store_settings();
        cx.notify();
    }
}
