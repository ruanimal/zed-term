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
    Color, ContextMenu, Divider, DividerColor, DropdownMenu, Icon, IconButton, IconName, IconSize,
    Label, LabelSize, Switch, TabBar, ToggleState,
};
use util::{ResultExt, shell::Shell as TerminalShell};

use crate::window_chrome;
use crate::window_options;

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
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Keymap => "Keymap",
            Self::Themes => "Themes",
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
            WorkingDirectoryMode::Fixed => "fixed directory",
        };
        let description = if self.working_directory.fell_back_from_workspace_setting {
            "Newly created terminals and panes use Home when an inherited workspace directory is configured."
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
                        }),
                ),
            window,
            cx,
        )
    }
}

impl SettingsPage {
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

    fn switch_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
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
            SettingsTab::Terminal => {}
        }
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
pub(crate) mod tests {
    use super::*;
    use gpui::ReadGlobal as _;
    use proptest::strategy::Strategy as _;

    struct FakeDirectoryAccess {
        canonical_path: PathBuf,
        canonicalize_error: Option<&'static str>,
        is_directory: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SupportedSetting {
        CursorBlinking,
        FontFamily,
        FontWeight,
        LineHeight,
        MinimumContrast,
        KeepSelectionOnCopy,
        OpenLinksInMouseMode,
        CursorShape,
        OptionAsMeta,
        CopyOnSelect,
        AlternateScroll,
        Bell,
        ScrollMultiplier,
        Scrollback,
        ScrollbarShow,
        StructuredShell,
        Environment,
        WorkingDirectory,
    }

    impl SupportedSetting {
        const ALL: [Self; 18] = [
            Self::CursorBlinking,
            Self::FontFamily,
            Self::FontWeight,
            Self::LineHeight,
            Self::MinimumContrast,
            Self::KeepSelectionOnCopy,
            Self::OpenLinksInMouseMode,
            Self::CursorShape,
            Self::OptionAsMeta,
            Self::CopyOnSelect,
            Self::AlternateScroll,
            Self::Bell,
            Self::ScrollMultiplier,
            Self::Scrollback,
            Self::ScrollbarShow,
            Self::StructuredShell,
            Self::Environment,
            Self::WorkingDirectory,
        ];

        fn dirty_setting(self) -> DirtySetting {
            match self {
                Self::CursorBlinking => DirtySetting::Blinking,
                Self::FontFamily => DirtySetting::FontFamily,
                Self::FontWeight => DirtySetting::FontWeight,
                Self::LineHeight => DirtySetting::LineHeight,
                Self::MinimumContrast => DirtySetting::MinimumContrast,
                Self::KeepSelectionOnCopy => DirtySetting::KeepSelectionOnCopy,
                Self::OpenLinksInMouseMode => DirtySetting::OpenLinksInMouseMode,
                Self::CursorShape => DirtySetting::CursorShape,
                Self::OptionAsMeta => DirtySetting::OptionAsMeta,
                Self::CopyOnSelect => DirtySetting::CopyOnSelect,
                Self::AlternateScroll => DirtySetting::AlternateScroll,
                Self::Bell => DirtySetting::Bell,
                Self::ScrollMultiplier => DirtySetting::ScrollMultiplier,
                Self::Scrollback => DirtySetting::MaxScrollHistoryLines,
                Self::ScrollbarShow => DirtySetting::Scrollbar,
                Self::StructuredShell => DirtySetting::Shell,
                Self::Environment => DirtySetting::Environment,
                Self::WorkingDirectory => DirtySetting::WorkingDirectory,
            }
        }

        fn json_key(self) -> &'static str {
            match self {
                Self::CursorBlinking => "blinking",
                Self::FontFamily => "font_family",
                Self::FontWeight => "font_weight",
                Self::LineHeight => "line_height",
                Self::MinimumContrast => "minimum_contrast",
                Self::KeepSelectionOnCopy => "keep_selection_on_copy",
                Self::OpenLinksInMouseMode => "open_links_in_mouse_mode",
                Self::CursorShape => "cursor_shape",
                Self::OptionAsMeta => "option_as_meta",
                Self::CopyOnSelect => "copy_on_select",
                Self::AlternateScroll => "alternate_scroll",
                Self::Bell => "bell",
                Self::ScrollMultiplier => "scroll_multiplier",
                Self::Scrollback => "max_scroll_history_lines",
                Self::ScrollbarShow => "scrollbar",
                Self::StructuredShell => "shell",
                Self::Environment => "env",
                Self::WorkingDirectory => "working_directory",
            }
        }
    }

    #[derive(Debug)]
    struct SupportedSettingsPatch {
        seeds: [u8; 18],
    }

    impl SupportedSettingsPatch {
        fn blinking(&self) -> settings::TerminalBlink {
            match self.seeds[0] % 3 {
                0 => settings::TerminalBlink::Off,
                1 => settings::TerminalBlink::TerminalControlled,
                _ => settings::TerminalBlink::On,
            }
        }

        fn font_family(&self) -> String {
            format!("Property Font {}", self.seeds[1])
        }

        fn raw_font_family(&self) -> String {
            format!("  {}  ", self.font_family())
        }

        fn font_weight(&self) -> f32 {
            100.0 + f32::from(self.seeds[2] % 9) * 100.0
        }

        fn line_height(&self) -> settings::TerminalLineHeight {
            match self.seeds[3] % 3 {
                0 => settings::TerminalLineHeight::Standard,
                1 => settings::TerminalLineHeight::Comfortable,
                _ => {
                    settings::TerminalLineHeight::Custom(1.0 + f32::from(self.seeds[3] % 9) * 0.25)
                }
            }
        }

        fn minimum_contrast(&self) -> f32 {
            f32::from(self.seeds[4] % 107)
        }

        fn keep_selection_on_copy(&self) -> bool {
            self.seeds[5].is_multiple_of(2)
        }

        fn open_links_in_mouse_mode(&self) -> bool {
            self.seeds[6].is_multiple_of(2)
        }

        fn cursor_shape(&self) -> CursorShape {
            match self.seeds[7] % 4 {
                0 => CursorShape::Block,
                1 => CursorShape::Underline,
                2 => CursorShape::Bar,
                _ => CursorShape::Hollow,
            }
        }

        fn option_as_meta(&self) -> bool {
            self.seeds[8].is_multiple_of(2)
        }

        fn copy_on_select(&self) -> bool {
            self.seeds[9].is_multiple_of(2)
        }

        fn alternate_scroll(&self) -> settings::AlternateScroll {
            if self.seeds[10].is_multiple_of(2) {
                settings::AlternateScroll::On
            } else {
                settings::AlternateScroll::Off
            }
        }

        fn bell(&self) -> settings::TerminalBell {
            if self.seeds[11].is_multiple_of(2) {
                settings::TerminalBell::System
            } else {
                settings::TerminalBell::Off
            }
        }

        fn scroll_multiplier(&self) -> f32 {
            0.5 + f32::from(self.seeds[12] % 40) * 0.5
        }

        fn scrollback(&self) -> usize {
            usize::from(u16::from_be_bytes([self.seeds[13], self.seeds[12]]))
        }

        fn scrollbar_show(&self) -> settings::ShowScrollbar {
            match self.seeds[14] % 4 {
                0 => settings::ShowScrollbar::Auto,
                1 => settings::ShowScrollbar::System,
                2 => settings::ShowScrollbar::Always,
                _ => settings::ShowScrollbar::Never,
            }
        }

        fn shell_form(&self) -> ShellForm {
            let program = format!("property-shell-{}", self.seeds[15]);
            match self.seeds[15] % 3 {
                0 => ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                1 => ShellForm {
                    mode: ShellMode::Program,
                    program: format!("  {program}  "),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                _ => ShellForm {
                    mode: ShellMode::WithArguments,
                    program: format!("  {program}  "),
                    arguments: vec!["--property".to_string(), format!("seed={}", self.seeds[15])],
                    title_override: format!("  Property {}  ", self.seeds[15]),
                },
            }
        }

        fn normalized_shell_form(&self) -> ShellForm {
            let mut form = self.shell_form();
            form.program = form.program.trim().to_string();
            form.title_override = form.title_override.trim().to_string();
            form
        }

        fn environment(&self) -> Vec<EnvironmentVariable> {
            vec![EnvironmentVariable {
                key: format!("  PROPERTY_{}  ", self.seeds[16]),
                value: format!("left={}==right", self.seeds[16]),
            }]
        }

        fn normalized_environment(&self) -> Vec<EnvironmentVariable> {
            vec![EnvironmentVariable {
                key: format!("PROPERTY_{}", self.seeds[16]),
                value: format!("left={}==right", self.seeds[16]),
            }]
        }

        fn working_directory(&self) -> WorkingDirectoryForm {
            if self.seeds[17].is_multiple_of(2) {
                WorkingDirectoryForm {
                    mode: WorkingDirectoryMode::Home,
                    directory: String::new(),
                    fell_back_from_workspace_setting: false,
                }
            } else {
                WorkingDirectoryForm {
                    mode: WorkingDirectoryMode::Fixed,
                    directory: "  ~  ".to_string(),
                    fell_back_from_workspace_setting: false,
                }
            }
        }
    }

    const PROPERTY_TWO_BASE_JSONC: &str = r#"{
  // retained theme comment
  "theme": "One Dark",
  "auto_update": false,
  "reduce_motion": "on"
}
"#;

    fn clean_test_settings_page(cx: &mut Context<SettingsPage>) -> SettingsPage {
        let baseline = settings_page_draft(cx);
        let mut page = test_settings_page(baseline.shell_form.clone(), cx);
        page.replace_draft(baseline);
        page.dirty_settings.clear();
        page.draft_revisions = DraftRevisions::default();
        page
    }

    fn apply_supported_control(
        page: &mut SettingsPage,
        setting: SupportedSetting,
        patch: &SupportedSettingsPatch,
        cx: &mut Context<SettingsPage>,
    ) {
        match setting {
            SupportedSetting::CursorBlinking => page.update_draft(
                DirtySetting::Blinking,
                |settings| settings.blinking = patch.blinking(),
                cx,
            ),
            SupportedSetting::FontFamily => {
                page.font_family = patch.raw_font_family();
                page.mark_dirty(DirtySetting::FontFamily, cx);
            }
            SupportedSetting::FontWeight => page.update_draft(
                DirtySetting::FontWeight,
                |settings| settings.font_weight = Some(gpui::FontWeight(patch.font_weight())),
                cx,
            ),
            SupportedSetting::LineHeight => page.update_draft(
                DirtySetting::LineHeight,
                |settings| settings.line_height = patch.line_height(),
                cx,
            ),
            SupportedSetting::MinimumContrast => page.update_draft(
                DirtySetting::MinimumContrast,
                |settings| settings.minimum_contrast = patch.minimum_contrast(),
                cx,
            ),
            SupportedSetting::KeepSelectionOnCopy => page.update_draft(
                DirtySetting::KeepSelectionOnCopy,
                |settings| settings.keep_selection_on_copy = patch.keep_selection_on_copy(),
                cx,
            ),
            SupportedSetting::OpenLinksInMouseMode => page.update_draft(
                DirtySetting::OpenLinksInMouseMode,
                |settings| settings.open_links_in_mouse_mode = patch.open_links_in_mouse_mode(),
                cx,
            ),
            SupportedSetting::CursorShape => page.update_draft(
                DirtySetting::CursorShape,
                |settings| settings.cursor_shape = patch.cursor_shape(),
                cx,
            ),
            SupportedSetting::OptionAsMeta => page.update_draft(
                DirtySetting::OptionAsMeta,
                |settings| settings.option_as_meta = patch.option_as_meta(),
                cx,
            ),
            SupportedSetting::CopyOnSelect => page.update_draft(
                DirtySetting::CopyOnSelect,
                |settings| settings.copy_on_select = patch.copy_on_select(),
                cx,
            ),
            SupportedSetting::AlternateScroll => page.update_draft(
                DirtySetting::AlternateScroll,
                |settings| settings.alternate_scroll = patch.alternate_scroll(),
                cx,
            ),
            SupportedSetting::Bell => page.update_draft(
                DirtySetting::Bell,
                |settings| settings.bell = patch.bell(),
                cx,
            ),
            SupportedSetting::ScrollMultiplier => page.update_draft(
                DirtySetting::ScrollMultiplier,
                |settings| settings.scroll_multiplier = patch.scroll_multiplier(),
                cx,
            ),
            SupportedSetting::Scrollback => page.update_draft(
                DirtySetting::MaxScrollHistoryLines,
                |settings| settings.max_scroll_history_lines = Some(patch.scrollback()),
                cx,
            ),
            SupportedSetting::ScrollbarShow => page.update_draft(
                DirtySetting::Scrollbar,
                |settings| settings.scrollbar.show = Some(patch.scrollbar_show()),
                cx,
            ),
            SupportedSetting::StructuredShell => {
                page.shell_form = patch.shell_form();
                page.mark_dirty(DirtySetting::Shell, cx);
            }
            SupportedSetting::Environment => {
                page.environment = patch.environment();
                page.mark_dirty(DirtySetting::Environment, cx);
            }
            SupportedSetting::WorkingDirectory => {
                page.working_directory = patch.working_directory();
                page.mark_dirty(DirtySetting::WorkingDirectory, cx);
            }
        }
    }

    fn cursor_shape_content(cursor_shape: CursorShape) -> settings::CursorShapeContent {
        match cursor_shape {
            CursorShape::Block => settings::CursorShapeContent::Block,
            CursorShape::Underline => settings::CursorShapeContent::Underline,
            CursorShape::Bar => settings::CursorShapeContent::Bar,
            CursorShape::Hollow => settings::CursorShapeContent::Hollow,
        }
    }

    fn assert_shell_effective_value(persisted: &settings::Shell, effective: &TerminalShell) {
        match (persisted, effective) {
            (settings::Shell::System, TerminalShell::System) => {}
            (settings::Shell::Program(expected), TerminalShell::Program(actual)) => {
                assert_eq!(actual, expected);
            }
            (
                settings::Shell::WithArguments {
                    program: expected_program,
                    args: expected_arguments,
                    title_override: expected_title,
                },
                TerminalShell::WithArguments {
                    program: actual_program,
                    args: actual_arguments,
                    title_override: actual_title,
                },
            ) => {
                assert_eq!(actual_program, expected_program);
                assert_eq!(actual_arguments, expected_arguments);
                assert_eq!(actual_title, expected_title);
            }
            _ => panic!("persisted and effective shell variants differ"),
        }
    }

