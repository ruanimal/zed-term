//! Transfer UI: per-tab overlay state, dialogs and the pane-top bar
//! (§6 of docs/TRANSFER_EXTENSION.md).

use std::sync::Arc;

use gpui::{
    App, Context, Entity, IntoElement, ParentElement, RenderOnce, Styled, Window, div, relative,
};
use settings::Settings as _;
use terminal_core::terminal_settings::{TerminalSettings, TransferSettings};
use terminal_core::transfer_mux::{TransferPolicy, TransferRegistry, TransferSetup};
use terminal_core::{Direction, Terminal, TransferUiEvent};
use theme::ActiveTheme as _;
use transfer_core::TransferProvider;
use ui::prelude::*;
use ui::{IconButtonShape, Tooltip};

use crate::providers_trzsz::TrzszProvider;
use crate::providers_zmodem::ZmodemProvider;
use crate::transfer_io::AppTransferHost;

// ─── Setup (settings → registry + host + policy, §8) ───────────────────────

/// The in-process providers this build ships, in default adjudication order.
/// Settings never name a protocol here: each provider declares its own
/// `default_config` and validates its own section, so adding one is a line in
/// this list (§8.3).
fn built_in_providers() -> Vec<Box<dyn TransferProvider>> {
    vec![Box::new(TrzszProvider), Box::new(ZmodemProvider)]
}

/// Build the transfer runtime configuration for a new terminal. Returns
/// `None` when every provider is disabled, which leaves the terminal with no
/// tap at all (zero overhead, zero false positives).
pub fn transfer_setup(settings: &TransferSettings) -> Option<TransferSetup> {
    // `configure` runs before the provider is frozen into the shared
    // registry (§4). A provider whose configuration fails validation is
    // disabled with a warning; other providers are unaffected.
    let mut providers: Vec<Arc<dyn TransferProvider>> = Vec::new();
    for mut provider in built_in_providers() {
        let id = provider.id();
        let config = settings
            .providers
            .get(&*id)
            .cloned()
            .unwrap_or_else(|| provider.manifest().default_config);
        if config.get("enabled") == Some(&serde_json::json!(false)) {
            continue;
        }
        match provider.configure(&config) {
            Ok(()) => providers.push(Arc::from(provider)),
            Err(error) => log::warn!("disabling transfer provider {id}: {error}"),
        }
    }
    if providers.is_empty() {
        log::info!("file transfer: all providers disabled");
        return None;
    }
    log::info!(
        "file transfer: enabled providers: {:?} (priority {:?})",
        providers
            .iter()
            .map(|provider| provider.id().to_string())
            .collect::<Vec<_>>(),
        settings.priority,
    );

    // User-configured priority order; unknown ids are ignored (§8.1).
    let priority: Vec<Arc<str>> = settings
        .priority
        .iter()
        .filter(|id| {
            providers
                .iter()
                .any(|provider| provider.id().as_ref() == id.as_str())
        })
        .map(|id| Arc::from(id.as_str()))
        .collect();

    let registry = TransferRegistry::new(providers, priority);

    for id in settings.providers.keys() {
        if registry.provider(id).is_none() {
            log::warn!("ignoring configuration for unknown transfer provider {id}");
        }
    }

    let host = AppTransferHost::new(
        settings.download_dir.clone(),
        settings.max_file_size_mb * 1024 * 1024,
    );
    let policy = TransferPolicy {
        picker_timeout: std::time::Duration::from_secs(settings.picker_timeout_secs),
        idle_timeout: std::time::Duration::from_secs(settings.idle_timeout_secs),
        max_file_size: settings.max_file_size_mb * 1024 * 1024,
        max_session_bytes: settings.max_session_mb * 1024 * 1024,
    };

    Some(TransferSetup {
        registry: Arc::new(registry),
        host: Arc::new(host),
        policy,
    })
}

/// The prompt used to pick a download destination: directories only. It must
/// never offer files, because a save-file prompt turns the picked path into
/// the file name and lets a placeholder like "download" become the file
/// (§7.2).
fn download_dir_prompt_options() -> gpui::PathPromptOptions {
    gpui::PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: None,
    }
}

// ─── Per-tab state ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveTransfer {
    provider: Arc<str>,
    direction: Option<Direction>,
    file_index: usize,
    file_count: usize,
    bytes_done: u64,
    bytes_total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FinishedTransfer {
    message: String,
    ok: bool,
}

/// Per-tab transfer state driving the overlay bar and the dialogs.
#[derive(Default)]
pub struct TransferUiState {
    active: Option<ActiveTransfer>,
    finished: Option<FinishedTransfer>,
    hint: Option<String>,
}

