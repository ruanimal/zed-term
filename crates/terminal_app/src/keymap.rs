//! Keymap tab support for the settings page.
//!
//! Lets the user inspect every effective key binding (built-in defaults plus
//! `keymap.json` overrides), search them, rebind a binding, and reset the user
//! keymap. The `keymap.json` rewriting and the conflict analysis are ported
//! from Zed's `keymap_editor` crate, which is no longer part of this fork.

pub mod keystroke_input;

use std::{cmp, rc::Rc, sync::Arc};

use collections::{HashMap, HashSet};
use gpui::{
    App, AppContext as _, Context, Entity, KeyBinding, PlatformKeyboardMapper, SharedString,
    Subscription, Window,
};
use settings::{KeybindSource, KeymapFile};
use ui::{
    AnyElement, Color, IconButton, IconName, Label, LabelSize, ParentElement as _, Styled as _,
    Tooltip, h_flex, prelude::*, v_flex,
};
use util::ResultExt as _;

pub use keystroke_input::{ClearKeystrokes, KeystrokeInput, StartRecording, StopRecording};

/// One row of the binding list: either an effective binding or an action that
/// has no binding at all, shown so the user can add one.
#[derive(Clone)]
pub enum ProcessedBinding {
    Mapped(Box<KeybindInformation>, Box<ActionInformation>),
    Unmapped(Box<ActionInformation>),
}

#[derive(Clone)]
pub struct KeybindInformation {
    keystrokes: Rc<[gpui::KeybindingKeystroke]>,
    keystroke_text: SharedString,
    pub source: KeybindSource,
    pub context: Option<SharedString>,
    pub is_no_action: bool,
    pub is_unbound_by_unbind: bool,
}

#[derive(Clone)]
pub struct ActionInformation {
    pub name: &'static str,
    pub humanized_name: SharedString,
    pub arguments: Option<SharedString>,
    pub documentation: Option<&'static str>,
}

impl ProcessedBinding {
    pub fn is_unbound(&self) -> bool {
        matches!(self, Self::Unmapped(_))
    }

    pub fn keystrokes(&self) -> Option<&[gpui::KeybindingKeystroke]> {
        match self {
            Self::Mapped(keybind, _) => Some(keybind.keystrokes.as_ref()),
            Self::Unmapped(_) => None,
        }
    }

    pub fn keystroke_text(&self, cx: &App) -> Option<SharedString> {
        self.keystrokes()
            .map(|keystrokes| ui::text_for_keybinding_keystrokes(keystrokes, cx).into())
    }

    pub fn source(&self) -> Option<KeybindSource> {
        match self {
            Self::Mapped(keybind, _) => Some(keybind.source),
            Self::Unmapped(_) => None,
        }
    }

    pub fn context_text(&self) -> Option<&SharedString> {
        match self {
            Self::Mapped(keybind, _) => keybind.context.as_ref(),
            Self::Unmapped(_) => None,
        }
    }

    pub fn action(&self) -> &ActionInformation {
        match self {
            Self::Mapped(_, action) | Self::Unmapped(action) => action,
        }
    }

    fn cmp(&self, other: &Self) -> cmp::Ordering {
        match (self, other) {
            (Self::Mapped(keybind1, action1), Self::Mapped(keybind2, action2)) => {
                match keybind1.source.cmp(&keybind2.source) {
                    cmp::Ordering::Equal => action1.humanized_name.cmp(&action2.humanized_name),
                    ordering => ordering,
                }
            }
            (Self::Mapped(..), Self::Unmapped(_)) => cmp::Ordering::Less,
            (Self::Unmapped(_), Self::Mapped(..)) => cmp::Ordering::Greater,
            (Self::Unmapped(action1), Self::Unmapped(action2)) => {
                action1.humanized_name.cmp(&action2.humanized_name)
            }
        }
    }
}

/// The keystrokes plus context that identify a binding in the keymap file.
#[derive(Debug, Default, PartialEq, Eq, Clone, Hash)]
pub struct ActionMapping {
    pub keystrokes: Rc<[gpui::KeybindingKeystroke]>,
    pub context: Option<SharedString>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct ConflictOrigin {
    pub override_source: KeybindSource,
    pub overridden_source: Option<KeybindSource>,
    pub index: usize,
}

impl ConflictOrigin {
    fn new(source: KeybindSource, index: usize) -> Self {
        Self {
            override_source: source,
            overridden_source: None,
            index,
        }
    }

    fn with_overridden_source(self, source: KeybindSource) -> Self {
        Self {
            overridden_source: Some(source),
            ..self
        }
    }

    fn get_conflict_with(&self, other: &Self) -> Option<Self> {
        if self.override_source == KeybindSource::User
            && other.override_source == KeybindSource::User
        {
            Some(
                Self::new(KeybindSource::User, other.index)
                    .with_overridden_source(self.override_source),
            )
        } else if self.override_source > other.override_source {
            Some(other.with_overridden_source(self.override_source))
        } else {
            None
        }
    }

