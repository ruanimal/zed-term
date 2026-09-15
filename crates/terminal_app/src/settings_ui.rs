//! Settings page for the standalone terminal application.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use collections::HashMap;
use gpui::{
    App, AppContext as _, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, Decorations,
    Element, FocusHandle, GlobalElementId, InputHandler, InspectorElementId,
    InteractiveElement as _, IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, PromptLevel, Render, SharedString,
    Style, Styled as _, UTF16Selection, UpdateGlobal, WeakEntity, Window, WindowBounds, WindowKind,
    px, size,
};
use settings::Settings as _;
use settings::SettingsStore;
use terminal_core::terminal_settings::{
    CursorShape, DEFAULT_TERMINAL_FONT_FAMILY, DEFAULT_TERMINAL_FONT_SIZE, TerminalSettings,
};
use theme::{ActiveTheme as _, FontFamilyCache, ThemeRegistry};
use ui::prelude::*;
use ui::utils::TRAFFIC_LIGHT_PADDING;
use ui::{
    ButtonLink, Color, ContextMenu, Divider, DividerColor, DropdownMenu, Icon, IconButton,
    IconName, IconSize, Label, LabelSize, Switch, TabBar, ToggleState,
};
use util::{ResultExt, shell::Shell as TerminalShell};

use crate::window_chrome;
use crate::window_options;

mod render;

#[cfg(test)]
pub(crate) mod tests;