impl TransferUiState {
    /// Whether any overlay content exists (bar should render).
    pub fn is_empty(&self) -> bool {
        self.active.is_none() && self.finished.is_none() && self.hint.is_none()
    }

    /// Clear the result or hint the bar is showing so it disappears. An
    /// active session is not affected: it is owned by the driver, and the
    /// button cancels it instead of dismissing the overlay.
    pub(crate) fn dismiss(&mut self) {
        self.finished = None;
        self.hint = None;
    }

    pub fn handle(
        &mut self,
        event: TransferUiEvent,
        terminal: &Entity<Terminal>,
        cx: &mut Context<crate::terminal::TerminalTab>,
    ) {
        match event {
            TransferUiEvent::Detected {
                provider_id: provider,
                direction,
                remote_names,
            } => {
                log::info!(
                    "transfer detected: provider={} direction={:?} names={remote_names:?}",
                    provider,
                    direction
                );
                self.active = Some(ActiveTransfer {
                    provider,
                    direction,
                    file_index: 0,
                    file_count: 0,
                    bytes_done: 0,
                    bytes_total: None,
                });
                self.finished = None;
                self.hint = None;
            }
            TransferUiEvent::AwaitingUploadPaths { .. } => {
                self.prompt_upload_paths(cx);
            }
            TransferUiEvent::AwaitingDownloadDir { .. } => {
                self.prompt_download_dir(terminal, cx);
            }
            TransferUiEvent::Progress {
                file_index,
                file_count,
                bytes_done,
                bytes_total,
                ..
            } => {
                if let Some(active) = &mut self.active {
                    active.file_index = file_index;
                    active.file_count = file_count;
                    active.bytes_done = bytes_done;
                    active.bytes_total = bytes_total;
                }
            }
            TransferUiEvent::Completed { paths } => {
                let summary = match paths.len() {
                    0 => "Transfer completed".to_string(),
                    1 => format!("Saved {}", paths[0].display()),
                    _ => format!("Transferred {} files", paths.len()),
                };
                self.finished = Some(FinishedTransfer {
                    message: summary,
                    ok: true,
                });
                self.active = None;
            }
            TransferUiEvent::Failed { reason } => {
                self.finished = Some(FinishedTransfer {
                    message: format!("Transfer failed: {reason}"),
                    ok: false,
                });
                self.active = None;
            }
            TransferUiEvent::Cancelled => self.on_cancelled(),
            TransferUiEvent::BusyRejected => {
                self.hint = Some("Already transferring — ignoring new transfer request".into());
            }
            TransferUiEvent::InputRefused => {
                self.hint = Some("Transfer in progress — press Esc to cancel".into());
            }
        }
    }

    /// A session-side cancellation. A dialog that failed to open has already
    /// explained the real reason, so its error message survives: cancelling
    /// the session is just the bookkeeping that follows (§6).
    fn on_cancelled(&mut self) {
        let explained_by_error = self.finished.as_ref().is_some_and(|finished| !finished.ok);
        if !explained_by_error {
            self.finished = Some(FinishedTransfer {
                message: "Transfer cancelled".to_string(),
                ok: true,
            });
        }
        self.active = None;
    }

    /// Show a failure in the overlay (used when a dialog cannot be opened).
    pub(crate) fn fail(&mut self, message: String) {
        self.finished = Some(FinishedTransfer { message, ok: false });
        self.active = None;
    }

