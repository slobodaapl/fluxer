// SPDX-License-Identifier: AGPL-3.0-or-later
fn main() {
    println!("cargo:rerun-if-changed=native/pipewire_runtime.cpp");
    println!("cargo:rerun-if-changed=native/file_playback.c");
    println!("cargo:rustc-check-cfg=cfg(ffmpeg_file_playback)");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        cc::Build::new()
            .cpp(true)
            .cpp_link_stdlib(None)
            .file("native/pipewire_runtime.cpp")
            .compile("fluxer_pipewire_runtime");
        let output = std::process::Command::new("pkg-config")
            .args([
                "--cflags",
                "--libs",
                "libavformat",
                "libavcodec",
                "libswscale",
                "libswresample",
                "libavutil",
            ])
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let flags = String::from_utf8_lossy(&output.stdout);
            let mut build = cc::Build::new();
            let mut libs = Vec::new();
            build.file("native/file_playback.c");
            for flag in flags.split_whitespace() {
                if let Some(path) = flag.strip_prefix("-I") {
                    build.include(path);
                } else if let Some(path) = flag.strip_prefix("-L") {
                    println!("cargo:rustc-link-search=native={path}");
                } else if let Some(lib) = flag.strip_prefix("-l") {
                    libs.push(lib.to_owned());
                } else if flag.starts_with("-D") || flag == "-pthread" {
                    build.flag(flag);
                }
            }
            build.compile("fluxer_file_playback");
            for lib in libs {
                println!("cargo:rustc-link-lib={lib}");
            }
            println!("cargo:rustc-cfg=ffmpeg_file_playback");
        } else {
            println!(
                "cargo:warning=FFmpeg development packages missing; native file playback is unavailable"
            );
        }
    }
}
