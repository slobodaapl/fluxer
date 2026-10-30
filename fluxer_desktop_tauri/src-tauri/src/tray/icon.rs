// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tray image: the base icon of `createTrayIcon` (DesktopTray.ts) and its composition in
//! fluxer_desktop/src/main/TrayIconImage.ts. Monochrome appearances recolour the icon body and
//! cut its bright glyph out; unread messages add a dot in the top-right corner with a
//! transparent gap around it; on Linux a monochrome icon is cropped to its glyph and given the
//! margins of a 22/16 canvas, at most 256 px.
//!
//! Electron composed the premultiplied bitmap Chromium decoded; the math here runs on the same
//! premultiplied values, in RGBA order with the blue and red coefficients swapped, and the
//! result is un-premultiplied for Tauri as Skia does. Each base icon has one representation:
//! Electron's 16 px macOS template with its `@2x` representation becomes the 32 px `@2x` file.
//! The base icons are the build channel's, embedded; the development 256 px icon has 16-bit
//! channels, which decode to their high bytes.

use crate::build_channel::BuildChannel;
use crate::png_image::RgbaImage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Appearance {
    Colored,
    Template,
    White,
    Black,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Windows,
    MacOs,
}

impl Platform {
    pub const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };
}

type Rgb = [u8; 3];

const DOT_RADIUS_RATIO: f64 = 0.22;
const DOT_GAP_RATIO: f64 = 0.0625;
const SUBSAMPLES: u32 = 4;
const MIN_GLYPH_CONTRAST: f64 = 32.0;
const GLYPH_CUTOUT_START: f64 = 0.1;
const GLYPH_CUTOUT_END: f64 = 0.5;
const LINUX_MONOCHROME_CANVAS_RATIO: f64 = 22.0 / 16.0;
const LINUX_MAX_PIXMAP_SIZE: usize = 256;
/// `DOT_BGR` `[0x00, 0x00, 0xdd]`.
const DOT_RGB: Rgb = [0xdd, 0x00, 0x00];
const TEMPLATE_DOT_RGB: Rgb = [0x00, 0x00, 0x00];
const WHITE_GLYPH_RGB: Rgb = [0xff, 0xff, 0xff];
const BLACK_GLYPH_RGB: Rgb = [0x1c, 0x1c, 0x1c];

macro_rules! channel_icon {
    ($file:literal) => {
        match BuildChannel::current() {
            BuildChannel::Stable => include_bytes!(concat!(
                "../../../../fluxer_desktop/build_resources/icons-stable/",
                $file
            )) as &[u8],
            BuildChannel::Canary => include_bytes!(concat!(
                "../../../../fluxer_desktop/build_resources/icons-canary/",
                $file
            )) as &[u8],
            BuildChannel::Development => include_bytes!(concat!(
                "../../../../fluxer_desktop/build_resources/icons-development/",
                $file
            )) as &[u8],
        }
    };
}

/// The first candidate of `createTrayIcon` for this platform, which every build ships.
#[cfg(target_os = "linux")]
pub const BASE_ICON: &[u8] = channel_icon!("256x256.png");
#[cfg(windows)]
pub const BASE_ICON: &[u8] = channel_icon!("16x16.png");
#[cfg(target_os = "macos")]
pub const BASE_ICON: &[u8] = channel_icon!("FluxerTrayTemplate@2x.png");

/// `resolveTrayIconAppearance`.
pub fn appearance(platform: Platform, colored: bool, dark_system_ui: bool) -> Appearance {
    match platform {
        Platform::MacOs => Appearance::Template,
        _ if colored => Appearance::Colored,
        Platform::Windows if !dark_system_ui => Appearance::Black,
        _ => Appearance::White,
    }
}

/// A premultiplied RGBA bitmap, the form `toBitmap()` handed TrayIconImage.ts.
struct Bitmap {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

/// Skia's `SkMulDiv255Round`.
fn mul_div_255_round(value: u8, alpha: u8) -> u8 {
    let product = u32::from(value) * u32::from(alpha) + 128;
    ((product + (product >> 8)) >> 8) as u8
}

fn premultiply(image: &RgbaImage) -> Bitmap {
    let mut pixels = image.pixels.clone();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3];
        for channel in &mut pixel[..3] {
            *channel = mul_div_255_round(*channel, alpha);
        }
    }
    Bitmap {
        width: image.width as usize,
        height: image.height as usize,
        pixels,
    }
}