    fn is_user_keybind_conflict(&self) -> bool {
        self.override_source == KeybindSource::User
            && self.overridden_source == Some(KeybindSource::User)
    }
}

#[derive(Clone, Copy)]
pub struct KeybindConflict {
    pub first_conflict_index: usize,
    pub remaining_conflict_amount: usize,
}

type ConflictKeybindMapping = HashMap<
    Rc<[gpui::KeybindingKeystroke]>,
    Vec<(
        Option<gpui::KeyBindingContextPredicate>,
        Vec<ConflictOrigin>,
    )>,
>;

/// Detects bindings that shadow each other, ported from Zed's `ConflictState`.
#[derive(Default)]
pub struct ConflictState {
    conflicts: Vec<Option<ConflictOrigin>>,
    keybind_mapping: ConflictKeybindMapping,
    has_user_conflicts: bool,
}

impl ConflictState {
    pub fn new(key_bindings: &[ProcessedBinding]) -> Self {
        let mut action_keybind_mapping = ConflictKeybindMapping::default();

        let mut largest_index = 0;
        for (index, binding) in key_bindings.iter().enumerate() {
            let ProcessedBinding::Mapped(keybind, _) = binding else {
                continue;
            };
            let predicate = keybind
                .context
                .as_deref()
                .and_then(|context| gpui::KeyBindingContextPredicate::parse(context).ok());
            let entry = action_keybind_mapping
                .entry(keybind.keystrokes.clone())
                .or_default();
            let origin = ConflictOrigin::new(keybind.source, index);
            if let Some((_, origins)) =
                entry
                    .iter_mut()
                    .find(|(other_predicate, _)| match (&predicate, other_predicate) {
                        (None, None) => true,
                        (Some(a), Some(b)) => normalized_ctx_eq(a, b),
                        _ => false,
                    })
            {
                origins.push(origin);
            } else {
                entry.push((predicate, vec![origin]));
            }
            largest_index = index;
        }

        let mut conflicts = vec![None; largest_index + 1];
        let mut has_user_conflicts = false;

        for entries in action_keybind_mapping.values_mut() {
            for (_, indices) in entries.iter_mut() {
                indices.sort_unstable_by_key(|origin| origin.override_source);
                let Some((first, second)) = indices.first().zip(indices.get(1)) else {
                    continue;
                };
                let (first, second) = (*first, *second);

                for origin in indices.iter() {
                    conflicts[origin.index] =
                        origin.get_conflict_with(if *origin == first { &second } else { &first })
                }

                has_user_conflicts |= first.override_source == KeybindSource::User
                    && second.override_source == KeybindSource::User;
            }
        }

        Self {
            conflicts,
            keybind_mapping: action_keybind_mapping,
            has_user_conflicts,
        }
    }

    pub fn conflicting_indices_for_mapping(
        &self,
        action_mapping: &ActionMapping,
        keybind_idx: Option<usize>,
    ) -> Option<KeybindConflict> {
        let predicate = action_mapping
            .context
            .as_deref()
            .and_then(|context| gpui::KeyBindingContextPredicate::parse(context).ok());
        self.keybind_mapping
            .get(&action_mapping.keystrokes)
            .and_then(|entries| {
                entries
                    .iter()
                    .find_map(|(other_predicate, indices)| {
                        match (&predicate, other_predicate) {
                            (None, None) => true,
                            (Some(pred), Some(other)) => normalized_ctx_eq(pred, other),
                            _ => false,
                        }
                        .then_some(indices)
                    })
                    .and_then(|indices| {
                        let mut indices = indices
                            .iter()
                            .filter(|&conflict| Some(conflict.index) != keybind_idx);
                        indices.next().map(|origin| KeybindConflict {
                            first_conflict_index: origin.index,
                            remaining_conflict_amount: indices.count(),
                        })
                    })
            })
    }

    pub fn conflict_for_idx(&self, index: usize) -> Option<ConflictOrigin> {
        self.conflicts.get(index).copied().flatten()
    }

    pub fn has_user_conflict(&self, candidate_index: usize) -> bool {
        self.conflict_for_idx(candidate_index)
            .is_some_and(|conflict| conflict.is_user_keybind_conflict())
    }

    pub fn any_user_binding_conflicts(&self) -> bool {
        self.has_user_conflicts
    }
}

fn disabled_binding_matches_context(disabled_binding: &KeyBinding, binding: &KeyBinding) -> bool {
    match (
        disabled_binding.predicate().as_deref(),
        binding.predicate().as_deref(),
    ) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(disabled_predicate), Some(predicate)) => disabled_predicate.is_superset(predicate),
    }
}

fn keystrokes_match_exactly(
    keystrokes1: &[gpui::KeybindingKeystroke],
    keystrokes2: &[gpui::KeybindingKeystroke],
) -> bool {
    keystrokes1.len() == keystrokes2.len()
        && keystrokes1.iter().zip(keystrokes2).all(|(a, b)| {
            a.inner().key == b.inner().key && a.inner().modifiers == b.inner().modifiers
        })
}

fn binding_is_unbound_by_unbind(
    binding: &KeyBinding,
    binding_index: usize,
    all_bindings: &[&KeyBinding],
) -> bool {
    all_bindings[binding_index + 1..]
        .iter()
        .rev()
        .any(|disabled_binding| {
            gpui::is_unbind(disabled_binding.action())
                && keystrokes_match_exactly(disabled_binding.keystrokes(), binding.keystrokes())
                && disabled_binding
                    .action()
                    .as_any()
                    .downcast_ref::<gpui::Unbind>()
                    .is_some_and(|unbind| unbind.0.as_ref() == binding.action().name())
                && disabled_binding_matches_context(disabled_binding, binding)
        })
}

/// Turns `terminal_app::SplitRight` into `Split Right`.
fn humanize_action_name(action_name: &str) -> SharedString {
    let name = action_name
        .split_once("::")
        .map_or(action_name, |(_, name)| name);
    let mut humanized = String::with_capacity(name.len() + 4);
    let mut previous_is_lowercase = false;
    for character in name.chars() {
        if character.is_uppercase() && previous_is_lowercase {
            humanized.push(' ');
        }
        previous_is_lowercase = character.is_lowercase() || character.is_numeric();
        humanized.push(character);
    }
    humanized.into()
}

