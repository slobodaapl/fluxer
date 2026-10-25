// SPDX-License-Identifier: AGPL-3.0-or-later
use eyre::{ensure, Result};
use std::io::{Read, Write};

pub const ENROLL_BYTES: usize = 144000 * 4;
pub const INPUT_BYTES: usize = 960 * 4;
pub const MAX_OUTPUT_SAMPLES: usize = 9600;

pub fn read(reader: &mut impl Read, limit: usize) -> Result<Option<(u32, Vec<u8>)>> {
    let mut header = [0u8; 8];
    if reader.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..])?;
    let kind = u32::from_le_bytes(header[..4].try_into().unwrap());
    let size = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
    ensure!(size <= limit, "incoming frame exceeds {limit} bytes");
    let mut body = vec![0; size];
    reader.read_exact(&mut body)?;
    Ok(Some((kind, body)))
}

pub fn write(writer: &mut impl Write, kind: u32, body: &[u8]) -> Result<()> {
    writer.write_all(&kind.to_le_bytes())?;
    writer.write_all(&u32::try_from(body.len())?.to_le_bytes())?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

pub fn samples(bytes: &[u8]) -> Result<Vec<f32>> {
    ensure!(bytes.len() % 4 == 0, "unaligned float32 payload");
    let values: Vec<_> = bytes
        .chunks_exact(4)
        .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
        .collect();
    ensure!(
        values.iter().all(|x| x.is_finite()),
        "nonfinite audio input"
    );
    Ok(values)
}
