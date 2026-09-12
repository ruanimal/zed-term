//! Keystroke recorder widget for the settings page's keymap tab.
//!
//! Ported from Zed's `keymap_editor` crate (`ui_components/keystroke_input.rs`),
//! which is no longer part of this fork. Only the parts that depend on `gpui`
//! and `ui` are kept; the workspace/editor integrations are dropped.

use gpui::{
    Animation, AnimationExt, Context, EventEmitter, FocusHandle, Focusable, FontWeight, KeyContext,
    KeybindingKeystroke, Keystroke, Modifiers, ModifiersChangedEvent, Subscription, Task, actions,
};
use ui::{
    ActiveTheme as _, Color, IconButton, IconButtonShape, IconName, IconSize, Label, LabelSize,
    ParentElement as _, Render, Styled as _, Tooltip, Window, prelude::*,
};

actions!(
    terminal_app_keymap,
    [
        /// Starts recording keystrokes
        StartRecording,
        /// Stops recording keystrokes
        StopRecording,
        /// Clears the recorded keystrokes
        ClearKeystrokes,
    ]
);

const KEY_CONTEXT_VALUE: &str = "KeystrokeInput";

const CLOSE_KEYSTROKE_CAPTURE_END_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(300);

enum CloseKeystrokeResult {
    Partial,
    Close,
    None,
}

impl PartialEq for CloseKeystrokeResult {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (CloseKeystrokeResult::Partial, CloseKeystrokeResult::Partial)
                | (CloseKeystrokeResult::Close, CloseKeystrokeResult::Close)
                | (CloseKeystrokeResult::None, CloseKeystrokeResult::None)
        )
    }
}

pub struct KeystrokeInput {
    keystrokes: Vec<KeybindingKeystroke>,
    placeholder_keystrokes: Option<Vec<KeybindingKeystroke>>,
    outer_focus_handle: FocusHandle,
    inner_focus_handle: FocusHandle,
    intercept_subscription: Option<Subscription>,
    _focus_subscriptions: [Subscription; 2],
    search: bool,
    /// The sequence of close keystrokes being typed
    close_keystrokes: Option<Vec<Keystroke>>,
    close_keystrokes_start: Option<usize>,
    previous_modifiers: Modifiers,
    /// In order to support inputting keystrokes that end with a prefix of the
    /// close keybind keystrokes, we clear the close keystroke capture info
    /// on a timeout after a close keystroke is pressed
    ///
    /// e.g. if close binding is `esc esc esc` and user wants to search for
    /// `ctrl-g esc`, after entering the `ctrl-g esc`, hitting `esc` twice would
    /// stop recording because of the sequence of three escapes making it
    /// impossible to search for anything ending in `esc`
    clear_close_keystrokes_timer: Option<Task<()>>,
    #[cfg(test)]
    recording: bool,
    pub actions_slot: Option<AnyElement>,
}

impl KeystrokeInput {
    const KEYSTROKE_COUNT_MAX: usize = 3;

    pub fn new(
        placeholder_keystrokes: Option<Vec<KeybindingKeystroke>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let outer_focus_handle = cx.focus_handle();
        let inner_focus_handle = cx.focus_handle();
        let _focus_subscriptions = [
            cx.on_focus_in(&inner_focus_handle, window, Self::on_inner_focus_in),
            cx.on_focus_out(&inner_focus_handle, window, Self::on_inner_focus_out),
        ];
        Self {
            keystrokes: Vec::new(),
            placeholder_keystrokes,
            inner_focus_handle,
            outer_focus_handle,
            intercept_subscription: None,
            _focus_subscriptions,
            search: false,
            close_keystrokes: None,
            close_keystrokes_start: None,
            previous_modifiers: Modifiers::default(),
            clear_close_keystrokes_timer: None,
            #[cfg(test)]
            recording: false,
            actions_slot: None,
        }
    }

    pub fn set_keystrokes(&mut self, keystrokes: Vec<KeybindingKeystroke>, cx: &mut Context<Self>) {
        self.keystrokes = keystrokes;
        self.keystrokes_changed(cx);
    }

