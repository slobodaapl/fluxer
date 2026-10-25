// SPDX-License-Identifier: AGPL-3.0-or-later
mod protocol;
mod upsampler;

use eyre::{ensure, Result};
use reuse_rs::streamr::{
    threaded::{Event, ThreadedPipeline},
    Engine,
};
use std::{io, path::Path};

fn collect(pipeline: &mut ThreadedPipeline, samples: &mut Vec<f32>) -> Result<()> {
    while let Some(event) = pipeline.try_output()? {
        match event {
            Event::Audio(audio) => samples.extend(audio),
            Event::Finished => eyre::bail!("unexpected end of live pipeline"),
        }
        ensure!(
            samples.len() <= protocol::MAX_OUTPUT_SAMPLES / 3,
            "native output queue overflow"
        );
    }
    Ok(())
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    ensure!(
        args.len() == 3,
        "usage: streamr-native MODEL_DIRECTORY cpu|vulkan"
    );
    let root = Path::new(&args[1]);
    let (engine, name) = match args[2].to_str() {
        Some("cpu") => (Engine::Cpu, "cpu"),
        #[cfg(feature = "vulkan")]
        Some("vulkan") => (Engine::Vulkan, "vulkan"),
        _ => eyre::bail!("unsupported inference backend"),
    };
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build_global()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let (kind, body) = protocol::read(&mut input, protocol::ENROLL_BYTES)?
        .ok_or_else(|| eyre::eyre!("missing enrollment"))?;
    ensure!(
        kind == 0 && body.len() == protocol::ENROLL_BYTES,
        "expected three-second 48kHz enrollment"
    );
    let enrollment = protocol::samples(&body)?;
    let mut pipeline = ThreadedPipeline::load_warmed(root, &enrollment, engine)?;
    let mut upsampler = upsampler::Upsampler::load(&root.join("resampler.f64"))?;
    protocol::write(&mut output, 0, name.as_bytes())?;
    while let Some((kind, body)) = protocol::read(&mut input, protocol::INPUT_BYTES)? {
        if kind == 2 {
            ensure!(body.is_empty(), "stop frame must be empty");
            break;
        }
        ensure!(
            kind == 1 && body.len() == protocol::INPUT_BYTES,
            "expected 960 float32 samples"
        );
        let samples = protocol::samples(&body)?;
        let mut processed = Vec::new();
        collect(&mut pipeline, &mut processed)?;
        ensure!(
            pipeline.try_push(&samples)?,
            "Streamr processing queue cannot keep up"
        );
        collect(&mut pipeline, &mut processed)?;
        let audio = upsampler.process(&processed);
        ensure!(
            audio.len() <= protocol::MAX_OUTPUT_SAMPLES && audio.iter().all(|v| v.is_finite()),
            "invalid native output"
        );
        let bytes: Vec<_> = audio.iter().flat_map(|x| x.to_le_bytes()).collect();
        protocol::write(&mut output, 1, &bytes)?;
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        let mut message = format!("{error:#}");
        if message.len() > 4096 {
            let mut end = 4096;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        let _ = protocol::write(&mut io::stdout().lock(), 2, message.as_bytes());
        eprintln!("Streamr: {message}");
        std::process::exit(1);
    }
}
