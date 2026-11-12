// SPDX-License-Identifier: AGPL-3.0-or-later
//! Protocol-driven probe for scripts/test_output_routing.py. Synthetic output
//! only: both selected sinks must carry the isolated test prefix. Never opens
//! an input or selects the default output.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use fluxer_system_audio::SpeakerOutput;
    use std::{
        io::{BufRead, Write},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    const PREFIX: &str = "output:pipewire:fluxer-output-test-";
    let a = std::env::var("FLUXER_OUTPUT_TEST_A")?;
    let b = std::env::var("FLUXER_OUTPUT_TEST_B")?;
    for id in [&a, &b] {
        if !id.starts_with(PREFIX) || id.len() <= PREFIX.len() {
            return Err("refusing output probe: explicit synthetic PipeWire sink required".into());
        }
    }
    if a == b {
        return Err("routing probe requires two distinct synthetic sinks".into());
    }
    let devices = SpeakerOutput::devices()?;
    if [&a, &b]
        .iter()
        .any(|id| !devices.iter().any(|d| &d.id == *id))
    {
        return Err("requested synthetic output was not found; refusing fallback".into());
    }

    let (error_tx, errors) = mpsc::channel();
    let rendered = Arc::new(AtomicUsize::new(0));
    let start = |id: &str| {
        let rendered = rendered.clone();
        let error_tx = error_tx.clone();
        let mut position = 0_u64;
        SpeakerOutput::start(
            Some(id),
            move |samples| {
                for stereo in samples.as_chunks_mut::<2>().0 {
                    let sample = ((position as f64 * std::f64::consts::TAU * 440. / 48_000.).sin()
                        * 0.2) as f32;
                    stereo.fill(sample);
                    position = (position + 1) % 48_000;
                }
                rendered.fetch_add(samples.len(), Ordering::Release);
            },
            move |error| {
                let _ = error_tx.send(error);
            },
        )
    };
    let phase = |name: &str| -> Result<(), Box<dyn std::error::Error>> {
        let before = rendered.load(Ordering::Acquire);
        let deadline = Instant::now() + Duration::from_secs(5);
        while rendered.load(Ordering::Acquire) == before {
            if errors.try_recv().is_ok() {
                return Err("synthetic output reported an error".into());
            }
            if Instant::now() >= deadline {
                return Err("synthetic output never requested rendered PCM".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("FLUXER_OUTPUT_PHASE {name}");
        std::io::stdout().flush()?;
        Ok(())
    };

    // Reader thread permits bounded waits even if an interrupted runner leaves
    // stdin open without sending the next command.
    let (commands_tx, commands) = mpsc::channel();
    std::thread::spawn(move || {
        for command in std::io::stdin().lock().lines() {
            if commands_tx.send(command).is_err() {
                break;
            }
        }
    });
    println!("FLUXER_OUTPUT_PROCESS {}", std::process::id());
    std::io::stdout().flush()?;
    let output = start(&a)?;
    phase("A")?;
    let mut output = Some(output);
    let mut expected = ["B", "STOP", "EXIT"].into_iter();
    loop {
        let command = commands.recv_timeout(Duration::from_secs(20))??;
        if errors.try_recv().is_ok() {
            return Err("synthetic output reported an error".into());
        }
        if Some(command.as_str()) != expected.next() {
            return Err("unexpected routing probe command".into());
        }
        match command.as_str() {
            "B" => {
                if let Some(previous) = output.take() {
                    previous.stop();
                }
                output = Some(start(&b)?);
                phase("B")?;
            }
            "STOP" => {
                if let Some(previous) = output.take() {
                    previous.stop();
                }
                let before = rendered.load(Ordering::Acquire);
                std::thread::sleep(Duration::from_millis(100));
                if rendered.load(Ordering::Acquire) != before {
                    return Err("output kept rendering after stop returned".into());
                }
                println!("FLUXER_OUTPUT_PHASE STOPPED");
                std::io::stdout().flush()?;
            }
            "EXIT" => break,
            _ => unreachable!(),
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The isolated PipeWire routing probe requires Linux.");
    std::process::exit(1);
}
