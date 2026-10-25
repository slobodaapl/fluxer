use reuse_rs::streamr::{Engine, Pipeline};
use std::path::Path;

#[test]
#[ignore = "requires STREAMR_MODELS and STREAMR_QUALITY fixture directories"]
fn failed_stage_initialization_releases_started_workers() {
    use reuse_rs::streamr::threaded::ThreadedPipeline;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    workers();
    let source = std::path::PathBuf::from(std::env::var_os("STREAMR_MODELS").unwrap());
    let quality = std::path::PathBuf::from(std::env::var_os("STREAMR_QUALITY").unwrap());
    #[cfg(feature = "vulkan")]
    let engine = if std::env::var("STREAMR_ENGINE").as_deref() == Ok("vulkan") {
        Engine::Vulkan
    } else {
        Engine::Cpu
    };
    #[cfg(not(feature = "vulkan"))]
    let engine = Engine::Cpu;
    let enrollment = read(&quality.join("clip005_enrollment.wav"), 48000);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("artifact")
        .join(format!(
            "missing-model-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&directory).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() != "reuse.safetensors" && entry.file_type().unwrap().is_file() {
            std::fs::hard_link(entry.path(), directory.join(entry.file_name())).unwrap();
        }
    }
    let model_dir = directory.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let error = ThreadedPipeline::load(&model_dir, &enrollment, engine)
            .err()
            .map(|error| format!("{error:#}"));
        sender.send(error).unwrap();
    });
    let result = receiver
        .recv_timeout(Duration::from_secs(120))
        .expect("initialization failure left a worker blocked");
    worker.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(result.is_some(), "missing RE-USE weights were accepted");
}

#[test]
#[ignore = "requires STREAMR_MODELS and STREAMR_QUALITY fixture directories"]
fn threaded_schedule_preserves_waveform_flush_and_terminal_state() {
    use reuse_rs::streamr::threaded::{Event, ThreadedPipeline};
    use std::time::{Duration, Instant};
    workers();
    let root = std::env::var("STREAMR_MODELS").unwrap();
    let quality = std::env::var("STREAMR_QUALITY").unwrap();
    let (root, quality) = (Path::new(&root), Path::new(&quality));
    #[cfg(feature = "vulkan")]
    let engine = if std::env::var("STREAMR_ENGINE").as_deref() == Ok("vulkan") {
        Engine::Vulkan
    } else {
        Engine::Cpu
    };
    #[cfg(not(feature = "vulkan"))]
    let engine = Engine::Cpu;
    let enrollment = read(&quality.join("clip005_enrollment.wav"), 48000);
    let input = read(&quality.join("clip005_smoke.wav"), 48000);
    for length in [0, 1, 959, input.len()] {
        let input = &input[..length];
        let expected = run(root, &enrollment, input, engine, &[960]);
        let mut pipeline = ThreadedPipeline::load(root, &enrollment, engine).unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        let (mut output, mut offset, mut turn) = (Vec::new(), 0, 0);
        while offset < input.len() {
            assert!(
                Instant::now() < deadline,
                "threaded pipeline stopped progressing"
            );
            let end = (offset + [1, 97, 1919, 960, 3840][turn % 5]).min(input.len());
            if pipeline.try_push(&input[offset..end]).unwrap() {
                offset = end;
                turn += 1;
            }
            while let Some(event) = pipeline.try_output().unwrap() {
                match event {
                    Event::Audio(audio) => output.extend(audio),
                    Event::Finished => panic!("finished before flush"),
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut finishing = false;
        loop {
            assert!(
                Instant::now() < deadline,
                "threaded flush stopped progressing"
            );
            if !finishing {
                finishing = pipeline.try_finish().unwrap();
            }
            match pipeline.try_output().unwrap() {
                Some(Event::Audio(audio)) => output.extend(audio),
                Some(Event::Finished) => break,
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        assert_eq!(output.len(), expected.len());
        assert!(output.iter().all(|x| x.is_finite()));
        assert!(relative(&output, &expected) < 1e-5);
        assert!(pipeline.try_push(&[]).is_err());
        assert!(pipeline.try_finish().is_err());
    }
}

fn workers() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build_global()
            .unwrap()
    });
}

fn read(path: &Path, rate: u32) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    assert_eq!(reader.spec().sample_rate, rate);
    assert_eq!(reader.spec().channels, 1);
    reader.samples::<f32>().collect::<Result<_, _>>().unwrap()
}
fn run(
    root: &Path,
    enrollment: &[f32],
    input: &[f32],
    engine: Engine,
    chunks: &[usize],
) -> Vec<f32> {
    let mut pipeline = Pipeline::load(root, enrollment, engine).unwrap();
    assert!(pipeline.process(&[]).unwrap().is_empty());
    assert!(pipeline.process(&[f32::NAN]).is_err());
    let mut output = Vec::new();
    let (mut offset, mut turn) = (0, 0);
    while offset < input.len() {
        let end = (offset + chunks[turn % chunks.len()]).min(input.len());
        output.extend(pipeline.process(&input[offset..end]).unwrap());
        offset = end;
        turn += 1;
    }
    output.extend(pipeline.flush().unwrap());
    assert_eq!(output.len(), input.len().div_ceil(3));
    assert!(output.iter().all(|v| v.is_finite()));
    assert!(pipeline.flush().is_err());
    assert!(pipeline.process(&[]).is_err());
    output
}
fn relative(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len());
    let error: f64 = a
        .iter()
        .zip(b)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum();
    let energy: f64 = b.iter().map(|&b| f64::from(b).powi(2)).sum();
    (error / energy.max(1e-30)).sqrt()
}

