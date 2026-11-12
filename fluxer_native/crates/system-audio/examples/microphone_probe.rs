// SPDX-License-Identifier: AGPL-3.0-or-later
//! List input sources, or sample a named source's peak for two seconds locally.
//! No audio is stored, played or sent to a server.
use fluxer_system_audio::MicrophoneInput;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Inputs: {:?}", MicrophoneInput::devices()?);
    let Some(id) = std::env::args().nth(1) else {
        return Ok(());
    };
    let input = MicrophoneInput::start(Some(&id), 1)?;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut frames = 0;
    let mut peak = 0.0_f32;
    while std::time::Instant::now() < until {
        if let Some(frame) = input.read()? {
            frames += 1;
            peak = frame
                .samples
                .iter()
                .fold(peak, |peak, sample| peak.max(sample.abs()));
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    input.stop();
    println!("Frames: {frames}; peak: {peak:.4}");
    if frames == 0 {
        return Err("No microphone frames received".into());
    }
    Ok(())
}
