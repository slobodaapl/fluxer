// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Serialize;

/// Error returned to the bridge. Electron rejected `invoke` with an `Error` whose message
/// embedded the JS error name, and the renderer classifies failures by that name, so every
/// command error carries the name the Electron main process used. The bridge rebuilds an
/// `Error` from it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BridgeError {
    name: &'static str,
    message: String,
}

impl BridgeError {
    pub fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }

    #[cfg(test)]
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

impl std::error::Error for BridgeError {}
