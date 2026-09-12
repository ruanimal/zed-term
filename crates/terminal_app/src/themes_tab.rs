//! Themes tab support for the settings page.
//!
//! Lets the user browse the Zed extension catalog, install a theme extension,
//! update an installed one, and remove it. All network and disk work goes
//! through [`crate::extension_store`]; this module owns the tab's state and
//! rendering.

use std::{path::PathBuf, sync::Arc};

use gpui::{Context, Pixels, SharedString, WeakEntity, px, uniform_list};
use http_client::HttpClient;
use ui::{
    AnyElement, Button, ButtonStyle, Color, Icon, IconButton, IconName, IconSize, Label, LabelSize,
    ParentElement as _, Styled as _, Tooltip, h_flex, prelude::*, v_flex,
};

use crate::extension_store::{
    self, CatalogEntry, ExtensionMetadata, InstalledExtension, StoreError,
};

/// Fixed height of one extension row.
///
/// Virtualization measures the first row and assumes the rest match, so the row
/// height is pinned rather than derived from its content.
const THEME_ROW_HEIGHT: Pixels = px(52.0);

/// What the tab is currently doing, so the controls can be disabled coherently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BusyState {
    Idle,
    Fetching,
    Installing,
    Uninstalling,
}

impl BusyState {
    fn is_busy(self) -> bool {
        self != Self::Idle
    }
}

/// One row of the list: a catalog entry, or an installed extension the catalog
/// does not mention.
#[derive(Clone)]
pub struct ThemeRow {
    kind: ThemeRowKind,
    /// Lowercased id, name, and description, joined by NUL.
    ///
    /// Precomputed when rows are rebuilt because the search runs on every
    /// keystroke.
    search_text: String,
}

#[derive(Clone)]
enum ThemeRowKind {
    Catalog(Box<CatalogEntry>),
    InstalledOnly(Box<InstalledExtension>),
}

impl ThemeRow {
    fn catalog(entry: CatalogEntry) -> Self {
        let metadata = &entry.metadata;
        Self {
            search_text: search_text(&metadata.id, &metadata.name, &metadata.description),
            kind: ThemeRowKind::Catalog(Box::new(entry)),
        }
    }

    fn installed_only(extension: InstalledExtension) -> Self {
        Self {
            search_text: search_text(&extension.id, &extension.name, &extension.description),
            kind: ThemeRowKind::InstalledOnly(Box::new(extension)),
        }
    }
}

/// Builds the lowercased haystack a row is matched against.
///
/// The fields are joined with a character no id, name, or description contains,
/// so a query spanning a field boundary cannot match by accident.
fn search_text(id: &str, name: &str, description: &str) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        id.to_lowercase(),
        name.to_lowercase(),
        description.to_lowercase()
    )
}

impl ThemeRow {
    pub fn id(&self) -> &str {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => &entry.metadata.id,
            ThemeRowKind::InstalledOnly(extension) => &extension.id,
        }
    }

    fn name(&self) -> &str {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => &entry.metadata.name,
            ThemeRowKind::InstalledOnly(extension) => &extension.name,
        }
    }

    fn description(&self) -> &str {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => &entry.metadata.description,
            ThemeRowKind::InstalledOnly(extension) => &extension.description,
        }
    }

    fn is_installed(&self) -> bool {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => entry.is_installed(),
            ThemeRowKind::InstalledOnly(_) => true,
        }
    }

    fn installed_version(&self) -> Option<&str> {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => entry.installed_version.as_deref(),
            ThemeRowKind::InstalledOnly(extension) => Some(&extension.version),
        }
    }

    fn latest_version(&self) -> Option<&str> {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => Some(&entry.metadata.version),
            ThemeRowKind::InstalledOnly(_) => None,
        }
    }

    fn update_available(&self) -> bool {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => entry.update_available,
            ThemeRowKind::InstalledOnly(_) => false,
        }
    }

    fn theme_count(&self) -> usize {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => entry.installed_theme_count,
            ThemeRowKind::InstalledOnly(extension) => extension.theme_count(),
        }
    }

    /// Downloads reported by the catalog.
    ///
    /// An extension that is installed but absent from the catalog has no
    /// download count, because the API is the only source of that number.
    fn download_count(&self) -> Option<u64> {
        match &self.kind {
            ThemeRowKind::Catalog(entry) => Some(entry.metadata.download_count),
            ThemeRowKind::InstalledOnly(_) => None,
        }
    }
}