    pub fn set_search(&mut self, search: bool) {
        self.search = search;
    }

    /// The keystrokes recorded so far, ignoring the display placeholder.
    pub fn recorded_keystrokes(&self) -> &[KeybindingKeystroke] {
        &self.keystrokes
    }

    pub fn keystrokes(&self) -> &[KeybindingKeystroke] {
        if let Some(placeholders) = self.placeholder_keystrokes.as_ref()
            && self.keystrokes.is_empty()
        {
            return placeholders;
        }
        if !self.search
            && self
                .keystrokes
                .last()
                .is_some_and(|last| last.key().is_empty())
        {
            return &self.keystrokes[..self.keystrokes.len() - 1];
        }
        &self.keystrokes
    }

    fn dummy(modifiers: Modifiers) -> KeybindingKeystroke {
        KeybindingKeystroke::from_keystroke(Keystroke {
            modifiers,
            key: "".to_string(),
            key_char: None,
        })
    }

    fn keystrokes_changed(&self, cx: &mut Context<Self>) {
        cx.emit(());
        cx.notify();
    }

    fn key_context() -> KeyContext {
        let mut key_context = KeyContext::default();
        key_context.add(KEY_CONTEXT_VALUE);
        key_context
    }

    fn determine_stop_recording_binding(window: &mut Window) -> Option<gpui::KeyBinding> {
        if cfg!(test) {
            Some(gpui::KeyBinding::new(
                "escape escape escape",
                StopRecording,
                Some(KEY_CONTEXT_VALUE),
            ))
        } else {
            window.highest_precedence_binding_for_action_in_context(
                &StopRecording,
                Self::key_context(),
            )
        }
    }

    fn upsert_close_keystrokes_start(&mut self, start: usize, cx: &mut Context<Self>) {
        if self.close_keystrokes_start.is_some() {
            return;
        }
        self.close_keystrokes_start = Some(start);
        self.update_clear_close_keystrokes_timer(cx);
    }

    fn update_clear_close_keystrokes_timer(&mut self, cx: &mut Context<Self>) {
        self.clear_close_keystrokes_timer = Some(cx.spawn(async |this, cx| {
            cx.background_executor()
                .timer(CLOSE_KEYSTROKE_CAPTURE_END_TIMEOUT)
                .await;
            this.update(cx, |this, _cx| {
                this.end_close_keystrokes_capture();
            })
            .ok();
        }));
    }

    /// Interrupt the capture of close keystrokes, but do not clear the close keystrokes
    /// from the input
    fn end_close_keystrokes_capture(&mut self) -> Option<usize> {
        self.close_keystrokes.take();
        self.clear_close_keystrokes_timer.take();
        self.close_keystrokes_start.take()
    }

