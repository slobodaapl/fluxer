// SPDX-License-Identifier: AGPL-3.0-or-later

//! The main window's saved bounds, `window-state.json` of fluxer_desktop/src/main/Window.ts:
//! `loadWindowBounds`, `saveWindowBounds`, `clearSavedWindowBounds` and `ensureWindowOnScreen`.
//! The file keeps Electron's shape and logical (DIP) pixels. It lives in `app_data_dir` beside
//! `settings.json` and is replaced by write-then-rename where Electron overwrote it in place.
//!
//! Bounds are Electron's outer bounds. On Linux that is the client window GTK sizes: a native
//! title bar's WM frame lies outside it, as it did for Chromium on X11. On Windows and macOS it
//! is the frame, so a size is applied as the frame size less the frame's border.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, LogicalSize, Manager, Monitor, WebviewWindow};

use crate::config::DesktopConfig;
use crate::config::js::is_truthy;
use crate::config::launch;
use crate::storage::private_file;

const FILE_NAME: &str = "window-state.json";
/// `VISIBILITY_MARGIN`: a window is visible when it overlaps a work area inset this far.
const VISIBILITY_MARGIN: f64 = 32.0;
pub const DEFAULT_SIZE: (f64, f64) = (1280.0, 800.0);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// A rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    /// `boundsIntersect`: a positive-area overlap.
    fn intersects(&self, other: &Bounds) -> bool {
        let overlap_x = (self.x + self.width).min(other.x + other.width) - self.x.max(other.x);
        let overlap_y = (self.y + self.height).min(other.y + other.height) - self.y.max(other.y);
        overlap_x > 0.0 && overlap_y > 0.0
    }

    fn inset(&self, margin: f64) -> Bounds {
        Bounds {
            x: self.x + margin,
            y: self.y + margin,
            width: self.width - 2.0 * margin,
            height: self.height - 2.0 * margin,
        }
    }

    /// `savedBoundsExceedWorkArea`.
    fn exceeds(&self, area: &Bounds) -> bool {
        self.x < area.x
            || self.y < area.y
            || self.x + self.width > area.x + area.width
            || self.y + self.height > area.y + area.height
    }
}

/// An `Electron.Display`: the monitor and its work area.
#[derive(Clone, Copy, Debug)]
pub struct Display {
    pub bounds: Bounds,
    pub work_area: Bounds,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavedState {
    pub bounds: Bounds,
    pub is_maximized: bool,
}

/// The `WindowBounds` object written to the file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateFile {
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    is_maximized: bool,
}

/// `Math.round`: halves round up, `-2.5` to `-2`.
fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

/// `findVisibleDisplay`.
fn find_visible_display<'a>(displays: &'a [Display], bounds: &Bounds) -> Option<&'a Display> {
    displays
        .iter()
        .find(|display| bounds.intersects(&display.work_area.inset(VISIBILITY_MARGIN)))
}

/// `min(1280, workArea.width)`×`min(800, workArea.height)`.
fn default_size(work_area: &Bounds) -> (f64, f64) {
    (
        DEFAULT_SIZE.0.min(work_area.width),
        DEFAULT_SIZE.1.min(work_area.height),
    )
}

/// `getDefaultBoundsForDisplay`: the default size centred in a work area. `createWindow` used
/// it too, as the default size with `center: true` on the primary display.
pub fn default_bounds(work_area: &Bounds) -> Bounds {
    let (width, height) = default_size(work_area);
    Bounds {
        x: js_round(work_area.x + (work_area.width - width) / 2.0) as f64,
        y: js_round(work_area.y + (work_area.height - height) / 2.0) as f64,
        width,
        height,
    }
}

/// `JSON.parse`, `findVisibleDisplay` and `sanitizeLoadedWindowBounds` over the file contents.
/// `Ok(None)` is a state no display shows, which includes one whose bounds are not all numbers:
/// Electron did no field validation, and the arithmetic on them came out NaN.
fn restore(raw: &str, displays: &[Display]) -> Result<Option<SavedState>, serde_json::Error> {
    let parsed: Value = serde_json::from_str(raw)?;
    let number = |key: &str| parsed.get(key).and_then(Value::as_f64);
    let (Some(x), Some(y), Some(width), Some(height)) =
        (number("x"), number("y"), number("width"), number("height"))
    else {
        return Ok(None);
    };
    let saved = SavedState {
        bounds: Bounds {
            x,
            y,
            width,
            height,
        },
        is_maximized: is_truthy(parsed.get("isMaximized")),
    };
    let Some(display) = find_visible_display(displays, &saved.bounds) else {
        return Ok(None);
    };
    if !saved.is_maximized || !saved.bounds.exceeds(&display.work_area) {
        return Ok(Some(saved));
    }
    let sanitized = SavedState {
        bounds: default_bounds(&display.work_area),
        is_maximized: true,
    };
    tracing::info!(
        ?saved,
        ?sanitized,
        "Ignoring display-sized normal bounds from saved maximized window state"
    );
    Ok(Some(sanitized))
}

