// SPDX-License-Identifier: AGPL-3.0-or-later
//! Run through `python3 crates/media/tests/file_output_runner.py` only.
#![cfg(target_os = "linux")]
use fluxer_media::{FilePlaybackOptions, FilePlaybackStatus, FilePlayer};
use std::{
    error::Error,
    f64::consts::TAU,
    fs::File,
    io::{BufRead, BufWriter, Write},
    thread,
    time::{Duration, Instant},
};
type TestResult = Result<(), Box<dyn Error>>;
fn phase(name: &str) -> TestResult {
    println!("FLUXER_OUTPUT_PHASE {name}");
    std::io::stdout().flush()?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::stdin().lock().read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    if rx.recv_timeout(Duration::from_secs(20))??.trim() != "NEXT" {
        return Err("monitor did not acknowledge phase".into());
    }
    Ok(())
}
fn wait(
    player: &FilePlayer,
    predicate: impl Fn(&fluxer_media::FilePlaybackSnapshot) -> bool,
) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = player.snapshot();
        if s.status == FilePlaybackStatus::Failed {
            return Err(format!("playback failed: {:?}", s.error).into());
        }
        if predicate(&s) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("playback timed out: {s:?}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn wave(path: &std::path::Path) -> TestResult {
    let frames = 48_000_u32 * 50;
    let bytes = frames * 4;
    let mut f = BufWriter::new(File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + bytes).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16_u32.to_le_bytes())?;
    f.write_all(&1_u16.to_le_bytes())?;
    f.write_all(&2_u16.to_le_bytes())?;
    f.write_all(&48_000_u32.to_le_bytes())?;
    f.write_all(&192_000_u32.to_le_bytes())?;
    f.write_all(&4_u16.to_le_bytes())?;
    f.write_all(&16_u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&bytes.to_le_bytes())?;
    for n in 0..frames {
        let gain = if n < 48_000 * 30 { 0.3 } else { 0.12 };
        let sample = ((n as f64 * TAU * 440.0 / 48_000.0).sin() * gain * 32767.0) as i16;
        f.write_all(&sample.to_le_bytes())?;
        f.write_all(&sample.to_le_bytes())?;
    }
    f.flush()?;
    Ok(())
}
#[test]
#[ignore = "requires isolated virtual sinks; run crates/media/tests/file_output_runner.py"]
fn file_output() -> TestResult {
    let a = std::env::var("FLUXER_OUTPUT_TEST_A")?;
    let b = std::env::var("FLUXER_OUTPUT_TEST_B")?;
    if a == b
        || [&a, &b]
            .iter()
            .any(|s| !s.starts_with("output:pipewire:fluxer-output-test-"))
    {
        return Err("refusing default or physical output".into());
    }
    let path = std::env::temp_dir().join(format!("fluxer-file-output-{}.wav", std::process::id()));
    wave(&path)?;
    let player = FilePlayer::open(
        path.clone(),
        FilePlaybackOptions {
            autoplay: true,
            output_device: Some(a),
            ..Default::default()
        },
    )?;
    println!("\nFLUXER_OUTPUT_PROCESS {}", std::process::id());
    std::io::stdout().flush()?;
    let result = (|| -> TestResult {
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Playing && s.position > 0.2
        })?;
        phase("A")?;
        player.set_volume(0.25);
        phase("GAIN")?;
        player.set_volume(1.0);
        player.set_muted(true);
        phase("MUTE")?;
        player.set_muted(false);
        player.pause();
        wait(&player, |s| s.status == FilePlaybackStatus::Paused)?;
        phase("PAUSE")?;
        player.seek(35.0);
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.position >= 35.0
        })?;
        player.play();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Playing && s.position >= 35.1
        })?;
        phase("SEEK")?;
        player.set_output_device(Some(b));
        thread::sleep(Duration::from_secs(2));
        phase("B")?;
        player.stop();
        thread::sleep(Duration::from_secs(1));
        phase("STOPPED")?;
        Ok(())
    })();
    drop(player);
    let _ = std::fs::remove_file(path);
    result
}
