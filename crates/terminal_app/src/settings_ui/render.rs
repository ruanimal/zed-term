use super::*;

impl SettingsPage {
    fn text_input_content(
        &self,
        value: &str,
        placeholder: &'static str,
        active: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if !active {
            return Label::new(if value.is_empty() {
                placeholder.to_string()
            } else {
                value.to_string()
            })
            .color(if value.is_empty() {
                Color::Muted
            } else {
                Color::Default
            })
            .into_any_element();
        }

        let text_length = value.encode_utf16().count();
        let selection_start = self.editing_selection.start.min(text_length);
        let selection_end = self
            .editing_selection
            .end
            .min(text_length)
            .max(selection_start);
        let mut content = h_flex().items_center().min_w_0().flex_1();
        let cursor = div().w(px(1.)).h_4().bg(cx.theme().colors().border_focused);

        if selection_start == selection_end {
            let before = substring_utf16(value, 0..selection_start);
            let after = substring_utf16(value, selection_start..text_length);
            if before.is_empty() && after.is_empty() {
                content = content.child(Label::new(placeholder).color(Color::Muted));
            } else {
                content = content.child(Label::new(before));
            }
            content = content.child(cursor).child(Label::new(after));
        } else {
            let before = substring_utf16(value, 0..selection_start);
            let selected = substring_utf16(value, selection_start..selection_end);
            let after = substring_utf16(value, selection_end..text_length);
            content = content
                .child(Label::new(before))
                .child(
                    div()
                        .bg(cx.theme().colors().ghost_element_selected)
                        .child(Label::new(selected)),
                )
                .child(Label::new(after));
        }
        content.into_any_element()
    }