/// `JSON.stringify(windowState, null, 2)`.
fn serialize(state: SavedState) -> String {
    let file = StateFile {
        x: js_round(state.bounds.x),
        y: js_round(state.bounds.y),
        width: js_round(state.bounds.width),
        height: js_round(state.bounds.height),
        is_maximized: state.is_maximized,
    };
    serde_json::to_string_pretty(&file).expect("window state serialises")
}

/// The bounds `ensureWindowOnScreen` moves a window to when no display shows it: the first
/// display's origin (its full bounds, not its work area), the size clamped to that display.
fn offscreen_correction(bounds: Bounds, displays: &[Display]) -> Option<Bounds> {
    if find_visible_display(displays, &bounds).is_some() {
        return None;
    }
    let first = displays.first()?.bounds;
    Some(Bounds {
        x: first.x,
        y: first.y,
        width: bounds.width.min(first.width),
        height: bounds.height.min(first.height),
    })
}

fn file_path(app: &AppHandle) -> tauri::Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join(FILE_NAME))
}

fn to_display(monitor: &Monitor) -> Display {
    let scale = monitor.scale_factor();
    let position = monitor.position().to_logical::<f64>(scale);
    let size = monitor.size().to_logical::<f64>(scale);
    let area = monitor.work_area();
    let area_position = area.position.to_logical::<f64>(scale);
    let area_size = area.size.to_logical::<f64>(scale);
    Display {
        bounds: Bounds {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        },
        work_area: Bounds {
            x: area_position.x,
            y: area_position.y,
            width: area_size.width,
            height: area_size.height,
        },
    }
}

/// `screen.getAllDisplays()`, each monitor in its own logical pixels.
pub fn displays(app: &AppHandle) -> Vec<Display> {
    match app.available_monitors() {
        Ok(monitors) => monitors.iter().map(to_display).collect(),
        Err(error) => {
            tracing::warn!(%error, "Failed to list the displays");
            Vec::new()
        }
    }
}

/// `screen.getPrimaryDisplay().workArea`.
pub fn primary_work_area(app: &AppHandle) -> Option<Bounds> {
    match app.primary_monitor() {
        Ok(monitor) => monitor.map(|monitor| to_display(&monitor).work_area),
        Err(error) => {
            tracing::warn!(%error, "Failed to read the primary display");
            None
        }
    }
}

/// `loadWindowBounds`.
pub fn load(app: &AppHandle, args: &[String], displays: &[Display]) -> Option<SavedState> {
    if launch::ignores_window_state(args) {
        tracing::info!("Ignoring saved window bounds for this launch");
        return None;
    }
    let config = app.state::<Arc<DesktopConfig>>();
    if !config.window_behavior().remember_window_state {
        return None;
    }
    let path = match file_path(app) {
        Ok(path) => path,
        Err(error) => {
            tracing::error!(%error, "Failed to load window bounds");
            return None;
        }
    };
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::error!(%error, "Failed to load window bounds");
            return None;
        }
    };
    match restore(&raw, displays) {
        Ok(Some(state)) => {
            tracing::info!(?state, "Restored window bounds");
            Some(state)
        }
        Ok(None) => {
            tracing::warn!("Saved window position is off-screen, using defaults");
            None
        }
        Err(error) => {
            tracing::error!(%error, "Failed to load window bounds");
            None
        }
    }
}

/// `clearSavedWindowBounds`.
pub fn clear(app: &AppHandle) {
    let removed = file_path(app)
        .map_err(|error| error.to_string())
        .and_then(|path| match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.to_string()),
        });
    match removed {
        Ok(true) => tracing::info!("Cleared saved window bounds"),
        Ok(false) => {}
        Err(error) => tracing::error!(%error, "Failed to clear saved window bounds"),
    }
}

/// `getBounds()`: the window's outer bounds in its logical pixels. `None` once it is gone.
pub fn current_bounds(window: &WebviewWindow) -> Option<Bounds> {
    let scale = window.scale_factor().ok()?;
    let position = window.outer_position().ok()?.to_logical::<f64>(scale);
    #[cfg(target_os = "linux")]
    let size = window.inner_size().ok()?;
    #[cfg(not(target_os = "linux"))]
    let size = window.outer_size().ok()?;
    let size = size.to_logical::<f64>(scale);
    Some(Bounds {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    })
}

