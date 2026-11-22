// SPDX-License-Identifier: AGPL-3.0-or-later
pub(crate) fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // The dependency's linker arguments do not reach the final executable.
        // Load Objective-C categories from WebRTC's static archive here.
        println!("cargo:rustc-link-arg=-Wl,-ObjC");
    }
}
