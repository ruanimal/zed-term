use std::time::Duration;

use collections::HashMap;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, BorrowAppContext as _, EntityId, Global,
    InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _, div, px, relative,
};
use terminal_core::{ProgressState, TerminalProgress};
use theme::ActiveTheme as _;

#[derive(Default)]
struct ProgressEntries {
    entries: HashMap<EntityId, (u64, TerminalProgress)>,
    revision: u64,
}

impl ProgressEntries {
    fn update(&mut self, pane: EntityId, progress: Option<TerminalProgress>) {
        if let Some(progress) = progress {
            self.revision += 1;
            self.entries.insert(pane, (self.revision, progress));
        } else {
            self.entries.remove(&pane);
        }
    }

    fn current(&self) -> Option<TerminalProgress> {
        self.entries
            .values()
            .max_by_key(|(revision, _)| revision)
            .map(|(_, progress)| *progress)
    }
}

struct DesktopProgress {
    entries: ProgressEntries,
    displayed: Option<TerminalProgress>,
    native: native::NativeProgress,
    #[cfg(target_os = "macos")]
    animation: Option<gpui::Task<()>>,
}

impl Global for DesktopProgress {}

pub(crate) fn init(cx: &mut App) {
    let native = native::NativeProgress::new(cx);
    cx.set_global(DesktopProgress {
        entries: ProgressEntries::default(),
        displayed: None,
        native,
        #[cfg(target_os = "macos")]
        animation: None,
    });
}

pub(crate) fn update(pane: EntityId, progress: Option<TerminalProgress>, cx: &mut App) {
    if cx.try_global::<DesktopProgress>().is_none() {
        return;
    }
    cx.update_global::<DesktopProgress, _>(|desktop, cx| {
        desktop.entries.update(pane, progress);
        let current = desktop.entries.current();
        if desktop.displayed == current {
            return;
        }
        desktop.displayed = current;
        desktop.native.update(current);

        #[cfg(target_os = "macos")]
        {
            desktop.animation = None;
            if current.is_some_and(|progress| progress.state == ProgressState::Indeterminate) {
                desktop.animation = Some(cx.spawn(async move |cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        cx.update(|cx| {
                            cx.global::<DesktopProgress>().native.display();
                        });
                    }
                }));
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _cx = cx;
        }
    });
}

pub(crate) fn render_bar(progress: TerminalProgress, cx: &App) -> AnyElement {
    let color = match progress.state {
        ProgressState::Normal | ProgressState::Indeterminate => cx.theme().colors().text_accent,
        ProgressState::Error => cx.theme().status().error,
        ProgressState::Paused => cx.theme().status().warning,
    };
    let fill = div()
        .id("osc-progress-fill")
        .debug_selector(|| "OSC-PROGRESS-FILL".to_string())
        .relative()
        .h_full()
        .bg(color);
    let fill = if progress.state == ProgressState::Indeterminate {
        let reduce_motion = cx.reduce_motion();
        fill.w(relative(0.3))
            .with_animation(
                "osc-progress-animation",
                Animation::new(Duration::from_millis(1400))
                    .repeat_synced()
                    .with_max_fps(30.),
                move |fill, phase| {
                    fill.left(relative(if reduce_motion {
                        0.35
                    } else {
                        phase * 1.3 - 0.3
                    }))
                },
            )
            .into_any_element()
    } else {
        fill.w(relative(
            f32::from(progress.percentage.unwrap_or(100)) / 100.,
        ))
        .into_any_element()
    };
    div()
        .id("osc-progress")
        .debug_selector(|| "OSC-PROGRESS".to_string())
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h(px(2.))
        .overflow_hidden()
        .child(fill)
        .into_any_element()
}

