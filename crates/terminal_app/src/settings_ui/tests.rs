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
            _ => settings::TerminalLineHeight::Custom(1.0 + f32::from(self.seeds[3] % 9) * 0.25),
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
        normalize_fixed_directory_input("/tmp/bad\0path", Some(Path::new("/home/test"))).is_err()
    );
    assert!(
        normalize_fixed_directory_input("relative/path", Some(Path::new("/home/test"))).is_err()
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
            |(home_components, directory_components, canonical_components, relative_components)| {
                FixedDirectoryScenario {
                    home_components,
                    directory_components,
                    canonical_components,
                    relative_components,
                }
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
                    terminal.project.working_directory = Some(validated_working_directory.clone());
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
    let nonexistent_access = RecordingFakeDirectoryAccess::nonexistent(PathBuf::from("/unused"));
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

fn shell_argument_edit_strategy() -> impl proptest::strategy::Strategy<Value = ShellArgumentEdit> {
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
            if let Some(index) = selected_shell_argument_index(*selector, form.arguments.len()) {
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
            Self::TrimmedEmptyProgram { mode, program } | Self::NulProgram { mode, program } => {
                ShellForm {
                    mode: *mode,
                    program: program.clone(),
                    arguments: Vec::new(),
                    title_override: String::new(),
                }
            }
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

fn invalid_shell_text_strategy() -> impl proptest::strategy::Strategy<Value = InvalidShellText> {
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

fn invalid_environment_strategy() -> impl proptest::strategy::Strategy<Value = InvalidEnvironment> {
    let whitespace_key =
        proptest::collection::vec(proptest::sample::select(WHITESPACE_CHARACTERS), 1..16).prop_map(
            |characters| InvalidEnvironment::WhitespaceKey {
                whitespace: characters.into_iter().collect(),
            },
        );
    let equals_key = environment_name_strategy().prop_map(|name| InvalidEnvironment::EqualsKey {
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
    let duplicate_key =
        environment_name_strategy().prop_map(|key| InvalidEnvironment::TrimmedDuplicateKey { key });

    proptest::prop_oneof![
        whitespace_key,
        equals_key,
        nul_key,
        nul_value,
        duplicate_key,
    ]
}

fn environment_scenario_strategy() -> impl proptest::strategy::Strategy<Value = EnvironmentScenario>
{
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

fn apply_reference_environment_edit(entries: &mut Vec<(String, String)>, edit: &EnvironmentEdit) {
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

    // A `Send Text` / `Send Keystroke` row must say what it sends: the
    // action name alone leaves the user unable to tell the bindings apart.
    assert!(
        cx.debug_bounds("keymap-payload").is_some(),
        "some Send row should explain the payload it sends"
    );
}

/// A binding that an `"unbind"` entry disabled must keep showing its keystrokes,
/// dimmed: the keystroke is the only thing telling the user which key stopped
/// working, and it is what "restore" brings back.
#[gpui::test]
fn keymap_tab_shows_a_disabled_binding_with_its_keystrokes(cx: &mut gpui::TestAppContext) {
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

    settings_page.update(cx, |page, cx| {
        page.switch_tab(SettingsTab::Keymap, cx);
    });
    cx.run_until_parked();

    // The state a hand-written `keymap.json` unbind entry loads into.
    settings_page.update(cx, |page, cx| {
        page.apply_user_keymap(
            r#"[{ "context": "TerminalWindow", "unbind": { "cmd-t": "terminal_app::NewTab" } }]"#,
            cx,
        );
    });
    cx.run_until_parked();

    let (disabled_index, has_keystrokes, is_mapped, is_suppressed, can_unbind) = settings_page
        .read_with(cx, |page, _| {
            let (index, binding) = page
                .keymap_tab
                .bindings()
                .iter()
                .enumerate()
                .find(|(_, binding)| {
                    binding.action().name == "terminal_app::NewTab"
                        && binding.is_unbound_by_unbind()
                })
                .expect("the disabled binding should stay listed so it can be restored");
            (
                index,
                binding
                    .keystrokes()
                    .is_some_and(|keystrokes| !keystrokes.is_empty()),
                !binding.is_unbound(),
                binding.is_unbound_by_unbind(),
                binding.can_unbind(),
            )
        });

    assert!(
        has_keystrokes && is_mapped && is_suppressed,
        "a disabled binding is still a mapped binding that keeps its keystrokes"
    );
    assert!(
        !can_unbind,
        "a disabled binding cannot be unbound a second time"
    );
    assert!(
        cx.debug_bounds("KEY_BINDING-t").is_some(),
        "the disabled row should render the keystroke it disabled, not a state word"
    );

    // A disabled binding keeps the pencil every other row has, so it can be
    // rebound in one step instead of restoring first; it trades the trash for
    // the restore control.
    // `debug_bounds` is `&'static str`-only, so the row selectors are leaked.
    let row_selector = |control: &str| -> &'static str {
        String::leak(format!("keymap-{control}-{disabled_index}"))
    };
    assert!(
        cx.debug_bounds(row_selector("edit")).is_some(),
        "a disabled binding should offer the same pencil as any other row"
    );
    assert!(
        cx.debug_bounds(row_selector("restore")).is_some(),
        "a disabled binding should offer restore"
    );
    assert!(
        cx.debug_bounds(row_selector("unbind")).is_none(),
        "a disabled binding cannot be unbound a second time"
    );

    // The pencil opens a replacement, not an addition: the old keystroke is
    // already accounted for by the unbind entry.
    settings_page.update_in(cx, |page, window, cx| {
        page.keymap_tab.begin_edit(disabled_index, window, cx);
        assert_eq!(
            page.keymap_tab.editing_index(),
            Some(disabled_index),
            "rebinding a disabled binding should replace it, not add a second one"
        );
        page.keymap_tab.cancel_edit(cx);
    });
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
    let extensions_dir = tempfile::tempdir().expect("a temporary directory should be creatable");
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
        page.themes_tab
            .set_installed_for_test(vec![crate::extension_store::InstalledExtension {
                id: "catppuccin".to_string(),
                name: "Catppuccin".to_string(),
                version: "1.0.0".to_string(),
                description: String::new(),
                repository: String::new(),
                theme_families: Vec::new(),
            }]);
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

fn revision_test_field_strategy() -> impl proptest::strategy::Strategy<Value = RevisionTestField> {
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
        let second_operation_id = settings_page.begin_write(captured_save_patch(settings_page), cx);
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
    let terminal_scroll_multiplier = 0.1 + f32::from(terminal_scroll_multiplier_seed % 100) / 10.0;

    cx.update(|cx| {
        install_atomic_failure_test_settings(cx, terminal_font_size, terminal_scroll_multiplier);
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
                        settings.font_size = Some(px(8.0 + f32::from(draft_font_size_seed % 65)))
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
                        settings.font_size = Some(px(73.0 + f32::from(pending_font_size_seed % 65)))
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
fn matching_success_reloads_baseline_and_replays_newer_field_edits(cx: &mut gpui::TestAppContext) {
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