    fn text_input(
        &self,
        id: impl Into<gpui::ElementId>,
        field: EditableField,
        value: String,
        placeholder: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_active = self.editing_field.as_ref() == Some(&field);
        let field_for_click = field.clone();
        div()
            .id(id)
            .min_w_48()
            .max_w_96()
            .h_8()
            .px_2()
            .flex()
            .items_center()
            .rounded_md()
            .cursor(CursorStyle::IBeam)
            .border_1()
            .border_color(if is_active {
                cx.theme().colors().border_focused
            } else {
                cx.theme().colors().border
            })
            .bg(cx.theme().colors().editor_background)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.begin_edit(field_for_click.clone(), window, cx);
                }),
            )
            .child(self.text_input_content(&value, placeholder, is_active, cx))
            .into_any_element()
    }

    /// A full-width text field for the Keymap tab's binding filter.
    fn keymap_search_input(&self, value: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        let is_active = self.editing_field.as_ref() == Some(&EditableField::KeymapSearch);
        div()
            .id("keymap-search")
            .debug_selector(|| "keymap-search".to_string())
            .flex_1()
            .min_w_0()
            .h_8()
            .px_2()
            .flex()
            .items_center()
            .rounded_md()
            .cursor(CursorStyle::IBeam)
            .border_1()
            .border_color(if is_active {
                cx.theme().colors().border_focused
            } else {
                cx.theme().colors().border
            })
            .bg(cx.theme().colors().editor_background)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.begin_edit(EditableField::KeymapSearch, window, cx);
                }),
            )
            .child(self.text_input_content(&value, "Filter by action or keystroke…", is_active, cx))
            .into_any_element()
    }

    /// A full-width text field for the Themes tab's extension filter.
    fn themes_search_input(&self, value: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        let is_active = self.editing_field.as_ref() == Some(&EditableField::ThemesSearch);
        div()
            .id("themes-search")
            .debug_selector(|| "themes-search".to_string())
            .flex_1()
            .min_w_0()
            .h_8()
            .px_2()
            .flex()
            .items_center()
            .rounded_md()
            .cursor(CursorStyle::IBeam)
            .border_1()
            .border_color(if is_active {
                cx.theme().colors().border_focused
            } else {
                cx.theme().colors().border
            })
            .bg(cx.theme().colors().editor_background)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.begin_edit(EditableField::ThemesSearch, window, cx);
                }),
            )
            .child(self.text_input_content(&value, "Filter theme extensions…", is_active, cx))
            .into_any_element()
    }

    fn status_row(&self) -> gpui::AnyElement {
        let Some(message) = self.status_message.as_ref() else {
            return div().into_any_element();
        };
        let color = match self.save_status {
            SaveStatus::Idle | SaveStatus::Saving => Color::Muted,
            SaveStatus::Succeeded => Color::Success,
            SaveStatus::Failed => Color::Error,
        };
        div()
            .px_8()
            .py_2()
            .child(
                Label::new(message.clone())
                    .size(LabelSize::Small)
                    .color(color),
            )
            .into_any_element()
    }

    fn row(
        &self,
        title: &'static str,
        description: &'static str,
        control: gpui::AnyElement,
    ) -> gpui::AnyElement {
        let id = element_id_for(title);
        v_flex()
            .id(id.clone())
            .group(format!("setting-item-{id}"))
            .w_full()
            .px_8()
            .py_2()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w_2_3()
                            .min_w_0()
                            .child(Label::new(SharedString::new_static(title)))
                            .child(
                                Label::new(SharedString::new_static(description))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(control),
            )
            .child(Divider::horizontal().color(DividerColor::BorderFaded))
            .into_any_element()
    }

    fn section_header(&self, label: &'static str, _cx: &mut Context<Self>) -> gpui::AnyElement {
        v_flex()
            .id(format!("section-{label}"))
            .w_full()
            .px_8()
            .pt_4()
            .pb_1()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .into_any_element()
    }

    fn enum_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        title: &'static str,
        description: &'static str,
        value: String,
        options: DropdownOptions,
    ) -> gpui::AnyElement {
        let page = cx.weak_entity();
        let menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let mut menu = menu;
            for (option_label, apply) in options {
                let page = page.clone();
                menu = menu.entry(option_label, None, move |window, cx| {
                    page.update(cx, |this, _| this.clear_active_edit())
                        .log_err();
                    apply(window, cx);
                });
            }
            menu
        });
        self.row(
            title,
            description,
            DropdownMenu::new(element_id_for(title), value, menu).into_any_element(),
        )
    }

    fn number_row(
        &self,
        cx: &mut Context<Self>,
        title: &'static str,
        description: &'static str,
        value: String,
        apply: impl Fn(&mut App, f32) + 'static + Clone,
    ) -> gpui::AnyElement {
        let id = element_id_for(title);
        let minus = apply.clone();
        let plus = apply;
        let control = h_flex()
            .items_center()
            .gap_2()
            .child(Label::new(value).color(Color::Muted))
            .child(
                IconButton::new(format!("{id}-minus"), IconName::SquareMinus)
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let minus = minus.clone();
                        cx.defer(move |cx| minus(cx, -1.0));
                    })),
            )
            .child(
                IconButton::new(format!("{id}-plus"), IconName::SquarePlus)
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let plus = plus.clone();
                        cx.defer(move |cx| plus(cx, 1.0));
                    })),
            )
            .into_any_element();
        self.row(title, description, control)
    }

    fn toggle_row(
        &self,
        title: &'static str,
        description: &'static str,
        enabled: bool,
        apply: impl Fn(&mut App) + 'static + Clone,
    ) -> gpui::AnyElement {
        let id = element_id_for(title);
        let control = Switch::new(
            format!("{id}-switch"),
            if enabled {
                ToggleState::Selected
            } else {
                ToggleState::Unselected
            },
        )
        .on_click(move |_, _window, cx| apply(cx))
        .into_any_element();
        self.row(title, description, control)
    }

    fn action_button(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        on_click: impl Fn(&mut Self, &gpui::ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let button = div()
            .id(id)
            .debug_selector(|| id.to_string())
            .px_2()
            .py_1()
            .rounded_md()
            .bg(if enabled {
                cx.theme().colors().element_background
            } else {
                cx.theme().colors().editor_background
            })
            .child(Label::new(label).size(LabelSize::Small).color(if enabled {
                Color::Default
            } else {
                Color::Muted
            }));
        if enabled {
            button.on_click(cx.listener(on_click)).into_any_element()
        } else {
            button.into_any_element()
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn title_bar_action_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: String,
        enabled: bool,
        show_unsaved_indicator: bool,
        show_success: bool,
        on_click: impl Fn(&mut Self, &gpui::ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = cx.theme().colors();
        let label_color = if show_success {
            Color::Success
        } else if enabled {
            Color::Default
        } else {
            Color::Muted
        };
        let button = h_flex()
            .id(id)
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(colors.ghost_element_background)
            .when(show_unsaved_indicator, |this| {
                this.child(
                    Icon::new(IconName::Circle)
                        .size(IconSize::XSmall)
                        .color(Color::Warning),
                )
            })
            .child(Label::new(label).size(LabelSize::Small).color(label_color))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });
        if enabled {
            button
                .cursor_pointer()
                .hover(|style| style.bg(colors.ghost_element_hover))
                .active(|style| style.bg(colors.ghost_element_active))
                .on_click(cx.listener(move |this, event, window, cx| {
                    cx.stop_propagation();
                    on_click(this, event, window, cx);
                }))
                .into_any_element()
        } else {
            button.into_any_element()
        }
    }

    fn theme_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        current: String,
        names: Vec<SharedString>,
    ) -> gpui::AnyElement {
        let page = cx.weak_entity();
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            let mut menu = menu;
            for name in names {
                let page = page.clone();
                menu = menu.entry(name.to_string(), None, move |_window, cx| {
                    page.update(cx, |this, cx| {
                        this.theme_name = name.to_string();
                        this.mark_dirty(DirtySetting::Theme, cx);
                    })
                    .log_err();
                });
            }
            menu
        });
        self.row(
            "Color theme",
            "Choose the color theme for the terminal and UI.",
            DropdownMenu::new(element_id_for("Color theme"), current, menu).into_any_element(),
        )
    }

    fn font_family_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        font_families: Vec<SharedString>,
    ) -> gpui::AnyElement {
        let page = cx.weak_entity();
        let application_default_label =
            format!("Application default ({DEFAULT_TERMINAL_FONT_FAMILY})");
        let menu_application_default_label = application_default_label.clone();
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            let inherited_page = page.clone();
            let mut menu = menu.entry(
                menu_application_default_label.clone(),
                None,
                move |_window, cx| {
                    inherited_page
                        .update(cx, |this, cx| {
                            this.font_family.clear();
                            this.mark_dirty(DirtySetting::FontFamily, cx);
                        })
                        .log_err();
                },
            );
            for font_family in font_families {
                let page = page.clone();
                menu = menu.entry(font_family.to_string(), None, move |_window, cx| {
                    page.update(cx, |this, cx| {
                        this.font_family = font_family.to_string();
                        this.mark_dirty(DirtySetting::FontFamily, cx);
                    })
                    .log_err();
                });
            }
            menu
        });
        self.row(
            "Font family",
            "Choose an installed font, or use the application default.",
            DropdownMenu::new(
                element_id_for("Font family"),
                if self.font_family.is_empty() {
                    application_default_label
                } else {
                    self.font_family.clone()
                },
                menu,
            )
            .into_any_element(),
        )
    }

    fn shell_mode_options(&self, cx: &mut Context<Self>) -> DropdownOptions {
        [
            ("system", ShellMode::System),
            ("custom program", ShellMode::Program),
            ("program with arguments", ShellMode::WithArguments),
        ]
        .into_iter()
        .map(|(label, mode)| {
            let page = cx.weak_entity();
            opt(label, move |window, cx| {
                page.update(cx, |this, cx| {
                    this.shell_form.mode = mode;
                    if mode == ShellMode::System {
                        this.clear_active_edit();
                    } else {
                        this.begin_edit(EditableField::ShellProgram, window, cx);
                    }
                    this.mark_dirty(DirtySetting::Shell, cx);
                })
                .log_err();
            })
        })
        .collect()
    }

    fn render_shell_form(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mode = match self.shell_form.mode {
            ShellMode::System => "system",
            ShellMode::Program => "custom program",
            ShellMode::WithArguments => "program with arguments",
        };
        let shell_mode_options = self.shell_mode_options(cx);
        let mut form = v_flex().id("shell-form").child(self.enum_row(
            window,
            cx,
            "Shell mode",
            "Choose the system shell or a structured program and argument list. Applies only to newly created terminals and panes.",
            mode.to_string(),
            shell_mode_options,
        ));
        if self.shell_form.mode != ShellMode::System {
            form = form.child(self.row(
                "Shell program",
                "The executable to run for each new terminal.",
                self.text_input(
                    "shell-program-input",
                    EditableField::ShellProgram,
                    self.shell_form.program.clone(),
                    "e.g. /bin/zsh",
                    cx,
                ),
            ));
        }
        if self.shell_form.mode == ShellMode::WithArguments {
            form = form.child(self.row(
                "Shell title override",
                "Optional title used for tabs started with this shell.",
                self.text_input(
                    "shell-title-input",
                    EditableField::ShellTitleOverride,
                    self.shell_form.title_override.clone(),
                    "Optional title",
                    cx,
                ),
            ));
            for (index, argument) in self.shell_form.arguments.iter().enumerate() {
                let remove_index = index;
                form = form.child(
                    h_flex()
                        .id(("shell-argument", index))
                        .px_8()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .child(Label::new(format!("Argument {}", index + 1)).color(Color::Muted))
                        .child(self.text_input(
                            format!("shell-argument-input-{index}"),
                            EditableField::ShellArgument(index),
                            argument.clone(),
                            "Argument",
                            cx,
                        ))
                        .child(
                            IconButton::new(
                                format!("remove-shell-argument-{index}"),
                                IconName::Close,
                            )
                            .icon_size(IconSize::XSmall)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if remove_index < this.shell_form.arguments.len() {
                                        this.clear_active_edit();
                                        this.shell_form.arguments.remove(remove_index);
                                        this.mark_dirty(DirtySetting::Shell, cx);
                                    }
                                },
                            )),
                        ),
                );
            }
            form = form.child(h_flex().px_8().py_2().child(self.action_button(
                "add-shell-argument",
                "Add argument",
                true,
                |this, _, window, cx| {
                    let index = this.shell_form.arguments.len();
                    this.shell_form.arguments.push(String::new());
                    this.begin_edit(EditableField::ShellArgument(index), window, cx);
                    this.mark_dirty(DirtySetting::Shell, cx);
                },
                cx,
            )));
        }
        form.into_any_element()
    }

    fn render_environment_editor(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut editor = v_flex().id("environment-editor");
        for (index, variable) in self.environment.iter().enumerate() {
            let remove_index = index;
            editor = editor.child(
                h_flex()
                    .id(("environment-row", index))
                    .px_8()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(self.text_input(
                        format!("environment-key-{index}"),
                        EditableField::EnvironmentKey(index),
                        variable.key.clone(),
                        "NAME",
                        cx,
                    ))
                    .child(Label::new("=").color(Color::Muted))
                    .child(self.text_input(
                        format!("environment-value-{index}"),
                        EditableField::EnvironmentValue(index),
                        variable.value.clone(),
                        "Value",
                        cx,
                    ))
                    .child(
                        IconButton::new(format!("remove-environment-{index}"), IconName::Close)
                            .icon_size(IconSize::XSmall)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if remove_index < this.environment.len() {
                                    this.clear_active_edit();
                                    this.environment.remove(remove_index);
                                    this.mark_dirty(DirtySetting::Environment, cx);
                                }
                            })),
                    ),
            );
        }
        editor
            .child(h_flex().px_8().py_2().child(self.action_button(
                "add-environment-variable",
                "Add variable",
                true,
                |this, _, window, cx| {
                    let index = this.environment.len();
                    this.environment.push(EnvironmentVariable {
                        key: String::new(),
                        value: String::new(),
                    });
                    this.begin_edit(EditableField::EnvironmentKey(index), window, cx);
                    this.mark_dirty(DirtySetting::Environment, cx);
                },
                cx,
            )))
            .into_any_element()
    }

    fn working_directory_options(&self, cx: &mut Context<Self>) -> DropdownOptions {
        [
            ("home", WorkingDirectoryMode::Home),
            ("previous tab", WorkingDirectoryMode::PreviousTab),
            ("fixed directory", WorkingDirectoryMode::Fixed),
        ]
        .into_iter()
        .map(|(label, mode)| {
            let page = cx.weak_entity();
            opt(label, move |window, cx| {
                page.update(cx, |this, cx| {
                    this.working_directory.mode = mode;
                    if mode == WorkingDirectoryMode::Fixed {
                        this.begin_edit(EditableField::WorkingDirectory, window, cx);
                    } else {
                        this.clear_active_edit();
                    }
                    this.mark_dirty(DirtySetting::WorkingDirectory, cx);
                })
                .log_err();
            })
        })
        .collect()
    }

    fn render_working_directory_form(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mode = match self.working_directory.mode {
            WorkingDirectoryMode::Home => "home",
            WorkingDirectoryMode::PreviousTab => "previous tab",
            WorkingDirectoryMode::Fixed => "fixed directory",
        };
        let description = if self.working_directory.fell_back_from_workspace_setting {
            "Newly created terminals and panes use Home when an inherited workspace directory is configured."
        } else if self.working_directory.mode == WorkingDirectoryMode::PreviousTab {
            "New tabs inherit the active tab's working directory when available; otherwise they use Home."
        } else {
            "Only newly created terminals and panes use the selected Home or verified fixed directory."
        };
        let working_directory_options = self.working_directory_options(cx);
        let mut form = v_flex().id("working-directory-form").child(self.enum_row(
            window,
            cx,
            "Working directory",
            description,
            mode.to_string(),
            working_directory_options,
        ));
        if self.working_directory.mode == WorkingDirectoryMode::Fixed {
            form = form.child(self.row(
                "Fixed working directory",
                "An existing directory. `~` is expanded before it is saved.",
                self.text_input(
                    "working-directory-input",
                    EditableField::WorkingDirectory,
                    self.working_directory.directory.clone(),
                    "/Users/name/project",
                    cx,
                ),
            ));
        }
        form.into_any_element()
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.draft_settings.clone();
        let font_size = settings.font_size.map(f32::from);
        let cursor_shape = format!("{:?}", settings.cursor_shape).to_lowercase();
        let blinking = match settings.blinking {
            settings::TerminalBlink::On => "on",
            settings::TerminalBlink::Off => "off",
            settings::TerminalBlink::TerminalControlled => "terminal-controlled",
        };
        let line_height = match settings.line_height {
            settings::TerminalLineHeight::Comfortable => "comfortable".to_string(),
            settings::TerminalLineHeight::Standard => "standard".to_string(),
            settings::TerminalLineHeight::Custom(value) => format!("custom ({value:.2})"),
        };
        let line_height_custom = match settings.line_height {
            settings::TerminalLineHeight::Custom(value) => Some(value),
            _ => None,
        };
        let font_weight = settings.font_weight.map(|weight| weight.0);
        let scrollbar_show = settings
            .scrollbar
            .show
            .map(|show| format!("{:?}", show).to_lowercase())
            .unwrap_or_else(|| "auto".to_string());
        let alternate_scroll = format!("{:?}", settings.alternate_scroll).to_lowercase();
        let bell = format!("{:?}", settings.bell).to_lowercase();
        let current_theme = self.theme_name.clone();
        let page = cx.weak_entity();

        let title_bar = self.render_title_bar(window, cx);
        // `render` runs on every keystroke, so the Terminal tab's large tree is
        // built lazily and only while that tab is visible.
        let terminal_content = |cx: &mut Context<Self>| {
            // Enumerating themes and font families is comparatively expensive, so
            // it happens only when this tab is actually built. `render` runs on
            // every keystroke.
            let theme_names = ThemeRegistry::global(cx).list_names();
            let font_families = FontFamilyCache::global(cx)
                .try_list_font_families()
                .unwrap_or_default();
            v_flex()
            .id("settings-content")
            .w_full()
            .child(self.status_row())
            .child(self.section_header("Appearance", cx))
            .child(self.theme_row(window, cx, current_theme.clone(), theme_names))
            .child(self.font_family_row(window, cx, font_families))
            .child(self.number_row(
                cx,
                "Font size",
                "The size of the terminal font. The default is the application terminal font size.",
                font_size.map_or_else(
                    || format!("Application default ({DEFAULT_TERMINAL_FONT_SIZE:.1} px)"),
                    |value| format!("{value:.1} px"),
                ),
                {
                    let page = page.clone();
                    move |cx, delta| {
                        page.update(cx, |this, cx| {
                            let current = this
                                .draft_settings
                                .font_size
                                .map(f32::from)
                                .unwrap_or(DEFAULT_TERMINAL_FONT_SIZE);
                            this.update_draft(
                                DirtySetting::FontSize,
                                |settings| {
                                    settings.font_size =
                                        Some(px(adjust_bounded(current, delta, 1.0, f32::MAX)))
                                },
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(self.number_row(
                cx,
                "Font weight",
                "CSS font weight from 100 to 900. The default uses the normal font weight.",
                font_weight.map_or_else(
                    || "400 (default)".to_string(),
                    |value| format!("{value:.0}"),
                ),
                {
                    let page = page.clone();
                    move |cx, delta| {
                        page.update(cx, |this, cx| {
                            let current = this
                                .draft_settings
                                .font_weight
                                .map(|weight| weight.0)
                                .unwrap_or(400.0);
                            this.update_draft(
                                DirtySetting::FontWeight,
                                |settings| {
                                    settings.font_weight = Some(gpui::FontWeight(adjust_bounded(
                                        current,
                                        delta * 100.0,
                                        100.0,
                                        900.0,
                                    )));
                                },
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(self.enum_row(
                window,
                cx,
                "Line height",
                "Standard is the standalone default and works best with terminal UIs.",
                line_height,
                vec![
                    draft_setting_option(
                        "standard",
                        page.clone(),
                        DirtySetting::LineHeight,
                        |settings| settings.line_height = settings::TerminalLineHeight::Standard,
                    ),
                    draft_setting_option(
                        "comfortable",
                        page.clone(),
                        DirtySetting::LineHeight,
                        |settings| settings.line_height = settings::TerminalLineHeight::Comfortable,
                    ),
                    draft_setting_option(
                        "custom",
                        page.clone(),
                        DirtySetting::LineHeight,
                        |settings| settings.line_height = settings::TerminalLineHeight::Custom(1.5),
                    ),
                ],
            ))
            .when_some(line_height_custom, |content, value| {
                content.child(self.number_row(
                    cx,
                    "Custom line height",
                    "A positive line-height multiplier.",
                    format!("{value:.2}"),
                    {
                        let page = page.clone();
                        move |cx, delta| {
                            page.update(cx, |this, cx| {
                                let current = match this.draft_settings.line_height {
                                    settings::TerminalLineHeight::Custom(value) => value,
                                    _ => 1.5,
                                };
                                this.update_draft(
                                    DirtySetting::LineHeight,
                                    |settings| {
                                        settings.line_height = settings::TerminalLineHeight::Custom(
                                            adjust_bounded(current, delta * 0.1, 1.0, f32::MAX),
                                        );
                                    },
                                    cx,
                                );
                            })
                            .log_err();
                        }
                    },
                ))
            })
            .child(self.number_row(
                cx,
                "Minimum contrast",
                "APCA minimum contrast from 0 to 106.",
                format!("{:.0}", settings.minimum_contrast),
                {
                    let page = page.clone();
                    move |cx, delta| {
                        page.update(cx, |this, cx| {
                            let next = adjust_bounded(
                                this.draft_settings.minimum_contrast,
                                delta,
                                0.0,
                                106.0,
                            );
                            this.update_draft(
                                DirtySetting::MinimumContrast,
                                |settings| settings.minimum_contrast = next,
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(self.section_header("Cursor", cx))
            .child(self.enum_row(
                window,
                cx,
                "Cursor shape",
                "The shape of the terminal cursor.",
                cursor_shape,
                vec![
                    draft_setting_option(
                        "block",
                        page.clone(),
                        DirtySetting::CursorShape,
                        |settings| settings.cursor_shape = CursorShape::Block,
                    ),
                    draft_setting_option(
                        "underline",
                        page.clone(),
                        DirtySetting::CursorShape,
                        |settings| settings.cursor_shape = CursorShape::Underline,
                    ),
                    draft_setting_option(
                        "bar",
                        page.clone(),
                        DirtySetting::CursorShape,
                        |settings| settings.cursor_shape = CursorShape::Bar,
                    ),
                    draft_setting_option(
                        "hollow",
                        page.clone(),
                        DirtySetting::CursorShape,
                        |settings| settings.cursor_shape = CursorShape::Hollow,
                    ),
                ],
            ))
            .child(self.enum_row(
                window,
                cx,
                "Cursor blinking",
                "Controls the actual cursor blink animation for each terminal pane.",
                blinking.to_string(),
                vec![
                    draft_setting_option("on", page.clone(), DirtySetting::Blinking, |settings| {
                        settings.blinking = settings::TerminalBlink::On
                    }),
                    draft_setting_option("off", page.clone(), DirtySetting::Blinking, |settings| {
                        settings.blinking = settings::TerminalBlink::Off
                    }),
                    draft_setting_option(
                        "terminal-controlled",
                        page.clone(),
                        DirtySetting::Blinking,
                        |settings| settings.blinking = settings::TerminalBlink::TerminalControlled,
                    ),
                ],
            ))
            .child(self.section_header("Behavior", cx))
            .child(self.toggle_row(
                "Option as meta",
                "Use the option key as the meta key.",
                settings.option_as_meta,
                {
                    let page = page.clone();
                    move |cx| {
                        page.update(cx, |this, cx| {
                            let next = !this.draft_settings.option_as_meta;
                            this.update_draft(
                                DirtySetting::OptionAsMeta,
                                |settings| settings.option_as_meta = next,
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(self.toggle_row(
                "Copy on select",
                "Automatically copy selected text to the clipboard.",
                settings.copy_on_select,
                {
                    let page = page.clone();
                    move |cx| {
                        page.update(cx, |this, cx| {
                            let next = !this.draft_settings.copy_on_select;
                            this.update_draft(
                                DirtySetting::CopyOnSelect,
                                |settings| settings.copy_on_select = next,
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(self.toggle_row(
                "Keep selection on copy",
                "Keep a terminal selection after it is copied manually.",
                settings.keep_selection_on_copy,
                {
                    let page = page.clone();
                    move |cx| {
                        page.update(cx, |this, cx| {
                            let next = !this.draft_settings.keep_selection_on_copy;
                            this.update_draft(
                                DirtySetting::KeepSelectionOnCopy,
                                |settings| settings.keep_selection_on_copy = next,
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(self.toggle_row(
                "Open links in mouse mode",
                "Allow Command-click links while an application receives mouse input.",
                settings.open_links_in_mouse_mode,
                {
                    let page = page.clone();
                    move |cx| {
                        page.update(cx, |this, cx| {
                            let next = !this.draft_settings.open_links_in_mouse_mode;
                            this.update_draft(
                                DirtySetting::OpenLinksInMouseMode,
                                |settings| settings.open_links_in_mouse_mode = next,
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(self.enum_row(
                window,
                cx,
                "Alternate scroll",
                "Convert scroll events to key presses in the alternate screen.",
                alternate_scroll,
                vec![
                    draft_setting_option(
                        "on",
                        page.clone(),
                        DirtySetting::AlternateScroll,
                        |settings| settings.alternate_scroll = settings::AlternateScroll::On,
                    ),
                    draft_setting_option(
                        "off",
                        page.clone(),
                        DirtySetting::AlternateScroll,
                        |settings| settings.alternate_scroll = settings::AlternateScroll::Off,
                    ),
                ],
            ))
            .child(self.enum_row(
                window,
                cx,
                "Bell",
                "What to do when the BEL character is printed. The standalone default is off.",
                bell,
                vec![
                    draft_setting_option("system", page.clone(), DirtySetting::Bell, |settings| {
                        settings.bell = settings::TerminalBell::System
                    }),
                    draft_setting_option("off", page.clone(), DirtySetting::Bell, |settings| {
                        settings.bell = settings::TerminalBell::Off
                    }),
                ],
            ))
            .child(self.section_header("Shell and environment", cx))
            .child(self.render_shell_form(window, cx))
            .child(self.row(
                "Environment variables",
                "Variables are validated and apply only to newly created terminals and panes.",
                div().into_any_element(),
            ))
            .child(self.render_environment_editor(cx))
            .child(self.render_working_directory_form(window, cx))
            .child(self.section_header("Input and scroll", cx))
            .child(self.number_row(
                cx,
                "Scroll multiplier",
                "The multiplier for scrolling with the mouse wheel.",
                format!("{:.2}x", settings.scroll_multiplier),
                {
                    let page = page.clone();
                    move |cx, delta| {
                        page.update(cx, |this, cx| {
                            let next = adjust_bounded(
                                this.draft_settings.scroll_multiplier,
                                delta,
                                0.1,
                                f32::MAX,
                            );
                            this.update_draft(
                                DirtySetting::ScrollMultiplier,
                                |settings| settings.scroll_multiplier = next,
                                cx,
                            );
                        })
                        .log_err();
                    }
                },
            ))
            .child(
                self.number_row(
                    cx,
                    "Max scrollback lines",
                    "The maximum number of lines to keep in scrollback for newly created terminals and panes.",
                    settings
                        .max_scroll_history_lines
                        .map_or_else(|| "10,000 (default)".to_string(), |value| value.to_string()),
                    {
                        let page = page.clone();
                        move |cx, delta| {
                            page.update(cx, |this, cx| {
                                let current = this
                                    .draft_settings
                                    .max_scroll_history_lines
                                    .unwrap_or(10_000);
                                let next = adjust_bounded(
                                    current as f32,
                                    delta * 10_000.0,
                                    0.0,
                                    100_000.0,
                                ) as usize;
                                this.update_draft(
                                    DirtySetting::MaxScrollHistoryLines,
                                    |settings| settings.max_scroll_history_lines = Some(next),
                                    cx,
                                );
                            })
                            .log_err();
                        }
                    },
                ),
            )
            .child(self.enum_row(
                window,
                cx,
                "Scrollbar",
                "Choose when the terminal scrollbar is visible.",
                scrollbar_show,
                vec![
                    draft_setting_option(
                        "auto",
                        page.clone(),
                        DirtySetting::Scrollbar,
                        |settings| {
                            settings.scrollbar.show = Some(settings::ShowScrollbar::Auto);
                        },
                    ),
                    draft_setting_option(
                        "system",
                        page.clone(),
                        DirtySetting::Scrollbar,
                        |settings| {
                            settings.scrollbar.show = Some(settings::ShowScrollbar::System);
                        },
                    ),
                    draft_setting_option(
                        "always",
                        page.clone(),
                        DirtySetting::Scrollbar,
                        |settings| {
                            settings.scrollbar.show = Some(settings::ShowScrollbar::Always);
                        },
                    ),
                    draft_setting_option(
                        "never",
                        page.clone(),
                        DirtySetting::Scrollbar,
                        |settings| {
                            settings.scrollbar.show = Some(settings::ShowScrollbar::Never);
                        },
                    ),
                ],
            ))
            .child(self.section_header("Reset", cx))
            .child(self.row(
                "Reset Terminal Defaults",
                "Restore terminal defaults in this draft. Click Save to persist the changes.",
                self.action_button(
                    "reset-terminal-defaults",
                    "Reset Terminal Defaults",
                    self.pending_write.is_none(),
                    |this, _, _, cx| this.reset_terminal_defaults(cx),
                    cx,
                ),
            ))
            .into_any_element()
        };

        window_chrome::client_side_decorations(
            v_flex()
                .id("settings-page")
                .key_context("SettingsPage")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(cx.theme().colors().panel_background)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_ref();
                    let extend_selection = event.keystroke.modifiers.shift;
                    let secondary_modifier = event.keystroke.modifiers.secondary();
                    let handled = match key {
                        "enter" => {
                            this.commit_active_edit(cx);
                            true
                        }
                        "escape" => {
                            this.clear_active_edit();
                            cx.notify();
                            true
                        }
                        "tab" => {
                            this.move_to_adjacent_editing_field(!extend_selection, window, cx);
                            true
                        }
                        "left" => {
                            this.move_editing_horizontally(true, extend_selection, cx);
                            true
                        }
                        "right" => {
                            this.move_editing_horizontally(false, extend_selection, cx);
                            true
                        }
                        "home" => {
                            this.move_editing_to_boundary(true, extend_selection, cx);
                            true
                        }
                        "end" => {
                            this.move_editing_to_boundary(false, extend_selection, cx);
                            true
                        }
                        "backspace" => {
                            this.delete_editing_backward(cx);
                            true
                        }
                        "delete" => {
                            this.delete_editing_forward(cx);
                            true
                        }
                        "a" if secondary_modifier => {
                            this.select_all_editing_text(cx);
                            true
                        }
                        "c" if secondary_modifier => {
                            this.copy_editing_selection(cx);
                            true
                        }
                        "x" if secondary_modifier => {
                            this.cut_editing_selection(cx);
                            true
                        }
                        "v" if secondary_modifier => {
                            this.paste_editing_text(cx);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        cx.stop_propagation();
                    }
                }))
                .child(SettingsPageInputElement {
                    focus_handle: self.focus_handle.clone(),
                    page: cx.weak_entity(),
                })
                .child(title_bar)
                .child(
                    div()
                        .id("settings-content-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(match self.active_tab {
                            SettingsTab::Terminal => terminal_content(cx),
                            SettingsTab::Keymap => {
                                let search_query = self.keymap_tab.search_query().to_string();
                                let search_input = self.keymap_search_input(search_query, cx);
                                self.keymap_tab.render(search_input, cx)
                            }
                            SettingsTab::Themes => {
                                let search_query = self.themes_tab.search_query().to_string();
                                let search_input = self.themes_search_input(search_query, cx);
                                self.themes_tab.render(search_input, cx)
                            }
                            SettingsTab::About => self.render_about_tab(),
                        }),
                ),
            window,
            cx,
        )
    }
}

impl SettingsPage {
    fn render_about_tab(&self) -> gpui::AnyElement {
        v_flex()
            .id("about-content")
            .w_full()
            .px_8()
            .py_4()
            .gap_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new("ZedTerm").size(LabelSize::Large))
                    .child(
                        Label::new("A standalone terminal application based on Zed's terminal.")
                            .color(Color::Muted),
                    ),
            )
            .child(self.row(
                "Version",
                "The version of ZedTerm currently running.",
                Label::new(env!("CARGO_PKG_VERSION")).into_any_element(),
            ))
            .child(
                self.row(
                    "Repository",
                    "View the ZedTerm source code.",
                    ButtonLink::new("GitHub repository", "https://github.com/ruanimal/zed-term")
                        .into_any_element(),
                ),
            )
            .child(self.row(
                "License",
                "ZedTerm is distributed under the GPL-3.0-or-later license.",
                Label::new("GPL-3.0-or-later").into_any_element(),
            ))
            .into_any_element()
    }

    /// Renders the merged title bar: the TabBar acts as the window title
    /// bar (as in the terminal window). Window controls are injected as
    /// TabBar start/end children on client-side decorations; on server-side
    /// decorations the traffic lights are system-drawn and a left padding
    /// reserves space for them. Save/Discard buttons sit in the TabBar's
    /// end area.
    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let titlebar_height = window_chrome::TITLE_BAR_HEIGHT;
        let has_unsaved_changes = !self.dirty_settings.is_empty();
        let saving = self.save_status == SaveStatus::Saving;
        let save_label = if saving {
            "Saving…"
        } else if self.save_status == SaveStatus::Succeeded && !has_unsaved_changes {
            "Saved"
        } else {
            "Save"
        }
        .to_string();
        let is_server_decorations = matches!(window.window_decorations(), Decorations::Server);
        let can_maximize =
            !is_server_decorations && window.is_resizable() && window.window_controls().maximize;
        let close_window = cx.listener(|this, _: &ClickEvent, window, cx| {
            if this.request_close(window, cx) {
                window.remove_window();
            }
        });
        let (left_controls, right_controls) =
            window_chrome::render_window_controls(window, cx, close_window);

        let save_discard = h_flex()
            .gap_2()
            .child(self.title_bar_action_button(
                "discard-settings",
                "Discard".to_string(),
                has_unsaved_changes && !saving,
                false,
                false,
                |this, _, _, cx| this.discard_drafts(cx),
                cx,
            ))
            .child(self.title_bar_action_button(
                "save-settings",
                save_label,
                has_unsaved_changes && !saving,
                has_unsaved_changes,
                self.save_status == SaveStatus::Succeeded && !has_unsaved_changes,
                |this, _, _, cx| this.save_drafts(cx),
                cx,
            ));

        let tab_bar = TabBar::new("settings-tab-bar")
            .height(titlebar_height)
            .child(self.render_settings_tab(SettingsTab::Terminal, cx))
            .child(self.render_settings_tab(SettingsTab::Keymap, cx))
            .child(self.render_settings_tab(SettingsTab::Themes, cx))
            .child(self.render_settings_tab(SettingsTab::About, cx))
            .end_child(save_discard);

        let tab_bar = if is_server_decorations {
            tab_bar
        } else {
            tab_bar.start_child(left_controls).end_child(right_controls)
        };

        div()
            .id("settings-title-bar")
            .h(titlebar_height)
            .flex()
            .flex_row()
            .flex_none()
            .when(is_server_decorations, |this| {
                this.pl(px(TRAFFIC_LIGHT_PADDING))
            })
            .bg(cx.theme().colors().tab_bar_background)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, _| {
                    this.titlebar_mouse_down.set(true);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| {
                    this.titlebar_mouse_down.set(false);
                }),
            )
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                if this.titlebar_mouse_down.get() {
                    this.titlebar_mouse_down.set(false);
                    window.start_window_move();
                }
            }))
            .on_click(cx.listener(move |_, event: &gpui::ClickEvent, window, _| {
                if can_maximize && event.click_count() >= 2 {
                    window.zoom_window();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(tab_bar),
            )
            .into_any_element()
    }

    fn render_settings_tab(&self, tab: SettingsTab, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected = self.active_tab == tab;
        let colors = cx.theme().colors();
        let text_color = if selected {
            colors.text
        } else {
            colors.text_muted
        };
        div()
            .id(format!("settings-tab-{}", tab.label()))
            .debug_selector(|| format!("settings-tab-{}", tab.label()))
            .relative()
            .h_full()
            .flex()
            .items_center()
            .px_4()
            .cursor_pointer()
            .text_color(text_color)
            .when(selected, |this| {
                // Painted as an absolutely positioned overlay rather than a border
                // so the indicator does not push the label out of line with the
                // unselected tabs.
                this.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .h_1()
                        .bg(colors.border),
                )
            })
            .when(!selected, |this| {
                this.hover(|style| style.bg(colors.ghost_element_hover))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.switch_tab(tab, cx);
            }))
            .child(Label::new(tab.label()).size(LabelSize::Small))
            .into_any_element()
    }

    pub(super) fn switch_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        if self.active_tab == tab {
            return;
        }
        self.clear_active_edit();
        self.active_tab = tab;
        match tab {
            // The keymap tab starts empty and is populated on first open;
            // without this it would render an empty list.
            SettingsTab::Keymap => self.keymap_tab.reload_bindings(cx),
            // The catalog is fetched on first open so the settings window still
            // opens instantly and offline; later opens reuse the loaded list.
            SettingsTab::Themes if !self.themes_tab.has_loaded() => {
                let page = cx.weak_entity();
                let http_client = cx.http_client();
                self.themes_tab.begin_load(http_client, page, cx);
            }
            // Installed extensions may have changed while the tab was closed
            // (another app sharing the data directory, a manual install), so
            // the disk state is re-read even when the catalog is cached.
            SettingsTab::Themes => self.themes_tab.refresh_installed(),
            SettingsTab::Terminal | SettingsTab::About => {}
        }
        cx.notify();
    }
}