/// Skia's `SkUnPreMultiply`: the fixed-point scale `(255 << 24 + a / 2) / a`, rounded.
fn unpremultiply(bitmap: Bitmap) -> RgbaImage {
    let mut pixels = bitmap.pixels;
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            continue;
        }
        let scale = ((255u32 << 24) + alpha / 2) / alpha;
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * scale + (1 << 23)) >> 24).min(255) as u8;
        }
    }
    RgbaImage {
        width: bitmap.width as u32,
        height: bitmap.height as u32,
        pixels,
    }
}

fn coverage(x: f64, y: f64, center_x: f64, center_y: f64, radius: f64) -> f64 {
    let mut hits = 0;
    for sy in 0..SUBSAMPLES {
        let dy = y + (f64::from(sy) + 0.5) / f64::from(SUBSAMPLES) - center_y;
        for sx in 0..SUBSAMPLES {
            let dx = x + (f64::from(sx) + 0.5) / f64::from(SUBSAMPLES) - center_x;
            if dx * dx + dy * dy <= radius * radius {
                hits += 1;
            }
        }
    }
    f64::from(hits) / f64::from(SUBSAMPLES * SUBSAMPLES)
}

/// Un-premultiplied luminance; `offset` is a pixel's first byte.
fn luminance(pixels: &[u8], offset: usize) -> f64 {
    let alpha = pixels[offset + 3];
    if alpha == 0 {
        return 0.0;
    }
    (0.0722 * f64::from(pixels[offset + 2])
        + 0.7152 * f64::from(pixels[offset + 1])
        + 0.2126 * f64::from(pixels[offset]))
        * (255.0 / f64::from(alpha))
}

/// The median luminance level of the opaque pixels.
fn body_luminance(pixels: &[u8]) -> Option<f64> {
    let mut histogram = [0u32; 256];
    let mut opaque = 0u32;
    for offset in (0..pixels.len()).step_by(4) {
        if pixels[offset + 3] != 255 {
            continue;
        }
        histogram[luminance(pixels, offset).round().min(255.0) as usize] += 1;
        opaque += 1;
    }
    let mut seen = 0;
    for (level, count) in histogram.iter().enumerate() {
        seen += count;
        if seen * 2 >= opaque && opaque > 0 {
            return Some(level as f64);
        }
    }
    None
}

fn paint_monochrome_glyph(pixels: &mut [u8], glyph: Rgb) {
    let body = body_luminance(pixels);
    let mut highlight = 0.0f64;
    for offset in (0..pixels.len()).step_by(4) {
        if pixels[offset + 3] >= 128 {
            highlight = highlight.max(luminance(pixels, offset));
        }
    }
    let contrast = body.map_or(0.0, |body| highlight - body);
    for offset in (0..pixels.len()).step_by(4) {
        let mut alpha = pixels[offset + 3];
        if let Some(body) = body
            && contrast >= MIN_GLYPH_CONTRAST
        {
            let position = (luminance(pixels, offset) - body) / contrast;
            let cutout = ((position - GLYPH_CUTOUT_START) / (GLYPH_CUTOUT_END - GLYPH_CUTOUT_START))
                .clamp(0.0, 1.0);
            alpha = (f64::from(alpha) * (1.0 - cutout)).round() as u8;
        }
        for channel in 0..3 {
            pixels[offset + channel] =
                (f64::from(glyph[channel]) * f64::from(alpha) / 255.0).round() as u8;
        }
        pixels[offset + 3] = alpha;
    }
}

fn paint_indicator_dot(bitmap: &mut Bitmap, dot: Rgb) {
    let (width, height) = (bitmap.width as f64, bitmap.height as f64);
    let size = width.min(height);
    let radius = size * DOT_RADIUS_RATIO;
    let cut_radius = radius + size * DOT_GAP_RATIO;
    let center_x = width - radius;
    let center_y = radius;
    let min_x = (center_x - cut_radius).floor().max(0.0) as usize;
    let max_y = height.min((center_y + cut_radius).ceil()) as usize;
    for y in 0..max_y {
        for x in min_x..bitmap.width {
            let cut = coverage(x as f64, y as f64, center_x, center_y, cut_radius);
            if cut == 0.0 {
                continue;
            }
            let covered = coverage(x as f64, y as f64, center_x, center_y, radius);
            let offset = (y * bitmap.width + x) * 4;
            let pixel = &mut bitmap.pixels[offset..offset + 4];
            for channel in 0..3 {
                pixel[channel] = (f64::from(pixel[channel]) * (1.0 - cut)
                    + f64::from(dot[channel]) * covered)
                    .round() as u8;
            }
            pixel[3] = (f64::from(pixel[3]) * (1.0 - cut) + 255.0 * covered).round() as u8;
        }
    }
}