    fn prompt_upload_paths(&self, cx: &mut Context<crate::terminal::TerminalTab>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let answer = match receiver.await {
                Ok(Ok(paths)) => paths,
                Ok(Err(error)) => {
                    // The picker itself failed (e.g. xdg-desktop-portal is
                    // unavailable on Linux): the session cannot continue, so
                    // end it and say why instead of silently "cancelling".
                    let _ = this.update(cx, |this, cx| {
                        this.terminal.update(cx, |terminal, _| {
                            terminal.transfer_answer_upload_paths(None);
                        });
                        this.transfer_ui
                            .fail(format!("无法打开文件选择器: {error}"));
                    });
                    return;
                }
                Err(_) => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.terminal.update(cx, |terminal, _| {
                    terminal.transfer_answer_upload_paths(answer);
                });
            });
        })
        .detach();
    }

    /// Ask for the directory a download lands in.
    ///
    /// This is a directory picker, not "save as": the file is named by the
    /// remote's sanitized basename (§7.2) and only becomes visible once the
    /// whole transfer committed. A save-file prompt here is what produced
    /// files literally called "download" — and made a directory the user
    /// picked look like it had been replaced by a file.
    fn prompt_download_dir(
        &self,
        terminal: &Entity<Terminal>,
        cx: &mut Context<crate::terminal::TerminalTab>,
    ) {
        // A configured download directory with confirmation turned off is
        // used as-is; otherwise the location is always confirmed (§7.1).
        if let Some(transfer) = TerminalSettings::get_global(cx).transfer.as_ref()
            && !transfer.confirm_before_download
            && let Some(directory) = transfer.download_dir.clone()
        {
            terminal.update(cx, |terminal, _| {
                terminal.transfer_answer_download_dir(Some(directory));
            });
            return;
        }

        let receiver = cx.prompt_for_paths(download_dir_prompt_options());
        cx.spawn(async move |this, cx| {
            let answer = match receiver.await {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.terminal.update(cx, |terminal, _| {
                            terminal.transfer_answer_download_dir(None);
                        });
                        this.transfer_ui
                            .fail(format!("无法打开目录选择器: {error}"));
                    });
                    return;
                }
                Err(_) => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.terminal.update(cx, |terminal, _| {
                    terminal.transfer_answer_download_dir(answer);
                });
            });
        })
        .detach();
    }
}

// ─── Overlay bar ───────────────────────────────────────────────────────────

fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// The pane-top transfer bar: progress + cancel while a transfer runs, the
/// final result afterwards. Mounted above the terminal content like the
/// search bar.
#[derive(IntoElement)]
pub struct TransferBar {
    tab: Entity<crate::terminal::TerminalTab>,
}

impl TransferBar {
    pub fn new(tab: Entity<crate::terminal::TerminalTab>) -> Self {
        Self { tab }
    }
}

