// SPDX-License-Identifier: AGPL-3.0-or-later

//! `window-live-resize-change` of fluxer_desktop/src/main/Window.ts on Windows. Electron's
//! `will-resize` came from `WM_SIZING` and `resized` from `WM_EXITSIZEMOVE`; a subclass of the
//! main window reads both. The renderer hears `true` once per resize and `false` when the
//! sizing loop ends, after 400 ms without a sizing step, on blur and on close.
//!
//! Electron sent nothing on Linux (`will-resize` is macOS and Windows only), and neither does
//! this shell. macOS would need the window's live-resize notifications, which Tauri does not
//! surface.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tauri::{Manager, WebviewWindow};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{WM_EXITSIZEMOVE, WM_NCDESTROY, WM_SIZING};

use crate::events::BridgeEvents;

const LIVE_RESIZE_EVENT: &str = "window-live-resize-change";
/// `LIVE_RESIZE_IDLE_MS`.
const IDLE: Duration = Duration::from_millis(400);
const SUBCLASS_ID: usize = 0x666c_7578;

#[derive(Default)]
pub struct LiveResize {
    /// `liveResizeActive`.
    active: AtomicBool,
    idle_generation: AtomicU64,
}

impl LiveResize {
    /// `sendLiveResizeState`: sends a change only.
    fn send(&self, window: &WebviewWindow, active: bool) {
        if self.active.swap(active, Ordering::SeqCst) != active {
            window
                .state::<BridgeEvents>()
                .emit(window.label(), LIVE_RESIZE_EVENT, active);
        }
    }

    /// `will-resize`.
    fn sizing(self: &Arc<Self>, window: &WebviewWindow) {
        self.send(window, true);
        let generation = self.idle_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let live_resize = self.clone();
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(IDLE).await;
            if live_resize.idle_generation.load(Ordering::SeqCst) == generation {
                live_resize.end(&window);
            }
        });
    }

    /// `endLiveResize`.
    pub fn end(&self, window: &WebviewWindow) {
        self.idle_generation.fetch_add(1, Ordering::SeqCst);
        self.send(window, false);
    }
}

struct Subclass {
    window: WebviewWindow,
    live_resize: Arc<LiveResize>,
}

/// Subclasses the window on its UI thread; the subclass removes itself when the window goes.
pub fn install(window: &WebviewWindow, live_resize: Arc<LiveResize>) -> tauri::Result<()> {
    let target = window.clone();
    window.run_on_main_thread(move || {
        let hwnd = match target.hwnd() {
            Ok(hwnd) => hwnd,
            Err(error) => {
                tracing::warn!(%error, "No window handle for live resize tracking");
                return;
            }
        };
        let data = Box::into_raw(Box::new(Subclass {
            window: target,
            live_resize,
        }));
        // SAFETY: `hwnd` is a live window owned by this (the UI) thread; `data` stays valid
        // until `WM_NCDESTROY` frees it, after which the subclass is gone.
        let installed =
            unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, data as usize) };
        if !installed.as_bool() {
            // SAFETY: the subclass was not installed, so nothing else holds `data`.
            drop(unsafe { Box::from_raw(data) });
            tracing::warn!("Failed to subclass the main window for live resize tracking");
        }
    })
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_SIZING | WM_EXITSIZEMOVE => {
            // SAFETY: `data` is the `Subclass` installed with this procedure and not yet freed.
            let subclass = unsafe { &*(data as *const Subclass) };
            if message == WM_SIZING {
                subclass.live_resize.sizing(&subclass.window);
            } else {
                subclass.live_resize.end(&subclass.window);
            }
        }
        WM_NCDESTROY => {
            // SAFETY: the window is going; no message reaches this procedure after removal.
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
                drop(Box::from_raw(data as *mut Subclass));
            }
        }
        _ => {}
    }
    // SAFETY: forwards the message to the next procedure in the chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