/// Snapshot of every binding in the app, split into displayable rows.
pub fn process_bindings(cx: &App) -> Vec<ProcessedBinding> {
    let key_bindings = cx.key_bindings();
    let lock = key_bindings.borrow();
    let key_bindings = lock.bindings().collect::<Vec<_>>();
    let mut unmapped_action_names = HashSet::from_iter(cx.all_action_names().iter().copied());
    let action_documentation = cx.action_documentation();

    let mut processed_bindings = Vec::new();

    for (binding_index, &key_binding) in key_bindings.iter().enumerate() {
        if gpui::is_unbind(key_binding.action()) {
            continue;
        }

        let source = key_binding
            .meta()
            .map(KeybindSource::from_meta)
            .unwrap_or(KeybindSource::Unknown);
        let is_no_action = gpui::is_no_action(key_binding.action());
        let is_unbound_by_unbind =
            binding_is_unbound_by_unbind(key_binding, binding_index, &key_bindings);

        let context = key_binding
            .predicate()
            .map(|predicate| predicate.to_string().into());

        let action_name = key_binding.action().name();
        unmapped_action_names.remove(&action_name);

        let keystroke_text: SharedString =
            ui::text_for_keybinding_keystrokes(key_binding.keystrokes(), cx).into();

        processed_bindings.push(ProcessedBinding::Mapped(
            Box::new(KeybindInformation {
                keystrokes: Rc::from(key_binding.keystrokes()),
                keystroke_text,
                source,
                context,
                is_no_action,
                is_unbound_by_unbind,
            }),
            Box::new(ActionInformation {
                name: action_name,
                humanized_name: humanize_action_name(action_name),
                arguments: key_binding.action_input(),
                documentation: action_documentation.get(action_name).copied(),
            }),
        ));
    }

    for action_name in unmapped_action_names.into_iter() {
        processed_bindings.push(ProcessedBinding::Unmapped(Box::new(ActionInformation {
            name: action_name,
            humanized_name: humanize_action_name(action_name),
            arguments: None,
            documentation: action_documentation.get(action_name).copied(),
        })));
    }

    processed_bindings.sort_by(ProcessedBinding::cmp);
    processed_bindings
}

/// The state backing the keymap tab.
pub struct KeymapTab {
    bindings: Vec<ProcessedBinding>,
    conflict_state: ConflictState,
    matches: Vec<usize>,
    search_query: String,
    only_conflicts: bool,
    only_user: bool,
    editing: Option<KeybindingEditor>,
    status: Option<(SharedString, bool)>,
    write_in_progress: bool,
}

struct KeybindingEditor {
    index: Option<usize>,
    action_name: &'static str,
    action_arguments: Option<SharedString>,
    existing_keystrokes: Vec<gpui::KeybindingKeystroke>,
    existing_context: Option<String>,
    existing_source: KeybindSource,
    keystrokes: Entity<KeystrokeInput>,
    context: String,
    _keystroke_subscription: Subscription,
}

impl KeymapTab {
    pub fn empty() -> Self {
        Self {
            bindings: Vec::new(),
            conflict_state: ConflictState::default(),
            matches: Vec::new(),
            search_query: String::new(),
            only_conflicts: false,
            only_user: false,
            editing: None,
            status: None,
            write_in_progress: false,
        }
    }