    fn non_terminal_json_projection(text: &str) -> serde_json::Value {
        let mut value = settings::parse_json_with_comments::<serde_json::Value>(text)
            .expect("Property 2 JSONC should parse");
        let object = value
            .as_object_mut()
            .expect("Property 2 JSONC root should be an object");
        object.remove("terminal");
        value
    }

    struct SupportedSettingRoundTripContext<'a> {
        patch: &'a SupportedSettingsPatch,
        terminal: &'a settings::TerminalSettingsContent,
        effective: &'a TerminalSettings,
        reloaded: &'a SettingsPageDraft,
        live: &'a crate::window::EffectiveLiveSettingsSnapshot,
        launch: &'a crate::window::FakeLaunchSnapshot,
        expected_working_directory: &'a settings::WorkingDirectory,
    }

    fn assert_supported_setting_round_trip(
        setting: SupportedSetting,
        context: &SupportedSettingRoundTripContext<'_>,
    ) {
        let patch = context.patch;
        let terminal = context.terminal;
        let effective = context.effective;
        let reloaded = context.reloaded;
        let live = context.live;
        let launch = context.launch;
        let expected_working_directory = context.expected_working_directory;
        match setting {
            SupportedSetting::CursorBlinking => {
                assert_eq!(terminal.blinking, Some(patch.blinking()));
                assert_eq!(effective.blinking, patch.blinking());
                assert_eq!(reloaded.draft_settings.blinking, patch.blinking());
                assert_eq!(live.blinking, patch.blinking());
            }
            SupportedSetting::FontFamily => {
                assert_eq!(
                    terminal
                        .font_family
                        .as_ref()
                        .map(|font_family| font_family.0.to_string()),
                    Some(patch.font_family())
                );
                assert_eq!(
                    effective
                        .font_family
                        .as_ref()
                        .map(|font_family| font_family.0.to_string()),
                    Some(patch.font_family())
                );
                assert_eq!(reloaded.font_family, patch.font_family());
                assert_eq!(live.font_family, Some(patch.font_family()));
            }
            SupportedSetting::FontWeight => {
                assert_eq!(
                    terminal.font_weight.map(|font_weight| font_weight.0),
                    Some(patch.font_weight())
                );
                assert_eq!(
                    effective.font_weight.map(|font_weight| font_weight.0),
                    Some(patch.font_weight())
                );
                assert_eq!(
                    reloaded
                        .draft_settings
                        .font_weight
                        .map(|font_weight| font_weight.0),
                    Some(patch.font_weight())
                );
                assert_eq!(live.font_weight, Some(patch.font_weight()));
            }
            SupportedSetting::LineHeight => {
                assert_eq!(terminal.line_height, Some(patch.line_height()));
                assert_eq!(effective.line_height, patch.line_height());
                assert_eq!(reloaded.draft_settings.line_height, patch.line_height());
                assert_eq!(live.line_height, patch.line_height());
            }
            SupportedSetting::MinimumContrast => {
                assert_eq!(terminal.minimum_contrast, Some(patch.minimum_contrast()));
                assert_eq!(effective.minimum_contrast, patch.minimum_contrast());
                assert_eq!(
                    reloaded.draft_settings.minimum_contrast,
                    patch.minimum_contrast()
                );
                assert_eq!(live.minimum_contrast, patch.minimum_contrast());
            }
            SupportedSetting::KeepSelectionOnCopy => {
                assert_eq!(
                    terminal.keep_selection_on_copy,
                    Some(patch.keep_selection_on_copy())
                );
                assert_eq!(
                    effective.keep_selection_on_copy,
                    patch.keep_selection_on_copy()
                );
                assert_eq!(
                    reloaded.draft_settings.keep_selection_on_copy,
                    patch.keep_selection_on_copy()
                );
                assert_eq!(live.keep_selection_on_copy, patch.keep_selection_on_copy());
            }
            SupportedSetting::OpenLinksInMouseMode => {
                assert_eq!(
                    terminal.open_links_in_mouse_mode,
                    Some(patch.open_links_in_mouse_mode())
                );
                assert_eq!(
                    effective.open_links_in_mouse_mode,
                    patch.open_links_in_mouse_mode()
                );
                assert_eq!(
                    reloaded.draft_settings.open_links_in_mouse_mode,
                    patch.open_links_in_mouse_mode()
                );
                assert_eq!(
                    live.open_links_in_mouse_mode,
                    patch.open_links_in_mouse_mode()
                );
            }
            SupportedSetting::CursorShape => {
                assert_eq!(
                    terminal.cursor_shape,
                    Some(cursor_shape_content(patch.cursor_shape()))
                );
                assert_eq!(effective.cursor_shape, patch.cursor_shape());
                assert_eq!(reloaded.draft_settings.cursor_shape, patch.cursor_shape());
                assert_eq!(live.cursor_shape, patch.cursor_shape());
            }
            SupportedSetting::OptionAsMeta => {
                assert_eq!(terminal.option_as_meta, Some(patch.option_as_meta()));
                assert_eq!(effective.option_as_meta, patch.option_as_meta());
                assert_eq!(
                    reloaded.draft_settings.option_as_meta,
                    patch.option_as_meta()
                );
                assert_eq!(live.option_as_meta, patch.option_as_meta());
            }
            SupportedSetting::CopyOnSelect => {
                assert_eq!(terminal.copy_on_select, Some(patch.copy_on_select()));
                assert_eq!(effective.copy_on_select, patch.copy_on_select());
                assert_eq!(
                    reloaded.draft_settings.copy_on_select,
                    patch.copy_on_select()
                );
                assert_eq!(live.copy_on_select, patch.copy_on_select());
            }
            SupportedSetting::AlternateScroll => {
                assert_eq!(terminal.alternate_scroll, Some(patch.alternate_scroll()));
                assert_eq!(effective.alternate_scroll, patch.alternate_scroll());
                assert_eq!(
                    reloaded.draft_settings.alternate_scroll,
                    patch.alternate_scroll()
                );
                assert_eq!(live.alternate_scroll, patch.alternate_scroll());
                assert_eq!(launch.alternate_scroll, patch.alternate_scroll());
            }
            SupportedSetting::Bell => {
                assert_eq!(terminal.bell, Some(patch.bell()));
                assert_eq!(effective.bell, patch.bell());
                assert_eq!(reloaded.draft_settings.bell, patch.bell());
                assert_eq!(live.bell, patch.bell());
            }
            SupportedSetting::ScrollMultiplier => {
                assert_eq!(terminal.scroll_multiplier, Some(patch.scroll_multiplier()));
                assert_eq!(effective.scroll_multiplier, patch.scroll_multiplier());
                assert_eq!(
                    reloaded.draft_settings.scroll_multiplier,
                    patch.scroll_multiplier()
                );
                assert_eq!(live.scroll_multiplier, patch.scroll_multiplier());
            }
            SupportedSetting::Scrollback => {
                assert_eq!(terminal.max_scroll_history_lines, Some(patch.scrollback()));
                assert_eq!(effective.max_scroll_history_lines, Some(patch.scrollback()));
                assert_eq!(
                    reloaded.draft_settings.max_scroll_history_lines,
                    Some(patch.scrollback())
                );
                assert_eq!(launch.max_scroll_history_lines, Some(patch.scrollback()));
            }
            SupportedSetting::ScrollbarShow => {
                assert_eq!(
                    terminal
                        .scrollbar
                        .as_ref()
                        .and_then(|scrollbar| scrollbar.show),
                    Some(patch.scrollbar_show())
                );
                assert_eq!(effective.scrollbar.show, Some(patch.scrollbar_show()));
                assert_eq!(
                    reloaded.draft_settings.scrollbar.show,
                    Some(patch.scrollbar_show())
                );
                assert_eq!(live.scrollbar, Some(patch.scrollbar_show()));
            }
            SupportedSetting::StructuredShell => {
                let expected_shell = validate_shell_form(&patch.shell_form())
                    .expect("generated Property 2 shell should be valid");
                assert_eq!(terminal.project.shell.as_ref(), Some(&expected_shell));
                assert_shell_effective_value(&expected_shell, &effective.shell);
                assert_eq!(reloaded.shell_form, patch.normalized_shell_form());
                assert_eq!(launch.shell, effective.shell);
            }
            SupportedSetting::Environment => {
                let expected_environment = validate_environment(&patch.environment())
                    .expect("generated Property 2 environment should be valid");
                assert_eq!(terminal.project.env.as_ref(), Some(&expected_environment));
                assert_eq!(effective.env, expected_environment);
                assert_eq!(reloaded.environment, patch.normalized_environment());
                assert_eq!(launch.environment, effective.env);
            }
            SupportedSetting::WorkingDirectory => {
                assert_eq!(
                    terminal.project.working_directory.as_ref(),
                    Some(expected_working_directory)
                );
                assert_eq!(&effective.working_directory, expected_working_directory);
                match expected_working_directory {
                    settings::WorkingDirectory::AlwaysHome => {
                        assert_eq!(reloaded.working_directory.mode, WorkingDirectoryMode::Home);
                        assert!(reloaded.working_directory.directory.is_empty());
                    }
                    settings::WorkingDirectory::Always { directory } => {
                        assert_eq!(reloaded.working_directory.mode, WorkingDirectoryMode::Fixed);
                        assert_eq!(&reloaded.working_directory.directory, directory);
                    }
                    _ => panic!("Property 2 generated an unsupported working directory"),
                }
                assert_eq!(launch.working_directory, Some(paths::home_dir().clone()));
            }
        }
    }

    /// Feature: terminal-settings-completion, Property 2: Supported setting persistence is isolated and round-trippable
    /// **Validates: Requirements 1.1, 2.1–2.8, 4.1–4.9, 8.1, 8.9**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn supported_setting_persistence_is_isolated_and_round_trippable(
        cx: &mut gpui::TestAppContext,
        seeds: [u8; 18],
    ) {
        let patch = SupportedSettingsPatch { seeds };
        cx.update(|cx| {
            let mut settings_store = SettingsStore::test(cx);
            settings_store
                .set_user_settings(PROPERTY_TWO_BASE_JSONC, cx)
                .expect("failed to load Property 2 base settings");
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });

        let settings_page = cx.new(clean_test_settings_page);
        settings_page.update(cx, |page, cx| {
            for setting in SupportedSetting::ALL {
                apply_supported_control(page, setting, &patch, cx);
            }
            let expected_dirty_settings: Vec<_> = SupportedSetting::ALL
                .into_iter()
                .map(SupportedSetting::dirty_setting)
                .collect();
            assert_eq!(page.dirty_settings, expected_dirty_settings);
            assert_eq!(page.font_family, patch.raw_font_family());
            assert_eq!(page.shell_form, patch.shell_form());
            assert_eq!(page.environment, patch.environment());
            assert_eq!(page.working_directory, patch.working_directory());
        });

        let home_directory = paths::home_dir().clone();
        let directory_access = FakeDirectoryAccess {
            canonical_path: home_directory.clone(),
            canonicalize_error: None,
            is_directory: true,
        };
        let captured_patch = settings_page.update(cx, |page, _| {
            page.capture_save_patch(
                Some(&home_directory),
                &directory_access,
                theme::Appearance::Dark,
            )
            .expect("generated Property 2 patch should validate")
        });
        let expected_working_directory = validate_working_directory_form(
            &patch.working_directory(),
            Some(&home_directory),
            &directory_access,
        )
        .expect("generated Property 2 working directory should validate");

        let updated_text = cx.update(|cx| {
            SettingsStore::global(cx)
                .new_text_for_update(PROPERTY_TWO_BASE_JSONC.to_string(), |content| {
                    captured_patch.apply(content)
                })
        });
        let updated_text = updated_text.expect("Property 2 JSONC update should succeed");
        assert_eq!(
            non_terminal_json_projection(&updated_text),
            non_terminal_json_projection(PROPERTY_TWO_BASE_JSONC)
        );
        for retained_fragment in [
            "// retained theme comment",
            "\"theme\": \"One Dark\"",
            "\"auto_update\": false",
            "\"reduce_motion\": \"on\"",
        ] {
            assert!(updated_text.contains(retained_fragment));
        }
        let parsed = settings::parse_json_with_comments::<serde_json::Value>(&updated_text)
            .expect("updated Property 2 JSONC should parse");
        let terminal_object = parsed
            .get("terminal")
            .and_then(serde_json::Value::as_object)
            .expect("updated Property 2 JSONC should contain a terminal object");
        assert_eq!(terminal_object.len(), SupportedSetting::ALL.len());
        for setting in SupportedSetting::ALL {
            assert!(
                terminal_object.contains_key(setting.json_key()),
                "missing persisted matrix entry for {setting:?}"
            );
        }

        let (terminal, effective, reloaded, live, launch) = cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(&updated_text, cx)
                    .expect("updated Property 2 settings should reload");
            });
            let terminal = SettingsStore::global(cx)
                .get_content_for_file(settings::SettingsFile::User)
                .and_then(|content| content.terminal.clone())
                .expect("reloaded user settings should contain terminal overrides");
            let effective = TerminalSettings::get_global(cx).clone();
            let reloaded = settings_page_draft(cx);
            let live = crate::window::EffectiveLiveSettingsSnapshot::capture(cx);
            let launch = crate::window::fake_launch_snapshot(cx);
            (terminal, effective, reloaded, live, launch)
        });

        let round_trip_context = SupportedSettingRoundTripContext {
            patch: &patch,
            terminal: &terminal,
            effective: &effective,
            reloaded: &reloaded,
            live: &live,
            launch: &launch,
            expected_working_directory: &expected_working_directory,
        };
        for setting in SupportedSetting::ALL {
            assert_supported_setting_round_trip(setting, &round_trip_context);
        }
    }

    #[derive(Debug)]
    struct ResetJsoncCase {
        original: String,
        expected: String,
        theme_name: String,
        retained_fragments: Vec<String>,
    }

    fn reset_jsonc_case_strategy() -> impl proptest::strategy::Strategy<Value = ResetJsoncCase> {
        (
            proptest::prop_oneof![
                proptest::strategy::Just(" ".to_string()),
                proptest::strategy::Just("   ".to_string()),
                proptest::strategy::Just("      ".to_string()),
                proptest::strategy::Just("\t".to_string()),
            ],
            proptest::prop_oneof![
                proptest::strategy::Just("  ".to_string()),
                proptest::strategy::Just("     ".to_string()),
                proptest::strategy::Just("\t".to_string()),
            ],
            8_u8..=72,
            proptest::prop_oneof![
                proptest::strategy::Just("block"),
                proptest::strategy::Just("bar"),
                proptest::strategy::Just("underline"),
                proptest::strategy::Just("hollow"),
            ],
            "[a-z]{1,12}",
            1_usize..=4,
            proptest::bool::ANY,
        )
            .prop_map(
                |(
                    indentation,
                    nested_indentation,
                    font_size,
                    cursor_shape,
                    comment_tag,
                    blank_line_count,
                    trailing_comma,
                )| {
                    let nested_indentation = format!("{indentation}{nested_indentation}");
                    let terminal_section = format!(
                        "{indentation}\"terminal\": {{\n{nested_indentation}\"font_size\": {font_size},\n{nested_indentation}\"cursor_shape\": \"{cursor_shape}\"\n{indentation}}},\n"
                    );
                    let theme_name = format!("Theme {comment_tag}");
                    let theme_comment = format!("// theme-before-{comment_tag}");
                    let theme_inline_comment = format!("// theme-inline-{comment_tag}");
                    let auto_update_comment = format!("// auto-update-before-{comment_tag}");
                    let auto_update_inline_comment =
                        format!("// auto-update-inline-{comment_tag}");
                    let blank_lines = "\n".repeat(blank_line_count);
                    let trailing_comma = if trailing_comma { "," } else { "" };
                    let retained_content = format!(
                        "{indentation}{theme_comment}\n{indentation}\"theme\": \"{theme_name}\", {theme_inline_comment}\n{blank_lines}{indentation}{auto_update_comment}\n{indentation}\"auto_update\": false, {auto_update_inline_comment}\n{indentation}\"reduce_motion\": \"on\"{trailing_comma}\n"
                    );
                    let original = format!("{{\n{terminal_section}{retained_content}}}\n");
                    let expected = format!("{{\n{retained_content}}}\n");

                    ResetJsoncCase {
                        original,
                        expected,
                        theme_name,
                        retained_fragments: vec![
                            theme_comment,
                            theme_inline_comment,
                            blank_lines,
                            auto_update_comment,
                            auto_update_inline_comment,
                            format!("{indentation}\"reduce_motion\": \"on\""),
                        ],
                    }
                },
            )
    }

    impl DirectoryAccess for FakeDirectoryAccess {
        fn canonicalize(&self, _path: &Path) -> std::io::Result<PathBuf> {
            if let Some(error) = self.canonicalize_error {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, error));
            }
            Ok(self.canonical_path.clone())
        }

        fn is_directory(&self, _path: &Path) -> bool {
            self.is_directory
        }
    }

    #[gpui::test]
    fn standalone_terminal_defaults_are_loaded_from_default_asset(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let default_settings_asset = settings::default_settings();
            let mut settings_store = SettingsStore::new(cx, &default_settings_asset);
            settings_store
                .set_user_settings("{}", cx)
                .expect("failed to load empty user settings");
            cx.set_global(settings_store);

            let effective_settings = TerminalSettings::get_global(cx);
            assert_eq!(
                effective_settings.line_height,
                settings::TerminalLineHeight::Standard
            );
            assert_eq!(effective_settings.bell, settings::TerminalBell::Off);
            assert_eq!(
                effective_settings.alternate_scroll,
                settings::AlternateScroll::On
            );
        });
    }

    #[test]
    fn shell_validation_normalizes_program_and_rejects_invalid_text() {
        let empty_program = ShellForm {
            mode: ShellMode::Program,
            program: "  ".to_string(),
            arguments: Vec::new(),
            title_override: String::new(),
        };
        assert_eq!(
            validate_shell_form(&empty_program),
            Err("A custom shell program is required.".to_string())
        );

        let with_arguments = ShellForm {
            mode: ShellMode::WithArguments,
            program: "  /bin/sh  ".to_string(),
            arguments: vec!["-c".to_string(), "printf test".to_string()],
            title_override: "  shell title  ".to_string(),
        };
        assert_eq!(
            validate_shell_form(&with_arguments),
            Ok(settings::Shell::WithArguments {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "printf test".to_string()],
                title_override: Some("shell title".to_string()),
            })
        );

        for (program, arguments, title_override, expected_error) in [
            (
                "/bin/sh\0",
                Vec::new(),
                "",
                "A shell program cannot contain NUL.",
            ),
            (
                "/bin/sh",
                vec!["bad\0argument".to_string()],
                "",
                "Shell arguments cannot contain NUL.",
            ),
            (
                "/bin/sh",
                Vec::new(),
                "bad\0title",
                "The title override cannot contain NUL.",
            ),
        ] {
            let form = ShellForm {
                mode: ShellMode::WithArguments,
                program: program.to_string(),
                arguments,
                title_override: title_override.to_string(),
            };
            assert_eq!(validate_shell_form(&form), Err(expected_error.to_string()));
        }
    }

    #[test]
    fn environment_validation_trims_keys_and_preserves_complete_values() {
        let entries = vec![EnvironmentVariable {
            key: "  TOKEN  ".to_string(),
            value: "left=middle=right".to_string(),
        }];
        let environment = validate_environment(&entries);
        assert_eq!(
            environment
                .as_ref()
                .ok()
                .and_then(|environment| environment.get("TOKEN"))
                .map(String::as_str),
            Some("left=middle=right")
        );

        let invalid_entries = [
            (
                vec![EnvironmentVariable {
                    key: "  ".to_string(),
                    value: String::new(),
                }],
                "Environment variable names cannot be empty.",
            ),
            (
                vec![EnvironmentVariable {
                    key: "NAME=VALUE".to_string(),
                    value: String::new(),
                }],
                "Environment variable names cannot contain `=`.",
            ),
            (
                vec![EnvironmentVariable {
                    key: "NA\0ME".to_string(),
                    value: String::new(),
                }],
                "Environment variable names and values cannot contain NUL.",
            ),
            (
                vec![EnvironmentVariable {
                    key: "NAME".to_string(),
                    value: "value\0".to_string(),
                }],
                "Environment variable names and values cannot contain NUL.",
            ),
            (
                vec![
                    EnvironmentVariable {
                        key: "NAME".to_string(),
                        value: "first".to_string(),
                    },
                    EnvironmentVariable {
                        key: " NAME ".to_string(),
                        value: "second".to_string(),
                    },
                ],
                "Environment variable names must be unique.",
            ),
        ];
        for (entries, expected_error) in invalid_entries {
            assert_eq!(
                validate_environment(&entries),
                Err(expected_error.to_string())
            );
        }
    }

    #[test]
    fn fixed_directory_normalization_is_lexical() {
        assert_eq!(
            normalize_fixed_directory_input("  ~/project  ", Some(Path::new("/home/test"))),
            Ok(PathBuf::from("/home/test/project"))
        );
        assert!(normalize_fixed_directory_input(" ", Some(Path::new("/home/test"))).is_err());
        assert!(
            normalize_fixed_directory_input("/tmp/bad\0path", Some(Path::new("/home/test")))
                .is_err()
        );
        assert!(
            normalize_fixed_directory_input("relative/path", Some(Path::new("/home/test")))
                .is_err()
        );
        assert!(normalize_fixed_directory_input("~/project", None).is_err());
    }

    #[test]
    fn directory_access_preserves_errors_and_checks_directory_type() {
        let canonical_path = PathBuf::from("/canonical/project");
        let accessible_directory = FakeDirectoryAccess {
            canonical_path: canonical_path.clone(),
            canonicalize_error: None,
            is_directory: true,
        };
        assert_eq!(
            validate_existing_directory(Path::new("/input/project"), &accessible_directory),
            Ok(canonical_path.clone())
        );

        let inaccessible_directory = FakeDirectoryAccess {
            canonical_path: canonical_path.clone(),
            canonicalize_error: Some("injected access failure"),
            is_directory: true,
        };
        assert_eq!(
            validate_existing_directory(Path::new("/input/project"), &inaccessible_directory),
            Err("The working directory does not exist: injected access failure".to_string())
        );

        let regular_file = FakeDirectoryAccess {
            canonical_path,
            canonicalize_error: None,
            is_directory: false,
        };
        assert_eq!(
            validate_existing_directory(Path::new("/input/project"), &regular_file),
            Err("The fixed working directory must be a directory.".to_string())
        );
    }

    #[derive(Debug)]
    struct FixedDirectoryScenario {
        home_components: Vec<String>,
        directory_components: Vec<String>,
        canonical_components: Vec<String>,
        relative_components: Vec<String>,
    }

    struct RecordingFakeDirectoryAccess {
        canonical_path: PathBuf,
        canonicalize_error: bool,
        is_directory: bool,
        canonicalized_paths: std::cell::RefCell<Vec<PathBuf>>,
        directory_checks: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl RecordingFakeDirectoryAccess {
        fn existing_directory(canonical_path: PathBuf) -> Self {
            Self {
                canonical_path,
                canonicalize_error: false,
                is_directory: true,
                canonicalized_paths: std::cell::RefCell::new(Vec::new()),
                directory_checks: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn nonexistent(canonical_path: PathBuf) -> Self {
            Self {
                canonical_path,
                canonicalize_error: true,
                is_directory: true,
                canonicalized_paths: std::cell::RefCell::new(Vec::new()),
                directory_checks: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn non_directory(canonical_path: PathBuf) -> Self {
            Self {
                canonical_path,
                canonicalize_error: false,
                is_directory: false,
                canonicalized_paths: std::cell::RefCell::new(Vec::new()),
                directory_checks: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn canonicalized_paths(&self) -> Vec<PathBuf> {
            self.canonicalized_paths.borrow().clone()
        }

        fn directory_checks(&self) -> Vec<PathBuf> {
            self.directory_checks.borrow().clone()
        }
    }

    impl DirectoryAccess for RecordingFakeDirectoryAccess {
        fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
            self.canonicalized_paths
                .borrow_mut()
                .push(path.to_path_buf());
            if self.canonicalize_error {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "injected missing directory",
                ));
            }
            Ok(self.canonical_path.clone())
        }

        fn is_directory(&self, path: &Path) -> bool {
            self.directory_checks.borrow_mut().push(path.to_path_buf());
            self.is_directory
        }
    }

    const DIRECTORY_COMPONENT_CHARACTERS: &[char] = &[
        'a', 'b', 'c', 'x', 'y', 'z', 'A', 'B', 'C', '0', '1', '2', '_', '-',
    ];

    fn directory_component_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        proptest::collection::vec(
            proptest::sample::select(DIRECTORY_COMPONENT_CHARACTERS),
            1..12,
        )
        .prop_map(|characters| characters.into_iter().collect())
    }

    fn directory_components_strategy() -> impl proptest::strategy::Strategy<Value = Vec<String>> {
        proptest::collection::vec(directory_component_strategy(), 1..4)
    }

    fn fixed_directory_scenario_strategy()
    -> impl proptest::strategy::Strategy<Value = FixedDirectoryScenario> {
        (
            directory_components_strategy(),
            directory_components_strategy(),
            directory_components_strategy(),
            directory_components_strategy(),
        )
            .prop_map(
                |(
                    home_components,
                    directory_components,
                    canonical_components,
                    relative_components,
                )| FixedDirectoryScenario {
                    home_components,
                    directory_components,
                    canonical_components,
                    relative_components,
                },
            )
    }

    fn fake_absolute_path(components: &[String]) -> PathBuf {
        #[cfg(windows)]
        let path = PathBuf::from(r"C:\\");
        #[cfg(not(windows))]
        let path = PathBuf::from("/");

        components
            .iter()
            .fold(path, |path, component| path.join(component))
    }

    fn fixed_directory_form(directory: String) -> WorkingDirectoryForm {
        WorkingDirectoryForm {
            mode: WorkingDirectoryMode::Fixed,
            directory,
            fell_back_from_workspace_setting: false,
        }
    }

    /// Feature: terminal-settings-completion, Property 6: Fixed working directories normalize and validate consistently
    /// **Validates: Requirements 4.8, 7.8, 7.9, 7.10**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn fixed_working_directories_normalize_and_validate_consistently(
        cx: &mut gpui::TestAppContext,
        #[strategy = fixed_directory_scenario_strategy()] scenario: FixedDirectoryScenario,
    ) {
        let home_directory = fake_absolute_path(&scenario.home_components);
        let relative_suffix = scenario.directory_components.join("/");
        let expected_expanded_path = scenario
            .directory_components
            .iter()
            .fold(home_directory.clone(), |path, component| {
                path.join(component)
            });
        let canonical_path = fake_absolute_path(&scenario.canonical_components);
        let valid_access = RecordingFakeDirectoryAccess::existing_directory(canonical_path.clone());
        let valid_form = fixed_directory_form(format!(" \t~/{relative_suffix}\n"));
        let expected_working_directory = settings::WorkingDirectory::Always {
            directory: canonical_path.to_string_lossy().into_owned(),
        };
        let validation_result =
            validate_working_directory_form(&valid_form, Some(&home_directory), &valid_access);
        assert_eq!(validation_result, Ok(expected_working_directory.clone()));
        assert_eq!(
            valid_access.canonicalized_paths(),
            vec![expected_expanded_path]
        );
        assert_eq!(valid_access.directory_checks(), vec![canonical_path]);

        if let Ok(validated_working_directory) = validation_result {
            let expected_construction_input = validated_working_directory.clone();
            let (persisted_working_directory, construction_working_directory) = cx.update(|cx| {
                let settings_store = SettingsStore::test(cx);
                cx.set_global(settings_store);

                SettingsStore::update_global(cx, |store, cx| {
                    store.update_user_settings(cx, |content| {
                        let terminal = content
                            .terminal
                            .get_or_insert_with(settings::TerminalSettingsContent::default);
                        terminal.project.working_directory =
                            Some(validated_working_directory.clone());
                    });
                });

                let persisted_working_directory = SettingsStore::global(cx)
                    .get_content_for_file(settings::SettingsFile::User)
                    .and_then(|content| content.terminal.as_ref())
                    .and_then(|terminal| terminal.project.working_directory.as_ref())
                    .cloned();
                let construction_working_directory =
                    TerminalSettings::get_global(cx).working_directory.clone();
                (persisted_working_directory, construction_working_directory)
            });

            assert_eq!(
                persisted_working_directory,
                Some(expected_construction_input.clone())
            );
            assert_eq!(construction_working_directory, expected_construction_input);
        }

        for invalid_input in [" \t\n ".to_string(), format!("/{relative_suffix}\0tail")] {
            let invalid_access =
                RecordingFakeDirectoryAccess::existing_directory(PathBuf::from("/unused"));
            assert!(
                validate_working_directory_form(
                    &fixed_directory_form(invalid_input),
                    Some(&home_directory),
                    &invalid_access,
                )
                .is_err()
            );
            assert!(invalid_access.canonicalized_paths().is_empty());
            assert!(invalid_access.directory_checks().is_empty());
        }

        let relative_input = scenario.relative_components.join("/");
        let relative_access =
            RecordingFakeDirectoryAccess::existing_directory(PathBuf::from("/unused"));
        assert_eq!(
            validate_working_directory_form(
                &fixed_directory_form(relative_input),
                Some(&home_directory),
                &relative_access,
            ),
            Err("A fixed working directory must be absolute or begin with `~/`.".to_string())
        );
        assert!(relative_access.canonicalized_paths().is_empty());
        assert!(relative_access.directory_checks().is_empty());

        let unavailable_home_access =
            RecordingFakeDirectoryAccess::existing_directory(PathBuf::from("/unused"));
        assert_eq!(
            validate_working_directory_form(&valid_form, None, &unavailable_home_access),
            Err("The platform home directory is unavailable.".to_string())
        );
        assert!(unavailable_home_access.canonicalized_paths().is_empty());
        assert!(unavailable_home_access.directory_checks().is_empty());

        let nonexistent_path = fake_absolute_path(&scenario.directory_components);
        let nonexistent_access =
            RecordingFakeDirectoryAccess::nonexistent(PathBuf::from("/unused"));
        assert_eq!(
            validate_working_directory_form(
                &fixed_directory_form(nonexistent_path.to_string_lossy().into_owned()),
                Some(&home_directory),
                &nonexistent_access,
            ),
            Err("The working directory does not exist: injected missing directory".to_string())
        );
        assert_eq!(
            nonexistent_access.canonicalized_paths(),
            vec![nonexistent_path]
        );
        assert!(nonexistent_access.directory_checks().is_empty());

        let non_directory_path = fake_absolute_path(&scenario.relative_components);
        let non_directory_canonical_path = fake_absolute_path(&scenario.canonical_components);
        let non_directory_access =
            RecordingFakeDirectoryAccess::non_directory(non_directory_canonical_path.clone());
        assert_eq!(
            validate_working_directory_form(
                &fixed_directory_form(non_directory_path.to_string_lossy().into_owned()),
                Some(&home_directory),
                &non_directory_access,
            ),
            Err("The fixed working directory must be a directory.".to_string())
        );
        assert_eq!(
            non_directory_access.canonicalized_paths(),
            vec![non_directory_path]
        );
        assert_eq!(
            non_directory_access.directory_checks(),
            vec![non_directory_canonical_path]
        );
    }

    #[test]
    fn bounded_adjustment_clamps_at_both_limits() {
        assert_eq!(adjust_bounded(100.0, -1.0, 100.0, 900.0), 100.0);
        assert_eq!(adjust_bounded(900.0, 1.0, 100.0, 900.0), 900.0);
        assert_eq!(adjust_bounded(400.0, 100.0, 100.0, 900.0), 500.0);
    }

    #[derive(Clone, Copy, Debug)]
    struct NumericControlDomain {
        name: &'static str,
        minimum: f32,
        maximum: f32,
        generated_maximum: f32,
        delta_scale: f32,
    }

    const NUMERIC_CONTROL_DOMAINS: [NumericControlDomain; 6] = [
        NumericControlDomain {
            name: "font weight",
            minimum: 100.0,
            maximum: 900.0,
            generated_maximum: 900.0,
            delta_scale: 100.0,
        },
        NumericControlDomain {
            name: "custom line height",
            minimum: 1.0,
            maximum: f32::MAX,
            generated_maximum: 4.0,
            delta_scale: 0.1,
        },
        NumericControlDomain {
            name: "minimum contrast",
            minimum: 0.0,
            maximum: 106.0,
            generated_maximum: 106.0,
            delta_scale: 1.0,
        },
        NumericControlDomain {
            name: "font size",
            minimum: 1.0,
            maximum: f32::MAX,
            generated_maximum: 256.0,
            delta_scale: 1.0,
        },
        NumericControlDomain {
            name: "scroll multiplier",
            minimum: 0.1,
            maximum: f32::MAX,
            generated_maximum: 100.0,
            delta_scale: 1.0,
        },
        NumericControlDomain {
            name: "scrollback",
            minimum: 0.0,
            maximum: 100_000.0,
            generated_maximum: 100_000.0,
            delta_scale: 1.0,
        },
    ];

    #[derive(Clone, Debug)]
    enum ShellArgumentEdit {
        Add { argument: String },
        Edit { selector: usize, argument: String },
        Remove { selector: usize },
    }

    #[derive(Debug)]
    struct StructuredShellScenario {
        program: String,
        title_override: String,
        initial_arguments: Vec<String>,
        edits: Vec<ShellArgumentEdit>,
    }

    fn shell_argument_edit_strategy() -> impl proptest::strategy::Strategy<Value = ShellArgumentEdit>
    {
        (
            0_u8..3,
            proptest::arbitrary::any::<usize>(),
            shell_text_strategy(0..24),
        )
            .prop_map(|(operation, selector, argument)| match operation {
                0 => ShellArgumentEdit::Add { argument },
                1 => ShellArgumentEdit::Edit { selector, argument },
                _ => ShellArgumentEdit::Remove { selector },
            })
    }

    fn structured_shell_scenario_strategy()
    -> impl proptest::strategy::Strategy<Value = StructuredShellScenario> {
        (
            shell_text_strategy(0..24).prop_map(|suffix| format!("/shell-{suffix}")),
            proptest::prop_oneof![
                proptest::strategy::Just(String::new()),
                shell_text_strategy(0..24).prop_map(|suffix| format!("Title {suffix}")),
            ],
            proptest::collection::vec(shell_text_strategy(0..24), 0..8),
            proptest::collection::vec(shell_argument_edit_strategy(), 0..24),
        )
            .prop_map(|(program, title_override, initial_arguments, mut edits)| {
                if !edits
                    .iter()
                    .any(|edit| matches!(edit, ShellArgumentEdit::Add { .. }))
                {
                    edits.push(ShellArgumentEdit::Add {
                        argument: "added argument".to_string(),
                    });
                }
                if !edits
                    .iter()
                    .any(|edit| matches!(edit, ShellArgumentEdit::Edit { .. }))
                {
                    edits.push(ShellArgumentEdit::Edit {
                        selector: 0,
                        argument: "edited argument".to_string(),
                    });
                }
                if !edits
                    .iter()
                    .any(|edit| matches!(edit, ShellArgumentEdit::Remove { .. }))
                {
                    edits.push(ShellArgumentEdit::Remove { selector: 0 });
                }
                StructuredShellScenario {
                    program,
                    title_override,
                    initial_arguments,
                    edits,
                }
            })
    }

    fn selected_shell_argument_index(selector: usize, argument_count: usize) -> Option<usize> {
        if argument_count == 0 {
            None
        } else {
            Some(selector % argument_count)
        }
    }

    #[test]
    fn empty_shell_argument_edits_are_safe() {
        let mut shell_form = ShellForm {
            mode: ShellMode::WithArguments,
            program: "/bin/sh".to_string(),
            arguments: Vec::new(),
            title_override: String::new(),
        };

        apply_shell_argument_edit(
            &mut shell_form,
            &ShellArgumentEdit::Edit {
                selector: usize::MAX,
                argument: "edited".to_string(),
            },
        );
        apply_shell_argument_edit(
            &mut shell_form,
            &ShellArgumentEdit::Remove {
                selector: usize::MAX,
            },
        );
        assert!(shell_form.arguments.is_empty());

        apply_shell_argument_edit(
            &mut shell_form,
            &ShellArgumentEdit::Add {
                argument: "added".to_string(),
            },
        );
        assert_eq!(shell_form.arguments, ["added"]);
    }

    fn apply_shell_argument_edit(form: &mut ShellForm, edit: &ShellArgumentEdit) {
        match edit {
            ShellArgumentEdit::Add { argument } => form.arguments.push(argument.clone()),
            ShellArgumentEdit::Edit { selector, argument } => {
                if let Some(index) = selected_shell_argument_index(*selector, form.arguments.len())
                    && let Some(existing_argument) = form.arguments.get_mut(index)
                {
                    *existing_argument = argument.clone();
                }
            }
            ShellArgumentEdit::Remove { selector } => {
                if let Some(index) = selected_shell_argument_index(*selector, form.arguments.len())
                {
                    form.arguments.remove(index);
                }
            }
        }
    }

    fn apply_reference_shell_argument_edit(arguments: &mut Vec<String>, edit: &ShellArgumentEdit) {
        match edit {
            ShellArgumentEdit::Add { argument } => arguments.push(argument.clone()),
            ShellArgumentEdit::Edit { selector, argument } if !arguments.is_empty() => {
                let index = *selector % arguments.len();
                if let Some(existing_argument) = arguments.get_mut(index) {
                    *existing_argument = argument.clone();
                }
            }
            ShellArgumentEdit::Remove { selector } if !arguments.is_empty() => {
                let index = *selector % arguments.len();
                arguments.remove(index);
            }
            ShellArgumentEdit::Edit { .. } | ShellArgumentEdit::Remove { .. } => {}
        }
    }

    /// Feature: terminal-settings-completion, Property 3: Structured shell editing and launch preserve order
    /// **Validates: Requirements 4.2, 4.3, 4.6**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn structured_shell_editing_and_launch_preserve_order(
        cx: &mut gpui::TestAppContext,
        #[strategy = structured_shell_scenario_strategy()] scenario: StructuredShellScenario,
    ) {
        let mut shell_form = ShellForm {
            mode: ShellMode::WithArguments,
            program: scenario.program,
            arguments: scenario.initial_arguments.clone(),
            title_override: scenario.title_override,
        };
        let mut reference_arguments = scenario.initial_arguments;

        for edit in &scenario.edits {
            apply_shell_argument_edit(&mut shell_form, edit);
            apply_reference_shell_argument_edit(&mut reference_arguments, edit);
            assert_eq!(shell_form.arguments, reference_arguments);
        }

        let normalized_program = shell_form.program.trim().to_string();
        let normalized_title_override = shell_form.title_override.trim().to_string();
        let normalized_title_override =
            (!normalized_title_override.is_empty()).then_some(normalized_title_override);
        let expected_shell = settings::Shell::WithArguments {
            program: normalized_program.clone(),
            args: reference_arguments.clone(),
            title_override: normalized_title_override.clone(),
        };
        let expected_launched_shell = TerminalShell::WithArguments {
            program: normalized_program.clone(),
            args: reference_arguments.clone(),
            title_override: normalized_title_override.clone(),
        };
        assert_eq!(validate_shell_form(&shell_form), Ok(expected_shell.clone()));

        let expected_draft = ShellForm {
            mode: ShellMode::WithArguments,
            program: normalized_program,
            arguments: reference_arguments,
            title_override: normalized_title_override.unwrap_or_default(),
        };

        let (persisted_shell, reloaded_draft, launched_shell) = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);

            let shell_for_update = expected_shell.clone();
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    let terminal = content
                        .terminal
                        .get_or_insert_with(settings::TerminalSettingsContent::default);
                    terminal.project.shell = Some(shell_for_update);
                });
            });

            let persisted_shell = SettingsStore::global(cx)
                .get_content_for_file(settings::SettingsFile::User)
                .and_then(|content| content.terminal.as_ref())
                .and_then(|terminal| terminal.project.shell.as_ref())
                .cloned();
            let reloaded_draft = settings_page_draft(cx).shell_form;
            let launched_shell = crate::window::fake_launch_shell_snapshot(cx);
            (persisted_shell, reloaded_draft, launched_shell)
        });

        assert_eq!(persisted_shell, Some(expected_shell));
        assert_eq!(reloaded_draft, expected_draft);
        assert_eq!(launched_shell, expected_launched_shell);
    }

    #[derive(Debug)]
    enum InvalidShellText {
        TrimmedEmptyProgram { mode: ShellMode, program: String },
        NulProgram { mode: ShellMode, program: String },
        NulArgument { program: String, argument: String },
        NulTitle { program: String, title: String },
    }

    impl InvalidShellText {
        fn shell_form(&self) -> ShellForm {
            match self {
                Self::TrimmedEmptyProgram { mode, program }
                | Self::NulProgram { mode, program } => ShellForm {
                    mode: *mode,
                    program: program.clone(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                Self::NulArgument { program, argument } => ShellForm {
                    mode: ShellMode::WithArguments,
                    program: program.clone(),
                    arguments: vec![argument.clone()],
                    title_override: String::new(),
                },
                Self::NulTitle { program, title } => ShellForm {
                    mode: ShellMode::WithArguments,
                    program: program.clone(),
                    arguments: Vec::new(),
                    title_override: title.clone(),
                },
            }
        }

        fn expected_error(&self) -> &'static str {
            match self {
                Self::TrimmedEmptyProgram {
                    mode: ShellMode::Program,
                    ..
                } => "A custom shell program is required.",
                Self::TrimmedEmptyProgram {
                    mode: ShellMode::WithArguments,
                    ..
                } => "A shell program is required.",
                Self::NulProgram {
                    mode: ShellMode::Program,
                    ..
                } => "A custom shell program cannot contain NUL.",
                Self::NulProgram {
                    mode: ShellMode::WithArguments,
                    ..
                } => "A shell program cannot contain NUL.",
                Self::NulArgument { .. } => "Shell arguments cannot contain NUL.",
                Self::NulTitle { .. } => "The title override cannot contain NUL.",
                Self::TrimmedEmptyProgram {
                    mode: ShellMode::System,
                    ..
                }
                | Self::NulProgram {
                    mode: ShellMode::System,
                    ..
                } => "",
            }
        }

        fn editable_field(&self) -> EditableField {
            match self {
                Self::TrimmedEmptyProgram { .. } | Self::NulProgram { .. } => {
                    EditableField::ShellProgram
                }
                Self::NulArgument { .. } => EditableField::ShellArgument(0),
                Self::NulTitle { .. } => EditableField::ShellTitleOverride,
            }
        }

        fn corrected_text(&self) -> &'static str {
            match self {
                Self::TrimmedEmptyProgram { .. } | Self::NulProgram { .. } => "/bin/sh",
                Self::NulArgument { .. } => "--login",
                Self::NulTitle { .. } => "shell title",
            }
        }
    }

    const SHELL_TEXT_CHARACTERS: &[char] = &[
        'a', 'b', 'c', 'x', 'y', 'z', '0', '1', '2', '/', '.', '_', '-', ' ', '\t',
    ];
    const WHITESPACE_CHARACTERS: &[char] = &[' ', '\t', '\n', '\r'];

    fn shell_mode_strategy() -> impl proptest::strategy::Strategy<Value = ShellMode> {
        proptest::prop_oneof![
            proptest::strategy::Just(ShellMode::Program),
            proptest::strategy::Just(ShellMode::WithArguments),
        ]
    }

    fn shell_text_strategy(
        length: std::ops::Range<usize>,
    ) -> impl proptest::strategy::Strategy<Value = String> {
        proptest::collection::vec(proptest::sample::select(SHELL_TEXT_CHARACTERS), length)
            .prop_map(|characters| characters.into_iter().collect())
    }

    fn invalid_shell_text_strategy() -> impl proptest::strategy::Strategy<Value = InvalidShellText>
    {
        let trimmed_empty = (
            shell_mode_strategy(),
            proptest::collection::vec(proptest::sample::select(WHITESPACE_CHARACTERS), 0..32)
                .prop_map(|characters| characters.into_iter().collect()),
        )
            .prop_map(|(mode, program)| InvalidShellText::TrimmedEmptyProgram { mode, program });
        let nul_program = (
            shell_mode_strategy(),
            shell_text_strategy(0..24),
            shell_text_strategy(0..24),
        )
            .prop_map(|(mode, prefix, suffix)| InvalidShellText::NulProgram {
                mode,
                program: format!("{prefix}\0{suffix}"),
            });
        let nul_argument = (
            shell_text_strategy(1..24),
            shell_text_strategy(0..24),
            shell_text_strategy(0..24),
        )
            .prop_map(|(program, prefix, suffix)| InvalidShellText::NulArgument {
                program,
                argument: format!("{prefix}\0{suffix}"),
            });
        let nul_title = (
            shell_text_strategy(1..24),
            shell_text_strategy(0..24),
            shell_text_strategy(0..24),
        )
            .prop_map(|(program, prefix, suffix)| InvalidShellText::NulTitle {
                program,
                title: format!("{prefix}\0{suffix}"),
            });

        proptest::prop_oneof![trimmed_empty, nul_program, nul_argument, nul_title]
    }

    #[derive(Clone, Debug)]
    enum EnvironmentEdit {
        Add { key: String, value: String },
        EditKey { selector: usize, key: String },
        EditValue { selector: usize, value: String },
        Remove { selector: usize },
    }

    #[derive(Debug)]
    enum InvalidEnvironment {
        WhitespaceKey { whitespace: String },
        EqualsKey { key: String },
        NulKey { key: String },
        NulValue { key: String, value: String },
        TrimmedDuplicateKey { key: String },
    }

    impl InvalidEnvironment {
        fn entries(&self) -> Vec<EnvironmentVariable> {
            match self {
                Self::WhitespaceKey { whitespace } => vec![EnvironmentVariable {
                    key: whitespace.clone(),
                    value: String::new(),
                }],
                Self::EqualsKey { key } | Self::NulKey { key } => {
                    vec![EnvironmentVariable {
                        key: key.clone(),
                        value: String::new(),
                    }]
                }
                Self::NulValue { key, value } => vec![EnvironmentVariable {
                    key: key.clone(),
                    value: value.clone(),
                }],
                Self::TrimmedDuplicateKey { key } => vec![
                    EnvironmentVariable {
                        key: key.clone(),
                        value: "first=value".to_string(),
                    },
                    EnvironmentVariable {
                        key: format!(" \t{key}\n"),
                        value: "second=value".to_string(),
                    },
                ],
            }
        }

        fn expected_error(&self) -> &'static str {
            match self {
                Self::WhitespaceKey { .. } => "Environment variable names cannot be empty.",
                Self::EqualsKey { .. } => "Environment variable names cannot contain `=`.",
                Self::NulKey { .. } | Self::NulValue { .. } => {
                    "Environment variable names and values cannot contain NUL."
                }
                Self::TrimmedDuplicateKey { .. } => "Environment variable names must be unique.",
            }
        }
    }

    #[derive(Debug)]
    struct EnvironmentScenario {
        edits: Vec<EnvironmentEdit>,
        invalid_environment: InvalidEnvironment,
    }

    const ENVIRONMENT_NAME_CHARACTERS: &[char] = &[
        'A', 'B', 'C', 'X', 'Y', 'Z', '0', '1', '2', '3', '4', '5', '_', '-', '.',
    ];
    const ENVIRONMENT_VALUE_CHARACTERS: &[char] = &[
        'a', 'b', 'c', 'X', 'Y', 'Z', '0', '1', '2', ' ', '\t', '/', ':', '_', '-', '.',
    ];

    fn environment_name_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        proptest::collection::vec(proptest::sample::select(ENVIRONMENT_NAME_CHARACTERS), 1..16)
            .prop_map(|characters| characters.into_iter().collect())
    }

    fn environment_key_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        (
            proptest::sample::select(vec!["", " ", "\t", " \t"]),
            environment_name_strategy(),
            proptest::sample::select(vec!["", " ", "\t", "\t "]),
        )
            .prop_map(|(prefix, name, suffix)| format!("{prefix}{name}{suffix}"))
    }

    fn environment_value_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        let fragment = || {
            proptest::collection::vec(
                proptest::sample::select(ENVIRONMENT_VALUE_CHARACTERS),
                0..16,
            )
            .prop_map(|characters| characters.into_iter().collect::<String>())
        };
        (fragment(), fragment(), fragment())
            .prop_map(|(left, middle, right)| format!("{left}={middle}={right}"))
    }

    fn environment_edit_strategy() -> impl proptest::strategy::Strategy<Value = EnvironmentEdit> {
        proptest::prop_oneof![
            (environment_key_strategy(), environment_value_strategy())
                .prop_map(|(key, value)| EnvironmentEdit::Add { key, value }),
            (
                proptest::arbitrary::any::<usize>(),
                environment_key_strategy()
            )
                .prop_map(|(selector, key)| EnvironmentEdit::EditKey { selector, key }),
            (
                proptest::arbitrary::any::<usize>(),
                environment_value_strategy(),
            )
                .prop_map(|(selector, value)| EnvironmentEdit::EditValue { selector, value }),
            proptest::arbitrary::any::<usize>()
                .prop_map(|selector| EnvironmentEdit::Remove { selector }),
        ]
    }

    fn invalid_environment_strategy()
    -> impl proptest::strategy::Strategy<Value = InvalidEnvironment> {
        let whitespace_key =
            proptest::collection::vec(proptest::sample::select(WHITESPACE_CHARACTERS), 1..16)
                .prop_map(|characters| InvalidEnvironment::WhitespaceKey {
                    whitespace: characters.into_iter().collect(),
                });
        let equals_key =
            environment_name_strategy().prop_map(|name| InvalidEnvironment::EqualsKey {
                key: format!("{name}=suffix"),
            });
        let nul_key = environment_name_strategy().prop_map(|name| InvalidEnvironment::NulKey {
            key: format!("{name}\0suffix"),
        });
        let nul_value =
            (environment_name_strategy(), environment_value_strategy()).prop_map(|(key, value)| {
                InvalidEnvironment::NulValue {
                    key,
                    value: format!("{value}\0suffix"),
                }
            });
        let duplicate_key = environment_name_strategy()
            .prop_map(|key| InvalidEnvironment::TrimmedDuplicateKey { key });

        proptest::prop_oneof![
            whitespace_key,
            equals_key,
            nul_key,
            nul_value,
            duplicate_key,
        ]
    }

    fn environment_scenario_strategy()
    -> impl proptest::strategy::Strategy<Value = EnvironmentScenario> {
        (
            proptest::collection::vec(environment_edit_strategy(), 1..48),
            invalid_environment_strategy(),
        )
            .prop_map(|(edits, invalid_environment)| EnvironmentScenario {
                edits,
                invalid_environment,
            })
    }

    fn selected_environment_index(selector: usize, length: usize) -> Option<usize> {
        (length > 0).then(|| selector % length)
    }

    fn apply_environment_edit(entries: &mut Vec<EnvironmentVariable>, edit: &EnvironmentEdit) {
        match edit {
            EnvironmentEdit::Add { key, value } => entries.push(EnvironmentVariable {
                key: key.clone(),
                value: value.clone(),
            }),
            EnvironmentEdit::EditKey { selector, key } => {
                if let Some(index) = selected_environment_index(*selector, entries.len())
                    && let Some(entry) = entries.get_mut(index)
                {
                    entry.key = key.clone();
                }
            }
            EnvironmentEdit::EditValue { selector, value } => {
                if let Some(index) = selected_environment_index(*selector, entries.len())
                    && let Some(entry) = entries.get_mut(index)
                {
                    entry.value = value.clone();
                }
            }
            EnvironmentEdit::Remove { selector } => {
                if let Some(index) = selected_environment_index(*selector, entries.len()) {
                    entries.remove(index);
                }
            }
        }
    }

    fn apply_reference_environment_edit(
        entries: &mut Vec<(String, String)>,
        edit: &EnvironmentEdit,
    ) {
        match edit {
            EnvironmentEdit::Add { key, value } => entries.push((key.clone(), value.clone())),
            EnvironmentEdit::EditKey { selector, key } => {
                if let Some(index) = selected_environment_index(*selector, entries.len())
                    && let Some((entry_key, _)) = entries.get_mut(index)
                {
                    *entry_key = key.clone();
                }
            }
            EnvironmentEdit::EditValue { selector, value } => {
                if let Some(index) = selected_environment_index(*selector, entries.len())
                    && let Some((_, entry_value)) = entries.get_mut(index)
                {
                    *entry_value = value.clone();
                }
            }
            EnvironmentEdit::Remove { selector } => {
                if let Some(index) = selected_environment_index(*selector, entries.len()) {
                    entries.remove(index);
                }
            }
        }
    }

    fn reference_validate_environment(
        entries: &[(String, String)],
    ) -> Result<collections::HashMap<String, String>, String> {
        let mut environment = collections::HashMap::default();
        for (entry_key, entry_value) in entries {
            let key = entry_key.trim();
            if key.is_empty() {
                return Err("Environment variable names cannot be empty.".to_string());
            }
            if key.contains('=') {
                return Err("Environment variable names cannot contain `=`.".to_string());
            }
            if key.contains('\0') || entry_value.contains('\0') {
                return Err("Environment variable names and values cannot contain NUL.".to_string());
            }
            if environment
                .insert(key.to_string(), entry_value.clone())
                .is_some()
            {
                return Err("Environment variable names must be unique.".to_string());
            }
        }
        Ok(environment)
    }

    /// Feature: terminal-settings-completion, Property 5: Environment editing validates keys and preserves values
    /// **Validates: Requirements 4.4, 4.7, 7.3, 7.4, 7.5, 7.6, 7.7**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn environment_editing_validates_keys_and_preserves_values(
        cx: &mut gpui::TestAppContext,
        #[strategy = environment_scenario_strategy()] scenario: EnvironmentScenario,
    ) {
        let mut entries = Vec::new();
        let mut reference_entries = Vec::new();
        for edit in &scenario.edits {
            apply_environment_edit(&mut entries, edit);
            apply_reference_environment_edit(&mut reference_entries, edit);
        }

        let edited_entries: Vec<_> = entries
            .iter()
            .map(|entry| (entry.key.as_str(), entry.value.as_str()))
            .collect();
        let reference_entry_text: Vec<_> = reference_entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        assert_eq!(edited_entries, reference_entry_text);

        let validation_result = validate_environment(&entries);
        let reference_validation_result = reference_validate_environment(&reference_entries);
        assert_eq!(validation_result, reference_validation_result);

        let invalid_entries = scenario.invalid_environment.entries();
        assert_eq!(
            validate_environment(&invalid_entries),
            Err(scenario.invalid_environment.expected_error().to_string())
        );

        if let Ok(validated_environment) = validate_environment(&entries) {
            for value in validated_environment.values() {
                assert!(value.matches('=').count() >= 2);
            }

            let expected_environment = validated_environment.clone();
            let (persisted_environment, construction_environment) = cx.update(|cx| {
                let settings_store = SettingsStore::test(cx);
                cx.set_global(settings_store);

                let environment_for_update = validated_environment.clone();
                SettingsStore::update_global(cx, |store, cx| {
                    store.update_user_settings(cx, |content| {
                        let terminal = content
                            .terminal
                            .get_or_insert_with(settings::TerminalSettingsContent::default);
                        terminal.project.env = Some(environment_for_update);
                    });
                });

                let persisted_environment = SettingsStore::global(cx)
                    .get_content_for_file(settings::SettingsFile::User)
                    .and_then(|content| content.terminal.as_ref())
                    .and_then(|terminal| terminal.project.env.as_ref())
                    .cloned();
                let construction_environment = TerminalSettings::get_global(cx).env.clone();
                (persisted_environment, construction_environment)
            });

            assert_eq!(persisted_environment, Some(expected_environment.clone()));
            assert_eq!(construction_environment, expected_environment);
        }
    }

    /// A minimal settings page for keymap-tab tests, with default form state.
    pub(crate) fn test_settings_page_stub(cx: &mut Context<SettingsPage>) -> SettingsPage {
        test_settings_page(
            ShellForm {
                mode: ShellMode::System,
                program: String::new(),
                arguments: Vec::new(),
                title_override: String::new(),
            },
            cx,
        )
    }

    fn test_settings_page(shell_form: ShellForm, cx: &mut Context<SettingsPage>) -> SettingsPage {
        theme::SystemAppearance::init(cx);
        let mut draft_revisions = DraftRevisions::default();
        draft_revisions.record_edit(DirtySetting::Shell);
        SettingsPage {
            focus_handle: cx.focus_handle(),
            titlebar_mouse_down: std::cell::Cell::new(false),
            active_tab: SettingsTab::Terminal,
            keymap_tab: crate::keymap::KeymapTab::empty(),
            themes_tab: crate::themes_tab::ThemesTab::for_test(),
            editing_field: None,
            editing_selection: 0..0,
            editing_anchor: 0,
            editing_cursor: 0,
            marked_range: None,
            draft_settings: TerminalSettings::get_global(cx).clone(),
            theme_name: String::new(),
            font_family: String::new(),
            shell_form,
            environment: Vec::new(),
            working_directory: WorkingDirectoryForm {
                mode: WorkingDirectoryMode::Home,
                directory: String::new(),
                fell_back_from_workspace_setting: false,
            },
            dirty_settings: vec![DirtySetting::Shell],
            draft_revisions,
            next_operation_id: 0,
            pending_write: None,
            close_prompt_open: false,
            save_status: SaveStatus::Idle,
            status_message: None,
            settings_write_request_count: 0,
        }
    }

    /// Feature: terminal-settings-completion, Property 4: Invalid structured shell text is rejected before persistence
    /// **Validates: Requirements 7.1, 7.2**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn invalid_structured_shell_text_is_rejected_before_persistence(
        cx: &mut gpui::TestAppContext,
        #[strategy = invalid_shell_text_strategy()] invalid_shell_text: InvalidShellText,
    ) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
        });

        let original_shell_form = invalid_shell_text.shell_form();
        let expected_error = invalid_shell_text.expected_error();
        let editable_field = invalid_shell_text.editable_field();
        let corrected_text = invalid_shell_text.corrected_text();
        let settings_page = cx.new(|cx| test_settings_page(original_shell_form.clone(), cx));

        settings_page.update(cx, |settings_page, cx| {
            settings_page.save_drafts(cx);

            assert_eq!(settings_page.settings_write_request_count, 0);
            assert_eq!(settings_page.save_status, SaveStatus::Failed);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some(expected_error)
            );
            assert_eq!(settings_page.shell_form, original_shell_form);
            assert!(settings_page.dirty_settings.contains(&DirtySetting::Shell));

            settings_page.editing_field = Some(editable_field);
            let text_length = settings_page.editing_text().encode_utf16().count();
            settings_page.editing_selection = 0..text_length;
            settings_page.replace_editing_text(None, corrected_text.to_string(), None, false, cx);

            assert_eq!(settings_page.editing_text(), corrected_text);
            assert_eq!(settings_page.save_status, SaveStatus::Idle);
            assert_eq!(settings_page.status_message, None);
            assert!(settings_page.dirty_settings.contains(&DirtySetting::Shell));
            assert!(validate_shell_form(&settings_page.shell_form).is_ok());
        });
    }

    #[test]
    fn reset_terminal_overrides_preserves_theme_and_non_terminal_settings() -> anyhow::Result<()> {
        let mut content = settings::SettingsContent::default();
        theme_settings::set_theme(
            &mut content,
            "Test Theme",
            theme::Appearance::Dark,
            theme::Appearance::Dark,
        );
        content.auto_update = Some(false);
        content.terminal = Some(settings::TerminalSettingsContent::default());
        let mut expected = content.clone();
        expected.terminal = None;

        reset_terminal_overrides(&mut content);

        assert!(content.terminal.is_none());
        assert_eq!(
            serde_json::to_value(content)?,
            serde_json::to_value(expected)?
        );
        Ok(())
    }

    /// Feature: terminal-settings-completion, Property 11: Reset removes only terminal overrides
    /// **Validates: Requirements 5.2, 5.3, 5.4, 5.5, 8.6, 8.7**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn reset_removes_only_terminal_overrides(
        cx: &mut gpui::TestAppContext,
        #[strategy = reset_jsonc_case_strategy()] case: ResetJsoncCase,
    ) {
        let reset_text = cx.update(|cx| {
            SettingsStore::test(cx).new_text_for_update(case.original.clone(), |content| {
                reset_terminal_overrides(content);
            })
        });
        let reset_text = reset_text.expect("valid JSONC reset should succeed");

        assert_eq!(&reset_text, &case.expected);
        for retained_fragment in &case.retained_fragments {
            assert!(
                reset_text.contains(retained_fragment),
                "retained fragment changed: {retained_fragment:?}"
            );
        }

        let parsed = settings::parse_json_with_comments::<serde_json::Value>(&reset_text)
            .expect("reset JSONC should remain valid");
        let parsed_object = parsed
            .as_object()
            .expect("reset JSONC root should remain an object");
        assert!(!parsed_object.contains_key("terminal"));
        assert_eq!(
            parsed_object
                .get("theme")
                .and_then(serde_json::Value::as_str),
            Some(case.theme_name.as_str())
        );
        assert_eq!(
            parsed_object
                .get("auto_update")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            parsed_object
                .get("reduce_motion")
                .and_then(serde_json::Value::as_str),
            Some("on")
        );

        let retained_key_positions =
            ["\"theme\"", "\"auto_update\"", "\"reduce_motion\""].map(|key| reset_text.find(key));
        assert!(
            retained_key_positions
                .windows(2)
                .all(|positions| positions[0] < positions[1]),
            "retained key order changed: {retained_key_positions:?}"
        );
    }

    /// The settings tabs share the title bar with the window controls, so a tab
    /// that does not fill the bar's full height leaves its hover highlight short
    /// of the bar's edges. Regression guard for the tabs being laid out at their
    /// content height instead of the title bar height.
    #[gpui::test]
    fn settings_tabs_fill_the_title_bar_height(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let (_settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(700.), px(3_000.)));
        cx.run_until_parked();

        let title_bar_height = window_chrome::TITLE_BAR_HEIGHT;
        let tab_selectors = [
            ("Terminal", "settings-tab-Terminal"),
            ("Keymap", "settings-tab-Keymap"),
            ("Themes", "settings-tab-Themes"),
        ];
        for (label, selector) in tab_selectors {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("settings tab {label} should be rendered"));
            assert_eq!(
                bounds.size.height, title_bar_height,
                "settings tab {label} should fill the title bar height"
            );
            assert_eq!(
                bounds.origin.y,
                px(0.),
                "settings tab {label} should start at the top of the title bar"
            );
        }
    }

    /// The Keymap tab must render real binding rows and its search/filter
    /// controls, not the former "Coming soon" placeholder.
    /// Regression: opening the binding editor must take text input away from
    /// the page, otherwise recorded keys land in the search box.
    #[gpui::test]
    fn opening_the_editor_clears_the_page_inline_edit(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::bind_default_keys(cx);
        });

        let (settings_page, cx) = cx.add_window_view(|_, cx| test_settings_page_stub(cx));
        settings_page.update(cx, |page, cx| page.keymap_tab.reload_bindings(cx));
        cx.run_until_parked();

        settings_page.update_in(cx, |page, window, cx| {
            // Simulate the user having the search box focused.
            page.begin_edit(EditableField::KeymapSearch, window, cx);
            assert!(page.editing_field.is_some());

            page.begin_keymap_binding_edit(0, window, cx);
            assert!(
                page.editing_field.is_none(),
                "recorded keystrokes must not be routed into the search box"
            );
            assert!(page.keymap_tab.is_editing());
        });
    }

    #[gpui::test]
    fn keymap_tab_renders_bindings_and_controls(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            crate::bind_default_keys(cx);
        });
        let (settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(900.), px(3_000.)));
        cx.run_until_parked();

        // Deliberately does not pre-load the bindings: opening the tab must
        // populate them by itself.
        settings_page.update(cx, |page, cx| {
            page.switch_tab(SettingsTab::Keymap, cx);
        });
        cx.run_until_parked();

        // The list is populated from the real keymap rather than being empty.
        for selector in [
            "keymap-search",
            "keymap-user-filter",
            "keymap-conflict-filter",
            "keymap-reset-defaults",
            "keymap-binding-0",
        ] {
            assert!(
                cx.debug_bounds(selector).is_some(),
                "the keymap tab should render {selector}"
            );
        }

        // The first row's edit control is what opens the rebind editor.
        assert!(
            cx.debug_bounds("keymap-edit-0").is_some(),
            "the first binding row should offer an edit control"
        );
    }

    /// Builds a fake HTTP client that answers the extension catalog request.
    fn fake_extension_api(entries: &[(&str, &str, &str)]) -> Arc<dyn http_client::HttpClient> {
        let data: Vec<String> = entries
            .iter()
            .enumerate()
            .map(|(index, (id, name, version))| {
                // Distinct counts let a test assert the badge reflects the
                // catalog value rather than a constant.
                let download_count = 1_250 * (index as u64 + 1);
                format!(
                    r#"{{"id":"{id}","name":"{name}","version":"{version}","description":"A test theme","schema_version":1,"provides":["themes"],"download_count":{download_count}}}"#
                )
            })
            .collect();
        let body = format!(r#"{{"data":[{}]}}"#, data.join(","));

        http_client::FakeHttpClient::create(move |_| {
            let body = body.clone();
            async move {
                Ok(http_client::Response::builder()
                    .status(200)
                    .body(http_client::AsyncBody::from(body))
                    .expect("the fake response should be buildable"))
            }
        })
    }

    /// An extension already on disk must offer Update and Uninstall rather than
    /// Install.
    #[gpui::test]
    fn themes_tab_offers_update_and_uninstall_for_an_installed_extension(
        cx: &mut gpui::TestAppContext,
    ) {
        // The installed set is built from a real (temporary) extension
        // directory, then injected, because the tab normally reads the
        // developer's data directory, which a test must not touch.
        let extensions_dir =
            tempfile::tempdir().expect("a temporary directory should be creatable");
        let extension_dir = extensions_dir.path().join("catppuccin");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::write(
            extension_dir.join("extension.toml"),
            "id = \"catppuccin\"\nname = \"Catppuccin\"\nversion = \"1.0.0\"\n\
             description = \"Installed copy\"\nthemes = []\n",
        )
        .unwrap();
        let installed = crate::extension_store::load_installed_extensions_in(extensions_dir.path());
        assert_eq!(installed.len(), 1, "the fixture should read as installed");

        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            cx.set_http_client(fake_extension_api(&[("catppuccin", "Catppuccin", "2.0.0")]));
        });
        let (settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(900.), px(3_000.)));
        cx.run_until_parked();

        // Open the tab first so its own load completes, then inject the
        // installed set: the tab re-reads the real data directory on open, and
        // the injected fixture must survive that.
        settings_page.update(cx, |page, cx| {
            page.switch_tab(SettingsTab::Themes, cx);
        });
        cx.run_until_parked();
        settings_page.update(cx, |page, cx| {
            page.themes_tab.set_installed_for_test(installed);
            cx.notify();
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("themes-update-catppuccin").is_some(),
            "a newer catalog version should offer Update"
        );
        assert!(
            cx.debug_bounds("themes-uninstall-catppuccin").is_some(),
            "an installed extension should offer Uninstall"
        );
        assert!(
            cx.debug_bounds("themes-install-catppuccin").is_none(),
            "an installed extension must not offer Install"
        );
    }

    /// A large catalog must not build a row element for every entry: the tab
    /// renders on every keystroke, so only visible rows may be constructed.
    #[gpui::test]
    fn themes_tab_virtualizes_a_large_catalog(cx: &mut gpui::TestAppContext) {
        let entries: Vec<(String, String, String)> = (0..400)
            .map(|index| {
                (
                    format!("theme-{index:03}"),
                    format!("Theme {index:03}"),
                    "1.0.0".to_string(),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str, &str)> = entries
            .iter()
            .map(|(id, name, version)| (id.as_str(), name.as_str(), version.as_str()))
            .collect();

        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            cx.set_http_client(fake_extension_api(&borrowed));
        });
        let (settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        // A short window so only a handful of rows can be visible.
        cx.simulate_resize(size(px(900.), px(700.)));
        cx.run_until_parked();

        settings_page.update(cx, |page, cx| {
            page.switch_tab(SettingsTab::Themes, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            settings_page.update(cx, |page, _| page.themes_tab.matches().len()),
            400,
            "the whole catalog should still be listed"
        );

        // Rows far past the viewport must not have been built.
        assert!(
            cx.debug_bounds("themes-row-150").is_none(),
            "virtualization should not build off-screen rows"
        );
        assert!(
            cx.debug_bounds("themes-row-0").is_some(),
            "the first visible row should still render"
        );
    }

    /// The installed filter must actually hide uninstalled extensions, so it can
    /// be used to find something to update or remove.
    #[gpui::test]
    fn themes_tab_installed_filter_hides_uninstalled_extensions(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            cx.set_http_client(fake_extension_api(&[
                ("catppuccin", "Catppuccin", "1.0.0"),
                ("gruvbox", "Gruvbox", "1.0.0"),
            ]));
        });
        let (settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(900.), px(3_000.)));
        cx.run_until_parked();

        settings_page.update(cx, |page, cx| {
            page.switch_tab(SettingsTab::Themes, cx);
        });
        cx.run_until_parked();

        // Both catalog entries are listed, so both rows are rendered.
        assert!(cx.debug_bounds("themes-row-0").is_some());
        assert!(cx.debug_bounds("themes-row-1").is_some());

        // Installing only catppuccin must leave exactly one row once the filter
        // is on, whichever row it lands in.
        settings_page.update(cx, |page, cx| {
            page.themes_tab.set_installed_for_test(vec![
                crate::extension_store::InstalledExtension {
                    id: "catppuccin".to_string(),
                    name: "Catppuccin".to_string(),
                    version: "1.0.0".to_string(),
                    description: String::new(),
                    repository: String::new(),
                    theme_families: Vec::new(),
                },
            ]);
            page.themes_tab.toggle_installed_filter(cx);
        });
        cx.run_until_parked();

        // Exactly one of the two rows survives, and it is catppuccin's.
        let first_visible = cx.debug_bounds("themes-row-0").is_some();
        let second_visible = cx.debug_bounds("themes-row-1").is_some();
        assert_eq!(
            u8::from(first_visible) + u8::from(second_visible),
            1,
            "only the installed extension should remain visible"
        );
        assert!(
            cx.debug_bounds("themes-row-0").is_some(),
            "catppuccin is the first catalog entry and is the installed one"
        );
        assert!(
            settings_page.update(cx, |page, _| page.themes_tab.only_installed()),
            "the filter should be marked as active"
        );
    }

    /// The Themes tab must render the catalog with real controls, not the
    /// former "Coming soon" placeholder.
    #[gpui::test]
    fn themes_tab_renders_the_catalog_and_controls(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            cx.set_http_client(fake_extension_api(&[("catppuccin", "Catppuccin", "1.0.0")]));
        });
        let (settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(900.), px(3_000.)));
        cx.run_until_parked();

        // Deliberately does not pre-load the catalog: opening the tab must
        // fetch it by itself.
        settings_page.update(cx, |page, cx| {
            page.switch_tab(SettingsTab::Themes, cx);
        });
        cx.run_until_parked();

        for selector in [
            "themes-search",
            "themes-reload",
            "themes-installed-filter",
            "themes-installed-count",
        ] {
            assert!(
                cx.debug_bounds(selector).is_some(),
                "the themes tab should render {selector}"
            );
        }
        assert!(
            cx.debug_bounds("themes-row-0").is_some(),
            "the catalog should produce at least one row"
        );
        assert!(
            cx.debug_bounds("themes-install-catppuccin").is_some(),
            "an uninstalled extension should offer an install control"
        );
        assert!(
            cx.debug_bounds("themes-uninstall-catppuccin").is_none(),
            "an uninstalled extension must not offer uninstall"
        );
        assert!(
            cx.debug_bounds("themes-downloads-catppuccin").is_some(),
            "a catalog row should show its download count"
        );

        let (text, is_success) = settings_page
            .update(cx, |page, _| {
                page.themes_tab
                    .status_text()
                    .map(|(text, success)| (text.to_string(), success))
            })
            .expect("a status should be shown after loading");
        assert!(text.contains('1'), "got: {text}");
        assert!(is_success);
    }

    /// A failing catalog request must surface an error instead of rendering an
    /// empty, unexplained list.
    #[gpui::test]
    fn themes_tab_reports_a_failed_catalog_fetch(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            cx.set_http_client(http_client::FakeHttpClient::create(|_| async move {
                Ok(http_client::Response::builder()
                    .status(503)
                    .body(http_client::AsyncBody::from("service unavailable"))
                    .expect("the fake response should be buildable"))
            }));
        });
        let (settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(900.), px(3_000.)));
        cx.run_until_parked();

        settings_page.update(cx, |page, cx| {
            page.switch_tab(SettingsTab::Themes, cx);
        });
        cx.run_until_parked();

        let (text, is_success) = settings_page
            .update(cx, |page, _| {
                page.themes_tab
                    .status_text()
                    .map(|(text, success)| (text.to_string(), success))
            })
            .expect("a status should be shown after a failed load");
        assert!(text.contains("503"), "got: {text}");
        assert!(!is_success);

        // The user must be able to retry rather than being stuck.
        assert!(cx.debug_bounds("themes-reload").is_some());
        settings_page.update(cx, |page, cx| {
            assert!(
                !page.themes_tab.is_busy(),
                "a failure must clear the busy state"
            );
            page.switch_tab(SettingsTab::Terminal, cx);
            page.switch_tab(SettingsTab::Themes, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn reset_terminal_defaults_action_is_rendered(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let (_settings_page, cx) = cx.add_window_view(|_, cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(700.), px(3_000.)));
        cx.run_until_parked();

        assert!(cx.debug_bounds("reset-terminal-defaults").is_some());
    }

    fn captured_save_patch(settings_page: &SettingsPage) -> CapturedPatch {
        CapturedPatch::Save(Box::new(CapturedSavePatch {
            draft: settings_page.current_draft(),
            dirty_settings: settings_page.dirty_settings.clone(),
            shell: None,
            environment: None,
            working_directory: None,
            appearance: theme::Appearance::Dark,
        }))
    }

    #[derive(Debug, PartialEq)]
    struct AtomicFailurePageSnapshot {
        draft_settings: String,
        theme_name: String,
        font_family: String,
        shell_form: ShellForm,
        environment: Vec<EnvironmentVariable>,
        working_directory: WorkingDirectoryForm,
        dirty_settings: Vec<DirtySetting>,
        next_revision: u64,
        field_revisions: HashMap<DirtySetting, u64>,
    }

    fn atomic_failure_page_snapshot(settings_page: &SettingsPage) -> AtomicFailurePageSnapshot {
        AtomicFailurePageSnapshot {
            draft_settings: format!("{:?}", settings_page.draft_settings),
            theme_name: settings_page.theme_name.clone(),
            font_family: settings_page.font_family.clone(),
            shell_form: settings_page.shell_form.clone(),
            environment: settings_page.environment.clone(),
            working_directory: settings_page.working_directory.clone(),
            dirty_settings: settings_page.dirty_settings.clone(),
            next_revision: settings_page.draft_revisions.next_revision,
            field_revisions: settings_page.draft_revisions.field_revisions.clone(),
        }
    }

    fn install_atomic_failure_test_settings(
        cx: &mut App,
        terminal_font_size: f32,
        terminal_scroll_multiplier: f32,
    ) {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |content| {
                content.auto_update = Some(false);
                let terminal = content
                    .terminal
                    .get_or_insert_with(settings::TerminalSettingsContent::default);
                terminal.font_size = Some(settings::FontSize(terminal_font_size));
                terminal.scroll_multiplier = Some(terminal_scroll_multiplier);
                terminal.alternate_scroll = Some(settings::AlternateScroll::Off);
            });
        });
    }

    fn user_settings_snapshot(cx: &App) -> Option<settings::SettingsContent> {
        SettingsStore::global(cx)
            .get_content_for_file(settings::SettingsFile::User)
            .cloned()
    }

    #[derive(Clone, Copy, Debug)]
    enum CompletionFailure {
        Writer,
        CanceledReceiver,
    }

    fn injected_completion_failure(
        failure: CompletionFailure,
        writer_failure_reason: &str,
    ) -> (Result<(), String>, String) {
        let (sender, completion) = futures::channel::oneshot::channel();
        let originating_reason = match failure {
            CompletionFailure::Writer => {
                assert!(
                    sender
                        .send(Err(anyhow::anyhow!(writer_failure_reason.to_string())))
                        .is_ok()
                );
                writer_failure_reason.to_string()
            }
            CompletionFailure::CanceledReceiver => {
                drop(sender);
                "The settings update was canceled.".to_string()
            }
        };
        let result = futures::executor::block_on(async move {
            match completion.await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => Err("The settings update was canceled.".to_string()),
            }
        });
        (result, originating_reason)
    }

    #[derive(Clone, Copy, Debug)]
    enum RevisionTestField {
        FontSize,
        ScrollMultiplier,
    }

    fn revision_test_field_strategy() -> impl proptest::strategy::Strategy<Value = RevisionTestField>
    {
        proptest::prop_oneof![
            proptest::strategy::Just(RevisionTestField::FontSize),
            proptest::strategy::Just(RevisionTestField::ScrollMultiplier),
        ]
    }

    /// Feature: terminal-settings-completion, Property 12: Write completion is revision-aware
    /// **Validates: Requirements 5.6, 5.7, 6.5, 6.6, 6.9, 6.10, 6.11**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn write_completion_is_revision_aware(
        cx: &mut gpui::TestAppContext,
        #[strategy = revision_test_field_strategy()] revised_field: RevisionTestField,
        captured_font_size: u8,
        captured_scroll_multiplier: u8,
        #[strategy = proptest::collection::vec(1_u8..=100, 1..12)] pending_values: Vec<u8>,
    ) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let (settings_page, cx) = cx.add_window_view(|_, cx| {
            let mut settings_page = test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            );
            settings_page.dirty_settings.clear();
            settings_page.draft_revisions.field_revisions.clear();
            settings_page
        });

        settings_page.update_in(cx, |settings_page, _window, cx| {
            settings_page.save_drafts(cx);
            assert_eq!(settings_page.settings_write_request_count, 0);
            assert!(settings_page.pending_write.is_none());
            assert_eq!(settings_page.save_status, SaveStatus::Succeeded);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("No settings changes to save.")
            );

            settings_page.update_draft(
                DirtySetting::FontSize,
                |settings| settings.font_size = Some(px(f32::from(captured_font_size) + 8.0)),
                cx,
            );
            settings_page.update_draft(
                DirtySetting::ScrollMultiplier,
                |settings| {
                    settings.scroll_multiplier = f32::from(captured_scroll_multiplier) / 10.0 + 0.1
                },
                cx,
            );

            let started_operation_id =
                settings_page.begin_write(captured_save_patch(settings_page), cx);
            assert_eq!(started_operation_id, Some(1));
            assert_eq!(settings_page.settings_write_request_count, 1);
            assert_eq!(settings_page.save_status, SaveStatus::Saving);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("Saving settings…")
            );

            settings_page.save_drafts(cx);
            assert_eq!(settings_page.settings_write_request_count, 1);
            assert_eq!(settings_page.save_status, SaveStatus::Saving);
            assert_eq!(
                settings_page
                    .pending_write
                    .as_ref()
                    .map(|pending| pending.operation_id),
                Some(1)
            );

            for value in &pending_values {
                match revised_field {
                    RevisionTestField::FontSize => settings_page.update_draft(
                        DirtySetting::FontSize,
                        |settings| settings.font_size = Some(px(f32::from(*value) + 50.0)),
                        cx,
                    ),
                    RevisionTestField::ScrollMultiplier => settings_page.update_draft(
                        DirtySetting::ScrollMultiplier,
                        |settings| settings.scroll_multiplier = f32::from(*value) / 10.0 + 10.0,
                        cx,
                    ),
                }
                assert_eq!(settings_page.save_status, SaveStatus::Saving);
                assert_eq!(settings_page.settings_write_request_count, 1);
                assert_eq!(
                    settings_page
                        .pending_write
                        .as_ref()
                        .map(|pending| pending.operation_id),
                    Some(1)
                );
            }

            assert!(!settings_page.request_close(_window, cx));
            assert_eq!(settings_page.save_status, SaveStatus::Saving);
            assert!(!settings_page.finish_write(2, Ok(()), cx));
            assert_eq!(settings_page.save_status, SaveStatus::Saving);
            assert_eq!(
                settings_page
                    .pending_write
                    .as_ref()
                    .map(|pending| pending.operation_id),
                Some(1)
            );
        });

        let captured_patch = settings_page.read_with(cx, |settings_page, _| {
            settings_page
                .pending_write
                .as_ref()
                .map(|pending| pending.captured_patch.clone())
        });
        assert!(captured_patch.is_some());
        if let Some(captured_patch) = captured_patch {
            cx.update(|_, cx| {
                SettingsStore::update_global(cx, |store, cx| {
                    store.update_user_settings(cx, |content| captured_patch.apply(content));
                });
            });
        }

        let last_pending_value = pending_values.last().copied().unwrap_or_default();
        settings_page.update_in(cx, |settings_page, _window, cx| {
            assert!(settings_page.finish_write(1, Ok(()), cx));
            assert!(settings_page.pending_write.is_none());
            assert_eq!(settings_page.save_status, SaveStatus::Succeeded);
            assert_eq!(settings_page.settings_write_request_count, 1);

            let expected_baseline_font_size = Some(px(f32::from(captured_font_size) + 8.0));
            let expected_baseline_scroll_multiplier =
                f32::from(captured_scroll_multiplier) / 10.0 + 0.1;
            let (expected_dirty_setting, cleared_setting) = match revised_field {
                RevisionTestField::FontSize => {
                    assert_eq!(
                        settings_page.draft_settings.font_size,
                        Some(px(f32::from(last_pending_value) + 50.0))
                    );
                    assert_eq!(
                        settings_page.draft_settings.scroll_multiplier,
                        expected_baseline_scroll_multiplier
                    );
                    (DirtySetting::FontSize, DirtySetting::ScrollMultiplier)
                }
                RevisionTestField::ScrollMultiplier => {
                    assert_eq!(
                        settings_page.draft_settings.font_size,
                        expected_baseline_font_size
                    );
                    assert_eq!(
                        settings_page.draft_settings.scroll_multiplier,
                        f32::from(last_pending_value) / 10.0 + 10.0
                    );
                    (DirtySetting::ScrollMultiplier, DirtySetting::FontSize)
                }
            };
            assert_eq!(settings_page.dirty_settings, vec![expected_dirty_setting]);
            assert!(
                settings_page
                    .draft_revisions
                    .field_revisions
                    .contains_key(&expected_dirty_setting)
            );
            assert!(
                !settings_page
                    .draft_revisions
                    .field_revisions
                    .contains_key(&cleared_setting)
            );
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("Settings saved. Additional changes remain.")
            );

            let draft_before_late_completion = settings_page.current_draft();
            let dirty_before_late_completion = settings_page.dirty_settings.clone();
            let second_operation_id =
                settings_page.begin_write(captured_save_patch(settings_page), cx);
            assert_eq!(second_operation_id, Some(2));
            assert!(!settings_page.finish_write(1, Ok(()), cx));
            assert_eq!(settings_page.save_status, SaveStatus::Saving);
            assert_eq!(settings_page.settings_write_request_count, 2);
            assert_eq!(
                settings_page
                    .pending_write
                    .as_ref()
                    .map(|pending| pending.operation_id),
                Some(2)
            );
            assert_eq!(
                settings_page.draft_settings.font_size,
                draft_before_late_completion.draft_settings.font_size
            );
            assert_eq!(
                settings_page.draft_settings.scroll_multiplier,
                draft_before_late_completion
                    .draft_settings
                    .scroll_multiplier
            );
            assert_eq!(settings_page.dirty_settings, dirty_before_late_completion);
        });
    }

    /// Feature: terminal-settings-completion, Property 13: Validation and persistence failures are atomic and visible
    /// **Validates: Requirements 5.10, 6.7, 6.8**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn validation_and_persistence_failures_are_atomic_and_visible(
        cx: &mut gpui::TestAppContext,
        #[strategy = invalid_shell_text_strategy()] invalid_shell_text: InvalidShellText,
        #[strategy = environment_name_strategy()] writer_failure_reason: String,
        terminal_font_size_seed: u8,
        terminal_scroll_multiplier_seed: u8,
        draft_font_size_seed: u8,
        pending_font_size_seed: u8,
    ) {
        let terminal_font_size = 8.0 + f32::from(terminal_font_size_seed % 65);
        let terminal_scroll_multiplier =
            0.1 + f32::from(terminal_scroll_multiplier_seed % 100) / 10.0;

        cx.update(|cx| {
            install_atomic_failure_test_settings(
                cx,
                terminal_font_size,
                terminal_scroll_multiplier,
            );
        });
        let file_before_validation = cx.update(|cx| user_settings_snapshot(cx));
        assert!(file_before_validation.is_some());
        let invalid_shell_form = invalid_shell_text.shell_form();
        let validation_reason = invalid_shell_text.expected_error();
        let validation_page = cx.new(|cx| test_settings_page(invalid_shell_form, cx));
        validation_page.update(cx, |settings_page, cx| {
            let page_before_validation = atomic_failure_page_snapshot(settings_page);

            settings_page.save_drafts(cx);

            assert_eq!(
                atomic_failure_page_snapshot(settings_page),
                page_before_validation
            );
            assert_eq!(settings_page.settings_write_request_count, 0);
            assert!(settings_page.pending_write.is_none());
            assert_eq!(settings_page.save_status, SaveStatus::Failed);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some(validation_reason)
            );
        });
        assert_eq!(
            cx.update(|cx| user_settings_snapshot(cx)),
            file_before_validation
        );

        for failure in [
            CompletionFailure::Writer,
            CompletionFailure::CanceledReceiver,
        ] {
            cx.update(|cx| {
                install_atomic_failure_test_settings(
                    cx,
                    terminal_font_size,
                    terminal_scroll_multiplier,
                );
            });
            let file_before_completion = cx.update(|cx| user_settings_snapshot(cx));
            assert!(file_before_completion.is_some());
            let settings_page = cx.new(|cx| {
                let mut settings_page = test_settings_page(
                    ShellForm {
                        mode: ShellMode::System,
                        program: String::new(),
                        arguments: Vec::new(),
                        title_override: String::new(),
                    },
                    cx,
                );
                settings_page.dirty_settings.clear();
                settings_page.draft_revisions = DraftRevisions::default();
                settings_page
            });

            let (operation_id, page_before_completion) =
                settings_page.update(cx, |settings_page, cx| {
                    settings_page.update_draft(
                        DirtySetting::FontSize,
                        |settings| {
                            settings.font_size =
                                Some(px(8.0 + f32::from(draft_font_size_seed % 65)))
                        },
                        cx,
                    );
                    settings_page.update_draft(
                        DirtySetting::ScrollMultiplier,
                        |settings| {
                            settings.scroll_multiplier =
                                0.1 + f32::from(terminal_scroll_multiplier_seed % 100) / 10.0
                        },
                        cx,
                    );
                    let operation_id = settings_page
                        .begin_write(captured_save_patch(settings_page), cx)
                        .unwrap_or_default();
                    assert_ne!(operation_id, 0);

                    settings_page.update_draft(
                        DirtySetting::FontSize,
                        |settings| {
                            settings.font_size =
                                Some(px(73.0 + f32::from(pending_font_size_seed % 65)))
                        },
                        cx,
                    );
                    (operation_id, atomic_failure_page_snapshot(settings_page))
                });

            let (completion_result, originating_reason) =
                injected_completion_failure(failure, &writer_failure_reason);
            settings_page.update(cx, |settings_page, cx| {
                assert!(settings_page.finish_write(operation_id, completion_result, cx));

                assert_eq!(
                    atomic_failure_page_snapshot(settings_page),
                    page_before_completion
                );
                assert!(settings_page.pending_write.is_none());
                assert_eq!(settings_page.save_status, SaveStatus::Failed);
                let expected_message = format!("Could not save settings: {originating_reason}");
                assert_eq!(
                    settings_page.status_message.as_deref(),
                    Some(expected_message.as_str())
                );
                assert!(expected_message.contains(&originating_reason));
            });

            let file_after_completion = cx.update(|cx| user_settings_snapshot(cx));
            assert_eq!(file_after_completion, file_before_completion);
        }
    }

    #[gpui::test]
    fn save_without_dirty_settings_does_not_start_a_write(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        let settings_page = cx.new(|cx| {
            let mut settings_page = test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            );
            settings_page.dirty_settings.clear();
            settings_page.draft_revisions.field_revisions.clear();
            settings_page
        });

        settings_page.update(cx, |settings_page, cx| {
            settings_page.save_drafts(cx);

            assert_eq!(settings_page.settings_write_request_count, 0);
            assert!(settings_page.pending_write.is_none());
            assert_eq!(settings_page.save_status, SaveStatus::Succeeded);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("No settings changes to save.")
            );
        });
    }

    #[gpui::test]
    fn matching_success_reloads_baseline_and_replays_newer_field_edits(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let settings_page = cx.new(|cx| {
            let mut settings_page = test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            );
            settings_page.dirty_settings.clear();
            settings_page.draft_revisions.field_revisions.clear();
            settings_page
        });

        let operation_id = settings_page.update(cx, |settings_page, cx| {
            settings_page.update_draft(
                DirtySetting::FontSize,
                |settings| settings.font_size = Some(px(20.)),
                cx,
            );
            let captured_patch = captured_save_patch(settings_page);
            let operation_id = settings_page
                .begin_write(captured_patch, cx)
                .unwrap_or_default();
            settings_page.update_draft(
                DirtySetting::ScrollMultiplier,
                |settings| settings.scroll_multiplier = 3.25,
                cx,
            );

            assert_eq!(settings_page.save_status, SaveStatus::Saving);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("Saving settings…")
            );
            operation_id
        });
        assert_ne!(operation_id, 0);

        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content
                        .terminal
                        .get_or_insert_with(settings::TerminalSettingsContent::default)
                        .font_size = Some(settings::FontSize(20.));
                });
            });
        });
        settings_page.update(cx, |settings_page, cx| {
            assert!(settings_page.finish_write(operation_id, Ok(()), cx));

            assert_eq!(settings_page.draft_settings.font_size, Some(px(20.)));
            assert_eq!(settings_page.draft_settings.scroll_multiplier, 3.25);
            assert_eq!(
                settings_page.dirty_settings,
                vec![DirtySetting::ScrollMultiplier]
            );
            assert!(
                settings_page
                    .draft_revisions
                    .field_revisions
                    .contains_key(&DirtySetting::ScrollMultiplier)
            );
            assert!(settings_page.pending_write.is_none());
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("Settings saved. Additional changes remain.")
            );
        });
    }

    #[gpui::test]
    fn failed_and_late_completions_do_not_replace_current_draft_or_state(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        let settings_page = cx.new(|cx| {
            let mut settings_page = test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            );
            settings_page.dirty_settings.clear();
            settings_page.draft_revisions.field_revisions.clear();
            settings_page
        });

        settings_page.update(cx, |settings_page, cx| {
            settings_page.update_draft(
                DirtySetting::ScrollMultiplier,
                |settings| settings.scroll_multiplier = 2.5,
                cx,
            );
            let first_operation_id = settings_page
                .begin_write(captured_save_patch(settings_page), cx)
                .unwrap_or_default();
            settings_page.update_draft(
                DirtySetting::ScrollMultiplier,
                |settings| settings.scroll_multiplier = 4.5,
                cx,
            );
            let draft_before_failure = settings_page.current_draft();
            let dirty_before_failure = settings_page.dirty_settings.clone();
            let revisions_before_failure = settings_page.draft_revisions.field_revisions.clone();

            assert!(settings_page.finish_write(
                first_operation_id,
                Err("The settings update was canceled.".to_string()),
                cx,
            ));
            assert_eq!(
                settings_page
                    .current_draft()
                    .draft_settings
                    .scroll_multiplier,
                draft_before_failure.draft_settings.scroll_multiplier
            );
            assert_eq!(settings_page.dirty_settings, dirty_before_failure);
            assert_eq!(
                settings_page.draft_revisions.field_revisions,
                revisions_before_failure
            );

            let second_operation_id = settings_page
                .begin_write(captured_save_patch(settings_page), cx)
                .unwrap_or_default();
            assert_ne!(first_operation_id, second_operation_id);
            assert!(!settings_page.finish_write(first_operation_id, Ok(()), cx));
            assert_eq!(settings_page.save_status, SaveStatus::Saving);
            assert_eq!(
                settings_page
                    .pending_write
                    .as_ref()
                    .map(|pending| pending.operation_id),
                Some(second_operation_id)
            );
        });
    }

    #[gpui::test]
    fn reset_terminal_defaults_updates_draft_until_saved(cx: &mut gpui::TestAppContext) {
        let default_font_size = cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            let default_font_size = TerminalSettings::get_global(cx).font_size;
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content
                        .terminal
                        .get_or_insert_with(settings::TerminalSettingsContent::default)
                        .font_size = Some(settings::FontSize(31.));
                });
            });
            default_font_size
        });
        let settings_page = cx.new(|cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });
        assert_eq!(
            settings_page.read_with(cx, |settings_page, _| settings_page
                .draft_settings
                .font_size),
            Some(px(31.))
        );

        settings_page.update(cx, |settings_page, cx| {
            settings_page.reset_terminal_defaults(cx);
            assert_eq!(settings_page.draft_settings.font_size, default_font_size);
            assert_eq!(
                settings_page.dirty_settings,
                vec![DirtySetting::ResetTerminalDefaults]
            );
            assert!(settings_page.pending_write.is_none());
            assert_eq!(settings_page.settings_write_request_count, 0);
            assert_eq!(settings_page.save_status, SaveStatus::Idle);
            assert_eq!(settings_page.status_message, None);
        });
        assert_eq!(
            cx.update(|cx| {
                user_settings_snapshot(cx)
                    .and_then(|content| content.terminal)
                    .and_then(|terminal| terminal.font_size)
            }),
            Some(settings::FontSize(31.))
        );

        // Simulate the persistence completion without starting RealFs I/O. This
        // test verifies the draft state before and after the completion.
        let captured_patch =
            settings_page.read_with(cx, |settings_page, _| captured_save_patch(settings_page));
        let operation_id = settings_page.update(cx, |settings_page, cx| {
            settings_page
                .begin_write(captured_patch.clone(), cx)
                .unwrap_or_default()
        });
        assert_ne!(operation_id, 0);
        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| captured_patch.apply(content));
            });
        });
        settings_page.update(cx, |settings_page, cx| {
            assert!(settings_page.finish_write(operation_id, Ok(()), cx));
            assert_eq!(settings_page.draft_settings.font_size, default_font_size);
            assert!(settings_page.dirty_settings.is_empty());
            assert_eq!(settings_page.save_status, SaveStatus::Succeeded);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("Settings saved.")
            );
        });
    }

    #[gpui::test]
    fn failed_reset_retains_draft_and_displays_the_reason(cx: &mut gpui::TestAppContext) {
        let default_font_size = cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            let default_font_size = TerminalSettings::get_global(cx).font_size;
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content
                        .terminal
                        .get_or_insert_with(settings::TerminalSettingsContent::default)
                        .font_size = Some(settings::FontSize(31.));
                });
            });
            default_font_size
        });
        let settings_page = cx.new(|cx| {
            test_settings_page(
                ShellForm {
                    mode: ShellMode::System,
                    program: String::new(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                },
                cx,
            )
        });

        settings_page.update(cx, |settings_page, cx| {
            settings_page.reset_terminal_defaults(cx);
            assert_eq!(settings_page.draft_settings.font_size, default_font_size);
            let captured_patch = captured_save_patch(settings_page);
            let operation_id = settings_page
                .begin_write(captured_patch, cx)
                .unwrap_or_default();
            assert!(settings_page.finish_write(
                operation_id,
                Err("settings file is read-only".to_string()),
                cx,
            ));

            assert_eq!(settings_page.draft_settings.font_size, default_font_size);
            assert_eq!(settings_page.save_status, SaveStatus::Failed);
            assert_eq!(
                settings_page.status_message.as_deref(),
                Some("Could not save settings: settings file is read-only")
            );
        });
        assert_eq!(
            cx.update(|cx| {
                user_settings_snapshot(cx)
                    .and_then(|content| content.terminal)
                    .and_then(|terminal| terminal.font_size)
            }),
            Some(settings::FontSize(31.))
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 128,
            failure_persistence: None,
            ..proptest::test_runner::Config::default()
        })]

        /// Feature: terminal-settings-completion, Property 1: Numeric controls remain in their valid domains
        /// **Validates: Requirements 2.2, 2.3, 2.4, 7.11**
        #[test]
        fn numeric_controls_remain_in_their_valid_domains(
            starting_positions in proptest::collection::vec(0_u16..=1_000, 6),
            adjustment_sequences in proptest::collection::vec(
                proptest::collection::vec(-32_i16..=32, 1..64),
                6,
            ),
        ) {
            for ((domain, starting_position), adjustments) in NUMERIC_CONTROL_DOMAINS
                .iter()
                .zip(&starting_positions)
                .zip(&adjustment_sequences)
            {
                let position = f32::from(*starting_position) / 1_000.0;
                let mut value = domain.minimum
                    + (domain.generated_maximum - domain.minimum) * position;

                for adjustment in adjustments {
                    value = adjust_bounded(
                        value,
                        f32::from(*adjustment) * domain.delta_scale,
                        domain.minimum,
                        domain.maximum,
                    );
                    proptest::prop_assert!(
                        value >= domain.minimum && value <= domain.maximum,
                        "{} escaped [{}, {}] after adjustment {}: {}",
                        domain.name,
                        domain.minimum,
                        domain.maximum,
                        adjustment,
                        value,
                    );
                }

                let minimum = adjust_bounded(
                    domain.minimum,
                    -domain.delta_scale,
                    domain.minimum,
                    domain.maximum,
                );
                proptest::prop_assert_eq!(minimum, domain.minimum, "{} minimum", domain.name);
                proptest::prop_assert_eq!(
                    adjust_bounded(
                        minimum,
                        -domain.delta_scale,
                        domain.minimum,
                        domain.maximum,
                    ),
                    minimum,
                    "{} repeated minimum clamp",
                    domain.name,
                );

                let maximum = adjust_bounded(
                    domain.maximum,
                    domain.delta_scale,
                    domain.minimum,
                    domain.maximum,
                );
                proptest::prop_assert_eq!(maximum, domain.maximum, "{} maximum", domain.name);
                proptest::prop_assert_eq!(
                    adjust_bounded(
                        maximum,
                        domain.delta_scale,
                        domain.minimum,
                        domain.maximum,
                    ),
                    maximum,
                    "{} repeated maximum clamp",
                    domain.name,
                );
            }
        }
    }
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