impl RenderOnce for TransferBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.tab.read(cx).transfer_ui_state();
        let colors = cx.theme().colors();

        let (text, ok, fraction, hint) = if let Some(active) = &state.active {
            let direction = match active.direction {
                Some(Direction::Upload) => "Uploading",
                Some(Direction::Download) => "Downloading",
                None => "Transferring",
            };
            let file = if active.file_count > 1 {
                format!(" (file {}/{})", active.file_index + 1, active.file_count)
            } else {
                String::new()
            };
            let total = active
                .bytes_total
                .map(|total| format!(" / {}", human_bytes(total)))
                .unwrap_or_default();
            (
                format!(
                    "{}{file}: {}{total}",
                    direction,
                    human_bytes(active.bytes_done)
                ),
                true,
                active
                    .bytes_total
                    .filter(|total| *total > 0)
                    .map(|total| (active.bytes_done as f32 / total as f32).clamp(0.0, 1.0)),
                // A hint raised while a transfer runs (refused input, a
                // rejected second trigger) used to be shadowed by the
                // progress text.
                state.hint.clone(),
            )
        } else if let Some(finished) = &state.finished {
            (
                finished.message.clone(),
                finished.ok,
                None,
                state.hint.clone(),
            )
        } else if let Some(hint) = &state.hint {
            (hint.clone(), true, None, None)
        } else {
            return div();
        };

        let terminal = self.tab.read(cx).terminal.clone();
        // The same button cancels a live transfer and dismisses the result
        // afterwards; a finished bar must be closable or it stays forever.
        let cancel_active = state.active.is_some();
        let tab = self.tab.clone();
        let progress = fraction.map(|fraction| {
            div()
                .h_1()
                .w_full()
                .rounded_sm()
                .bg(colors.element_background)
                .child(
                    div()
                        .h_1()
                        .rounded_sm()
                        .bg(colors.border_focused)
                        .w(relative(fraction)),
                )
        });

        div()
            .flex()
            .flex_col()
            .px_2()
            .py_1()
            .gap_1()
            .bg(colors.element_background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new(text).size(LabelSize::Small).color(if ok {
                        Color::Default
                    } else {
                        Color::Error
                    }))
                    .child(
                        IconButton::new("transfer-close", IconName::Close)
                            .shape(IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text(if cancel_active {
                                "Cancel transfer"
                            } else {
                                "Dismiss"
                            }))
                            .on_click(move |_, window, cx| {
                                if cancel_active {
                                    terminal.update(cx, |terminal, _| terminal.transfer_cancel());
                                } else {
                                    tab.update(cx, |tab, cx| tab.dismiss_transfer(window, cx));
                                }
                            }),
                    ),
            )
            .when_some(hint, |bar, hint| {
                bar.child(Label::new(hint).size(LabelSize::Small).color(Color::Muted))
            })
            .when_some(progress, |bar, progress| bar.child(progress))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: with no `[terminal.transfer]` settings section the tap
    /// used to be left unmounted entirely, so trigger detection could never
    /// fire. Default settings must arm every built-in provider (§8.3).
    #[test]
    fn default_settings_arm_every_built_in_provider() {
        let setup = transfer_setup(&TransferSettings::default())
            .expect("default settings must arm file transfer");
        assert!(setup.registry.provider("trzsz").is_some());
        assert!(setup.registry.provider("zmodem").is_some());
        assert!(!setup.registry.is_empty());
    }

    #[test]
    fn disabled_provider_yields_no_setup() {
        let mut settings = TransferSettings::default();
        for id in ["trzsz", "zmodem"] {
            settings
                .providers
                .insert(id.to_owned(), serde_json::json!({ "enabled": false }));
        }
        assert!(transfer_setup(&settings).is_none());
    }

    /// A provider whose section fails validation is disabled on its own:
    /// the other providers, and the terminal itself, are unaffected (§8.3).
    #[test]
    fn invalid_provider_config_only_disables_that_provider() {
        let mut settings = TransferSettings::default();
        settings
            .providers
            .insert("zmodem".to_owned(), serde_json::json!({ "enabled": "yes" }));
        let setup = transfer_setup(&settings).expect("trzsz must stay armed");
        assert!(setup.registry.provider("zmodem").is_none());
        assert!(setup.registry.provider("trzsz").is_some());
    }

    /// Regression: `TransferSettings` used to derive `Default`, so an
    /// unconfigured `[terminal.transfer]` section produced a zero picker
    /// timeout. The driver then expired the picker watchdog on its very next
    /// loop and failed every transfer with "timed out waiting for file
    /// selection" before the user could choose anything.
    #[test]
    fn default_settings_have_documented_watchdogs() {
        let setup = transfer_setup(&TransferSettings::default())
            .expect("default settings must arm file transfer");
        assert_eq!(
            setup.policy.picker_timeout,
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            setup.policy.idle_timeout,
            std::time::Duration::from_secs(30)
        );
        assert_eq!(setup.policy.max_file_size, 2048 * 1024 * 1024);
        assert_eq!(setup.policy.max_session_bytes, 4096 * 1024 * 1024);
    }

    fn active_transfer() -> ActiveTransfer {
        ActiveTransfer {
            provider: "trzsz".into(),
            direction: Some(Direction::Download),
            file_index: 0,
            file_count: 1,
            bytes_done: 0,
            bytes_total: Some(10),
        }
    }

    /// Regression: a finished bar had a disabled Close button whose only
    /// action was `transfer_cancel`, so "Saved <path>" stayed on screen
    /// forever after a download.
    #[test]
    fn dismiss_clears_the_finished_bar() {
        let mut state = TransferUiState {
            finished: Some(FinishedTransfer {
                message: "Saved /tmp/report.csv".into(),
                ok: true,
            }),
            ..TransferUiState::default()
        };
        assert!(!state.is_empty());

        state.dismiss();
        assert!(state.is_empty(), "the bar must disappear after dismiss");
    }

    #[test]
    fn dismiss_clears_a_hint_and_leaves_an_active_transfer_alone() {
        let mut state = TransferUiState {
            hint: Some("Already transferring".into()),
            active: Some(active_transfer()),
            ..TransferUiState::default()
        };

        state.dismiss();
        assert!(state.hint.is_none());
        assert!(
            state.active.is_some(),
            "a running session is cancelled, never dismissed"
        );
    }

    #[test]
    fn cancel_without_an_error_reports_cancelled() {
        let mut state = TransferUiState {
            active: Some(active_transfer()),
            ..TransferUiState::default()
        };

        state.on_cancelled();
        assert!(state.active.is_none());
        let finished = state.finished.expect("a cancel must be reported");
        assert!(finished.ok);
        assert_eq!(finished.message, "Transfer cancelled");
    }

    /// A picker that could not be opened reports the real reason; the
    /// session cancellation that follows must not replace it with the
    /// generic "Transfer cancelled".
    #[test]
    fn cancel_keeps_a_dialog_error_message() {
        let mut state = TransferUiState::default();
        state.fail("无法打开目录选择器: portal missing".into());

        state.on_cancelled();
        let finished = state.finished.expect("the error must survive");
        assert!(!finished.ok);
        assert!(finished.message.contains("无法打开目录选择器"));
    }

    #[test]
    fn download_picker_asks_for_a_directory_only() {
        let options = download_dir_prompt_options();
        assert!(!options.files);
        assert!(options.directories);
        assert!(!options.multiple);
    }
}