/// Renders a download count compactly, since the list column is narrow.
///
/// Exact numbers are kept below 1000; above that the count is abbreviated, which
/// is all the precision a popularity signal needs.
fn format_download_count(count: u64) -> String {
    let (scaled, suffix) = if count >= 1_000_000 {
        (count as f64 / 1_000_000.0, "M")
    } else if count >= 1_000 {
        (count as f64 / 1_000.0, "K")
    } else {
        return count.to_string();
    };
    let rounded = (scaled * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}{suffix}")
    } else {
        format!("{rounded:.1}{suffix}")
    }
}

/// A message shown under the toolbar.
struct StatusMessage {
    text: SharedString,
    kind: StatusKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Success,
    Error,
}

pub struct ThemesTab {
    /// Directory the installed extensions are read from.
    ///
    /// Held as a field rather than read from the global paths so the tab can be
    /// exercised against a temporary directory; production uses the app's
    /// extensions directory.
    extensions_dir: PathBuf,
    /// The catalog as last fetched; empty until the first successful load.
    catalog: Vec<ExtensionMetadata>,
    rows: Vec<ThemeRow>,
    matches: Vec<usize>,
    search_query: String,
    /// When set, only extensions already installed are listed, which is the
    /// quickest way to find something to update or uninstall.
    only_installed: bool,
    installed: Vec<InstalledExtension>,
    busy: BusyState,
    /// Id of the extension an install or uninstall is running for, so only that
    /// row shows a spinner and disables its buttons.
    busy_extension: Option<String>,
    status: Option<StatusMessage>,
    loaded_once: bool,
}

impl ThemesTab {
    pub fn empty() -> Self {
        Self::new(paths::extensions_dir().clone())
    }