    pub fn bindings(&self) -> &[ProcessedBinding] {
        &self.bindings
    }

    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    pub fn conflict_state(&self) -> &ConflictState {
        &self.conflict_state
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    pub fn editing_index(&self) -> Option<usize> {
        self.editing.as_ref().and_then(|editor| editor.index)
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn status(&self) -> Option<&(SharedString, bool)> {
        self.status.as_ref()
    }

    pub fn editing_keystrokes(&self) -> Option<&Entity<KeystrokeInput>> {
        self.editing.as_ref().map(|editor| &editor.keystrokes)
    }

    pub fn editing_context(&self) -> Option<&str> {
        self.editing.as_ref().map(|editor| editor.context.as_str())
    }

    /// Records the outcome of a finished write, replacing the in-progress
    /// status so the tab cannot stay stuck on "Saving keybinding…".
    pub fn clear_write_status(&mut self, message: &str) {
        self.write_in_progress = false;
        self.status = Some((message.into(), true));
    }

    /// Records a failed write, replacing the in-progress status.
    pub fn set_failure(&mut self, message: String) {
        self.write_in_progress = false;
        self.status = Some((message.into(), false));
    }

    /// Rebuilds the binding list and the derived conflict state.
    pub fn reload_bindings(&mut self, cx: &mut App) {
        self.bindings = process_bindings(cx);
        self.conflict_state = ConflictState::new(&self.bindings);
        self.refresh_matches();
    }

    fn refresh_matches(&mut self) {
        let query = self.search_query.to_lowercase();
        self.matches = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(index, binding)| {
                if self.only_conflicts && self.conflict_state.conflict_for_idx(*index).is_none() {
                    return false;
                }
                if self.only_user && binding.source() != Some(KeybindSource::User) {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                let action = binding.action();
                if action.humanized_name.to_lowercase().contains(&query)
                    || action.name.to_lowercase().contains(&query)
                {
                    return true;
                }
                if let ProcessedBinding::Mapped(keybind, _) = binding {
                    if keybind.keystroke_text.to_lowercase().contains(&query) {
                        return true;
                    }
                    if keybind
                        .keystrokes
                        .iter()
                        .map(|keystroke| keystroke.inner().unparse())
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_lowercase()
                        .contains(&query)
                    {
                        return true;
                    }
                }
                false
            })
            .map(|(index, _)| index)
            .collect();
    }

    pub fn set_search_query(
        &mut self,
        query: String,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        self.search_query = query;
        self.refresh_matches();
        cx.notify();
    }

    pub fn toggle_user_filter(&mut self, cx: &mut Context<super::settings_ui::SettingsPage>) {
        self.only_user = !self.only_user;
        self.refresh_matches();
        cx.notify();
    }

    pub fn toggle_conflict_filter(&mut self, cx: &mut Context<super::settings_ui::SettingsPage>) {
        self.only_conflicts = !self.only_conflicts;
        self.refresh_matches();
        cx.notify();
    }

    /// Starts editing a binding's keystrokes, or adding one for an unbound action.
    pub fn begin_edit(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        let Some(binding) = self.bindings.get(index) else {
            return;
        };
        let action_name = binding.action().name;
        let action_arguments = binding.action().arguments.clone();
        let existing_keystrokes = binding.keystrokes().unwrap_or_default().to_vec();
        let existing_context = binding.context_text().map(|context| context.to_string());
        let existing_source = binding.source().unwrap_or(KeybindSource::User);

        // The existing keystrokes are the *placeholder*, mirroring Zed: the
        // input itself starts empty so recording builds a fresh chord instead of
        // appending to the old one (`super-v` + `ctrl-shift-v`).
        let placeholder = (!existing_keystrokes.is_empty()).then(|| existing_keystrokes.clone());
        let keystrokes = cx.new(|cx| KeystrokeInput::new(placeholder, window, cx));
        let _keystroke_subscription =
            cx.subscribe_in(&keystrokes, window, |_, _, _: &(), _, cx| cx.notify());

        self.editing = Some(KeybindingEditor {
            // An action with no binding yet is an addition, not a replacement:
            // `Replace` would try to locate a binding that does not exist.
            index: (!binding.is_unbound()).then_some(index),
            action_name,
            action_arguments,
            existing_keystrokes,
            existing_context: existing_context.clone(),
            existing_source,
            keystrokes,
            context: existing_context.unwrap_or_default(),
            _keystroke_subscription,
        });
        self.status = None;
        cx.notify();
    }

    pub fn cancel_edit(&mut self, cx: &mut Context<super::settings_ui::SettingsPage>) {
        self.editing = None;
        cx.notify();
    }

    /// Validates the in-progress edit, warns about conflicts, and writes `keymap.json`.
    pub fn commit_edit(&mut self, cx: &mut Context<super::settings_ui::SettingsPage>) {
        let Some(editor) = self.editing.as_ref() else {
            return;
        };

        let mut keystrokes = editor.keystrokes.read(cx).keystrokes().to_vec();
        if keystrokes.is_empty() {
            self.status = Some(("Keystrokes cannot be empty.".into(), false));
            cx.notify();
            return;
        }
        for keystroke in &mut keystrokes {
            keystroke.remove_key_char();
        }
        if let Err(error) = gpui::KeyBindingContextPredicate::parse(&editor.context) {
            self.status = Some((format!("Invalid context: {error}").into(), false));
            cx.notify();
            return;
        }

        let action_mapping = ActionMapping {
            keystrokes: Rc::from(keystrokes.as_slice()),
            context: (!editor.context.is_empty()).then(|| editor.context.clone().into()),
        };

        if let Some(conflict) = self
            .conflict_state
            .conflicting_indices_for_mapping(&action_mapping, editor.index)
        {
            let conflicting_action = self
                .bindings
                .get(conflict.first_conflict_index)
                .map(|binding| binding.action().humanized_name.clone());
            let message = match conflicting_action {
                Some(name) if conflict.remaining_conflict_amount > 0 => format!(
                    "This binding conflicts with \"{name}\" and {} other binding(s).",
                    conflict.remaining_conflict_amount
                ),
                Some(name) => format!("This binding conflicts with \"{name}\"."),
                None => "This binding conflicts with another binding.".to_string(),
            };
            self.status = Some((message.into(), false));
            cx.notify();
            return;
        }

        let pending = PendingKeybindWrite {
            action_name: editor.action_name,
            action_arguments: editor.action_arguments.clone(),
            existing_keystrokes: editor.existing_keystrokes.clone(),
            existing_context: editor.existing_context.clone(),
            existing_source: editor.existing_source,
            creating: editor.index.is_none(),
            new_keystrokes: keystrokes,
            new_context: (!editor.context.is_empty()).then(|| editor.context.clone()),
        };

        self.editing = None;
        self.write_keybinding(pending, cx);
    }

    /// Starts the async write for `pending`; `SettingsPage` applies the result.
    fn write_keybinding(
        &mut self,
        pending: PendingKeybindWrite,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        if self.write_in_progress {
            return;
        }
        self.write_in_progress = true;
        self.status = Some(("Saving keybinding…".into(), true));
        cx.notify();

        let fs: Arc<dyn fs::Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
        let keyboard_mapper = cx.keyboard_mapper().clone();
        let deprecated_aliases = cx.deprecated_actions_to_preferred_actions().clone();

        let completion = cx.spawn(async move |page, cx| {
            let result =
                write_keybinding_file(pending, &fs, keyboard_mapper.as_ref(), &deprecated_aliases)
                    .await;
            page.update(cx, |page, cx| {
                page.keymap_tab.write_in_progress = false;
                match result {
                    Ok(contents) => page.apply_user_keymap(&contents, cx),
                    Err(error) => {
                        page.fail_keymap_write(format!("Could not save keybinding: {error}"), cx)
                    }
                }
            })
            .log_err();
        });
        completion.detach();
    }

    /// Restores `keymap.json` to the initial template, dropping all overrides.
    pub fn reset_to_defaults(&mut self, cx: &mut Context<super::settings_ui::SettingsPage>) {
        if self.write_in_progress {
            return;
        }
        self.write_in_progress = true;
        self.status = Some(("Resetting keymap…".into(), true));
        cx.notify();

        let fs: Arc<dyn fs::Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
        let contents = settings::initial_keymap_content().to_string();
        let reset_contents = contents.clone();
        let completion = cx.spawn(async move |page, cx| {
            let result = fs
                .write(paths::keymap_file().as_path(), contents.as_bytes())
                .await;
            page.update(cx, |page, cx| {
                page.keymap_tab.write_in_progress = false;
                match result {
                    Ok(()) => {
                        page.apply_user_keymap(&reset_contents, cx);
                        page.keymap_tab
                            .clear_write_status("Keymap reset to defaults.");
                    }
                    Err(error) => {
                        page.fail_keymap_write(format!("Could not reset keymap: {error}"), cx)
                    }
                }
            })
            .log_err();
        });
        completion.detach();
    }

    pub fn render(
        &mut self,
        search_input: AnyElement,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) -> AnyElement {
        if self.editing.is_some() {
            let editor = self.render_editor(cx);
            return v_flex()
                .id("settings-keymap-content")
                .w_full()
                .child(editor)
                .into_any_element();
        }

        let mut list = v_flex().id("keymap-list").w_full();
        if self.matches.is_empty() {
            list = list.child(
                v_flex().px_8().py_4().child(
                    Label::new("No matching key bindings.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            );
        } else {
            let matches = self.matches.clone();
            for index in matches {
                list = list.child(self.render_binding_row(index, cx));
            }
        }

        v_flex()
            .id("settings-keymap-content")
            .w_full()
            .child(self.render_toolbar(search_input, cx))
            .child(list)
            .into_any_element()
    }

    fn render_editor(&self, cx: &mut Context<super::settings_ui::SettingsPage>) -> AnyElement {
        let Some(editor) = self.editing.as_ref() else {
            return gpui::Empty.into_any_element();
        };
        let action_label = humanize_action_name(editor.action_name);
        let context = editor.context.clone();

        v_flex()
            .id("keymap-editor")
            .w_full()
            .px_8()
            .py_4()
            .gap_3()
            .child(
                Label::new(if editor.index.is_some() {
                    format!("Change keybinding for {action_label}")
                } else {
                    format!("Add keybinding for {action_label}")
                })
                .size(LabelSize::Small),
            )
            .child(editor.keystrokes.clone())
            .child(
                Label::new(
                    "Click the ▶ button (or press Enter) to record, then press \
                     Escape three times to finish.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new("Context")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(if context.is_empty() {
                            "<global>".to_string()
                        } else {
                            context
                        })
                        .size(LabelSize::Small),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(self.editor_button(
                        "keymap-editor-save",
                        "Save",
                        true,
                        |tab, cx| tab.commit_edit(cx),
                        cx,
                    ))
                    .child(self.editor_button(
                        "keymap-editor-cancel",
                        "Cancel",
                        false,
                        |tab, cx| tab.cancel_edit(cx),
                        cx,
                    )),
            )
            .when_some(self.status.clone(), |this, (message, is_success)| {
                this.child(
                    Label::new(message)
                        .size(LabelSize::Small)
                        .color(if is_success {
                            Color::Default
                        } else {
                            Color::Error
                        }),
                )
            })
            .into_any_element()
    }

    fn editor_button(
        &self,
        id: &'static str,
        label: &'static str,
        primary: bool,
        on_click: impl Fn(&mut Self, &mut Context<super::settings_ui::SettingsPage>) + 'static,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .px_3()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .bg(if primary {
                colors.element_background
            } else {
                colors.editor_background
            })
            .hover(|style| style.bg(colors.ghost_element_hover))
            .child(
                Label::new(label)
                    .size(LabelSize::Small)
                    .color(Color::Default),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                on_click(&mut this.keymap_tab, cx);
            }))
            .into_any_element()
    }