type OptionAction = Box<dyn Fn(&mut Window, &mut App) + 'static>;
type DropdownOptions = Vec<(&'static str, OptionAction)>;

fn element_id_for(title: &str) -> gpui::ElementId {
    format!("setting-{title}").into()
}

fn opt(
    label: &'static str,
    apply: impl Fn(&mut Window, &mut App) + 'static,
) -> (&'static str, OptionAction) {
    (label, Box::new(apply))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveStatus {
    Idle,
    Saving,
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsTab {
    Terminal,
    Keymap,
    Themes,
    About,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Keymap => "Keymap",
            Self::Themes => "Themes",
            Self::About => "About",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellMode {
    System,
    Program,
    WithArguments,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShellForm {
    mode: ShellMode,
    program: String,
    arguments: Vec<String>,
    title_override: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EnvironmentVariable {
    key: String,
    value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkingDirectoryMode {
    Home,
    Fixed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkingDirectoryForm {
    mode: WorkingDirectoryMode,
    directory: String,
    fell_back_from_workspace_setting: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EditableField {
    KeymapSearch,
    ThemesSearch,
    ShellProgram,
    ShellTitleOverride,
    ShellArgument(usize),
    EnvironmentKey(usize),
    EnvironmentValue(usize),
    WorkingDirectory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DirtySetting {
    Theme,
    ResetTerminalDefaults,
    FontFamily,
    FontSize,
    FontWeight,
    LineHeight,
    MinimumContrast,
    CursorShape,
    Blinking,
    OptionAsMeta,
    CopyOnSelect,
    KeepSelectionOnCopy,
    OpenLinksInMouseMode,
    AlternateScroll,
    Bell,
    Shell,
    Environment,
    WorkingDirectory,
    ScrollMultiplier,
    MaxScrollHistoryLines,
    Scrollbar,
}

#[derive(Clone)]
struct SettingsPageDraft {
    draft_settings: TerminalSettings,
    theme_name: String,
    font_family: String,
    shell_form: ShellForm,
    environment: Vec<EnvironmentVariable>,
    working_directory: WorkingDirectoryForm,
}

#[derive(Default)]
struct DraftRevisions {
    next_revision: u64,
    field_revisions: HashMap<DirtySetting, u64>,
}

impl DraftRevisions {
    fn record_edit(&mut self, setting: DirtySetting) -> u64 {
        self.next_revision = self.next_revision.wrapping_add(1);
        if self.next_revision == 0 {
            self.next_revision = 1;
        }
        self.field_revisions.insert(setting, self.next_revision);
        self.next_revision
    }
}

#[derive(Clone)]
struct CapturedSavePatch {
    draft: SettingsPageDraft,
    dirty_settings: Vec<DirtySetting>,
    shell: Option<settings::Shell>,
    environment: Option<HashMap<String, String>>,
    working_directory: Option<settings::WorkingDirectory>,
    appearance: theme::Appearance,
}

#[derive(Clone)]
enum CapturedPatch {
    Save(Box<CapturedSavePatch>),
}

struct PendingWrite {
    operation_id: u64,
    started_revision: u64,
    captured_field_revisions: HashMap<DirtySetting, u64>,
    #[cfg(test)]
    captured_patch: CapturedPatch,
}

pub struct SettingsPage {
    focus_handle: FocusHandle,
    titlebar_mouse_down: std::cell::Cell<bool>,
    active_tab: SettingsTab,
    pub(crate) keymap_tab: crate::keymap::KeymapTab,
    pub(crate) themes_tab: crate::themes_tab::ThemesTab,
    editing_field: Option<EditableField>,
    editing_selection: Range<usize>,
    editing_anchor: usize,
    editing_cursor: usize,
    marked_range: Option<Range<usize>>,
    draft_settings: TerminalSettings,
    theme_name: String,
    font_family: String,
    shell_form: ShellForm,
    environment: Vec<EnvironmentVariable>,
    working_directory: WorkingDirectoryForm,
    dirty_settings: Vec<DirtySetting>,
    draft_revisions: DraftRevisions,
    next_operation_id: u64,
    pending_write: Option<PendingWrite>,
    close_prompt_open: bool,
    save_status: SaveStatus,
    status_message: Option<String>,
    #[cfg(test)]
    settings_write_request_count: usize,
}

pub fn open_settings_window(cx: &mut App) {
    if let Some(existing_window) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<SettingsPage>())
    {
        existing_window
            .update(cx, |page, window, cx| {
                page.focus_handle.clone().focus(window, cx);
                window.activate_window();
            })
            .log_err();
        return;
    }

    let mut options = window_options(gpui::Bounds::centered(None, size(px(660.), px(700.)), cx));
    options.kind = WindowKind::Floating;
    options.window_bounds = Some(WindowBounds::Windowed(gpui::Bounds::centered(
        None,
        size(px(660.), px(700.)),
        cx,
    )));
    cx.open_window(options, |window, cx| {
        let view = cx.new(SettingsPage::new);
        let weak_view = view.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            match weak_view.update(cx, |view, cx| view.request_close(window, cx)) {
                Ok(should_close) => should_close,
                Err(error) => {
                    log::error!(
                        "could not update settings page while closing the window: {error:#}"
                    );
                    true
                }
            }
        });
        view.read(cx).focus_handle.clone().focus(window, cx);
        view
    })
    .log_err();
}

fn settings_page_draft(cx: &App) -> SettingsPageDraft {
    let theme_name = theme_settings::ThemeSettings::get_global(cx)
        .theme
        .name(theme::SystemAppearance::global(cx).0)
        .0
        .to_string();
    settings_page_draft_from_settings(TerminalSettings::get_global(cx).clone(), theme_name)
}

fn default_settings_page_draft(theme_name: String) -> Result<SettingsPageDraft, String> {
    let content: settings::SettingsContent =
        settings::parse_json_with_comments(&settings::default_settings())
            .map_err(|error| format!("Could not load terminal defaults: {error}"))?;
    Ok(settings_page_draft_from_settings(
        TerminalSettings::from_settings(&content),
        theme_name,
    ))
}

fn settings_page_draft_from_settings(
    draft_settings: TerminalSettings,
    theme_name: String,
) -> SettingsPageDraft {
    let shell_form = match draft_settings.shell.clone() {
        TerminalShell::System => ShellForm {
            mode: ShellMode::System,
            program: String::new(),
            arguments: Vec::new(),
            title_override: String::new(),
        },
        TerminalShell::Program(program) => ShellForm {
            mode: ShellMode::Program,
            program,
            arguments: Vec::new(),
            title_override: String::new(),
        },
        TerminalShell::WithArguments {
            program,
            args,
            title_override,
        } => ShellForm {
            mode: ShellMode::WithArguments,
            program,
            arguments: args,
            title_override: title_override.unwrap_or_default(),
        },
    };
    let working_directory = match draft_settings.working_directory.clone() {
        settings::WorkingDirectory::Always { directory } => WorkingDirectoryForm {
            mode: WorkingDirectoryMode::Fixed,
            directory,
            fell_back_from_workspace_setting: false,
        },
        settings::WorkingDirectory::AlwaysHome => WorkingDirectoryForm {
            mode: WorkingDirectoryMode::Home,
            directory: String::new(),
            fell_back_from_workspace_setting: false,
        },
        _ => WorkingDirectoryForm {
            mode: WorkingDirectoryMode::Home,
            directory: String::new(),
            fell_back_from_workspace_setting: true,
        },
    };
    let mut environment: Vec<_> = draft_settings
        .env
        .iter()
        .map(|(key, value)| EnvironmentVariable {
            key: key.clone(),
            value: value.clone(),
        })
        .collect();
    environment.sort_by(|left, right| left.key.cmp(&right.key));

    SettingsPageDraft {
        theme_name,
        font_family: draft_settings
            .font_family
            .as_ref()
            .map(|font_family| font_family.0.to_string())
            .unwrap_or_default(),
        draft_settings,
        shell_form,
        environment,
        working_directory,
    }
}

fn reset_terminal_overrides(content: &mut settings::SettingsContent) {
    content.terminal = None;
}

impl CapturedPatch {
    fn apply(&self, content: &mut settings::SettingsContent) {
        match self {
            CapturedPatch::Save(patch) => {
                let reset_terminal_defaults = patch
                    .dirty_settings
                    .contains(&DirtySetting::ResetTerminalDefaults);
                if reset_terminal_defaults {
                    reset_terminal_overrides(content);
                }

                if patch.dirty_settings.contains(&DirtySetting::Theme) {
                    theme_settings::set_theme(
                        content,
                        patch.draft.theme_name.clone(),
                        patch.appearance,
                        patch.appearance,
                    );
                }

                if !patch.dirty_settings.iter().any(|setting| {
                    !matches!(
                        setting,
                        DirtySetting::Theme | DirtySetting::ResetTerminalDefaults
                    )
                }) {
                    return;
                }

                let terminal = content
                    .terminal
                    .get_or_insert_with(settings::TerminalSettingsContent::default);
                for setting in &patch.dirty_settings {
                    match setting {
                        DirtySetting::Theme | DirtySetting::ResetTerminalDefaults => {}
                        DirtySetting::FontFamily => {
                            terminal.font_family =
                                (!patch.draft.font_family.is_empty()).then(|| {
                                    settings::FontFamilyName(patch.draft.font_family.clone().into())
                                });
                        }
                        DirtySetting::FontSize => {
                            terminal.font_size = patch
                                .draft
                                .draft_settings
                                .font_size
                                .map(f32::from)
                                .map(settings::FontSize);
                        }
                        DirtySetting::FontWeight => {
                            terminal.font_weight = patch
                                .draft
                                .draft_settings
                                .font_weight
                                .map(|weight| settings::FontWeightContent(weight.0));
                        }
                        DirtySetting::LineHeight => {
                            terminal.line_height =
                                Some(patch.draft.draft_settings.line_height.clone());
                        }
                        DirtySetting::MinimumContrast => {
                            terminal.minimum_contrast =
                                Some(patch.draft.draft_settings.minimum_contrast);
                        }
                        DirtySetting::CursorShape => {
                            terminal.cursor_shape =
                                Some(match patch.draft.draft_settings.cursor_shape {
                                    CursorShape::Block => settings::CursorShapeContent::Block,
                                    CursorShape::Underline => {
                                        settings::CursorShapeContent::Underline
                                    }
                                    CursorShape::Bar => settings::CursorShapeContent::Bar,
                                    CursorShape::Hollow => settings::CursorShapeContent::Hollow,
                                });
                        }
                        DirtySetting::Blinking => {
                            terminal.blinking = Some(patch.draft.draft_settings.blinking);
                        }
                        DirtySetting::OptionAsMeta => {
                            terminal.option_as_meta =
                                Some(patch.draft.draft_settings.option_as_meta);
                        }
                        DirtySetting::CopyOnSelect => {
                            terminal.copy_on_select =
                                Some(patch.draft.draft_settings.copy_on_select);
                        }
                        DirtySetting::KeepSelectionOnCopy => {
                            terminal.keep_selection_on_copy =
                                Some(patch.draft.draft_settings.keep_selection_on_copy);
                        }
                        DirtySetting::OpenLinksInMouseMode => {
                            terminal.open_links_in_mouse_mode =
                                Some(patch.draft.draft_settings.open_links_in_mouse_mode);
                        }
                        DirtySetting::AlternateScroll => {
                            terminal.alternate_scroll =
                                Some(patch.draft.draft_settings.alternate_scroll);
                        }
                        DirtySetting::Bell => {
                            terminal.bell = Some(patch.draft.draft_settings.bell);
                        }
                        DirtySetting::Shell => terminal.project.shell = patch.shell.clone(),
                        DirtySetting::Environment => {
                            terminal.project.env = patch.environment.clone();
                        }
                        DirtySetting::WorkingDirectory => {
                            terminal.project.working_directory = patch.working_directory.clone();
                        }
                        DirtySetting::ScrollMultiplier => {
                            terminal.scroll_multiplier =
                                Some(patch.draft.draft_settings.scroll_multiplier);
                        }
                        DirtySetting::MaxScrollHistoryLines => {
                            terminal.max_scroll_history_lines =
                                patch.draft.draft_settings.max_scroll_history_lines;
                        }
                        DirtySetting::Scrollbar => {
                            terminal.scrollbar.get_or_insert_default().show =
                                patch.draft.draft_settings.scrollbar.show;
                        }
                    }
                }
            }
        }
    }
}

impl SettingsPage {
    fn new(cx: &mut Context<Self>) -> Self {
        let font_family_cache = FontFamilyCache::global(cx);
        cx.spawn(async move |this, cx| {
            font_family_cache.prefetch(cx).await;
            this.update(cx, |_, cx| cx.notify())?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);

        let SettingsPageDraft {
            draft_settings,
            theme_name,
            font_family,
            shell_form,
            environment,
            working_directory,
        } = settings_page_draft(cx);
        Self {
            focus_handle: cx.focus_handle(),
            titlebar_mouse_down: std::cell::Cell::new(false),
            active_tab: SettingsTab::Terminal,
            keymap_tab: crate::keymap::KeymapTab::empty(),
            themes_tab: crate::themes_tab::ThemesTab::empty(),
            editing_field: None,
            editing_selection: 0..0,
            editing_anchor: 0,
            editing_cursor: 0,
            marked_range: None,
            draft_settings,
            theme_name,
            font_family,
            shell_form,
            environment,
            working_directory,
            dirty_settings: Vec::new(),
            draft_revisions: DraftRevisions::default(),
            next_operation_id: 0,
            pending_write: None,
            close_prompt_open: false,
            save_status: SaveStatus::Idle,
            status_message: None,
            #[cfg(test)]
            settings_write_request_count: 0,
        }
    }

    fn current_draft(&self) -> SettingsPageDraft {
        SettingsPageDraft {
            draft_settings: self.draft_settings.clone(),
            theme_name: self.theme_name.clone(),
            font_family: self.font_family.clone(),
            shell_form: self.shell_form.clone(),
            environment: self.environment.clone(),
            working_directory: self.working_directory.clone(),
        }
    }

    fn preview_draft(&self, cx: &mut Context<Self>) {
        let reset_terminal_defaults = self
            .dirty_settings
            .contains(&DirtySetting::ResetTerminalDefaults);
        let mut preview_settings = TerminalSettings::get_global(cx).clone();
        if reset_terminal_defaults {
            preview_settings = self.draft_settings.clone();
        } else {
            for setting in &self.dirty_settings {
                match setting {
                    DirtySetting::Theme
                    | DirtySetting::ResetTerminalDefaults
                    | DirtySetting::FontFamily
                    | DirtySetting::Shell
                    | DirtySetting::Environment
                    | DirtySetting::WorkingDirectory => {}
                    DirtySetting::FontSize => {
                        preview_settings.font_size = self.draft_settings.font_size;
                    }
                    DirtySetting::FontWeight => {
                        preview_settings.font_weight = self.draft_settings.font_weight;
                    }
                    DirtySetting::LineHeight => {
                        preview_settings.line_height = self.draft_settings.line_height.clone();
                    }
                    DirtySetting::MinimumContrast => {
                        preview_settings.minimum_contrast = self.draft_settings.minimum_contrast;
                    }
                    DirtySetting::CursorShape => {
                        preview_settings.cursor_shape = self.draft_settings.cursor_shape;
                    }
                    DirtySetting::Blinking => {
                        preview_settings.blinking = self.draft_settings.blinking;
                    }
                    DirtySetting::OptionAsMeta => {
                        preview_settings.option_as_meta = self.draft_settings.option_as_meta;
                    }
                    DirtySetting::CopyOnSelect => {
                        preview_settings.copy_on_select = self.draft_settings.copy_on_select;
                    }
                    DirtySetting::KeepSelectionOnCopy => {
                        preview_settings.keep_selection_on_copy =
                            self.draft_settings.keep_selection_on_copy;
                    }
                    DirtySetting::OpenLinksInMouseMode => {
                        preview_settings.open_links_in_mouse_mode =
                            self.draft_settings.open_links_in_mouse_mode;
                    }
                    DirtySetting::AlternateScroll => {
                        preview_settings.alternate_scroll = self.draft_settings.alternate_scroll;
                    }
                    DirtySetting::Bell => {
                        preview_settings.bell = self.draft_settings.bell;
                    }
                    DirtySetting::ScrollMultiplier => {
                        preview_settings.scroll_multiplier = self.draft_settings.scroll_multiplier;
                    }
                    DirtySetting::MaxScrollHistoryLines => {
                        preview_settings.max_scroll_history_lines =
                            self.draft_settings.max_scroll_history_lines;
                    }
                    DirtySetting::Scrollbar => {
                        preview_settings.scrollbar.show = self.draft_settings.scrollbar.show;
                    }
                }
            }
        }

        if self.dirty_settings.contains(&DirtySetting::FontFamily) {
            preview_settings.font_family = (!self.font_family.is_empty())
                .then(|| settings::FontFamilyName(self.font_family.clone().into()));
        }
        if self.dirty_settings.contains(&DirtySetting::Shell) {
            match validate_shell_form(&self.shell_form) {
                Ok(shell) => {
                    preview_settings.shell = match shell {
                        settings::Shell::System => TerminalShell::System,
                        settings::Shell::Program(program) => TerminalShell::Program(program),
                        settings::Shell::WithArguments {
                            program,
                            args,
                            title_override,
                        } => TerminalShell::WithArguments {
                            program,
                            args,
                            title_override,
                        },
                    };
                }
                Err(error) => log::debug!("Skipping shell settings preview: {error}"),
            }
        }
        if self.dirty_settings.contains(&DirtySetting::Environment) {
            match validate_environment(&self.environment) {
                Ok(environment) => preview_settings.env = environment,
                Err(error) => log::debug!("Skipping environment settings preview: {error}"),
            }
        }
        if self
            .dirty_settings
            .contains(&DirtySetting::WorkingDirectory)
        {
            match validate_working_directory_form(
                &self.working_directory,
                Some(paths::home_dir().as_path()),
                &RealDirectoryAccess,
            ) {
                Ok(working_directory) => preview_settings.working_directory = working_directory,
                Err(error) => log::debug!("Skipping working directory preview: {error}"),
            }
        }

        let appearance = theme::SystemAppearance::global(cx).0;
        let mut preview_theme = theme_settings::ThemeSettings::get_global(cx).clone();
        if self.dirty_settings.contains(&DirtySetting::Theme) {
            let theme_name = theme_settings::ThemeName(self.theme_name.clone().into());
            match &mut preview_theme.theme {
                theme_settings::ThemeSelection::Static(current) => *current = theme_name,
                theme_settings::ThemeSelection::Dynamic { mode, light, dark } => {
                    match appearance {
                        theme::Appearance::Light => *light = theme_name,
                        theme::Appearance::Dark => *dark = theme_name,
                    }
                    if *mode != theme_settings::ThemeAppearanceMode::System {
                        *mode = theme_settings::appearance_to_mode(appearance);
                    }
                }
            }
        }

        SettingsStore::update_global(cx, |store, _| {
            store.override_global(preview_settings);
            store.override_global(preview_theme);
        });
        if cx.try_global::<theme::GlobalTheme>().is_some()
            && theme::ThemeRegistry::try_global(cx).is_some()
        {
            theme_settings::reload_theme(cx);
        } else {
            cx.refresh_windows();
        }
    }

    fn restore_preview(&self, cx: &mut Context<Self>) {
        SettingsStore::update_global(cx, |store, _| {
            if !store.recompute_setting::<TerminalSettings>() {
                log::error!("TerminalSettings is not registered while restoring settings preview");
            }
            if !store.recompute_setting::<theme_settings::ThemeSettings>() {
                log::error!("ThemeSettings is not registered while restoring settings preview");
            }
        });
        if cx.try_global::<theme::GlobalTheme>().is_some()
            && theme::ThemeRegistry::try_global(cx).is_some()
        {
            theme_settings::reload_theme(cx);
        } else {
            cx.refresh_windows();
        }
    }

    fn replace_draft(&mut self, draft: SettingsPageDraft) {
        self.draft_settings = draft.draft_settings;
        self.theme_name = draft.theme_name;
        self.font_family = draft.font_family;
        self.shell_form = draft.shell_form;
        self.environment = draft.environment;
        self.working_directory = draft.working_directory;
        self.clear_active_edit();
    }

    fn replay_field(&mut self, setting: DirtySetting, source: &SettingsPageDraft) {
        match setting {
            DirtySetting::Theme => self.theme_name.clone_from(&source.theme_name),
            DirtySetting::ResetTerminalDefaults => {}
            DirtySetting::FontFamily => self.font_family.clone_from(&source.font_family),
            DirtySetting::FontSize => {
                self.draft_settings.font_size = source.draft_settings.font_size;
            }
            DirtySetting::FontWeight => {
                self.draft_settings.font_weight = source.draft_settings.font_weight;
            }
            DirtySetting::LineHeight => self
                .draft_settings
                .line_height
                .clone_from(&source.draft_settings.line_height),
            DirtySetting::MinimumContrast => {
                self.draft_settings.minimum_contrast = source.draft_settings.minimum_contrast;
            }
            DirtySetting::CursorShape => {
                self.draft_settings.cursor_shape = source.draft_settings.cursor_shape;
            }
            DirtySetting::Blinking => {
                self.draft_settings.blinking = source.draft_settings.blinking;
            }
            DirtySetting::OptionAsMeta => {
                self.draft_settings.option_as_meta = source.draft_settings.option_as_meta;
            }
            DirtySetting::CopyOnSelect => {
                self.draft_settings.copy_on_select = source.draft_settings.copy_on_select;
            }
            DirtySetting::KeepSelectionOnCopy => {
                self.draft_settings.keep_selection_on_copy =
                    source.draft_settings.keep_selection_on_copy;
            }
            DirtySetting::OpenLinksInMouseMode => {
                self.draft_settings.open_links_in_mouse_mode =
                    source.draft_settings.open_links_in_mouse_mode;
            }
            DirtySetting::AlternateScroll => {
                self.draft_settings.alternate_scroll = source.draft_settings.alternate_scroll;
            }
            DirtySetting::Bell => self.draft_settings.bell = source.draft_settings.bell,
            DirtySetting::Shell => self.shell_form.clone_from(&source.shell_form),
            DirtySetting::Environment => self.environment.clone_from(&source.environment),
            DirtySetting::WorkingDirectory => {
                self.working_directory.clone_from(&source.working_directory)
            }
            DirtySetting::ScrollMultiplier => {
                self.draft_settings.scroll_multiplier = source.draft_settings.scroll_multiplier;
            }
            DirtySetting::MaxScrollHistoryLines => {
                self.draft_settings.max_scroll_history_lines =
                    source.draft_settings.max_scroll_history_lines;
            }
            DirtySetting::Scrollbar => {
                self.draft_settings.scrollbar.show = source.draft_settings.scrollbar.show;
            }
        }
    }

    fn reload_drafts_from_globals(&mut self, cx: &mut Context<Self>) {
        self.replace_draft(settings_page_draft(cx));
        self.dirty_settings.clear();
        self.draft_revisions.field_revisions.clear();
    }

    fn discard_drafts(&mut self, cx: &mut Context<Self>) {
        if self.pending_write.is_some() {
            return;
        }
        self.restore_preview(cx);
        self.reload_drafts_from_globals(cx);
        self.save_status = SaveStatus::Idle;
        self.status_message = Some("Unsaved changes discarded.".to_string());
        cx.notify();
    }

    fn begin_write(
        &mut self,
        _captured_patch: CapturedPatch,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        if self.pending_write.is_some() {
            return None;
        }

        let Some(operation_id) = self.next_operation_id.checked_add(1) else {
            self.save_status = SaveStatus::Failed;
            self.status_message = Some("Could not start another settings write.".to_string());
            cx.notify();
            return None;
        };
        self.next_operation_id = operation_id;
        self.pending_write = Some(PendingWrite {
            operation_id,
            started_revision: self.draft_revisions.next_revision,
            captured_field_revisions: self.draft_revisions.field_revisions.clone(),
            #[cfg(test)]
            captured_patch: _captured_patch,
        });
        self.save_status = SaveStatus::Saving;
        self.status_message = Some("Saving settings…".to_string());
        #[cfg(test)]
        {
            self.settings_write_request_count = self.settings_write_request_count.wrapping_add(1);
        }
        cx.notify();
        Some(operation_id)
    }

    fn finish_write(
        &mut self,
        operation_id: u64,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .pending_write
            .as_ref()
            .map(|pending| pending.operation_id)
            != Some(operation_id)
        {
            return false;
        }
        let Some(pending) = self.pending_write.take() else {
            return false;
        };

        match result {
            Ok(()) => {
                let edited_draft = self.current_draft();
                let newer_fields: Vec<_> = self
                    .draft_revisions
                    .field_revisions
                    .iter()
                    .filter_map(|(setting, revision)| {
                        let captured_revision = pending
                            .captured_field_revisions
                            .get(setting)
                            .copied()
                            .unwrap_or_default();
                        (*revision > pending.started_revision && *revision > captured_revision)
                            .then_some((*setting, *revision))
                    })
                    .collect();

                self.replace_draft(settings_page_draft(cx));
                self.dirty_settings.clear();
                self.draft_revisions.field_revisions.clear();
                for (setting, revision) in newer_fields {
                    self.replay_field(setting, &edited_draft);
                    self.dirty_settings.push(setting);
                    self.draft_revisions
                        .field_revisions
                        .insert(setting, revision);
                }

                self.save_status = SaveStatus::Succeeded;
                self.status_message = Some(if self.dirty_settings.is_empty() {
                    "Settings saved.".to_string()
                } else {
                    "Settings saved. Additional changes remain.".to_string()
                });
                if !self.dirty_settings.is_empty() {
                    self.preview_draft(cx);
                }
                cx.refresh_windows();
            }
            Err(error) => {
                self.save_status = SaveStatus::Failed;
                self.status_message = Some(format!("Could not save settings: {error}"));
            }
        }
        cx.notify();
        true
    }

    fn reset_terminal_defaults(&mut self, cx: &mut Context<Self>) {
        if self.pending_write.is_some() {
            return;
        }
        let draft = match default_settings_page_draft(self.theme_name.clone()) {
            Ok(draft) => draft,
            Err(error) => {
                self.fail_validation(error, cx);
                return;
            }
        };
        let theme_was_dirty = self.dirty_settings.contains(&DirtySetting::Theme);
        self.replace_draft(draft);
        self.dirty_settings.clear();
        self.draft_revisions.field_revisions.clear();
        if theme_was_dirty {
            self.dirty_settings.push(DirtySetting::Theme);
            self.draft_revisions.record_edit(DirtySetting::Theme);
        }
        self.dirty_settings
            .push(DirtySetting::ResetTerminalDefaults);
        self.draft_revisions
            .record_edit(DirtySetting::ResetTerminalDefaults);
        self.save_status = SaveStatus::Idle;
        self.status_message = None;
        self.preview_draft(cx);
        cx.notify();
    }

    /// Opens the key binding editor for `index`, taking over text input from
    /// the page: while recording keystrokes the IME handler must not keep
    /// appending them to whichever inline field was last active.
    pub(crate) fn begin_keymap_binding_edit(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_active_edit();
        self.keymap_tab.begin_edit(index, window, cx);
    }

    /// Rebuilds the app keymap from `contents` and refreshes the Keymap tab.
    pub(crate) fn apply_user_keymap(&mut self, contents: &str, cx: &mut Context<Self>) {
        crate::reload_keymaps(contents, cx);
        self.keymap_tab.reload_bindings(cx);
        self.keymap_tab.clear_write_status("Keybinding saved.");
        cx.notify();
    }

    /// Reports a failed keymap write without discarding the user's edit state.
    pub(crate) fn fail_keymap_write(&mut self, error: String, cx: &mut Context<Self>) {
        self.keymap_tab.set_failure(error);
        cx.notify();
    }

    /// Refreshes the Keymap tab after the keymap changed outside this page.
    pub(crate) fn refresh_keymap_tab(&mut self, cx: &mut Context<Self>) {
        self.keymap_tab.reload_bindings(cx);
        cx.notify();
    }

    pub(crate) fn clear_active_edit(&mut self) {
        self.editing_field = None;
        self.editing_selection = 0..0;
        self.editing_anchor = 0;
        self.editing_cursor = 0;
        self.marked_range = None;
    }

    fn begin_edit(&mut self, field: EditableField, window: &mut Window, cx: &mut Context<Self>) {
        let field_changed = self.editing_field.as_ref() != Some(&field);
        self.editing_field = Some(field);
        if field_changed {
            let length = self.editing_text().encode_utf16().count();
            self.editing_selection = 0..length;
            self.editing_anchor = 0;
            self.editing_cursor = length;
            self.marked_range = None;
        }
        if self.pending_write.is_none() {
            self.status_message = None;
        }
        self.focus_handle.clone().focus(window, cx);
        cx.notify();
    }

    fn editing_text(&self) -> String {
        match self.editing_field.as_ref() {
            Some(EditableField::KeymapSearch) => self.keymap_tab.search_query().to_string(),
            Some(EditableField::ThemesSearch) => self.themes_tab.search_query().to_string(),
            Some(EditableField::ShellProgram) => self.shell_form.program.clone(),
            Some(EditableField::ShellTitleOverride) => self.shell_form.title_override.clone(),
            Some(EditableField::ShellArgument(index)) => self
                .shell_form
                .arguments
                .get(*index)
                .cloned()
                .unwrap_or_default(),
            Some(EditableField::EnvironmentKey(index)) => self
                .environment
                .get(*index)
                .map(|variable| variable.key.clone())
                .unwrap_or_default(),
            Some(EditableField::EnvironmentValue(index)) => self
                .environment
                .get(*index)
                .map(|variable| variable.value.clone())
                .unwrap_or_default(),
            Some(EditableField::WorkingDirectory) => self.working_directory.directory.clone(),
            None => String::new(),
        }
    }

    fn replace_editing_text(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: String,
        new_selected_range: Option<Range<usize>>,
        mark_text: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(field) = self.editing_field.clone() else {
            return;
        };
        let current_text = self.editing_text();
        let range = replacement_range
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.editing_selection.clone());
        let range_start = range.start.min(current_text.encode_utf16().count());
        let (updated_text, cursor) = replace_utf16_range(&current_text, range, &text);
        let dirty_setting = match field {
            EditableField::KeymapSearch => {
                self.keymap_tab.set_search_query(updated_text, cx);
                None
            }
            EditableField::ThemesSearch => {
                self.themes_tab.set_search_query(updated_text, cx);
                None
            }
            EditableField::ShellProgram => {
                self.shell_form.program = updated_text;
                Some(DirtySetting::Shell)
            }
            EditableField::ShellTitleOverride => {
                self.shell_form.title_override = updated_text;
                Some(DirtySetting::Shell)
            }
            EditableField::ShellArgument(index) => {
                if let Some(argument) = self.shell_form.arguments.get_mut(index) {
                    *argument = updated_text;
                }
                Some(DirtySetting::Shell)
            }
            EditableField::EnvironmentKey(index) => {
                if let Some(variable) = self.environment.get_mut(index) {
                    variable.key = updated_text;
                }
                Some(DirtySetting::Environment)
            }
            EditableField::EnvironmentValue(index) => {
                if let Some(variable) = self.environment.get_mut(index) {
                    variable.value = updated_text;
                }
                Some(DirtySetting::Environment)
            }
            EditableField::WorkingDirectory => {
                self.working_directory.directory = updated_text;
                Some(DirtySetting::WorkingDirectory)
            }
        };
        self.marked_range = mark_text
            .then_some(range_start..cursor)
            .filter(|range| !range.is_empty());
        self.editing_selection = new_selected_range
            .map(|range| range_start + range.start..range_start + range.end)
            .unwrap_or(cursor..cursor);
        self.editing_anchor = self.editing_selection.start;
        self.editing_cursor = self.editing_selection.end;
        if let Some(setting) = dirty_setting {
            self.mark_dirty(setting, cx);
        } else {
            cx.notify();
        }
    }

    fn update_editing_cursor(
        &mut self,
        cursor: usize,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let length = self.editing_text().encode_utf16().count();
        let cursor = cursor.min(length);
        if extend_selection {
            self.editing_cursor = cursor;
            self.editing_selection = if self.editing_anchor <= cursor {
                self.editing_anchor..cursor
            } else {
                cursor..self.editing_anchor
            };
        } else {
            self.editing_anchor = cursor;
            self.editing_cursor = cursor;
            self.editing_selection = cursor..cursor;
        }
        self.marked_range = None;
        cx.notify();
    }

    fn move_editing_horizontally(
        &mut self,
        move_left: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        if self.editing_field.is_none() {
            return;
        }
        let current_text = self.editing_text();
        let target = if !extend_selection && !self.editing_selection.is_empty() {
            if move_left {
                self.editing_selection.start
            } else {
                self.editing_selection.end
            }
        } else if move_left {
            previous_utf16_boundary(&current_text, self.editing_cursor)
        } else {
            next_utf16_boundary(&current_text, self.editing_cursor)
        };
        self.update_editing_cursor(target, extend_selection, cx);
    }

    fn move_editing_to_boundary(
        &mut self,
        move_to_start: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        if self.editing_field.is_none() {
            return;
        }
        let target = if move_to_start {
            0
        } else {
            self.editing_text().encode_utf16().count()
        };
        self.update_editing_cursor(target, extend_selection, cx);
    }

    fn delete_editing_backward(&mut self, cx: &mut Context<Self>) {
        if self.editing_field.is_none() {
            return;
        }
        self.marked_range = None;
        if self.editing_selection.is_empty() {
            let previous = previous_utf16_boundary(&self.editing_text(), self.editing_cursor);
            if previous == self.editing_cursor {
                return;
            }
            self.editing_selection = previous..self.editing_cursor;
        }
        self.replace_editing_text(None, String::new(), None, false, cx);
    }

    fn delete_editing_forward(&mut self, cx: &mut Context<Self>) {
        if self.editing_field.is_none() {
            return;
        }
        self.marked_range = None;
        if self.editing_selection.is_empty() {
            let next = next_utf16_boundary(&self.editing_text(), self.editing_cursor);
            if next == self.editing_cursor {
                return;
            }
            self.editing_selection = self.editing_cursor..next;
        }
        self.replace_editing_text(None, String::new(), None, false, cx);
    }

    fn select_all_editing_text(&mut self, cx: &mut Context<Self>) {
        if self.editing_field.is_none() {
            return;
        }
        let length = self.editing_text().encode_utf16().count();
        self.editing_anchor = 0;
        self.editing_cursor = length;
        self.editing_selection = 0..length;
        self.marked_range = None;
        cx.notify();
    }

    fn copy_editing_selection(&self, cx: &mut App) {
        if self.editing_selection.is_empty() {
            return;
        }
        let text = substring_utf16(&self.editing_text(), self.editing_selection.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn paste_editing_text(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|text| text.replace(['\n', '\r'], " "))
        else {
            return;
        };
        self.replace_editing_text(None, text, None, false, cx);
    }

    fn cut_editing_selection(&mut self, cx: &mut Context<Self>) {
        if self.editing_selection.is_empty() {
            return;
        }
        self.copy_editing_selection(cx);
        self.replace_editing_text(None, String::new(), None, false, cx);
    }

    fn editable_fields(&self) -> Vec<EditableField> {
        let mut fields = Vec::new();
        if self.active_tab == SettingsTab::Keymap && !self.keymap_tab.is_editing() {
            fields.push(EditableField::KeymapSearch);
        }
        if self.active_tab == SettingsTab::Themes && !self.themes_tab.is_busy() {
            fields.push(EditableField::ThemesSearch);
        }
        if self.shell_form.mode != ShellMode::System {
            fields.push(EditableField::ShellProgram);
        }
        if self.shell_form.mode == ShellMode::WithArguments {
            fields.push(EditableField::ShellTitleOverride);
            fields.extend((0..self.shell_form.arguments.len()).map(EditableField::ShellArgument));
        }
        for index in 0..self.environment.len() {
            fields.push(EditableField::EnvironmentKey(index));
            fields.push(EditableField::EnvironmentValue(index));
        }
        if self.working_directory.mode == WorkingDirectoryMode::Fixed {
            fields.push(EditableField::WorkingDirectory);
        }
        fields
    }

    fn move_to_adjacent_editing_field(
        &mut self,
        move_forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fields = self.editable_fields();
        if fields.is_empty() {
            return;
        }
        let current_index = self
            .editing_field
            .as_ref()
            .and_then(|field| fields.iter().position(|candidate| candidate == field));
        let next_index = match current_index {
            Some(index) if move_forward => (index + 1) % fields.len(),
            Some(0) => fields.len() - 1,
            Some(index) => index - 1,
            None if move_forward => 0,
            None => fields.len() - 1,
        };
        if let Some(field) = fields.into_iter().nth(next_index) {
            self.begin_edit(field, window, cx);
        }
    }

    fn fail_validation(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.save_status = SaveStatus::Failed;
        self.status_message = Some(message.into());
        cx.notify();
    }

    fn mark_dirty(&mut self, setting: DirtySetting, cx: &mut Context<Self>) {
        if !self.dirty_settings.contains(&setting) {
            self.dirty_settings.push(setting);
        }
        self.draft_revisions.record_edit(setting);
        if self.pending_write.is_none() {
            self.save_status = SaveStatus::Idle;
            self.status_message = None;
        }
        self.preview_draft(cx);
        cx.notify();
    }

    fn update_draft(
        &mut self,
        setting: DirtySetting,
        update: impl FnOnce(&mut TerminalSettings),
        cx: &mut Context<Self>,
    ) {
        update(&mut self.draft_settings);
        self.mark_dirty(setting, cx);
    }

    fn capture_save_patch(
        &self,
        home_directory: Option<&Path>,
        directory_access: &dyn DirectoryAccess,
        appearance: theme::Appearance,
    ) -> Result<CapturedPatch, String> {
        let dirty_settings = self.dirty_settings.clone();
        let shell = dirty_settings
            .contains(&DirtySetting::Shell)
            .then(|| validate_shell_form(&self.shell_form))
            .transpose()?;
        let environment = dirty_settings
            .contains(&DirtySetting::Environment)
            .then(|| validate_environment(&self.environment))
            .transpose()?;
        let working_directory = dirty_settings
            .contains(&DirtySetting::WorkingDirectory)
            .then(|| {
                validate_working_directory_form(
                    &self.working_directory,
                    home_directory,
                    directory_access,
                )
            })
            .transpose()?;

        let mut draft = self.current_draft();
        draft.font_family = draft.font_family.trim().to_string();
        Ok(CapturedPatch::Save(Box::new(CapturedSavePatch {
            draft,
            dirty_settings,
            shell,
            environment,
            working_directory,
            appearance,
        })))
    }

    fn save_drafts(&mut self, cx: &mut Context<Self>) {
        if self.pending_write.is_some() {
            return;
        }
        if self.dirty_settings.is_empty() {
            self.save_status = SaveStatus::Succeeded;
            self.status_message = Some("No settings changes to save.".to_string());
            cx.notify();
            return;
        }

        let captured_patch = match self.capture_save_patch(
            Some(paths::home_dir().as_path()),
            &RealDirectoryAccess,
            theme::SystemAppearance::global(cx).0,
        ) {
            Ok(captured_patch) => captured_patch,
            Err(error) => {
                self.fail_validation(error, cx);
                return;
            }
        };
        let Some(operation_id) = self.begin_write(captured_patch.clone(), cx) else {
            return;
        };

        let fs: Arc<dyn fs::Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
        let completion = SettingsStore::update_global(cx, |store, _| {
            store.update_settings_file_with_completion(fs, move |content, _| {
                captured_patch.apply(content);
            })
        });

        cx.spawn(async move |this, cx| {
            let result = match completion.await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => Err("The settings update was canceled.".to_string()),
            };
            this.update(cx, |this, cx| {
                this.finish_write(operation_id, result, cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.pending_write.is_some() {
            self.status_message = Some("Settings are still being saved.".to_string());
            cx.notify();
            return false;
        }
        if self.dirty_settings.is_empty() {
            return true;
        }
        if self.close_prompt_open {
            return false;
        }

        self.close_prompt_open = true;
        cx.notify();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let answer = match window_handle.update(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    "Discard unsaved settings changes?",
                    Some("Your changes have not been saved."),
                    &["Discard changes", "Cancel"],
                    cx,
                )
            }) {
                Ok(receiver) => receiver.await,
                Err(error) => {
                    log::error!("could not show the unsaved settings confirmation: {error:#}");
                    this.update(cx, |this, cx| {
                        this.close_prompt_open = false;
                        cx.notify();
                    })?;
                    return anyhow::Ok(());
                }
            };
            let discard_changes = matches!(answer, Ok(0));
            let should_remove = this.update(cx, |this, cx| {
                this.close_prompt_open = false;
                if discard_changes {
                    this.restore_preview(cx);
                }
                cx.notify();
                discard_changes
            })?;
            if should_remove {
                window_handle
                    .update(cx, |_, window, _| window.remove_window())
                    .log_err();
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
        false
    }

    fn commit_active_edit(&mut self, cx: &mut Context<Self>) {
        self.clear_active_edit();
        cx.notify();
    }
}

fn draft_setting_option(
    label: &'static str,
    page: WeakEntity<SettingsPage>,
    setting: DirtySetting,
    update: impl Fn(&mut TerminalSettings) + 'static,
) -> (&'static str, OptionAction) {
    opt(label, move |_window, cx| {
        page.update(cx, |this, cx| this.update_draft(setting, &update, cx))
            .log_err();
    })
}

fn previous_utf16_boundary(text: &str, offset: usize) -> usize {
    let mut previous = 0;
    let mut current = 0;
    for character in text.chars() {
        let next = current + character.len_utf16();
        if offset <= current {
            return previous;
        }
        if offset <= next {
            return current;
        }
        previous = current;
        current = next;
    }
    current
}

fn next_utf16_boundary(text: &str, offset: usize) -> usize {
    let mut current = 0;
    for character in text.chars() {
        let next = current + character.len_utf16();
        if offset < next {
            return next;
        }
        current = next;
    }
    current
}

fn byte_offset_for_utf16(text: &str, utf16_offset: usize) -> usize {
    let mut consumed_utf16 = 0;
    for (byte_offset, character) in text.char_indices() {
        if consumed_utf16 >= utf16_offset {
            return byte_offset;
        }
        let next = consumed_utf16 + character.len_utf16();
        if utf16_offset < next {
            return byte_offset;
        }
        consumed_utf16 = next;
    }
    text.len()
}

fn substring_utf16(text: &str, range: Range<usize>) -> String {
    let start = byte_offset_for_utf16(text, range.start);
    let end = byte_offset_for_utf16(text, range.end.max(range.start));
    text[start..end].to_string()
}

fn replace_utf16_range(text: &str, range: Range<usize>, replacement: &str) -> (String, usize) {
    let start_utf16 = range.start.min(text.encode_utf16().count());
    let end_utf16 = range.end.max(start_utf16).min(text.encode_utf16().count());
    let start = byte_offset_for_utf16(text, start_utf16);
    let end = byte_offset_for_utf16(text, end_utf16);
    let mut updated = String::with_capacity(text.len() + replacement.len());
    updated.push_str(&text[..start]);
    updated.push_str(replacement);
    updated.push_str(&text[end..]);
    (updated, start_utf16 + replacement.encode_utf16().count())
}

fn validate_shell_form(form: &ShellForm) -> Result<settings::Shell, String> {
    match form.mode {
        ShellMode::System => Ok(settings::Shell::System),
        ShellMode::Program => {
            let program = form.program.trim().to_string();
            if program.is_empty() {
                return Err("A custom shell program is required.".to_string());
            }
            if program.contains('\0') {
                return Err("A custom shell program cannot contain NUL.".to_string());
            }
            Ok(settings::Shell::Program(program))
        }
        ShellMode::WithArguments => {
            let program = form.program.trim().to_string();
            if program.is_empty() {
                return Err("A shell program is required.".to_string());
            }
            if program.contains('\0') {
                return Err("A shell program cannot contain NUL.".to_string());
            }
            if form
                .arguments
                .iter()
                .any(|argument| argument.contains('\0'))
            {
                return Err("Shell arguments cannot contain NUL.".to_string());
            }
            let title_override = form.title_override.trim().to_string();
            if title_override.contains('\0') {
                return Err("The title override cannot contain NUL.".to_string());
            }
            Ok(settings::Shell::WithArguments {
                program,
                args: form.arguments.clone(),
                title_override: (!title_override.is_empty()).then_some(title_override),
            })
        }
    }
}

fn validate_environment(
    entries: &[EnvironmentVariable],
) -> Result<collections::HashMap<String, String>, String> {
    let mut environment = collections::HashMap::default();
    for entry in entries {
        let key = entry.key.trim();
        if key.is_empty() {
            return Err("Environment variable names cannot be empty.".to_string());
        }
        if key.contains('=') {
            return Err("Environment variable names cannot contain `=`.".to_string());
        }
        if key.contains('\0') || entry.value.contains('\0') {
            return Err("Environment variable names and values cannot contain NUL.".to_string());
        }
        if environment
            .insert(key.to_string(), entry.value.clone())
            .is_some()
        {
            return Err("Environment variable names must be unique.".to_string());
        }
    }
    Ok(environment)
}

fn normalize_fixed_directory_input(
    input: &str,
    home_directory: Option<&Path>,
) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("A fixed working directory is required.".to_string());
    }
    if input.contains('\0') {
        return Err("A fixed working directory cannot contain NUL.".to_string());
    }

    let path = if input == "~" || input.starts_with("~/") {
        let home_directory = home_directory
            .ok_or_else(|| "The platform home directory is unavailable.".to_string())?;
        let suffix = input.strip_prefix('~').unwrap_or_default();
        home_directory.join(suffix.trim_start_matches('/'))
    } else {
        PathBuf::from(input)
    };
    if !path.is_absolute() {
        return Err("A fixed working directory must be absolute or begin with `~/`.".to_string());
    }
    Ok(path)
}

trait DirectoryAccess {
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf>;
    fn is_directory(&self, path: &Path) -> bool;
}

struct RealDirectoryAccess;

impl DirectoryAccess for RealDirectoryAccess {
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn is_directory(&self, path: &Path) -> bool {
        path.is_dir()
    }
}

fn validate_existing_directory(
    path: &Path,
    directory_access: &dyn DirectoryAccess,
) -> Result<PathBuf, String> {
    let canonical = directory_access
        .canonicalize(path)
        .map_err(|error| format!("The working directory does not exist: {error}"))?;
    if !directory_access.is_directory(&canonical) {
        return Err("The fixed working directory must be a directory.".to_string());
    }
    Ok(canonical)
}

fn validate_working_directory_form(
    form: &WorkingDirectoryForm,
    home_directory: Option<&Path>,
    directory_access: &dyn DirectoryAccess,
) -> Result<settings::WorkingDirectory, String> {
    match form.mode {
        WorkingDirectoryMode::Home => Ok(settings::WorkingDirectory::AlwaysHome),
        WorkingDirectoryMode::Fixed => {
            let path = normalize_fixed_directory_input(&form.directory, home_directory)?;
            let path = validate_existing_directory(&path, directory_access)?;
            Ok(settings::WorkingDirectory::Always {
                directory: path.to_string_lossy().into_owned(),
            })
        }
    }
}

fn adjust_bounded(value: f32, delta: f32, minimum: f32, maximum: f32) -> f32 {
    (value + delta).clamp(minimum, maximum)
}

struct SettingsPageInputElement {
    focus_handle: FocusHandle,
    page: WeakEntity<SettingsPage>,
}

impl IntoElement for SettingsPageInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SettingsPageInputElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        window.set_focus_handle(&self.focus_handle, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            SettingsPageInput {
                page: self.page.clone(),
            },
            cx,
        );
    }
}

struct SettingsPageInput {
    page: WeakEntity<SettingsPage>,
}

impl InputHandler for SettingsPageInput {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.page
            .read_with(cx, |page, _| {
                let length = page.editing_text().encode_utf16().count();
                let start = page.editing_selection.start.min(length);
                let end = page.editing_selection.end.min(length).max(start);
                UTF16Selection {
                    range: start..end,
                    reversed: page.editing_cursor < page.editing_anchor,
                }
            })
            .ok()
    }

    fn marked_text_range(
        &mut self,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<std::ops::Range<usize>> {
        self.page
            .read_with(cx, |page, _| page.marked_range.clone())
            .ok()
            .flatten()
    }

    fn text_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        _actual_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.page
            .read_with(cx, |page, _| {
                substring_utf16(&page.editing_text(), range_utf16)
            })
            .ok()
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.page
            .update(cx, |page, cx| {
                page.replace_editing_text(replacement_range, text.to_string(), None, false, cx)
            })
            .log_err();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<std::ops::Range<usize>>,
        new_text: &str,
        new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.page
            .update(cx, |page, cx| {
                page.replace_editing_text(
                    range_utf16,
                    new_text.to_string(),
                    new_selected_range,
                    true,
                    cx,
                )
            })
            .log_err();
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.page
            .update(cx, |page, cx| {
                page.marked_range = None;
                cx.notify();
            })
            .log_err();
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.page
            .update(cx, |page, cx| {
                let length = page.editing_text().encode_utf16().count();
                let start = range_utf16.start.min(length);
                let end = range_utf16.end.min(length).max(start);
                page.editing_selection = start..end;
                page.editing_anchor = start;
                page.editing_cursor = end;
                page.marked_range = None;
                cx.notify();
            })
            .log_err();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds::default())
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }

    fn element_bounds(&mut self, _window: &mut Window, _cx: &mut App) -> Option<Bounds<Pixels>> {
        Some(Bounds::default())
    }

    fn text_length_utf16(&mut self, _window: &mut Window, cx: &mut App) -> Option<usize> {
        self.page
            .read_with(cx, |page, _| page.editing_text().encode_utf16().count())
            .ok()
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }

    fn prefers_ime_for_printable_keys(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        true
    }
}

/// Notifies every open settings page that the keymap changed on disk.
pub fn keymap_changed(cx: &mut App) {
    let pages: Vec<_> = cx
        .windows()
        .into_iter()
        .filter_map(|handle| handle.downcast::<SettingsPage>())
        .collect();
    for page in pages {
        page.update(cx, |page, _window, cx| page.refresh_keymap_tab(cx))
            .log_err();
    }
}
