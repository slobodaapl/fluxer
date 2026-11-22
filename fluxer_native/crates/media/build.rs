// SPDX-License-Identifier: AGPL-3.0-or-later
#[path = "../link_webrtc.rs"]
mod link_webrtc;

fn main() {
    link_webrtc::main();
    deep_filter_model();
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

fn deep_filter_model() {
    use sha2::{Digest, Sha256};
    use std::{io::Read, path::PathBuf, time::Duration};
    // This is the exact archive pinned by the existing client's LFS pointer and
    // NOTICE.md. A full checkout uses it directly. Pointer-only source archives
    // fetch a hash-verified copy at build time, never during microphone capture.
    const SHA256: &str = "c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616";
    const SIZE: u64 = 7_983_136;
    const URL: &str = "https://raw.githubusercontent.com/Rikorose/DeepFilterNet/main/models/DeepFilterNet3_onnx.tar.gz";
    let source = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../../fluxer_app/src/features/voice/utils/noise_suppression/deepfilternet3/DeepFilterNet3_onnx.tar.gz");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-env-changed=FLUXER_DEEP_FILTER_MODEL");
    let override_path = std::env::var_os("FLUXER_DEEP_FILTER_MODEL").map(PathBuf::from);
    let cached =
        PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join(format!("dfn3-{SHA256}.tar.gz"));
    let valid = |bytes: &[u8]| {
        bytes.len() as u64 == SIZE && format!("{:x}", Sha256::digest(bytes)) == SHA256
    };
    let mut selected = None;
    for path in override_path.iter().chain([&source, &cached]) {
        if let Ok(file) = std::fs::File::open(path) {
            let mut bytes = Vec::new();
            file.take(SIZE + 1)
                .read_to_end(&mut bytes)
                .expect("Read DFN model");
            if valid(&bytes) {
                selected = Some(path.clone());
                break;
            }
        }
        assert!(
            override_path.as_ref() != Some(path),
            "FLUXER_DEEP_FILTER_MODEL must point to the DFN3 archive with SHA-256 {SHA256}"
        );
    }
    let selected = selected.unwrap_or_else(|| {
        println!("cargo:warning=Fetching hash-verified DeepFilterNet3 model (8 MB) for embedding; use Git LFS or FLUXER_DEEP_FILTER_MODEL for offline builds");
        let response = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120)).build().expect("DFN download client")
            .get(URL).send().and_then(|r| r.error_for_status())
            .expect("Fetch DFN model; for offline builds hydrate its Git LFS archive or set FLUXER_DEEP_FILTER_MODEL");
        let mut bytes = Vec::with_capacity(SIZE as usize);
        response.take(SIZE + 1).read_to_end(&mut bytes).expect("Download DFN model");
        assert!(valid(&bytes), "Downloaded DFN model failed SHA-256/size verification");
        let temp = cached.with_extension("tmp");
        std::fs::write(&temp, bytes).expect("Cache DFN model");
        std::fs::rename(temp, &cached).expect("Finalize DFN model cache");
        cached
    });
    let selected = selected.canonicalize().expect("Resolve DFN model path");
    println!("cargo:rerun-if-changed={}", selected.display());
    println!(
        "cargo:rustc-env=FLUXER_DEEP_FILTER_MODEL_PATH={}",
        selected.display()
    );
}
