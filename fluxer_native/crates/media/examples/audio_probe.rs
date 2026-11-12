// SPDX-License-Identifier: AGPL-3.0-or-later
//! Enumerate devices; pass --meter explicitly to record a five-second local
//! microphone level test. No audio is published, stored, or played.
use fluxer_media::{MediaEngine, MediaEvent};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut meter = false;
    let mut input = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--meter" => meter = true,
            "--input" => input = Some(args.next().ok_or("--input requires a device ID")?),
            _ => return Err(format!("Unknown option: {arg}").into()),
        }
    }
    let mut media = MediaEngine::new()?;
    println!("Devices: {:?}", media.devices());
    if let Some(input) = input {
        media.set_input_device(&input)?;
        println!("Selected input: {input}");
    }
    if meter {
        let mut events = media.start_microphone_test()?;
        let meter = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(meter);
        let mut count = 0;
        loop {
            tokio::select! {
                _ = &mut meter => break,
                event = events.recv() => match event {
                    Some(MediaEvent::AudioLevels { input, .. }) => { println!("Microphone peak: {input:.3}"); count += 1; }
                    Some(MediaEvent::Error(message)) => return Err(message.into()),
                    None => break,
                    _ => {},
                }
            }
        }
        media.stop_microphone_test()?;
        if count == 0 {
            return Err("No microphone frames received".into());
        }
    }
    Ok(())
}