    fn render_toolbar(
        &self,
        search_input: AnyElement,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) -> AnyElement {
        let status = self.status.clone();
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .px_8()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .child(search_input)
                    .child(
                        Label::new(format!("{} / {}", self.matches.len(), self.bindings.len()))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        div()
                            .debug_selector(|| "keymap-user-filter".to_string())
                            .child(
                                IconButton::new("keymap-user-filter", IconName::Person)
                                    .toggle_state(self.only_user)
                                    .tooltip(Tooltip::text("Show user overrides only"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.keymap_tab.toggle_user_filter(cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "keymap-conflict-filter".to_string())
                            .child(
                                IconButton::new("keymap-conflict-filter", IconName::Warning)
                                    .toggle_state(self.only_conflicts)
                                    .tooltip(Tooltip::text("Show conflicting bindings only"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.keymap_tab.toggle_conflict_filter(cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "keymap-reset-defaults".to_string())
                            .child(
                                IconButton::new("keymap-reset-defaults", IconName::RotateCcw)
                                    .tooltip(Tooltip::text("Reset keymap.json to defaults"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.keymap_tab.reset_to_defaults(cx);
                                    })),
                            ),
                    ),
            )
            .when_some(status, |this, (message, is_success)| {
                this.child(
                    h_flex()
                        .px_8()
                        .py_1()
                        .child(
                            Label::new(message)
                                .size(LabelSize::Small)
                                .color(if is_success {
                                    Color::Default
                                } else {
                                    Color::Error
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_binding_row(
        &self,
        index: usize,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) -> AnyElement {
        let Some(binding) = self.bindings.get(index) else {
            return gpui::Empty.into_any_element();
        };
        let colors = cx.theme().colors();
        let conflict = self.conflict_state.conflict_for_idx(index);
        let action = binding.action();

        let keystroke_control: AnyElement = match binding.keystrokes() {
            Some(keystrokes) => {
                ui::KeyBinding::from_keystrokes(Rc::from(keystrokes.to_vec()), false)
                    .into_any_element()
            }
            None => Label::new("Unbound")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
        };

        let context_label = binding
            .context_text()
            .map_or_else(|| "<global>".to_string(), |context| context.to_string());

        let source_label = match binding.source() {
            Some(KeybindSource::User) => "user",
            Some(KeybindSource::Base) => "base",
            Some(KeybindSource::Vim) => "vim",
            Some(KeybindSource::Default) => "default",
            Some(KeybindSource::Unknown) | None => "",
        };

        let mut row = h_flex()
            .id(("keymap-binding", index))
            .debug_selector(move || format!("keymap-binding-{index}"))
            .w_full()
            .px_8()
            .py_2()
            .gap_3()
            .items_center()
            .hover(|style| style.bg(colors.ghost_element_hover))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(action.humanized_name.clone()))
                    .child(
                        Label::new(action.name)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                div().flex_none().min_w_24().child(
                    Label::new(context_label)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div().flex_none().min_w_16().child(
                    Label::new(source_label)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            );

        if let Some(conflict) = conflict {
            row = row.child(
                IconButton::new(format!("keymap-conflict-{index}"), IconName::Warning)
                    .icon_color(Color::Conflict)
                    .tooltip(Tooltip::text(conflict_description(&conflict))),
            );
        }

        row.child(div().flex_none().min_w_32().child(keystroke_control))
            .child(
                div()
                    .debug_selector(move || format!("keymap-edit-{index}"))
                    .child(
                        IconButton::new(format!("keymap-edit-{index}"), IconName::Pencil)
                            .tooltip(Tooltip::text(if binding.is_unbound() {
                                "Add keybinding"
                            } else {
                                "Change keybinding"
                            }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_keymap_binding_edit(index, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

/// Owned form of a pending keymap write, so it can cross into the async task.
struct PendingKeybindWrite {
    action_name: &'static str,
    action_arguments: Option<SharedString>,
    existing_keystrokes: Vec<gpui::KeybindingKeystroke>,
    existing_context: Option<String>,
    existing_source: KeybindSource,
    creating: bool,
    new_keystrokes: Vec<gpui::KeybindingKeystroke>,
    new_context: Option<String>,
}

async fn write_keybinding_file(
    pending: PendingKeybindWrite,
    fs: &Arc<dyn fs::Fs>,
    keyboard_mapper: &dyn PlatformKeyboardMapper,
    deprecated_aliases: &HashMap<&'static str, &'static str>,
) -> anyhow::Result<String> {
    let keymap_contents = KeymapFile::load_keymap_file(fs)
        .await
        .map_err(|error| error.context("Failed to load keymap file"))?;
    let tab_size = settings::infer_json_indent_size(&keymap_contents);

    let existing_arguments = pending.action_arguments.as_deref();
    let target = settings::KeybindUpdateTarget {
        context: pending.existing_context.as_deref(),
        keystrokes: &pending.existing_keystrokes,
        action_name: pending.action_name,
        action_arguments: existing_arguments,
    };
    let source = settings::KeybindUpdateTarget {
        context: pending.new_context.as_deref(),
        keystrokes: &pending.new_keystrokes,
        action_name: pending.action_name,
        action_arguments: existing_arguments,
    };

    let operation = if pending.creating {
        settings::KeybindUpdateOperation::Add {
            source,
            from: Some(target),
        }
    } else {
        settings::KeybindUpdateOperation::Replace {
            target,
            target_keybind_source: pending.existing_source,
            source,
        }
    };

    let updated = KeymapFile::update_keybinding(
        operation,
        keymap_contents,
        tab_size,
        keyboard_mapper,
        deprecated_aliases,
    )
    .map_err(|error| error.context("Could not update keymap"))?;

    fs.write(paths::keymap_file().as_path(), updated.as_bytes())
        .await
        .map_err(|error| error.context("Failed to write keymap file"))?;
    Ok(updated)
}

/// Loads the user keymap from `keymap.json`, reporting any parse failure.
pub async fn load_user_keymap(fs: &Arc<dyn fs::Fs>) -> anyhow::Result<String> {
    KeymapFile::load_keymap_file(fs).await
}

/// Parses keymap text into user bindings, returning them plus an optional
/// error message describing entries that failed to load.
pub fn parse_user_bindings(contents: &str, cx: &App) -> (Vec<KeyBinding>, Option<String>) {
    match KeymapFile::load(contents, cx) {
        settings::KeymapFileLoadResult::Success { key_bindings } => (key_bindings, None),
        settings::KeymapFileLoadResult::SomeFailedToLoad {
            key_bindings,
            error_message,
        } => (key_bindings, Some(error_message.0)),
        settings::KeymapFileLoadResult::JsonParseFailure { error } => (
            Vec::new(),
            Some(format!("Could not parse keymap.json: {error}")),
        ),
    }
}

/// Describes a conflict from the perspective of the row it is shown on.
///
/// Zed attributes a conflict to the *losing* binding: `override_source` is the
/// source that wins, and `overridden_source` is this row's own source.
fn conflict_description(conflict: &ConflictOrigin) -> String {
    match conflict.overridden_source {
        Some(_) => format!(
            "Overridden by a {} binding",
            conflict.override_source.name().to_lowercase()
        ),
        None => "This binding takes precedence".to_string(),
    }
}

fn normalized_ctx_eq(
    a: &gpui::KeyBindingContextPredicate,
    b: &gpui::KeyBindingContextPredicate,
) -> bool {
    use gpui::KeyBindingContextPredicate::*;
    match (a, b) {
        (Identifier(a), Identifier(b)) => a == b,
        (Equal(a1, a2), Equal(b1, b2)) | (NotEqual(a1, a2), NotEqual(b1, b2)) => {
            (a1 == b1 && a2 == b2) || (a1 == b2 && a2 == b1)
        }
        (Descendant(a1, a2), Descendant(b1, b2)) => {
            normalized_ctx_eq(a1, b1) && normalized_ctx_eq(a2, b2)
        }
        (Not(a), Not(b)) => normalized_ctx_eq(a, b),
        (And(a1, a2), And(b1, b2)) => {
            (normalized_ctx_eq(a1, b1) && normalized_ctx_eq(a2, b2))
                || (normalized_ctx_eq(a1, b2) && normalized_ctx_eq(a2, b1))
        }
        (Or(a1, a2), Or(b1, b2)) => {
            (normalized_ctx_eq(a1, b1) && normalized_ctx_eq(a2, b2))
                || (normalized_ctx_eq(a1, b2) && normalized_ctx_eq(a2, b1))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn humanized_action_names_split_on_case_boundaries() {
        assert_eq!(
            humanize_action_name("terminal_app::SplitRight"),
            "Split Right"
        );
        assert_eq!(humanize_action_name("SplitRight"), "Split Right");
        assert_eq!(humanize_action_name("NewTab"), "New Tab");
        assert_eq!(
            humanize_action_name("ActivateNextPane"),
            "Activate Next Pane"
        );
        assert_eq!(humanize_action_name("Copy"), "Copy");
    }

    #[test]
    fn context_predicates_compare_independently_of_operand_order() {
        let parse = |source: &str| gpui::KeyBindingContextPredicate::parse(source).unwrap();
        assert!(normalized_ctx_eq(&parse("a && b"), &parse("b && a")));
        assert!(normalized_ctx_eq(&parse("a || b"), &parse("b || a")));
        assert!(!normalized_ctx_eq(&parse("a && b"), &parse("a || b")));
        assert!(!normalized_ctx_eq(&parse("a"), &parse("b")));
    }

    /// End-to-end check that a rebind rewrites `keymap.json` with a user
    /// override that reloads as a `User`-sourced binding.
    #[gpui::test]
    async fn rebinding_writes_a_user_override_into_keymap_json(cx: &mut TestAppContext) {
        use std::path::Path;

        cx.update(|cx| {
            settings::init(cx);
            crate::bind_default_keys(cx);
        });

        let fake_fs = fs::FakeFs::new(cx.background_executor.clone());
        fake_fs
            .insert_tree(
                Path::new("/config"),
                serde_json::json!({ "keymap.json": "[]\n" }),
            )
            .await;
        let fs: Arc<dyn fs::Fs> = fake_fs;

        let pending = PendingKeybindWrite {
            action_name: "terminal_app::NewTab",
            action_arguments: None,
            existing_keystrokes: vec![gpui::KeybindingKeystroke::from_keystroke(
                gpui::Keystroke::parse("cmd-t").unwrap(),
            )],
            existing_context: Some("TerminalWindow".to_string()),
            existing_source: KeybindSource::Default,
            creating: false,
            new_keystrokes: vec![gpui::KeybindingKeystroke::from_keystroke(
                gpui::Keystroke::parse("cmd-shift-t").unwrap(),
            )],
            new_context: Some("TerminalWindow".to_string()),
        };

        let (keyboard_mapper, deprecated_aliases) = cx.update(|cx| {
            (
                cx.keyboard_mapper().clone(),
                cx.deprecated_actions_to_preferred_actions().clone(),
            )
        });

        // The file is read from and written back to the same fake path.
        let updated =
            write_keybinding_file(pending, &fs, keyboard_mapper.as_ref(), &deprecated_aliases)
                .await
                .expect("writing the keybinding should succeed");

        let parsed = settings::parse_json_with_comments::<serde_json::Value>(&updated)
            .expect("the rewritten keymap must stay valid JSONC");
        assert!(
            parsed
                .as_array()
                .expect("the keymap root is an array")
                .iter()
                .any(|section| section.get("bindings").is_some()),
            "a user binding section should have been written: {updated}"
        );

        cx.update(|cx| {
            let (bindings, error) = parse_user_bindings(&updated, cx);
            assert!(
                error.is_none(),
                "reloading should not report errors: {error:?}"
            );
            let new_tab = bindings
                .iter()
                .find(|binding| binding.action().name() == "terminal_app::NewTab")
                .expect("the reloaded keymap should contain the new binding");
            assert!(
                new_tab
                    .keystrokes()
                    .iter()
                    .any(|keystroke| keystroke.key() == "t"),
                "the new binding should use the recorded keystroke"
            );
        });
    }

    /// An action without any binding must take the add path: `Replace` would
    /// look for an existing binding and fail to find one.
    #[gpui::test]
    fn unbound_actions_take_the_add_path(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::bind_default_keys(cx);
        });

        let unbound = cx.update(|cx| {
            process_bindings(cx)
                .iter()
                .position(ProcessedBinding::is_unbound)
                .expect("some registered action should have no binding")
        });

        let (settings_page, cx) =
            cx.add_window_view(|_, cx| crate::settings_ui::tests::test_settings_page_stub(cx));
        settings_page.update(cx, |page, cx| page.keymap_tab.reload_bindings(cx));
        cx.run_until_parked();

        settings_page.update_in(cx, |page, window, cx| {
            page.keymap_tab.begin_edit(unbound, window, cx);
            assert!(
                page.keymap_tab.is_editing(),
                "editing an unbound action should open the editor"
            );
            assert_eq!(
                page.keymap_tab.editing_index(),
                None,
                "an unbound action must be recorded as an addition"
            );
        });
    }

    /// Regression: the editor must start with an *empty* recording buffer.
    /// Seeding the old keystrokes as the value made a re-record append to the
    /// previous chord (producing `super-v ctrl-shift-v`).
    #[gpui::test]
    fn rebind_starts_from_an_empty_recording(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::bind_default_keys(cx);
        });

        let (settings_page, cx) =
            cx.add_window_view(|_, cx| crate::settings_ui::tests::test_settings_page_stub(cx));
        settings_page.update(cx, |page, cx| page.keymap_tab.reload_bindings(cx));
        cx.run_until_parked();

        let (bound, existing_len) = settings_page.read_with(cx, |page, _| {
            let index = page
                .keymap_tab
                .bindings()
                .iter()
                .position(|binding| !binding.is_unbound())
                .expect("some binding should exist");
            let len = page.keymap_tab.bindings()[index]
                .keystrokes()
                .expect("a bound row has keystrokes")
                .len();
            (index, len)
        });

        settings_page.update_in(cx, |page, window, cx| {
            page.keymap_tab.begin_edit(bound, window, cx);
            assert!(page.keymap_tab.is_editing());
            assert_eq!(
                page.keymap_tab.editing_index(),
                Some(bound),
                "editing an existing binding must be a replacement"
            );
            let input = page
                .keymap_tab
                .editing_keystrokes()
                .expect("the editor should own a keystroke input")
                .read(cx);
            // `keystrokes()` deliberately falls back to the placeholder for
            // display, so the recorded buffer is what must be empty: a
            // pre-filled buffer would append to the old chord on re-record.
            assert!(
                input.recorded_keystrokes().is_empty(),
                "recording must start empty, not pre-filled with the old chord"
            );
            assert_eq!(
                input.keystrokes().len(),
                existing_len,
                "the previous chord should still be shown as a placeholder"
            );
        });
    }

    /// Regression: saving must always resolve the "Saving keybinding…" status.
    #[gpui::test]
    fn write_status_is_cleared_on_success_and_failure(cx: &mut TestAppContext) {
        let mut tab = KeymapTab::empty();
        tab.status = Some(("Saving keybinding…".into(), true));
        tab.write_in_progress = true;

        tab.clear_write_status("Keybinding saved.");
        assert!(
            !tab.write_in_progress,
            "a finished write must release the lock"
        );
        assert_eq!(
            tab.status().map(|(message, _)| message.to_string()),
            Some("Keybinding saved.".to_string())
        );

        tab.write_in_progress = true;
        tab.set_failure("Could not save keybinding: disk on fire".to_string());
        assert!(!tab.write_in_progress);
        assert_eq!(
            tab.status().map(|(_, success)| *success),
            Some(false),
            "a failed write must be reported as a failure"
        );
        let _ = cx;
    }

    /// The recorder looks up its stop binding in the window's keymap, so
    /// without a `KeystrokeInput`-context binding recording could be started
    /// but never stopped.
    #[gpui::test]
    fn keystroke_recorder_bindings_are_registered(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::bind_default_keys(cx);
        });
        let (settings_page, cx) =
            cx.add_window_view(|_, cx| crate::settings_ui::tests::test_settings_page_stub(cx));
        cx.run_until_parked();

        settings_page.update_in(cx, |_, window, cx| {
            let key_context = gpui::KeyContext::parse("KeystrokeInput").unwrap();
            let stop = window
                .highest_precedence_binding_for_action_in_context(&StopRecording, key_context)
                .expect("stopping the recorder must be bound");
            assert_eq!(
                stop.keystrokes().len(),
                3,
                "the stop binding is the three-escape sequence"
            );
            let _ = cx;
        });
    }

    /// Built-in bindings alone must not be reported as user conflicts, even
    /// though the same chord is commonly bound in several contexts.
    #[gpui::test]
    fn built_in_bindings_do_not_conflict_with_themselves(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            crate::bind_default_keys(cx);
        });
        cx.update(|cx| {
            let bindings = process_bindings(cx);
            assert!(!bindings.is_empty(), "expected built-in bindings");
            let conflict_state = ConflictState::new(&bindings);
            assert!(
                !conflict_state.any_user_binding_conflicts(),
                "built-in bindings must not report user conflicts"
            );
        });
    }

    /// Every built-in action should be discoverable in the processed list.
    #[gpui::test]
    fn process_bindings_covers_all_registered_actions(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            crate::bind_default_keys(cx);
        });
        cx.update(|cx| {
            let names = process_bindings(cx)
                .iter()
                .map(|binding| binding.action().name)
                .collect::<HashSet<_>>();
            for action_name in cx.all_action_names() {
                assert!(
                    names.contains(action_name),
                    "action {action_name} should be present in the keymap list"
                );
            }
        });
    }

    /// A user override at the same chord as a built-in binding is a conflict.
    #[gpui::test]
    fn user_override_of_a_built_in_binding_is_flagged(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            crate::bind_default_keys(cx);
        });
        cx.update(|cx| {
            let mut user_binding = KeyBinding::new("cmd-t", crate::NewTab, Some("TerminalWindow"));
            user_binding.set_meta(KeybindSource::User.meta());
            cx.bind_keys([user_binding]);

            let bindings = process_bindings(cx);
            let conflict_state = ConflictState::new(&bindings);
            let user_index = bindings
                .iter()
                .position(|binding| {
                    binding.action().name == "terminal_app::NewTab"
                        && binding.source() == Some(KeybindSource::User)
                })
                .expect("user binding should be listed");
            // The conflict is attributed to the losing (default) row, and the
            // winning source is the user override.
            let default_index = bindings
                .iter()
                .position(|binding| {
                    binding.action().name == "terminal_app::NewTab"
                        && binding.source() == Some(KeybindSource::Default)
                })
                .expect("built-in binding should be listed");
            let conflict = conflict_state
                .conflict_for_idx(default_index)
                .expect("the shadowed default should be reported as a conflict");
            assert_eq!(conflict.override_source, KeybindSource::User);
            assert_eq!(conflict.overridden_source, Some(KeybindSource::Default));

            // The winning row is flagged as a user-level conflict overall.
            let winning = conflict_state
                .conflicting_indices_for_mapping(
                    &ActionMapping {
                        keystrokes: bindings[user_index]
                            .keystrokes()
                            .expect("user binding has keystrokes")
                            .to_vec()
                            .into(),
                        context: bindings[user_index].context_text().cloned(),
                    },
                    Some(user_index),
                )
                .expect("the mapping should still report the other binding");
            assert_eq!(
                bindings[winning.first_conflict_index].action().name,
                "terminal_app::NewTab"
            );
        });
    }

    /// Filters narrow the visible rows without mutating the binding list.
    #[gpui::test]
    fn search_filters_match_actions_and_keystrokes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            crate::bind_default_keys(cx);
        });
        cx.update(|cx| {
            let mut tab = KeymapTab::empty();
            tab.reload_bindings(cx);
            let total = tab.matches().len();
            assert!(total > 0);
            assert_eq!(total, tab.bindings().len());

            tab.search_query = "spl".to_string();
            tab.refresh_matches();
            assert!(
                !tab.matches().is_empty(),
                "searching 'spl' should match the split actions"
            );
            assert!(tab.matches().len() < total);

            // Keystroke search matches the platform-rendered text the user sees
            // (e.g. `Super-T` on Linux, `Command-T` on macOS).
            let rendered = tab
                .bindings()
                .iter()
                .find(|binding| binding.action().name == "terminal_app::NewTab")
                .and_then(|binding| binding.keystroke_text(cx))
                .expect("the New Tab binding should render keystrokes");
            tab.search_query = rendered.to_lowercase();
            tab.refresh_matches();
            assert!(
                !tab.matches().is_empty(),
                "searching {rendered:?} should match its binding"
            );
            assert!(
                tab.matches()
                    .iter()
                    .any(|&index| tab.bindings()[index].action().name == "terminal_app::NewTab"),
                "the New Tab binding should be among the matches"
            );
        });
    }
}
