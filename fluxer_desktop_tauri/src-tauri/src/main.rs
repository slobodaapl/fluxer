// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    fluxer_desktop_tauri_lib::run();
}