#[cfg(target_os = "macos")]
mod native {
    use gpui::App;
    use objc2::{MainThreadMarker, MainThreadOnly as _, rc::Retained};
    use objc2_app_kit::{
        NSApplication, NSImageView, NSProgressIndicator, NSProgressIndicatorStyle,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use terminal_core::{ProgressState, TerminalProgress};

    pub(super) struct NativeProgress {
        image: Option<Retained<NSImageView>>,
        indicator: Option<Retained<NSProgressIndicator>>,
    }

    impl NativeProgress {
        pub(super) fn new(_cx: &mut App) -> Self {
            Self {
                image: None,
                indicator: None,
            }
        }

        pub(super) fn update(&mut self, progress: Option<TerminalProgress>) {
            let Some(marker) = MainThreadMarker::new() else {
                log::error!("Dock progress must be updated on the main thread");
                return;
            };
            let application = NSApplication::sharedApplication(marker);
            let tile = application.dockTile();
            let Some(progress) = progress else {
                if let Some(indicator) = self.indicator.take() {
                    unsafe { indicator.stopAnimation(None) };
                }
                tile.setContentView(None);
                self.image = None;
                tile.display();
                return;
            };
            if self.image.is_none() {
                let size = tile.size();
                let image = NSImageView::initWithFrame(
                    NSImageView::alloc(marker),
                    NSRect::new(NSPoint::new(0., 0.), size),
                );
                image.setImage(application.applicationIconImage().as_deref());
                let indicator = NSProgressIndicator::initWithFrame(
                    NSProgressIndicator::alloc(marker),
                    NSRect::new(
                        NSPoint::new(size.width * 0.08, size.height * 0.05),
                        NSSize::new(size.width * 0.84, size.height * 0.1),
                    ),
                );
                indicator.setStyle(NSProgressIndicatorStyle::Bar);
                indicator.setDisplayedWhenStopped(true);
                indicator.setMinValue(0.);
                indicator.setMaxValue(100.);
                image.addSubview(&indicator);
                tile.setContentView(Some(&image));
                self.image = Some(image);
                self.indicator = Some(indicator);
            }
            if let Some(indicator) = &self.indicator {
                let indeterminate = progress.state == ProgressState::Indeterminate;
                indicator.setIndeterminate(indeterminate);
                indicator.setDoubleValue(f64::from(progress.percentage.unwrap_or(100)));
                unsafe {
                    if indeterminate {
                        indicator.startAnimation(None);
                    } else {
                        indicator.stopAnimation(None);
                    }
                }
            }
            tile.display();
        }

        pub(super) fn display(&self) {
            if let Some(marker) = MainThreadMarker::new() {
                NSApplication::sharedApplication(marker)
                    .dockTile()
                    .display();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_progress_falls_back_when_a_task_finishes_or_closes() {
        let first = EntityId::from(1);
        let second = EntityId::from(2);
        let normal = TerminalProgress {
            state: ProgressState::Normal,
            percentage: Some(25),
        };
        let error = TerminalProgress {
            state: ProgressState::Error,
            percentage: Some(75),
        };
        let mut entries = ProgressEntries::default();
        entries.update(first, Some(normal));
        entries.update(second, Some(error));
        assert_eq!(entries.current(), Some(error));
        entries.update(second, None);
        assert_eq!(entries.current(), Some(normal));
        entries.update(first, None);
        assert_eq!(entries.current(), None);
    }
}

#[cfg(target_os = "linux")]
mod native {
    use std::{collections::HashMap, time::Duration};

    use futures::{FutureExt as _, StreamExt as _, channel::mpsc};
    use gpui::{App, AppContext as _};
    use terminal_core::{ProgressState, TerminalProgress};
    use zbus::{Connection, zvariant::Value};

    pub(super) struct NativeProgress {
        sender: mpsc::UnboundedSender<Option<TerminalProgress>>,
        _task: gpui::Task<()>,
    }

    impl NativeProgress {
        pub(super) fn new(cx: &mut App) -> Self {
            let (sender, mut receiver) = mpsc::unbounded();
            let executor = cx.background_executor().clone();
            let task = cx.background_spawn(async move {
                let mut connection: Option<Connection> = None;
                let mut current: Option<TerminalProgress> = None;
                loop {
                    let update = if current.is_some() {
                        // Re-publish active progress after the dock/taskbar restarts, even if the
                        // terminal's percentage has not changed in the meantime.
                        futures::select! {
                            update = receiver.next().fuse() => update,
                            _ = executor.timer(Duration::from_secs(5)).fuse() => Some(current),
                        }
                    } else {
                        receiver.next().await
                    };
                    let Some(progress) = update else { break };
                    current = progress;
                    if connection.is_none() {
                        match Connection::session().await {
                            Ok(session) => connection = Some(session),
                            Err(error) => {
                                log::warn!(
                                    "Could not connect taskbar progress to the session bus: {error}"
                                );
                                continue;
                            }
                        }
                    }
                    if let Some(session) = &connection
                        && let Err(error) = send(session, progress).await
                    {
                        log::warn!("Could not update taskbar progress: {error}");
                        connection = None;
                    }
                }
            });
            Self {
                sender,
                _task: task,
            }
        }

        pub(super) fn update(&mut self, progress: Option<TerminalProgress>) {
            if let Err(error) = self.sender.unbounded_send(progress) {
                log::warn!("Taskbar progress worker stopped: {error}");
            }
        }
    }

    fn launcher_update(
        progress: Option<TerminalProgress>,
    ) -> (String, HashMap<&'static str, Value<'static>>) {
        let percentage = progress
            .filter(|progress| progress.state != ProgressState::Indeterminate)
            .and_then(|progress| progress.percentage);
        let properties = HashMap::from([
            ("progress-visible", Value::Bool(percentage.is_some())),
            (
                "progress",
                Value::F64(f64::from(percentage.unwrap_or(0)) / 100.),
            ),
        ]);
        (
            format!("application://{}.desktop", crate::DESKTOP_ID),
            properties,
        )
    }

    async fn send(connection: &Connection, progress: Option<TerminalProgress>) -> zbus::Result<()> {
        connection
            .emit_signal(
                None::<&str>,
                "/dev/zed/ZedTerm/Progress",
                "com.canonical.Unity.LauncherEntry",
                "Update",
                &launcher_update(progress),
            )
            .await
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn launcher_messages_use_plasma_and_gnome_docks_expected_dbus_types() -> anyhow::Result<()>
        {
            for (progress, visible, fraction) in [
                (
                    Some(TerminalProgress {
                        state: ProgressState::Normal,
                        percentage: Some(0),
                    }),
                    true,
                    0.,
                ),
                (
                    Some(TerminalProgress {
                        state: ProgressState::Normal,
                        percentage: Some(50),
                    }),
                    true,
                    0.5,
                ),
                (
                    Some(TerminalProgress {
                        state: ProgressState::Normal,
                        percentage: Some(100),
                    }),
                    true,
                    1.,
                ),
                (
                    Some(TerminalProgress {
                        state: ProgressState::Paused,
                        percentage: Some(50),
                    }),
                    true,
                    0.5,
                ),
                (
                    Some(TerminalProgress {
                        state: ProgressState::Error,
                        percentage: Some(50),
                    }),
                    true,
                    0.5,
                ),
                (
                    Some(TerminalProgress {
                        state: ProgressState::Indeterminate,
                        percentage: Some(50),
                    }),
                    false,
                    0.,
                ),
                (None, false, 0.),
            ] {
                let message = zbus::Message::signal(
                    "/dev/zed/ZedTerm/Progress",
                    "com.canonical.Unity.LauncherEntry",
                    "Update",
                )?
                .build(&launcher_update(progress))?;
                let (launcher, properties): (String, HashMap<String, zbus::zvariant::OwnedValue>) =
                    message.body().deserialize()?;
                assert_eq!(launcher, "application://dev.zed.ZedTerm.desktop");
                assert_eq!(bool::try_from(&properties["progress-visible"])?, visible);
                assert_eq!(f64::try_from(&properties["progress"])?, fraction);
            }
            Ok(())
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod native {
    pub(super) struct NativeProgress;

    impl NativeProgress {
        pub(super) fn new(_cx: &mut gpui::App) -> Self {
            Self
        }
        pub(super) fn update(&mut self, _progress: Option<terminal_core::TerminalProgress>) {}
    }
}