/// `cropTrayGlyph`: the non-transparent bounds, centred in a square.
fn crop_glyph(bitmap: &Bitmap) -> Option<Bitmap> {
    let (mut left, mut top, mut right, mut bottom) = (bitmap.width, bitmap.height, 0, 0);
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if bitmap.pixels[(y * bitmap.width + x) * 4 + 3] == 0 {
                continue;
            }
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if right <= left || bottom <= top {
        return None;
    }
    let (glyph_width, glyph_height) = (right - left, bottom - top);
    let size = glyph_width.max(glyph_height);
    let mut pixels = vec![0; size * size * 4];
    let (offset_x, offset_y) = ((size - glyph_width) / 2, (size - glyph_height) / 2);
    for y in 0..glyph_height {
        let start = ((top + y) * bitmap.width + left) * 4;
        let target = ((offset_y + y) * size + offset_x) * 4;
        pixels[target..target + glyph_width * 4]
            .copy_from_slice(&bitmap.pixels[start..start + glyph_width * 4]);
    }
    Some(Bitmap {
        width: size,
        height: size,
        pixels,
    })
}

/// `downscaleTrayBitmap`: an area-weighted box filter to `size` square.
fn downscale(bitmap: Bitmap, size: usize) -> Bitmap {
    if bitmap.width <= size {
        return bitmap;
    }
    let scale = bitmap.width as f64 / size as f64;
    let mut pixels = vec![0; size * size * 4];
    for y in 0..size {
        let top = y as f64 * scale;
        let bottom = top + scale;
        for x in 0..size {
            let left = x as f64 * scale;
            let right = left + scale;
            let mut sums = [0.0f64; 4];
            for sy in top.floor() as usize..bitmap.height.min(bottom.ceil() as usize) {
                let weight_y = bottom.min(sy as f64 + 1.0) - top.max(sy as f64);
                for sx in left.floor() as usize..bitmap.width.min(right.ceil() as usize) {
                    let weight = weight_y * (right.min(sx as f64 + 1.0) - left.max(sx as f64));
                    let offset = (sy * bitmap.width + sx) * 4;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum += f64::from(bitmap.pixels[offset + channel]) * weight;
                    }
                }
            }
            for (channel, sum) in sums.iter().enumerate() {
                pixels[(y * size + x) * 4 + channel] = (sum / (scale * scale)).round() as u8;
            }
        }
    }
    Bitmap {
        width: size,
        height: size,
        pixels,
    }
}

/// `addMonochromeTrayMargins`: centres the square `bitmap` in a `size` square.
fn add_margins(bitmap: Bitmap, size: usize) -> Bitmap {
    let inset = (size - bitmap.width) / 2;
    let mut pixels = vec![0; size * size * 4];
    let row = bitmap.width * 4;
    for y in 0..bitmap.height {
        let target = ((inset + y) * size + inset) * 4;
        pixels[target..target + row].copy_from_slice(&bitmap.pixels[y * row..(y + 1) * row]);
    }
    Bitmap {
        width: size,
        height: size,
        pixels,
    }
}