    fn handle_possible_close_keystroke(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CloseKeystrokeResult {
        let Some(keybind_for_close_action) = Self::determine_stop_recording_binding(window) else {
            log::trace!("No keybinding to stop recording keystrokes in keystroke input");
            self.end_close_keystrokes_capture();
            return CloseKeystrokeResult::None;
        };
        let action_keystrokes = keybind_for_close_action.keystrokes();

        if let Some(mut close_keystrokes) = self.close_keystrokes.take() {
            let mut index = 0;

            while index < action_keystrokes.len() && index < close_keystrokes.len() {
                if !close_keystrokes[index].should_match(&action_keystrokes[index]) {
                    break;
                }
                index += 1;
            }
            if index == close_keystrokes.len() {
                if index >= action_keystrokes.len() {
                    self.end_close_keystrokes_capture();
                    return CloseKeystrokeResult::None;
                }
                if keystroke.should_match(&action_keystrokes[index]) {
                    close_keystrokes.push(keystroke.clone());
                    if close_keystrokes.len() == action_keystrokes.len() {
                        return CloseKeystrokeResult::Close;
                    } else {
                        self.close_keystrokes = Some(close_keystrokes);
                        self.update_clear_close_keystrokes_timer(cx);
                        return CloseKeystrokeResult::Partial;
                    }
                } else {
                    self.end_close_keystrokes_capture();
                    return CloseKeystrokeResult::None;
                }
            }
        } else if let Some(first_action_keystroke) = action_keystrokes.first()
            && keystroke.should_match(first_action_keystroke)
        {
            self.close_keystrokes = Some(vec![keystroke.clone()]);
            return CloseKeystrokeResult::Partial;
        }
        self.end_close_keystrokes_capture();
        CloseKeystrokeResult::None
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        let keystrokes_len = self.keystrokes.len();

        if self.previous_modifiers.modified()
            && event.modifiers.is_subset_of(&self.previous_modifiers)
        {
            self.previous_modifiers &= event.modifiers;
            return;
        }
        self.keystrokes_changed(cx);

        if let Some(last) = self.keystrokes.last_mut()
            && last.key().is_empty()
            && keystrokes_len <= Self::KEYSTROKE_COUNT_MAX
        {
            if !self.search && !event.modifiers.modified() {
                self.keystrokes.pop();
                return;
            }
            if self.search {
                if self.previous_modifiers.modified() {
                    let modifiers = *last.modifiers() | event.modifiers;
                    last.set_modifiers(modifiers);
                } else {
                    self.keystrokes.push(Self::dummy(event.modifiers));
                }
                self.previous_modifiers |= event.modifiers;
            } else {
                last.set_modifiers(event.modifiers);
                return;
            }
        } else if keystrokes_len < Self::KEYSTROKE_COUNT_MAX {
            self.keystrokes.push(Self::dummy(event.modifiers));
            if self.search {
                self.previous_modifiers |= event.modifiers;
            }
        }
        if keystrokes_len >= Self::KEYSTROKE_COUNT_MAX {
            self.clear_keystrokes(&ClearKeystrokes, window, cx);
        }
    }

    fn handle_keystroke(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();

        let close_keystroke_result = self.handle_possible_close_keystroke(keystroke, window, cx);
        if close_keystroke_result == CloseKeystrokeResult::Close {
            self.stop_recording(&StopRecording, window, cx);
            return;
        }

        let keystroke = KeybindingKeystroke::new_with_mapper(
            keystroke.clone(),
            false,
            cx.keyboard_mapper().as_ref(),
        );
        if let Some(last) = self.keystrokes.last()
            && last.key().is_empty()
            && (!self.search || self.previous_modifiers.modified())
        {
            self.keystrokes.pop();
        }

        if close_keystroke_result == CloseKeystrokeResult::Partial {
            self.upsert_close_keystrokes_start(self.keystrokes.len(), cx);
            if self.keystrokes.len() >= Self::KEYSTROKE_COUNT_MAX {
                return;
            }
        }

        if self.keystrokes.len() >= Self::KEYSTROKE_COUNT_MAX {
            self.clear_keystrokes(&ClearKeystrokes, window, cx);
            return;
        }

        self.keystrokes.push(keystroke);
        self.keystrokes_changed(cx);

        // The reason we use the real modifiers from the window instead of the keystroke's modifiers
        // is that for keystrokes like `ctrl-$` the modifiers reported by keystroke is `ctrl` which
        // is wrong, it should be `ctrl-shift`. The window's modifiers are always correct.
        let real_modifiers = window.modifiers();
        if self.search {
            self.previous_modifiers = real_modifiers;
            return;
        }
        if self.keystrokes.len() < Self::KEYSTROKE_COUNT_MAX && real_modifiers.modified() {
            self.keystrokes.push(Self::dummy(real_modifiers));
        }
    }

    fn on_inner_focus_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.intercept_subscription.is_none() {
            let listener = cx.listener(|this, event: &gpui::KeystrokeEvent, window, cx| {
                this.handle_keystroke(&event.keystroke, window, cx);
            });
            self.intercept_subscription = Some(cx.intercept_keystrokes(listener))
        }
    }

