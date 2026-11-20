// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded still-image preparation and square crop for profile avatars.
//!
//! The caller should run `load` off the UI thread. The returned preview is
//! derived from the exact same pixels as the encoded upload.
use gpui::RenderImage;
use image::{ImageDecoder, ImageEncoder, ImageFormat, RgbaImage};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
};

const MAX_FILE: u64 = 10 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const OUTPUT_SIZE: u32 = 512;

/// Zoom is relative to the largest centered square. Pan ranges from -1 to 1
/// and selects the portion of the remaining image on each axis.
#[derive(Clone, Copy, Debug)]
pub(super) struct Crop {
    pub zoom: f32,
    pub x: f32,
    pub y: f32,
}
impl Default for Crop {
    fn default() -> Self {
        Self {
            zoom: 1.,
            x: 0.,
            y: 0.,
        }
    }
}

pub(super) struct AvatarImage {
    pixels: RgbaImage,
}
pub(super) struct CroppedAvatar {
    pub preview: Arc<RenderImage>,
    pub png: Vec<u8>,
}

impl AvatarImage {
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Could not open image: {e}"))?;
        let meta = file
            .metadata()
            .map_err(|e| format!("Could not read image: {e}"))?;
        if !meta.is_file() {
            return Err("Choose a regular image file".into());
        }
        if meta.len() > MAX_FILE {
            return Err("Image exceeds 10 MB".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Could not read image: {e}"))?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("Image exceeds 10 MB".into());
        }
        Self::decode(&bytes)
    }
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let format = image::guess_format(bytes).map_err(|_| "Choose a PNG or JPEG image")?;
        if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
            return Err(
                "Choose a static PNG or JPEG image; animated images are not supported".into(),
            );
        }
        if format == ImageFormat::Png && png_is_animated(bytes)? {
            return Err("Animated images are not supported in this editor".into());
        }
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_DIMENSION);
        limits.max_image_height = Some(MAX_DIMENSION);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let mut decoder = reader
            .into_decoder()
            .map_err(|e| format!("Could not decode image: {e}"))?;
        let (w, h) = decoder.dimensions();
        if w == 0
            || h == 0
            || u64::from(w) * u64::from(h) > MAX_PIXELS
            || decoder.total_bytes() > 128 * 1024 * 1024
        {
            return Err("Image dimensions are too large".into());
        }
        let orientation = decoder
            .orientation()
            .map_err(|e| format!("Could not read image orientation: {e}"))?;
        let mut image = image::DynamicImage::from_decoder(decoder)
            .map_err(|e| format!("Could not decode image: {e}"))?;
        image.apply_orientation(orientation);
        Ok(Self {
            pixels: image.into_rgba8(),
        })
    }
    pub fn crop(&self, crop: Crop) -> Result<CroppedAvatar, String> {
        let finite = |value: f32, fallback: f32| if value.is_finite() { value } else { fallback };
        let zoom = finite(crop.zoom, 1.).clamp(1., 5.);
        let x = finite(crop.x, 0.).clamp(-1., 1.);
        let y = finite(crop.y, 0.).clamp(-1., 1.);
        let (width, height) = self.pixels.dimensions();
        let side = ((width.min(height) as f32 / zoom).round() as u32).clamp(1, width.min(height));
        let left = (((width - side) as f32 * (x + 1.) / 2.).round() as u32).min(width - side);
        let top = (((height - side) as f32 * (y + 1.) / 2.).round() as u32).min(height - side);
        let pixels = image::imageops::crop_imm(&self.pixels, left, top, side, side).to_image();
        let pixels = image::imageops::resize(
            &pixels,
            OUTPUT_SIZE,
            OUTPUT_SIZE,
            image::imageops::FilterType::Lanczos3,
        );
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                pixels.as_raw(),
                OUTPUT_SIZE,
                OUTPUT_SIZE,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("Could not prepare avatar: {e}"))?;
        // GPUI render images use BGRA, while the encoded PNG remains RGBA.
        let mut preview_pixels = pixels;
        for pixel in preview_pixels.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        let preview = Arc::new(RenderImage::new(vec![image::Frame::new(preview_pixels)]));
        Ok(CroppedAvatar { preview, png })
    }
}

// APNG declares animation before IDAT; validate chunk boundaries before reading.
fn png_is_animated(bytes: &[u8]) -> Result<bool, String> {
    let mut offset = 8usize;
    while let Some(header) = bytes.get(offset..offset.saturating_add(8)) {
        let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(12)
            .and_then(|v| v.checked_add(len))
            .ok_or("Invalid PNG")?;
        if end > bytes.len() {
            return Err("Invalid PNG".into());
        }
        match &header[4..8] {
            b"acTL" => return Ok(true),
            b"IDAT" | b"IEND" => return Ok(false),
            _ => offset = end,
        }
    }
    Err("Invalid PNG".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crop_matches_pan_and_zoom_and_encodes_preview_pixels() {
        let pixels = RgbaImage::from_fn(8, 4, |x, y| {
            image::Rgba([x as u8 * 20, y as u8 * 20, 0, 255])
        });
        let source = AvatarImage { pixels };
        let result = source
            .crop(Crop {
                zoom: 2.,
                x: 1.,
                y: -1.,
            })
            .unwrap();
        let decoded = image::load_from_memory(&result.png).unwrap().into_rgba8();
        assert_eq!(decoded.dimensions(), (512, 512));
        assert!((110..=125).contains(&decoded.get_pixel(0, 0).0[0]));
        assert!((135..=150).contains(&decoded.get_pixel(511, 511).0[0]));
        assert!(decoded.get_pixel(511, 511).0[1] > decoded.get_pixel(0, 0).0[1]);
    }
    #[test]
    fn rejects_animation_and_invalid_png_chunks() {
        let mut apng = b"\x89PNG\r\n\x1a\n".to_vec();
        apng.extend_from_slice(&0u32.to_be_bytes());
        apng.extend_from_slice(b"acTL\0\0\0\0");
        assert!(
            AvatarImage::decode(&apng)
                .err()
                .unwrap()
                .contains("Animated")
        );
        assert!(png_is_animated(b"\x89PNG\r\n\x1a\n\xff\xff\xff\xffacTL").is_err());
        assert!(AvatarImage::decode(b"GIF89a").is_err());
    }
}
