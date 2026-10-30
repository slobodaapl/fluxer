// SPDX-License-Identifier: AGPL-3.0-or-later
use reuse_rs::{
    streamr::{
        frontend::Frontend,
        threaded::{Event, ThreadedPipeline},
        Engine, Pipeline,
    },
    Config, Denoiser,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn threaded(root: &Path, input: &[f32], engine: Engine) -> Vec<f32> {
    let mut pipeline = ThreadedPipeline::load_gamr_warmed(root, engine).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut offset, mut finishing, mut output) = (0, false, Vec::new());
    loop {
        assert!(Instant::now() < deadline, "Gamr stopped progressing");
        if offset < input.len() {
            let end = (offset + 959).min(input.len());
            if pipeline.try_push(&input[offset..end]).unwrap() {
                offset = end;
            }
        } else if !finishing {
            finishing = pipeline.try_finish().unwrap();
        }
        while let Some(event) = pipeline.try_output().unwrap() {
            match event {
                Event::Audio(audio) => output.extend(audio),
                Event::Finished => {
                    assert!(pipeline.try_push(&[]).is_err());
                    assert!(pipeline.try_finish().is_err());
                    return output;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn equivalent(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    assert!(actual.iter().all(|x| x.is_finite()));
    let error: f64 = actual
        .iter()
        .zip(expected)
        .map(|(&a, &b)| f64::from(a - b).powi(2))
        .sum();
    let energy: f64 = expected.iter().map(|&x| f64::from(x).powi(2)).sum();
    assert!(
        (error / energy.max(1e-20)).sqrt() < 1e-4,
        "Gamr differs from standalone RE-USE"
    );
}

#[test]
#[ignore = "requires STREAMR_MODELS and STREAMR_QUALITY fixture directories"]
fn gamr_needs_only_reuse_and_matches_direct_enhancement_across_partitions() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build_global()
        .unwrap();
    let source = std::path::PathBuf::from(std::env::var_os("STREAMR_MODELS").unwrap());
    let quality = std::path::PathBuf::from(std::env::var_os("STREAMR_QUALITY").unwrap());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("artifact")
        .join(format!("gamr-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for file in ["reuse.safetensors", "resampler.f64"] {
        std::fs::hard_link(source.join(file), root.join(file)).unwrap();
    }
    #[cfg(feature = "vulkan")]
    let engine = if std::env::var("STREAMR_ENGINE").as_deref() == Ok("vulkan") {
        Engine::Vulkan
    } else {
        Engine::Cpu
    };
    #[cfg(not(feature = "vulkan"))]
    let engine = Engine::Cpu;
    let mut wav = hound::WavReader::open(quality.join("clip005_smoke.wav")).unwrap();
    assert_eq!(wav.spec().sample_rate, 48000);
    let input: Vec<f32> = wav
        .samples::<f32>()
        .take(48000)
        .map(Result::unwrap)
        .collect();
    for length in [0, 1, 959, input.len()] {
        let input = &input[..length];
        let mut frontend = Frontend::load(&root).unwrap();
        let mut resampled = frontend.process(input).unwrap();
        resampled.extend(frontend.flush().unwrap());
        let mut standalone = Denoiser::with_config(
            root.join("reuse.safetensors"),
            Config {
                sample_rate: 16000,
                layers: 4,
                look_ahead: 0,
            },
        )
        .unwrap();
        let mut expected = standalone.process(&resampled).unwrap();
        expected.extend(standalone.flush().unwrap());
        assert_eq!(expected.len(), input.len().div_ceil(3));
        for partition in [127, 1921] {
            let mut pipeline = Pipeline::load_gamr(&root, engine).unwrap();
            let mut output = Vec::new();
            for chunk in input.chunks(partition) {
                output.extend(pipeline.process(chunk).unwrap());
            }
            output.extend(pipeline.flush().unwrap());
            equivalent(&output, &expected);
            assert!(pipeline.flush().is_err());
            assert!(pipeline.process(&[]).is_err());
        }
        equivalent(&threaded(&root, input, engine), &expected);
    }
    std::fs::remove_dir_all(root).unwrap();
}
