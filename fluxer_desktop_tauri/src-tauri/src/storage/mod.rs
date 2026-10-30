// SPDX-License-Identifier: AGPL-3.0-or-later

//! The desktop app store: accounts, scoped storage, known instances and markers, the port of
//! fluxer_desktop/src/main/DesktopAppStorage.ts over the native `fluxer_app_store` crate.

pub mod commands;
mod errors;
mod health;
mod operations;
pub(crate) mod private_file;
mod service;
mod startup;
mod telemetry;
mod validation;

pub use startup::open_desktop_app_storage;