/// Gives the window an outer size of `width`×`height` logical pixels.
pub fn apply_size(window: &WebviewWindow, width: f64, height: f64) -> tauri::Result<()> {
    #[cfg(not(target_os = "linux"))]
    let (width, height) = {
        let scale = window.scale_factor()?;
        let outer = window.outer_size()?.to_logical::<f64>(scale);
        let inner = window.inner_size()?.to_logical::<f64>(scale);
        (
            width - (outer.width - inner.width),
            height - (outer.height - inner.height),
        )
    };
    window.set_size(LogicalSize::new(width, height))
}

/// `ensureWindowOnScreen`, run before the window is shown.
pub fn ensure_on_screen(window: &WebviewWindow) {
    let Some(bounds) = current_bounds(window) else {
        return;
    };
    let Some(corrected) = offscreen_correction(bounds, &displays(window.app_handle())) else {
        return;
    };
    tracing::warn!(
        ?corrected,
        "Window is off-screen, repositioning to primary display"
    );
    let moved = window
        .set_position(tauri::LogicalPosition::new(corrected.x, corrected.y))
        .and_then(|()| apply_size(window, corrected.width, corrected.height));
    if let Err(error) = moved {
        tracing::warn!(%error, "Failed to move the window on screen");
    }
}

/// What `saveWindowBounds` reads for one main window: the normal (restored) bounds, which Tauri
/// does not report, and `lastRestorableMainWindowMaximized`.
pub struct Tracker {
    normal: Mutex<Bounds>,
    last_restorable_maximized: AtomicBool,
    save_generation: AtomicU64,
}

impl Tracker {
    pub fn new(normal: Bounds, maximized: bool) -> Self {
        Self {
            normal: Mutex::new(normal),
            last_restorable_maximized: AtomicBool::new(maximized),
            save_generation: AtomicU64::new(0),
        }
    }

