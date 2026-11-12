// SPDX-License-Identifier: AGPL-3.0-or-later
fn main() {
    println!("cargo:rerun-if-changed=native/pipewire_runtime.cpp");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        cc::Build::new()
            .cpp(true)
            .cpp_link_stdlib(None)
            .file("native/pipewire_runtime.cpp")
            .compile("fluxer_pipewire_runtime");
    }
}
