//! Window-geometry persistence.
//!
//! Restores each main window's size and position across launches; tabs and
//! shell state are deliberately not saved (PTY sessions cannot survive a
//! restart). Stored as one JSON file in the app data dir, keeping with the
//! lightweight no-database approach of this fork.

use std::path::PathBuf;

use gpui::{App, Bounds, Pixels, Point, Size, px};
use serde::{Deserialize, Serialize};

use crate::TERM_PROGRAM;

const GEOMETRY_FORMAT_VERSION: u32 = 1;
const WINDOW_GEOMETRY_FILE: &str = "window-geometry.json";
/// Guards against runaway growth if a bug ever spawns many windows.
const MAX_WINDOWS: usize = 32;
/// Smallest dimensions we restore; below these a corrupt entry is treated as
/// unusable and falls back to the default window size.
const MIN_RESTORED_WIDTH: f32 = 320.0;
const MIN_RESTORED_HEIGHT: f32 = 200.0;

#[derive(Debug, Serialize, Deserialize)]
struct StoredGeometry {
    version: u32,
    windows: Vec<StoredWindow>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredWindow {
    /// Serialized in logical pixels exactly as gpui hands it to us.
    bounds: Bounds<Pixels>,
}

fn geometry_path() -> PathBuf {
    paths::data_dir()
        .join(TERM_PROGRAM)
        .join(WINDOW_GEOMETRY_FILE)
}

/// Reads the persisted main-window geometries from the last quit. Returns an
/// empty vec when the file is missing or malformed.
fn load_stored_geometries() -> Vec<Bounds<Pixels>> {
    let Ok(text) = std::fs::read_to_string(geometry_path()) else {
        return Vec::new();
    };
    let Ok(stored) = serde_json::from_str::<StoredGeometry>(&text) else {
        log::info!("Ignoring unreadable window geometry file");
        return Vec::new();
    };
    if stored.version != GEOMETRY_FORMAT_VERSION {
        return Vec::new();
    }
    stored
        .windows
        .into_iter()
        .take(MAX_WINDOWS)
        .map(|window| window.bounds)
        .collect()
}

/// Returns the restored geometry for the first main window, clamped to be
/// usable and shifted onto a connected display when the saved position no
/// longer resolves (e.g. an external monitor was unplugged). Returns `None`
/// when there is nothing valid to restore.
pub fn first_window_bounds(cx: &App) -> Option<Bounds<Pixels>> {
    // Note: not `Vec::drain(..1)`, which panics instead of yielding `None`
    // on an empty list (e.g. first launch, before any file was written).
    let bounds = load_stored_geometries().into_iter().next()?;
    sanitize_bounds(bounds, cx)
}

/// Persists the given main-window geometries for the next launch.
pub fn save_window_geometries(window_bounds: &[Bounds<Pixels>]) {
    let stored = StoredGeometry {
        version: GEOMETRY_FORMAT_VERSION,
        windows: window_bounds
            .iter()
            .take(MAX_WINDOWS)
            .map(|&bounds| StoredWindow { bounds })
            .collect(),
    };
    let path = geometry_path();
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        log::error!("Failed to create data dir for window geometry: {error}");
        return;
    }
    let json = match serde_json::to_string_pretty(&stored) {
        Ok(json) => json,
        Err(error) => {
            log::error!("Failed to serialize window geometry: {error}");
            return;
        }
    };
    if let Err(error) = std::fs::write(&path, json) {
        log::error!("Failed to write window geometry file: {error}");
    }
}

/// Checks a stored geometry is sane (finite, not tiny) and lies on some
/// connected display, falling back to a centered default-size window on the
/// main display otherwise.
fn sanitize_bounds(bounds: Bounds<Pixels>, cx: &App) -> Option<Bounds<Pixels>> {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    if !width.is_finite() || !height.is_finite() || width < MIN_RESTORED_WIDTH || height < MIN_RESTORED_HEIGHT {
        return None;
    }
    let displays = cx.displays();
    if displays.is_empty() {
        return None;
    }
    let visible_on_any_display = displays.iter().any(|display| {
        let display_bounds = display.bounds();
        bounds.left() < display_bounds.right()
            && bounds.right() > display_bounds.left()
            && bounds.top() < display_bounds.bottom()
            && bounds.bottom() > display_bounds.top()
    });
    if visible_on_any_display {
        Some(bounds)
    } else {
        None
    }
}

/// Default geometry used whenever nothing valid is restored: a default-sized
/// window centered on the main display.
pub fn default_first_window_bounds(cx: &App, default_size: Size<Pixels>) -> Bounds<Pixels> {
    if let Some(display) = cx.displays().first() {
        let display_bounds = display.bounds();
        let origin_x =
            display_bounds.left().as_f32() + ((display_bounds.size.width - default_size.width).as_f32() / 2.0).max(0.0);
        let origin_y = display_bounds.top().as_f32()
            + ((display_bounds.size.height - default_size.height).as_f32() / 2.0).max(0.0);        return Bounds {
            origin: Point {
                x: px(origin_x),
                y: px(origin_y),
            },
            size: default_size,
        };
    }
    Bounds::new(Point { x: px(0.0), y: px(0.0) }, default_size)
}