    fn on_inner_focus_out(
        &mut self,
        _event: gpui::FocusOutEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.intercept_subscription.take();
        cx.notify();
    }

    fn render_keystrokes(&self, is_recording: bool) -> impl Iterator<Item = Div> {
        let keystrokes = if let Some(placeholders) = self.placeholder_keystrokes.as_ref()
            && self.keystrokes.is_empty()
        {
            if is_recording {
                &[]
            } else {
                placeholders.as_slice()
            }
        } else {
            &self.keystrokes
        };
        keystrokes.iter().map(move |keystroke| {
            h_flex().children(ui::render_keybinding_keystroke(
                keystroke,
                Some(Color::Default),
                Some(rems(0.875).into()),
                ui::PlatformStyle::platform(),
                false,
            ))
        })
    }

    pub fn start_recording(
        &mut self,
        _: &StartRecording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.inner_focus_handle, cx);
        self.clear_keystrokes(&ClearKeystrokes, window, cx);
        self.previous_modifiers = window.modifiers();
        #[cfg(test)]
        {
            self.recording = true;
        }
        cx.stop_propagation();
    }

    pub fn stop_recording(
        &mut self,
        _: &StopRecording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_recording(window) {
            return;
        }
        window.focus(&self.outer_focus_handle, cx);
        if let Some(close_keystrokes_start) = self.close_keystrokes_start.take()
            && close_keystrokes_start < self.keystrokes.len()
        {
            self.keystrokes.drain(close_keystrokes_start..);
            self.keystrokes_changed(cx);
        }
        self.end_close_keystrokes_capture();
        #[cfg(test)]
        {
            self.recording = false;
        }
        cx.notify();
    }

    pub fn clear_keystrokes(
        &mut self,
        _: &ClearKeystrokes,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keystrokes.clear();
        self.keystrokes_changed(cx);
        self.end_close_keystrokes_capture();
    }

    fn is_recording(&self, window: &Window) -> bool {
        #[cfg(test)]
        {
            if true {
                // in tests, we just need a simple bool that is toggled on start and stop recording
                return self.recording;
            }
        }
        // however, in the real world, checking if the inner focus handle is focused
        // is a much more reliable check, as the intercept keystroke handlers are installed
        // on focus of the inner focus handle, thereby ensuring our recording state does
        // not get de-synced
        self.inner_focus_handle.is_focused(window)
    }

    pub fn actions_slot(mut self, action: impl IntoElement) -> Self {
        self.actions_slot = Some(action.into_any_element());
        self
    }
}

impl EventEmitter<()> for KeystrokeInput {}

impl Focusable for KeystrokeInput {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.outer_focus_handle.clone()
    }
}