    /// `move`/`resize`: the 500 ms debounced save. The bounds are the normal bounds only while
    /// the window is neither maximized, minimized nor fullscreen; tao also reports moves and
    /// resizes for those state changes.
    pub fn geometry_changed(self: &Arc<Self>, window: &WebviewWindow) {
        let maximized = window.is_maximized().unwrap_or(true);
        let minimized = window.is_minimized().unwrap_or(true);
        let fullscreen = window.is_fullscreen().unwrap_or(true);
        if !maximized
            && !minimized
            && !fullscreen
            && let Some(bounds) = current_bounds(window)
        {
            *self.normal.lock().expect("window state lock") = bounds;
        }
        let generation = self.save_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let tracker = self.clone();
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(SAVE_DEBOUNCE).await;
            if tracker.save_generation.load(Ordering::SeqCst) == generation {
                tracker.save(&window);
            }
        });
    }

    /// Drops a pending debounced save (`clearTimeout(saveTimeout)` on close).
    pub fn cancel_pending_save(&self) {
        self.save_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// `maximize`: saved at once.
    pub fn maximized(&self, window: &WebviewWindow) {
        self.last_restorable_maximized.store(true, Ordering::SeqCst);
        self.save(window);
    }

    /// `unmaximize`: saved on the next turn, keeping the flag when the window was minimized
    /// from maximized.
    pub fn unmaximized(self: &Arc<Self>, window: &WebviewWindow) {
        let tracker = self.clone();
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(minimized) = window.is_minimized() else {
                return;
            };
            if !minimized {
                tracker
                    .last_restorable_maximized
                    .store(false, Ordering::SeqCst);
            }
            tracker.save(&window);
        });
    }

    /// `saveWindowBounds`.
    pub fn save(&self, window: &WebviewWindow) {
        let config = window.state::<Arc<DesktopConfig>>();
        if !config.window_behavior().remember_window_state {
            return;
        }
        let (Ok(minimized), Ok(maximized)) = (window.is_minimized(), window.is_maximized()) else {
            return;
        };
        let is_maximized = if minimized {
            self.last_restorable_maximized.load(Ordering::SeqCst)
        } else {
            maximized
        };
        self.last_restorable_maximized
            .store(is_maximized, Ordering::SeqCst);
        let state = SavedState {
            bounds: *self.normal.lock().expect("window state lock"),
            is_maximized,
        };
        let written = file_path(window.app_handle())
            .map_err(|error| error.to_string())
            .and_then(|path| {
                if let Some(directory) = path.parent() {
                    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
                }
                private_file::write(&path, &serialize(state)).map_err(|error| error.to_string())
            });
        match written {
            Ok(()) => tracing::debug!(?state, "Saved window bounds"),
            Err(error) => tracing::error!(%error, "Failed to save window bounds"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Bounds {
        Bounds {
            x,
            y,
            width,
            height,
        }
    }

    fn display(bounds: Bounds, work_area: Bounds) -> Display {
        Display { bounds, work_area }
    }

    /// A 1920×1080 display with a 40 px bottom panel, and a 1280×720 one to its right.
    fn two_displays() -> Vec<Display> {
        vec![
            display(
                rect(0.0, 0.0, 1920.0, 1080.0),
                rect(0.0, 0.0, 1920.0, 1040.0),
            ),
            display(
                rect(1920.0, 0.0, 1280.0, 720.0),
                rect(1920.0, 0.0, 1280.0, 720.0),
            ),
        ]
    }

    fn file(x: f64, y: f64, width: f64, height: f64, is_maximized: bool) -> String {
        serialize(SavedState {
            bounds: rect(x, y, width, height),
            is_maximized,
        })
    }

    #[test]
    fn the_file_has_electrons_shape() {
        assert_eq!(
            file(-0.5, 20.4, 1280.0, 799.6, true),
            "{\n  \"x\": 0,\n  \"y\": 20,\n  \"width\": 1280,\n  \"height\": 800,\n  \"isMaximized\": true\n}"
        );
    }

    #[test]
    fn saved_bounds_round_trip_on_a_display_that_shows_them() {
        let restored = restore(&file(100.0, 50.0, 1000.0, 700.0, false), &two_displays());
        assert_eq!(
            restored.unwrap(),
            Some(SavedState {
                bounds: rect(100.0, 50.0, 1000.0, 700.0),
                is_maximized: false,
            })
        );
    }

    #[test]
    fn bounds_inside_the_margin_band_only_are_off_screen() {
        let displays = two_displays();
        // Right of the second display's inset work area (it ends at 3168).
        let band = file(3170.0, 100.0, 400.0, 300.0, false);
        assert_eq!(restore(&band, &displays).unwrap(), None);
        // One pixel into the inset area counts.
        let edge = file(3167.0, 100.0, 400.0, 300.0, false);
        assert!(restore(&edge, &displays).unwrap().is_some());
        assert_eq!(restore(&band, &[]).unwrap(), None);
    }

    #[test]
    fn non_numeric_or_missing_bounds_are_off_screen_and_bad_json_fails() {
        let displays = two_displays();
        for raw in [
            r#"{"x": 10, "y": 10, "width": 800}"#,
            r#"{"x": "10", "y": 10, "width": 800, "height": 600}"#,
            "[]",
        ] {
            assert_eq!(restore(raw, &displays).unwrap(), None, "{raw}");
        }
        assert!(restore("{", &displays).is_err());
    }

    #[test]
    fn maximized_state_with_display_sized_bounds_is_centred_on_its_display() {
        let displays = two_displays();
        let restored = restore(&file(1910.0, 0.0, 1300.0, 720.0, true), &displays).unwrap();
        assert_eq!(
            restored,
            Some(SavedState {
                bounds: rect(1920.0, 0.0, 1280.0, 720.0),
                is_maximized: true,
            })
        );
        let centred = restore(&file(0.0, 0.0, 1920.0, 1080.0, true), &displays).unwrap();
        assert_eq!(
            centred,
            Some(SavedState {
                bounds: rect(320.0, 120.0, 1280.0, 800.0),
                is_maximized: true,
            })
        );
        // Not maximized, the same bounds are kept as saved.
        let kept = restore(&file(0.0, 0.0, 1920.0, 1080.0, false), &displays).unwrap();
        assert_eq!(kept.unwrap().bounds, rect(0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn the_maximized_flag_is_javascript_truthiness() {
        let raw = r#"{"x": 10, "y": 10, "width": 800, "height": 600, "isMaximized": 1}"#;
        assert!(restore(raw, &two_displays()).unwrap().unwrap().is_maximized);
    }

    #[test]
    fn default_bounds_are_clamped_and_centred_in_the_work_area() {
        assert_eq!(
            default_bounds(&rect(-1024.0, 0.0, 1024.0, 740.0)),
            rect(-1024.0, 0.0, 1024.0, 740.0)
        );
        assert_eq!(
            default_bounds(&rect(0.0, 25.0, 1915.0, 1055.0)),
            rect(318.0, 153.0, 1280.0, 800.0)
        );
    }

    #[test]
    fn off_screen_windows_move_to_the_first_display_origin() {
        let displays = two_displays();
        assert_eq!(
            offscreen_correction(rect(200.0, 200.0, 800.0, 600.0), &displays),
            None
        );
        assert_eq!(
            offscreen_correction(rect(5000.0, 3000.0, 2500.0, 600.0), &displays),
            Some(rect(0.0, 0.0, 1920.0, 600.0))
        );
        assert_eq!(
            offscreen_correction(rect(5000.0, 3000.0, 800.0, 600.0), &[]),
            None
        );
    }
}
