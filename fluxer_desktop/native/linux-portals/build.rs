// SPDX-License-Identifier: AGPL-3.0-or-later

fn main() {
    #[cfg(feature = "napi")]
    napi_build::setup();
}
