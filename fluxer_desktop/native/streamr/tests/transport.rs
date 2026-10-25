// SPDX-License-Identifier: AGPL-3.0-or-later
#[path = "../src/protocol.rs"]
mod protocol;
#[path = "../src/upsampler.rs"]
mod upsampler;
use std::io::{Cursor, Read};

#[test]
fn framed_stream_handles_short_reads_eof_and_truncation() {
    struct Fragmented(Cursor<Vec<u8>>);
    impl Read for Fragmented {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let size = buffer.len().min(1);
            self.0.read(&mut buffer[..size])
        }
    }
    let mut bytes = Vec::new();
    protocol::write(&mut bytes, 1, &1.25f32.to_le_bytes()).unwrap();
    protocol::write(&mut bytes, 2, &[]).unwrap();
    let mut reader = Fragmented(Cursor::new(bytes.clone()));
    let (kind, body) = protocol::read(&mut reader, 4).unwrap().unwrap();
    assert_eq!(kind, 1);
    assert_eq!(protocol::samples(&body).unwrap(), [1.25]);
    assert_eq!(protocol::read(&mut reader, 4).unwrap(), Some((2, vec![])));
    assert_eq!(protocol::read(&mut reader, 4).unwrap(), None);
    for length in 1..12 {
        assert!(protocol::read(&mut Cursor::new(&bytes[..length]), 4).is_err());
    }
    assert!(protocol::read(&mut Cursor::new(&bytes), 3).is_err());
    assert!(protocol::samples(&[0, 0, 0]).is_err());
    assert!(protocol::samples(&f32::NAN.to_le_bytes()).is_err());
    assert!(protocol::samples(&f32::INFINITY.to_le_bytes()).is_err());
}

#[test]
fn polyphase_resampling_matches_zero_insertion_convolution_and_partitioning() {
    let root = std::env::var_os("STREAMR_MODELS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../streamr-models")
        });
    let file = root.join("resampler.f64");
    let bytes = std::fs::read(&file).unwrap();
    let taps: Vec<f64> = bytes
        .chunks_exact(8)
        .map(|x| f64::from_le_bytes(x.try_into().unwrap()))
        .collect();
    let input: Vec<f32> = (0..1600)
        .map(|i| ((i as f32 * 0.137).sin() + (i as f32 * 0.571).cos()) * 0.2)
        .collect();
    let mut expanded = vec![0.0f64; input.len() * 3];
    for (i, x) in input.iter().enumerate() {
        expanded[3 * i] = f64::from(*x);
    }
    let expected: Vec<f32> = (192..expanded.len())
        .map(|i| {
            (3.0 * (0..=i.min(taps.len() - 1))
                .map(|k| expanded[i - k] * taps[k])
                .sum::<f64>()) as f32
        })
        .collect();
    let actual = upsampler::Upsampler::load(&file).unwrap().process(&input);
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(&expected) {
        assert!((a - b).abs() <= f32::EPSILON, "{a} != {b}");
    }
    let mut state = upsampler::Upsampler::load(&file).unwrap();
    let mut split = Vec::new();
    for chunk in input.chunks(37) {
        split.extend(state.process(chunk));
    }
    assert_eq!(actual, split);
    let dc = upsampler::Upsampler::load(&file)
        .unwrap()
        .process(&vec![1.0; 1000]);
    for value in &dc[600..] {
        assert!((value - 1.0).abs() < 0.001, "DC gain {value}");
    }
}

#[test]
fn frontend_matches_centered_fir_decimation_and_preserves_stream_boundaries() {
    use reuse_rs::streamr::frontend::Frontend;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../streamr-models");
    let taps: Vec<f64> = std::fs::read(root.join("resampler.f64"))
        .unwrap()
        .chunks_exact(8)
        .map(|bytes| f64::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let center = taps.len() / 2;
    for length in [0, 1, 2, 3, 191, 192, 193, 959, 960, 1921] {
        let input: Vec<f32> = (0..length).map(|i| (i as f32 * 0.137).sin()).collect();
        let expected: Vec<f32> = (0..length)
            .step_by(3)
            .map(|sample| {
                taps.iter()
                    .enumerate()
                    .map(|(tap, weight)| {
                        let offset = sample as isize + center as isize - tap as isize;
                        input
                            .get(offset as usize)
                            .copied()
                            .map(f64::from)
                            .unwrap_or(0.0)
                            * weight
                    })
                    .sum::<f64>() as f32
            })
            .collect();
        for partition in [1, 37, 960] {
            let mut frontend = Frontend::load(&root).unwrap();
            let mut actual = Vec::new();
            for chunk in input.chunks(partition) {
                actual.extend(frontend.process(chunk).unwrap());
            }
            actual.extend(frontend.flush().unwrap());
            assert_eq!(actual, expected);
            assert!(frontend.flush().is_err());
            assert!(frontend.process(&[]).is_err());
        }
    }
    let mut frontend = Frontend::load(&root).unwrap();
    assert!(frontend.process(&[f32::NAN]).is_err());
    assert!(frontend.process(&[f32::INFINITY]).is_err());
    assert!(frontend.flush().unwrap().is_empty());
}