impl Render for KeystrokeInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let is_focused = self.outer_focus_handle.contains_focused(window, cx);
        let is_recording = self.is_recording(window);

        let width = rems_from_px(64_f32);

        let recording_bg_color = colors
            .editor_background
            .blend(colors.text_accent.opacity(0.1));

        let recording_pulse = |color: Color| {
            Icon::new(IconName::Circle)
                .size(IconSize::Small)
                .color(Color::Error)
                .with_animation(
                    "recording-pulse",
                    Animation::new(std::time::Duration::from_secs(2))
                        .repeat()
                        .with_easing(gpui::pulsating_between(0.4, 0.8)),
                    {
                        let color = color.color(cx);
                        move |this, delta| this.color(Color::Custom(color.opacity(delta)))
                    },
                )
        };

        let recording_indicator = h_flex()
            .h_4()
            .pr_1()
            .gap_0p5()
            .border_1()
            .border_color(colors.border)
            .bg(colors
                .editor_background
                .blend(colors.text_accent.opacity(0.1)))
            .rounded_sm()
            .child(recording_pulse(Color::Error))
            .child(
                Label::new("REC")
                    .size(LabelSize::XSmall)
                    .weight(FontWeight::SEMIBOLD)
                    .color(Color::Error),
            );

        let search_indicator = h_flex()
            .h_4()
            .pr_1()
            .gap_0p5()
            .border_1()
            .border_color(colors.border)
            .bg(colors
                .editor_background
                .blend(colors.text_accent.opacity(0.1)))
            .rounded_sm()
            .child(recording_pulse(Color::Accent))
            .child(
                Label::new("SEARCH")
                    .size(LabelSize::XSmall)
                    .weight(FontWeight::SEMIBOLD)
                    .color(Color::Accent),
            );

        let record_icon = if self.search {
            IconName::MagnifyingGlass
        } else {
            IconName::PlayFilled
        };

        h_flex()
            .id("keystroke-input")
            .track_focus(&self.outer_focus_handle)
            .key_context(Self::key_context())
            .on_action(cx.listener(Self::start_recording))
            .on_action(cx.listener(Self::clear_keystrokes))
            .py_2()
            .px_3()
            .gap_2()
            .min_h_10()
            .w_full()
            .flex_1()
            .justify_between()
            .rounded_md()
            .overflow_hidden()
            .map(|this| {
                if is_recording {
                    this.bg(recording_bg_color)
                } else {
                    this.bg(colors.editor_background)
                }
            })
            .border_1()
            .map(|this| {
                if is_focused {
                    this.border_color(colors.border_focused)
                } else {
                    this.border_color(colors.border_variant)
                }
            })
            .child(
                h_flex()
                    .w(width)
                    .gap_0p5()
                    .justify_start()
                    .flex_none()
                    .when(is_recording, |this| {
                        this.map(|this| {
                            if self.search {
                                this.child(search_indicator)
                            } else {
                                this.child(recording_indicator)
                            }
                        })
                    }),
            )
            .child(
                h_flex()
                    .id("keystroke-input-inner")
                    .track_focus(&self.inner_focus_handle)
                    .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
                    .when(!self.search, |this| {
                        this.focus(|mut style| {
                            style.border_color = Some(colors.border_focused);
                            style
                        })
                    })
                    .size_full()
                    .min_w_0()
                    .justify_center()
                    .flex_wrap()
                    .gap_1()
                    .children(self.render_keystrokes(is_recording)),
            )
            .child(
                h_flex()
                    .w(width)
                    .gap_0p5()
                    .justify_end()
                    .flex_none()
                    .map(|this| {
                        if is_recording {
                            this.child(
                                IconButton::new("stop-record-btn", IconName::Stop)
                                    .shape(IconButtonShape::Square)
                                    .map(|this| {
                                        this.tooltip(Tooltip::for_action_title(
                                            if self.search {
                                                "Stop Searching"
                                            } else {
                                                "Stop Recording"
                                            },
                                            &StopRecording,
                                        ))
                                    })
                                    .icon_color(Color::Error)
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.stop_recording(&StopRecording, window, cx);
                                    })),
                            )
                        } else {
                            this.child(
                                IconButton::new("record-btn", record_icon)
                                    .shape(IconButtonShape::Square)
                                    .map(|this| {
                                        this.tooltip(Tooltip::for_action_title(
                                            if self.search {
                                                "Start Searching"
                                            } else {
                                                "Start Recording"
                                            },
                                            &StartRecording,
                                        ))
                                    })
                                    .when(!is_focused, |this| this.icon_color(Color::Muted))
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.start_recording(&StartRecording, window, cx);
                                    })),
                            )
                        }
                    })
                    .when_some(self.actions_slot.take(), |this, action| this.child(action))
                    .when(is_recording, |this| {
                        this.child(
                            IconButton::new("clear-btn", IconName::Backspace)
                                .shape(IconButtonShape::Square)
                                .tooltip(move |_, cx| {
                                    Tooltip::with_meta(
                                        "Clear Keystrokes",
                                        Some(&ClearKeystrokes),
                                        "Hit it three times to execute",
                                        cx,
                                    )
                                })
                                .when(!is_focused, |this| this.icon_color(Color::Muted))
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.clear_keystrokes(&ClearKeystrokes, window, cx);
                                })),
                        )
                    }),
            )
    }
}