    /// A tab pointed at a path that does not exist, for tests that must not
    /// read the developer's real extensions directory.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::new(std::env::temp_dir().join(format!(
            "zedterm-themes-tab-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )))
    }

    /// Creates a tab that reads installed extensions from `extensions_dir`.
    pub fn new(extensions_dir: PathBuf) -> Self {
        Self {
            extensions_dir,
            catalog: Vec::new(),
            rows: Vec::new(),
            matches: Vec::new(),
            search_query: String::new(),
            only_installed: false,
            installed: Vec::new(),
            busy: BusyState::Idle,
            busy_extension: None,
            status: None,
            loaded_once: false,
        }
    }

    pub fn rows(&self) -> &[ThemeRow] {
        &self.rows
    }

    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn only_installed(&self) -> bool {
        self.only_installed
    }

    pub fn installed(&self) -> &[InstalledExtension] {
        &self.installed
    }

    pub fn is_busy(&self) -> bool {
        self.busy.is_busy()
    }

    pub fn status_text(&self) -> Option<(&str, bool)> {
        self.status
            .as_ref()
            .map(|status| (status.text.as_ref(), status.kind != StatusKind::Error))
    }

    pub fn busy_extension(&self) -> Option<&str> {
        self.busy_extension.as_deref()
    }

    pub fn has_loaded(&self) -> bool {
        self.loaded_once
    }

    /// Replaces the installed set, for tests that must not read the developer's
    /// real data directory.
    #[cfg(test)]
    pub fn set_installed_for_test(&mut self, installed: Vec<InstalledExtension>) {
        self.installed = installed;
        self.rebuild_rows();
        self.loaded_once = true;
    }

    /// Re-reads the installed extensions from disk and re-merges them with the
    /// cached catalog, so installs made elsewhere show up on tab open.
    pub fn refresh_installed(&mut self) {
        self.installed = extension_store::load_installed_extensions_in(&self.extensions_dir);
        self.rebuild_rows();
    }

    /// Recomputes the visible rows from the cached catalog and installed set.
    fn rebuild_rows(&mut self) {
        let mut rows: Vec<ThemeRow> =
            extension_store::merge_catalog(self.catalog.clone(), &self.installed)
                .into_iter()
                .map(ThemeRow::catalog)
                .collect();
        // Extensions the catalog omits are still shown so they can be removed.
        rows.extend(
            extension_store::installed_absent_from_catalog(&self.catalog, &self.installed)
                .into_iter()
                .map(ThemeRow::installed_only),
        );
        self.rows = rows;
        self.refresh_matches();
    }

    /// Replaces the cached catalog and recomputes the rows.
    fn set_catalog(&mut self, catalog: Vec<ExtensionMetadata>) {
        self.catalog = catalog;
        self.rebuild_rows();
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

    /// Recomputes which rows pass the search and installed filters.
    ///
    /// Filtering is applied locally rather than by re-querying the API so that
    /// typing stays responsive and works offline; the search matches the id,
    /// name, and description.
    fn refresh_matches(&mut self) {
        let query = self.search_query.trim().to_lowercase();
        let only_installed = self.only_installed;
        self.matches = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                if only_installed && !row.is_installed() {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                query.is_empty() || row.search_text.contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
    }

    /// Toggles the installed-only filter.
    pub fn toggle_installed_filter(&mut self, cx: &mut Context<super::settings_ui::SettingsPage>) {
        self.only_installed = !self.only_installed;
        self.refresh_matches();
        cx.notify();
    }

    fn set_status(&mut self, text: impl Into<SharedString>, kind: StatusKind) {
        self.status = Some(StatusMessage {
            text: text.into(),
            kind,
        });
    }

    /// Reports a store failure, naming the stage that failed.
    fn set_store_error(&mut self, action: &str, error: StoreError) {
        self.set_status(format!("Could not {action}: {error}"), StatusKind::Error);
    }

    /// Fetches the catalog for the current tab open.
    pub fn begin_load(
        &mut self,
        http_client: Arc<dyn HttpClient>,
        page: WeakEntity<super::settings_ui::SettingsPage>,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        if self.busy.is_busy() {
            return;
        }
        self.busy = BusyState::Fetching;
        // The installed set is read from disk immediately so rows can render
        // their installed state before the network answers.
        self.refresh_installed();
        self.set_status("Loading theme extensions…", StatusKind::Info);
        cx.notify();

        // The catalog search box filters locally, so the full theme list is
        // fetched once instead of re-querying the API on every keystroke.
        extension_store::spawn_fetch_catalog(
            page,
            http_client,
            None,
            move |page, result, cx| {
                page.themes_tab.finish_load(result);
                cx.notify();
            },
            cx,
        )
        .detach();
    }

    /// Applies a finished catalog fetch.
    pub fn finish_load(&mut self, result: Result<Vec<ExtensionMetadata>, StoreError>) {
        self.busy = BusyState::Idle;
        self.busy_extension = None;
        self.loaded_once = true;
        match result {
            Ok(catalog) => {
                self.installed =
                    extension_store::load_installed_extensions_in(&self.extensions_dir);
                let count = catalog.len();
                self.set_catalog(catalog);
                self.set_status(
                    format!("Loaded {count} theme extensions."),
                    StatusKind::Success,
                );
            }
            Err(error) => {
                // An empty catalog would hide the extensions already on disk,
                // so the cached rows are kept and only the status changes.
                self.set_store_error("load theme extensions", error);
            }
        }
    }

    /// Retries a failed catalog fetch.
    pub fn reload(
        &mut self,
        http_client: Arc<dyn HttpClient>,
        page: WeakEntity<super::settings_ui::SettingsPage>,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        if self.busy.is_busy() {
            return;
        }
        self.begin_load(http_client, page, cx);
    }

    /// Installs or updates an extension, keeping the row's installed state
    /// intact until the operation actually succeeds.
    pub fn install(
        &mut self,
        extension_id: String,
        version: Option<String>,
        http_client: Arc<dyn HttpClient>,
        page: WeakEntity<super::settings_ui::SettingsPage>,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        if self.busy.is_busy() {
            return;
        }
        self.busy = BusyState::Installing;
        self.busy_extension = Some(extension_id.clone());
        self.set_status(format!("Installing {extension_id}…"), StatusKind::Info);
        cx.notify();

        extension_store::spawn_install(
            page,
            http_client,
            extension_id,
            version,
            move |page, extension_id, result, cx| {
                page.themes_tab
                    .finish_mutation("install", extension_id, result, cx);
            },
            cx,
        )
        .detach();
    }

    /// Removes an installed extension.
    pub fn uninstall(
        &mut self,
        extension_id: String,
        page: WeakEntity<super::settings_ui::SettingsPage>,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        if self.busy.is_busy() {
            return;
        }
        self.busy = BusyState::Uninstalling;
        self.busy_extension = Some(extension_id.clone());
        self.set_status(format!("Removing {extension_id}…"), StatusKind::Info);
        cx.notify();

        extension_store::spawn_uninstall(
            page,
            extension_id,
            move |page, extension_id, result, cx| {
                page.themes_tab.finish_mutation(
                    "remove",
                    extension_id,
                    result.map(|()| String::new()),
                    cx,
                );
            },
            cx,
        )
        .detach();
    }

    /// Applies a finished install or uninstall and re-resolves active themes.
    fn finish_mutation(
        &mut self,
        action: &str,
        extension_id: &str,
        result: Result<String, StoreError>,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) {
        self.busy = BusyState::Idle;
        self.busy_extension = None;
        match result {
            Ok(version) => {
                self.installed =
                    extension_store::load_installed_extensions_in(&self.extensions_dir);
                self.rebuild_rows();
                // The registry must learn about the new themes before the
                // Terminal tab's picker can offer them.
                crate::load_extension_themes(cx);
                let message = if action == "install" {
                    if version.is_empty() {
                        format!("Installed {extension_id}.")
                    } else {
                        format!("Installed {extension_id} {version}.")
                    }
                } else {
                    format!("Removed {extension_id}.")
                };
                self.set_status(message, StatusKind::Success);
            }
            Err(error) => {
                self.set_store_error(&format!("{action} {extension_id}"), error);
            }
        }
        cx.notify();
    }

    /// Renders the tab.
    pub fn render(
        &mut self,
        search_input: AnyElement,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) -> AnyElement {
        let toolbar = self.render_toolbar(search_input, cx);

        if self.matches.is_empty() {
            let message = if self.busy == BusyState::Fetching {
                "Loading theme extensions…"
            } else if self.rows.is_empty() {
                "No theme extensions are available."
            } else {
                "No theme extensions match this search."
            };
            return v_flex()
                .id("settings-themes-content")
                .w_full()
                .child(toolbar)
                .child(
                    v_flex().px_8().py_4().child(
                        Label::new(message)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
                .into_any_element();
        }

        // The catalog holds hundreds of extensions and this render runs on every
        // keystroke, so only the visible rows are built.
        let matches = self.matches.clone();
        let list = uniform_list(
            "themes-list",
            matches.len(),
            cx.processor(
                move |page: &mut super::settings_ui::SettingsPage,
                      range: std::ops::Range<usize>,
                      _window,
                      cx| {
                    let http_client = cx.http_client();
                    range
                        .filter_map(|position| matches.get(position).copied())
                        .map(|index| page.themes_tab.render_row(index, http_client.clone(), cx))
                        .collect::<Vec<_>>()
                },
            ),
        )
        .flex_1();

        v_flex()
            .id("settings-themes-content")
            .w_full()
            .h_full()
            .min_h_0()
            .child(toolbar)
            .child(list)
            .into_any_element()
    }

    fn render_toolbar(
        &self,
        search_input: AnyElement,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) -> AnyElement {
        let installed_count = self.installed.len();
        let status = self.status.as_ref();
        let busy = self.busy.is_busy();

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
                        Label::new(format!("{} / {}", self.matches.len(), self.rows.len()))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        div()
                            .debug_selector(|| "themes-installed-count".to_string())
                            .child(
                                Label::new(format!("{installed_count} installed"))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "themes-installed-filter".to_string())
                            .child(
                                IconButton::new("themes-installed-filter", IconName::Check)
                                    .toggle_state(self.only_installed)
                                    .tooltip(Tooltip::text("Show installed extensions only"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.themes_tab.toggle_installed_filter(cx);
                                    })),
                            ),
                    )
                    .child(
                        div().debug_selector(|| "themes-reload".to_string()).child(
                            IconButton::new("themes-reload", IconName::RotateCw)
                                .disabled(busy)
                                .tooltip(Tooltip::text("Reload the theme extension list"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let http_client = cx.http_client();
                                    let page = cx.weak_entity();
                                    this.themes_tab.reload(http_client, page, cx);
                                })),
                        ),
                    ),
            )
            .when_some(status, |this, status| {
                this.child(
                    h_flex().px_8().py_1().child(
                        Label::new(status.text.clone())
                            .size(LabelSize::Small)
                            .color(match status.kind {
                                StatusKind::Info => Color::Muted,
                                StatusKind::Success => Color::Success,
                                StatusKind::Error => Color::Error,
                            }),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_row(
        &self,
        index: usize,
        http_client: Arc<dyn HttpClient>,
        cx: &mut Context<super::settings_ui::SettingsPage>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(index) else {
            return gpui::Empty.into_any_element();
        };
        let colors = cx.theme().colors();
        let extension_id = row.id().to_string();
        let row_busy = self.busy_extension.as_deref() == Some(extension_id.as_str());
        let any_busy = self.busy.is_busy();
        let is_installed = row.is_installed();
        let update_available = row.update_available();
        let installed_version = row.installed_version().map(str::to_string);
        let latest_version = row.latest_version().map(str::to_string);
        let theme_count = row.theme_count();

        let version_label = match (&installed_version, &latest_version) {
            (Some(installed), Some(latest)) if update_available => {
                format!("{installed} → {latest}")
            }
            (Some(installed), _) => installed.clone(),
            (None, Some(latest)) => latest.clone(),
            (None, None) => String::new(),
        };

        let mut badges = h_flex().gap_1().items_center().flex_none();
        if update_available {
            badges = badges.child(
                Icon::new(IconName::ArrowCircle)
                    .size(IconSize::XSmall)
                    .color(Color::Warning),
            );
        }
        if is_installed && theme_count > 0 {
            badges = badges.child(
                Label::new(format!("{theme_count} themes"))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
        }
        if !version_label.is_empty() {
            badges = badges.child(
                Label::new(version_label)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
        }
        // The download count is the only popularity signal the API offers, and
        // it is absent for extensions the catalog does not list.
        if let Some(download_count) = row.download_count() {
            badges = badges.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .debug_selector({
                        let id = extension_id.clone();
                        move || format!("themes-downloads-{id}")
                    })
                    .child(
                        Icon::new(IconName::Download)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(SharedString::from(format_download_count(download_count)))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            );
        }

        let mut controls = h_flex().gap_1().items_center().flex_none();

        if row_busy {
            // Progress replaces the controls so the row cannot be acted on
            // twice while an operation is running.
            controls = controls.child(
                Label::new(match self.busy {
                    BusyState::Installing => "Installing…",
                    BusyState::Uninstalling => "Removing…",
                    BusyState::Fetching | BusyState::Idle => "Working…",
                })
                .size(LabelSize::Small)
                .color(Color::Muted),
            );
        } else {
            if update_available {
                let page = cx.weak_entity();
                let id = extension_id.clone();
                let version = latest_version.clone();
                controls = controls.child(
                    div()
                        .debug_selector({
                            let id = extension_id.clone();
                            move || format!("themes-update-{id}")
                        })
                        .child(
                            Button::new(
                                SharedString::from(format!("themes-update-{extension_id}")),
                                "Update",
                            )
                            .style(ButtonStyle::Filled)
                            .label_size(LabelSize::Small)
                            .disabled(any_busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.themes_tab.install(
                                        id.clone(),
                                        version.clone(),
                                        http_client.clone(),
                                        page.clone(),
                                        cx,
                                    );
                                },
                            )),
                        ),
                );
            } else if !is_installed {
                let page = cx.weak_entity();
                let id = extension_id.clone();
                controls = controls.child(
                    div()
                        .debug_selector({
                            let id = extension_id.clone();
                            move || format!("themes-install-{id}")
                        })
                        .child(
                            Button::new(
                                SharedString::from(format!("themes-install-{extension_id}")),
                                "Install",
                            )
                            .style(ButtonStyle::Outlined)
                            .label_size(LabelSize::Small)
                            .disabled(any_busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.themes_tab.install(
                                        id.clone(),
                                        None,
                                        http_client.clone(),
                                        page.clone(),
                                        cx,
                                    );
                                },
                            )),
                        ),
                );
            }

            if is_installed {
                let page = cx.weak_entity();
                let id = extension_id.clone();
                controls = controls.child(
                    div()
                        .debug_selector({
                            let id = extension_id.clone();
                            move || format!("themes-uninstall-{id}")
                        })
                        .child(
                            IconButton::new(
                                SharedString::from(format!("themes-uninstall-{extension_id}")),
                                IconName::Trash,
                            )
                            .disabled(any_busy)
                            .tooltip(Tooltip::text("Uninstall this theme extension"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.themes_tab.uninstall(id.clone(), page.clone(), cx);
                                },
                            )),
                        ),
                );
            }
        }

        h_flex()
            .id(("theme-extension", index))
            .debug_selector(move || format!("themes-row-{index}"))
            .w_full()
            // Rows are virtualized, which requires a uniform height; the labels
            // truncate instead of wrapping so a long description cannot change
            // it.
            .h(THEME_ROW_HEIGHT)
            .flex_none()
            .px_8()
            .gap_3()
            .items_center()
            .hover(|style| style.bg(colors.ghost_element_hover))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(SharedString::from(row.name().to_string())).truncate())
                    .child(
                        Label::new(SharedString::from(row.description().to_string()))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    ),
            )
            .child(badges)
            .child(controls)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_store::StoreErrorKind;

    fn metadata(id: &str, name: &str, version: &str, description: &str) -> ExtensionMetadata {
        metadata_with_downloads(id, name, version, description, 0)
    }

    fn metadata_with_downloads(
        id: &str,
        name: &str,
        version: &str,
        description: &str,
        download_count: u64,
    ) -> ExtensionMetadata {
        ExtensionMetadata {
            id: id.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            description: description.to_string(),
            authors: Vec::new(),
            repository: String::new(),
            provides: vec!["themes".to_string()],
            download_count,
        }
    }

    fn installed(id: &str, version: &str) -> InstalledExtension {
        InstalledExtension {
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            description: String::new(),
            repository: String::new(),
            theme_families: vec![extension_store::InstalledThemeFamily {
                family_name: id.to_string(),
                theme_names: vec!["Dark".to_string(), "Light".to_string()],
            }],
        }
    }

    /// A tab backed by an empty temporary directory.
    ///
    /// The tab reads installed extensions from disk, so tests must never point
    /// it at the developer's real extensions directory.
    fn isolated_tab() -> ThemesTab {
        let directory = tempfile::tempdir().expect("a temporary directory should be creatable");
        ThemesTab::new(directory.path().to_path_buf())
    }

    fn tab_with(catalog: Vec<ExtensionMetadata>, installed: Vec<InstalledExtension>) -> ThemesTab {
        let mut tab = isolated_tab();
        tab.set_installed_for_test(installed);
        tab.set_catalog(catalog);
        tab
    }

    fn catalog_fixture() -> Vec<ExtensionMetadata> {
        vec![
            metadata("catppuccin", "Catppuccin", "1.0.0", "Soothing pastel theme"),
            metadata("gruvbox", "Gruvbox", "0.9.0", "Retro groove colors"),
        ]
    }

    fn matching_ids(tab: &mut ThemesTab, query: &str) -> Vec<String> {
        tab.search_query = query.to_string();
        tab.refresh_matches();
        tab.matches()
            .iter()
            .filter_map(|index| tab.rows().get(*index))
            .map(|row| row.id().to_string())
            .collect()
    }

    #[test]
    fn the_search_filter_matches_id_name_and_description() {
        let mut tab = tab_with(catalog_fixture(), Vec::new());
        assert_eq!(tab.matches().len(), 2);

        assert_eq!(matching_ids(&mut tab, "cat"), vec!["catppuccin"]);
        // The description is searched too.
        assert_eq!(matching_ids(&mut tab, "retro"), vec!["gruvbox"]);
        // Case is ignored, and the id matches as well as the name.
        assert_eq!(matching_ids(&mut tab, "GRUV"), vec!["gruvbox"]);
        // An empty query restores every row.
        assert_eq!(matching_ids(&mut tab, "   ").len(), 2);
    }

    #[test]
    fn a_search_with_no_matches_yields_an_empty_list() {
        let mut tab = tab_with(catalog_fixture(), Vec::new());
        assert!(matching_ids(&mut tab, "nothing matches this").is_empty());
    }

    #[test]
    fn an_installed_extension_shows_as_installed_with_an_update() {
        let tab = tab_with(
            vec![metadata("catppuccin", "Catppuccin", "2.0.0", "Pastel")],
            vec![installed("catppuccin", "1.0.0")],
        );
        let row = &tab.rows()[0];
        assert!(row.is_installed());
        assert!(row.update_available());
        assert_eq!(row.installed_version(), Some("1.0.0"));
        assert_eq!(row.latest_version(), Some("2.0.0"));
        assert_eq!(row.theme_count(), 2);
    }

    #[test]
    fn a_catalog_entry_that_is_not_installed_offers_install() {
        let tab = tab_with(
            vec![metadata("catppuccin", "Catppuccin", "1.0.0", "Pastel")],
            Vec::new(),
        );
        let row = &tab.rows()[0];
        assert!(!row.is_installed());
        assert!(!row.update_available());
        assert_eq!(row.installed_version(), None);
    }

    #[test]
    fn a_locally_installed_extension_missing_from_the_catalog_is_still_listed() {
        let tab = tab_with(Vec::new(), vec![installed("hand-installed", "1.0.0")]);
        assert_eq!(tab.rows().len(), 1);
        let row = &tab.rows()[0];
        assert_eq!(row.id(), "hand-installed");
        assert!(row.is_installed());
        assert!(!row.update_available());
    }

    #[test]
    fn a_failed_load_reports_the_error_without_claiming_success() {
        let mut tab = tab_with(catalog_fixture(), Vec::new());
        tab.finish_load(Err(StoreError {
            kind: StoreErrorKind::Network,
            message: "offline".to_string(),
        }));

        let (text, is_success) = tab.status_text().expect("a status should be shown");
        assert!(text.contains("network error"), "got: {text}");
        assert!(!is_success);
        assert!(!tab.is_busy());
        assert!(tab.has_loaded());
        // A failed reload must not discard a catalog that was already loaded:
        // the user keeps browsing what is known rather than losing the list.
        assert_eq!(tab.rows().len(), 2, "the cached catalog should survive");
    }

    #[test]
    fn a_failed_first_load_leaves_an_empty_but_usable_list() {
        let mut tab = isolated_tab();
        tab.finish_load(Err(StoreError {
            kind: StoreErrorKind::Network,
            message: "offline".to_string(),
        }));

        assert!(tab.rows().is_empty());
        assert!(
            !tab.is_busy(),
            "a failure must not leave the tab stuck busy"
        );
        let (_, is_success) = tab.status_text().expect("a status should be shown");
        assert!(!is_success);
    }

    #[test]
    fn a_successful_load_reports_the_catalog_size() {
        let mut tab = isolated_tab();
        tab.finish_load(Ok(catalog_fixture()));

        let (text, is_success) = tab.status_text().expect("a status should be shown");
        assert!(text.contains('2'), "got: {text}");
        assert!(is_success);
        assert!(tab.has_loaded());
        assert!(!tab.is_busy());
    }

    #[test]
    fn a_reload_after_a_failed_load_clears_the_error_state() {
        let mut tab = isolated_tab();
        tab.finish_load(Err(StoreError {
            kind: StoreErrorKind::Api,
            message: "HTTP 500".to_string(),
        }));
        assert!(!tab.is_busy());

        tab.finish_load(Ok(catalog_fixture()));
        assert_eq!(tab.matches().len(), 2);
        let (_, is_success) = tab.status_text().unwrap();
        assert!(is_success);
    }

    #[test]
    fn download_counts_are_formatted_compactly() {
        assert_eq!(format_download_count(0), "0");
        assert_eq!(format_download_count(999), "999");
        assert_eq!(format_download_count(1_000), "1K");
        assert_eq!(format_download_count(1_500), "1.5K");
        assert_eq!(format_download_count(69_813), "69.8K");
        assert_eq!(format_download_count(1_040_022), "1M");
        assert_eq!(format_download_count(1_250_000), "1.3M");
    }

    #[test]
    fn a_catalog_row_exposes_its_download_count() {
        let mut tab = isolated_tab();
        tab.set_catalog(vec![metadata_with_downloads(
            "catppuccin",
            "Catppuccin",
            "1.0.0",
            "Pastel",
            1040022,
        )]);
        assert_eq!(tab.rows()[0].download_count(), Some(1_040_022));
    }

    #[test]
    fn an_extension_only_present_locally_has_no_download_count() {
        // The API is the only source of a download count, so an extension the
        // catalog does not list cannot report one.
        let tab = tab_with(Vec::new(), vec![installed("hand-installed", "1.0.0")]);
        assert_eq!(tab.rows()[0].download_count(), None);
    }

    #[test]
    fn the_installed_filter_keeps_only_installed_rows() {
        let mut tab = tab_with(catalog_fixture(), vec![installed("gruvbox", "0.9.0")]);
        assert_eq!(tab.rows().len(), 2);
        assert_eq!(matching_ids(&mut tab, "").len(), 2);

        tab.only_installed = true;
        assert_eq!(matching_ids(&mut tab, ""), vec!["gruvbox"]);
    }

    #[test]
    fn the_installed_filter_includes_extensions_absent_from_the_catalog() {
        // Those rows exist precisely so they can be uninstalled, so the filter
        // must not hide them.
        let mut tab = tab_with(
            catalog_fixture(),
            vec![installed("hand-installed", "1.0.0")],
        );
        tab.only_installed = true;
        assert_eq!(matching_ids(&mut tab, ""), vec!["hand-installed"]);
    }

    #[test]
    fn the_installed_filter_combines_with_the_search_query() {
        // Only catppuccin is installed; gruvbox is not.
        let mut tab = tab_with(catalog_fixture(), vec![installed("catppuccin", "1.0.0")]);
        tab.only_installed = true;

        // An installed extension still matches on its name.
        assert_eq!(matching_ids(&mut tab, "cat"), vec!["catppuccin"]);
        // "retro" only matches gruvbox's description, and gruvbox is not
        // installed, so the installed filter must exclude it.
        assert!(matching_ids(&mut tab, "retro").is_empty());

        // The same query does find it once the filter is off.
        tab.only_installed = false;
        assert_eq!(matching_ids(&mut tab, "retro"), vec!["gruvbox"]);
    }

    #[test]
    fn the_installed_filter_can_be_toggled_off_again() {
        let mut tab = tab_with(catalog_fixture(), vec![installed("gruvbox", "0.9.0")]);
        tab.only_installed = true;
        tab.refresh_matches();
        assert_eq!(tab.matches().len(), 1);

        tab.only_installed = false;
        tab.refresh_matches();
        assert_eq!(tab.matches().len(), 2);
    }

    #[test]
    fn a_row_is_shown_for_every_visible_position() {
        // `uniform_list` maps a visible range to positions in `matches`, so the
        // two must stay in step; an off-by-one would silently drop a row.
        let tab = tab_with(catalog_fixture(), vec![installed("gruvbox", "0.9.0")]);
        assert_eq!(tab.matches().len(), 2);
        for (position, index) in tab.matches().iter().enumerate() {
            assert!(
                tab.rows().get(*index).is_some(),
                "position {position} maps to a missing row {index}"
            );
        }
    }

    /// A large catalog must stay consistent, since it is the case the list is
    /// virtualized for.
    #[test]
    fn filtering_a_large_catalog_stays_consistent() {
        let catalog: Vec<ExtensionMetadata> = (0..628)
            .map(|index| {
                metadata_with_downloads(
                    &format!("theme-{index}"),
                    &format!("Theme {index}"),
                    "1.0.0",
                    "A theme extension",
                    index as u64,
                )
            })
            .collect();
        let mut tab = tab_with(catalog, vec![installed("theme-7", "1.0.0")]);
        assert_eq!(tab.rows().len(), 628);
        assert_eq!(tab.matches().len(), 628);

        // The last match must still resolve to a real row.
        let last_index = *tab.matches().last().expect("the catalog is not empty");
        assert!(tab.rows().get(last_index).is_some());

        // Substring matching means "theme-7" also matches theme-70..theme-79;
        // what matters is that filtering narrows the list.
        let narrowed = matching_ids(&mut tab, "theme-70");
        assert!(
            narrowed.len() < 20 && narrowed.contains(&"theme-70".to_string()),
            "got: {narrowed:?}"
        );

        tab.only_installed = true;
        assert_eq!(matching_ids(&mut tab, ""), vec!["theme-7"]);
    }

    #[test]
    fn busy_states_are_reported_consistently() {
        assert!(BusyState::Fetching.is_busy());
        assert!(BusyState::Installing.is_busy());
        assert!(BusyState::Uninstalling.is_busy());
        assert!(!BusyState::Idle.is_busy());
    }
}
