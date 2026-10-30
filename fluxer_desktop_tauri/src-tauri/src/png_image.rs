// SPDX-License-Identifier: AGPL-3.0-or-later

//! Decoding PNGs to straight RGBA, the form `tauri::image::Image` takes: the bundled icons (tray
//! icons, Windows badge frames), which are all RGBA, and notification icons, which may be any
//! PNG. 16-bit channels are cut to 8 bits. Decoding stops at the png crate's default 64 MiB
//! buffer limit.

use tauri::image::Image;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// Straight (not premultiplied) RGBA, row-major.
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    pub fn as_image(&self) -> Image<'_> {
        Image::new(&self.pixels, self.width, self.height)
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Png(png::DecodingError),
    NotRgba(png::ColorType),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Png(error) => write!(formatter, "{error}"),
            Self::NotRgba(color) => write!(formatter, "expected an RGBA image, found {color:?}"),
        }
    }
}

pub fn decode(bytes: &[u8]) -> Result<RgbaImage, DecodeError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(DecodeError::Png)?;
    let mut pixels = vec![0; reader.output_buffer_size().unwrap_or_default()];
    let frame = reader.next_frame(&mut pixels).map_err(DecodeError::Png)?;
    if frame.color_type != png::ColorType::Rgba {
        return Err(DecodeError::NotRgba(frame.color_type));
    }
    pixels.truncate(frame.buffer_size());
    Ok(RgbaImage {
        width: frame.width,
        height: frame.height,
        pixels,
    })
}

/// Any PNG: grey, indexed and RGB images gain the channels RGBA needs.
pub fn decode_any(bytes: &[u8]) -> Result<RgbaImage, DecodeError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(DecodeError::Png)?;
    let mut pixels = vec![0; reader.output_buffer_size().unwrap_or_default()];
    let frame = reader.next_frame(&mut pixels).map_err(DecodeError::Png)?;
    pixels.truncate(frame.buffer_size());
    let pixels = match frame.color_type {
        png::ColorType::Rgba => pixels,
        png::ColorType::GrayscaleAlpha => pixels
            .chunks_exact(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        other => return Err(DecodeError::NotRgba(other)),
    };
    Ok(RgbaImage {
        width: frame.width,
        height: frame.height,
        pixels,
    })
}

#[cfg(any(test, windows))]
pub fn encode(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&image.pixels))
        .expect("a whole RGBA image encodes");
    bytes
}