/// `renderTrayIcon` for a base icon of one representation at scale 1. A colored icon without
/// the dot is the base itself; a Linux monochrome icon without a visible pixel too.
pub fn render(
    base: &RgbaImage,
    appearance: Appearance,
    indicator: bool,
    platform: Platform,
) -> RgbaImage {
    let monochrome = matches!(appearance, Appearance::White | Appearance::Black);
    if !monochrome && !indicator {
        return base.clone();
    }
    let linux_monochrome = platform == Platform::Linux && monochrome;
    let premultiplied = premultiply(base);
    let Some(mut bitmap) = (if linux_monochrome {
        crop_glyph(&premultiplied)
    } else {
        Some(premultiplied)
    }) else {
        return base.clone();
    };
    let logical_canvas_size = (bitmap.width as f64 * LINUX_MONOCHROME_CANVAS_RATIO).ceil();
    match appearance {
        Appearance::White => paint_monochrome_glyph(&mut bitmap.pixels, WHITE_GLYPH_RGB),
        Appearance::Black => paint_monochrome_glyph(&mut bitmap.pixels, BLACK_GLYPH_RGB),
        Appearance::Colored | Appearance::Template => {}
    }
    if indicator {
        let dot = if appearance == Appearance::Template {
            TEMPLATE_DOT_RGB
        } else {
            DOT_RGB
        };
        paint_indicator_dot(&mut bitmap, dot);
    }
    if linux_monochrome {
        let canvas_size = (logical_canvas_size as usize).min(LINUX_MAX_PIXMAP_SIZE);
        let inner = (canvas_size as f64 / LINUX_MONOCHROME_CANVAS_RATIO).floor() as usize;
        bitmap = add_margins(downscale(bitmap, inner), canvas_size);
    }
    unpremultiply(bitmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> RgbaImage {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&pixel(x, y));
            }
        }
        RgbaImage {
            width,
            height,
            pixels,
        }
    }

    fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * image.width + x) * 4) as usize;
        image.pixels[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn appearance_follows_platform_setting_and_taskbar_theme() {
        assert_eq!(appearance(Platform::MacOs, true, false), Appearance::Template);
        assert_eq!(appearance(Platform::Linux, true, false), Appearance::Colored);
        assert_eq!(appearance(Platform::Linux, false, false), Appearance::White);
        assert_eq!(appearance(Platform::Windows, false, false), Appearance::Black);
        assert_eq!(appearance(Platform::Windows, false, true), Appearance::White);
    }

    #[test]
    fn a_colored_icon_without_unread_messages_is_the_base() {
        let base = image(3, 3, |x, y| [x as u8 * 50, y as u8 * 70, 9, 100]);
        assert_eq!(render(&base, Appearance::Colored, false, Platform::Linux), base);
    }

    /// 16 px: the dot has radius 3.52 at (12.48, 3.52), the gap reaches 4.52.
    #[test]
    fn the_dot_is_red_inside_a_transparent_ring_and_blends_at_its_edge() {
        let base = image(16, 16, |_, _| [255, 255, 255, 255]);
        let icon = render(&base, Appearance::Colored, true, Platform::Windows);
        assert_eq!((icon.width, icon.height), (16, 16));
        // Every sample of (12, 3) is inside the dot.
        assert_eq!(pixel(&icon, 12, 3), [0xdd, 0, 0, 255]);
        // Every sample of (12, 7) lies between the dot and the gap's edge.
        assert_eq!(pixel(&icon, 12, 7), [0, 0, 0, 0]);
        // (15, 0): 13 of 16 samples in the gap, none in the dot, so 3/16 of the white stays:
        // round(255 * 3/16) = 48 premultiplied, white again once un-premultiplied.
        assert_eq!(pixel(&icon, 15, 0), [255, 255, 255, 48]);
        // Far from the dot nothing changes.
        assert_eq!(pixel(&icon, 2, 12), [255, 255, 255, 255]);
    }

    #[test]
    fn the_template_dot_is_black() {
        let base = image(16, 16, |_, _| [0, 0, 0, 0]);
        let icon = render(&base, Appearance::Template, true, Platform::MacOs);
        assert_eq!(pixel(&icon, 12, 3), [0, 0, 0, 255]);
        assert_eq!(pixel(&icon, 2, 12), [0, 0, 0, 0]);
    }

    /// Opaque luminances 40, 40, 80, 240: the body (median) is 40 and the contrast 200. Level 80
    /// sits at 0.2 of it, a quarter into the cutout ramp, keeping round(255 * 0.75) = 191.
    #[test]
    fn monochrome_fills_the_body_and_cuts_the_bright_glyph_out() {
        let grey = |level: u8| [level, level, level, 255];
        let levels = [grey(40), grey(40), grey(80), grey(240), [0, 0, 0, 0]];
        let base = image(5, 1, |x, _| levels[x as usize]);
        let white = render(&base, Appearance::White, false, Platform::Windows);
        assert_eq!(pixel(&white, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&white, 1, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&white, 2, 0), [255, 255, 255, 191]);
        assert_eq!(pixel(&white, 3, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&white, 4, 0), [0, 0, 0, 0]);
        // Black glyph 0x1c: round(28 * 191 / 255) = 21 premultiplied, 28 again straight.
        let black = render(&base, Appearance::Black, false, Platform::Windows);
        assert_eq!(pixel(&black, 0, 0), [0x1c, 0x1c, 0x1c, 255]);
        assert_eq!(pixel(&black, 2, 0), [0x1c, 0x1c, 0x1c, 191]);
    }

    /// Red weighs 0.2126 and blue 0.0722: blue body at level 18 (18.41), red highlight 54.21,
    /// contrast 36.21 >= 32, so red is cut out. Swapped weights would make blue the highlight
    /// with no contrast left, and nothing would be cut.
    #[test]
    fn luminance_weighs_red_over_blue() {
        let colors = [[0, 0, 255, 255], [0, 0, 255, 255], [255, 0, 0, 255]];
        let base = image(3, 1, |x, _| colors[x as usize]);
        let white = render(&base, Appearance::White, false, Platform::Windows);
        assert_eq!(pixel(&white, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&white, 2, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn low_contrast_icons_are_filled_without_a_cutout() {
        let levels = [[40, 40, 40, 255], [60, 60, 60, 255], [0, 0, 0, 0]];
        let base = image(3, 1, |x, _| levels[x as usize]);
        let white = render(&base, Appearance::White, false, Platform::Windows);
        assert_eq!(pixel(&white, 1, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&white, 2, 0), [0, 0, 0, 0]);
    }

    /// A 4x2 glyph in a 6x4 icon: cropped to a 4 px square (glyph rows 1 and 2), the canvas is
    /// ceil(4 * 22/16) = 6 with an inner floor(6 / 1.375) = 4, so no downscale and a 1 px inset.
    #[test]
    fn linux_monochrome_icons_are_cropped_and_given_margins() {
        let base = image(6, 4, |x, y| {
            if (1..5).contains(&x) && (1..3).contains(&y) {
                [10, 10, 10, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let icon = render(&base, Appearance::White, false, Platform::Linux);
        assert_eq!((icon.width, icon.height), (6, 6));
        for y in 0..6 {
            for x in 0..6 {
                let inside = (1..5).contains(&x) && (2..4).contains(&y);
                let expected = if inside { [255, 255, 255, 255] } else { [0, 0, 0, 0] };
                assert_eq!(pixel(&icon, x, y), expected, "({x}, {y})");
            }
        }
        // Elsewhere the monochrome icon keeps its size and place.
        let windows = render(&base, Appearance::White, false, Platform::Windows);
        assert_eq!((windows.width, windows.height), (6, 4));
        assert_eq!(pixel(&windows, 1, 1), [255, 255, 255, 255]);
    }

    /// A full 256 px glyph: canvas min(352, 256) = 256, inner floor(256 / 1.375) = 186, inset 35.
    #[test]
    fn a_full_size_linux_glyph_is_downscaled_inside_the_256_px_cap() {
        let base = image(256, 256, |_, _| [30, 30, 30, 255]);
        let icon = render(&base, Appearance::White, false, Platform::Linux);
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(pixel(&icon, 34, 128)[3], 0);
        assert_eq!(pixel(&icon, 35, 128), [255, 255, 255, 255]);
        assert_eq!(pixel(&icon, 35 + 185, 35 + 185), [255, 255, 255, 255]);
        assert_eq!(pixel(&icon, 35 + 186, 128)[3], 0);
    }

    #[test]
    fn the_box_filter_averages_premultiplied_pixels() {
        // 4 px to 2 px: each output averages a 2x2 block.
        let pixels = [0u8, 0, 0, 0, 200, 100, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0];
        let row = |values: &[u8]| values.iter().copied().cycle().take(16).collect::<Vec<_>>();
        let bitmap = Bitmap {
            width: 4,
            height: 4,
            pixels: [row(&pixels), row(&pixels), row(&pixels), row(&pixels)].concat(),
        };
        let small = downscale(bitmap, 2);
        assert_eq!(&small.pixels[..4], &[100, 50, 0, 128]);
    }

    #[test]
    fn the_bundled_base_icon_renders_every_appearance() {
        let base = crate::png_image::decode(BASE_ICON).unwrap();
        for appearance in [Appearance::Colored, Appearance::White, Appearance::Black] {
            for indicator in [false, true] {
                let icon = render(&base, appearance, indicator, Platform::CURRENT);
                assert!(icon.pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
            }
        }
    }
}