#[test]
#[ignore = "requires STREAMR_MODELS and STREAMR_QUALITY fixture directories"]
fn waveform_partition_lifecycle() {
    workers();
    let root = std::env::var("STREAMR_MODELS").unwrap();
    let quality = std::env::var("STREAMR_QUALITY").unwrap();
    let (root, quality) = (Path::new(&root), Path::new(&quality));
    let engine = match std::env::var("STREAMR_ENGINE").as_deref() {
        Ok("vulkan") => {
            #[cfg(feature = "vulkan")]
            {
                Engine::Vulkan
            }
            #[cfg(not(feature = "vulkan"))]
            {
                panic!("Vulkan not compiled")
            }
        }
        _ => Engine::Cpu,
    };
    for prefix in ["clip005", "clip018"] {
        let enrollment = read(&quality.join(format!("{prefix}_enrollment.wav")), 48000);
        let input = read(&quality.join(format!("{prefix}_smoke.wav")), 48000);
        let baseline = run(root, &enrollment, &input, engine, &[3840]);
        let partitioned = run(
            root,
            &enrollment,
            &input,
            engine,
            &[1, 127, 256, 1921, 480, 7680],
        );
        let drift = relative(&partitioned, &baseline);
        eprintln!("{prefix} partition relative L2: {drift}");
        assert!(
            drift < 1e-5,
            "partition changed waveform beyond FP32 roundoff"
        );
        let repeated = run(root, &enrollment, &input, engine, &[3840]);
        assert!(baseline
            .iter()
            .zip(repeated)
            .all(|(a, b)| a.to_bits() == b.to_bits()));
        for length in [0, 1, 257, 1921] {
            run(root, &enrollment, &input[..length], engine, &[127]);
        }
    }
}

#[test]
#[ignore = "requires STREAMR_MODELS and STREAMR_QUALITY fixture directories"]
fn same_input_stage_references() {
    workers();
    use reuse_rs::{streamr::vox::Vox, Config, Denoiser};
    let root = std::env::var("STREAMR_MODELS").unwrap();
    let quality = std::env::var("STREAMR_QUALITY").unwrap();
    let (root, quality) = (Path::new(&root), Path::new(&quality));
    let engine = match std::env::var("STREAMR_ENGINE").as_deref() {
        Ok("vulkan") => {
            #[cfg(feature = "vulkan")]
            {
                Engine::Vulkan
            }
            #[cfg(not(feature = "vulkan"))]
            {
                panic!("Vulkan not compiled")
            }
        }
        _ => Engine::Cpu,
    };
    let enrollment = read(&quality.join("stage_reference_enrollment.wav"), 16000);
    let filtered = read(&quality.join("stage_reference_front.wav"), 16000);
    let mut vox = Vox::load(root, &enrollment, engine).unwrap();
    let mut output = Vec::new();
    for chunk in filtered.chunks(1280) {
        output.extend(vox.process(chunk).unwrap());
    }
    output.extend(vox.flush().unwrap());
    let reference = read(&quality.join("stage_reference_vox.wav"), 16000);
    let error = relative(&output, &reference);
    eprintln!("VoxLock same-input relative L2: {error}");
    assert!(error < 1e-5);
    let config = Config {
        sample_rate: 16000,
        layers: 4,
        look_ahead: 0,
    };
    let mut reuse = match engine {
        Engine::Cpu => Denoiser::with_config(root.join("reuse.safetensors"), config).unwrap(),
        #[cfg(feature = "vulkan")]
        Engine::Vulkan => {
            Denoiser::with_vulkan_config(root.join("reuse.safetensors"), config).unwrap()
        }
    };
    let mut output = Vec::new();
    for chunk in reference.chunks(1280) {
        output.extend(reuse.process(chunk).unwrap());
    }
    output.extend(reuse.flush().unwrap());
    let reference = read(&quality.join("stage_reference_reuse.wav"), 16000);
    let error = relative(&output, &reference);
    eprintln!("RE-USE same-input relative L2: {error}");
    assert!(error < 1e-5);
}
