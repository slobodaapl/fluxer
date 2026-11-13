// SPDX-License-Identifier: AGPL-3.0-or-later
//! Concealed media loads through the normal image cache, but only a separate,
//! strongly blurred first-frame asset can reach the concealed image element.
use gpui::{App, Asset, ImageCacheError, ImageSource, ImgResourceLoader, RenderImage, Resource};
use image::{
    ImageBuffer, Rgba,
    imageops::{self, FilterType},
};
use std::{future::Future, sync::Arc};

const DETAIL_EDGE: u32 = 24;
const DISPLAY_EDGE: u32 = 256;
const BLUR_SIGMA: f32 = 3.;

/// Use this only inside an actually visible preview's image factory. Its clear
/// source is deliberately fetched and cached, ready for immediate explicit
/// reveal; the custom source itself never returns that clear asset.
pub(super) fn blurred_source(url: String) -> ImageSource {
    // Preserve the exact original URL string as the normal Img resource key.
    // URL normalization here could create a second cache entry/HTTP request.
    let validated = crate::image_gallery::safe_image_url(&url).is_some();
    ImageSource::Custom(Arc::new(move |window, cx| {
        if !validated {
            return Some(Err(ImageCacheError::Asset(
                "Invalid concealed image URL".into(),
            )));
        }
        window.use_asset::<BlurredImageAsset>(&url, cx)
    }))
}

enum BlurredImageAsset {}

impl Asset for BlurredImageAsset {
    type Source = String;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        // ImgResourceLoader is exactly AssetLogger<ImageAssetLoader>, used by
        // ImageSource::Resource when no custom image cache is installed. This
        // shares its in-flight request and fully decoded GIF/WebP frame cache.
        let (clear, _) = cx.fetch_asset::<ImgResourceLoader>(&Resource::Uri(source.into()));
        let executor = cx.background_executor().clone();
        async move {
            let clear = clear.await?;
            executor
                .spawn(async move { blur_first_frame(&clear).map(Arc::new) })
                .await
        }
    }
}

fn scaled_dimensions(width: u32, height: u32, longest_edge: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || longest_edge == 0 {
        return None;
    }
    let scale = f64::from(longest_edge) / f64::from(width.max(height));
    Some((
        (f64::from(width) * scale).round().max(1.) as u32,
        (f64::from(height) * scale).round().max(1.) as u32,
    ))
}

fn blur_first_frame(clear: &RenderImage) -> Result<RenderImage, ImageCacheError> {
    let invalid = || ImageCacheError::Asset("Invalid concealed image pixels".into());
    // Check the first frame before size(0), which indexes and would panic on
    // an empty decoded frame sequence.
    let bytes = clear.as_bytes(0).ok_or_else(invalid)?;
    let dimensions = clear.size(0);
    let width = u32::from(dimensions.width);
    let height = u32::from(dimensions.height);
    let tiny =
        scaled_dimensions(width, height, DETAIL_EDGE.min(width.max(height))).ok_or_else(invalid)?;
    let display = scaled_dimensions(width, height, DISPLAY_EDGE).ok_or_else(invalid)?;

    // Borrow the full decoded frame: do not clone a potentially large image or
    // any later GIF frames. Although image's container is named Rgba, the bytes
    // remain GPUI BGRA throughout; resize/blur process channels independently.
    let first = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, bytes).ok_or_else(invalid)?;
    let reduced = imageops::resize(&first, tiny.0, tiny.1, FilterType::Triangle);
    let blurred = imageops::blur(&reduced, BLUR_SIGMA);
    let displayed = imageops::resize(&blurred, display.0, display.1, FilterType::Triangle);
    // A single frame guarantees concealed media never runs an animation clock.
    Ok(RenderImage::new(vec![image::Frame::new(displayed)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Frame, RgbaImage};

    #[test]
    fn concealed_asset_is_distinct_first_frame_only_and_preserves_bgra_channels() {
        let first = RgbaImage::from_pixel(80, 40, Rgba([11, 29, 203, 255]));
        let second = RgbaImage::from_pixel(80, 40, Rgba([199, 31, 7, 255]));
        let clear = RenderImage::new(vec![Frame::new(first), Frame::new(second)]);
        let concealed = blur_first_frame(&clear).unwrap();
        assert_ne!(concealed.id, clear.id);
        assert_eq!(concealed.frame_count(), 1);
        assert_eq!(clear.frame_count(), 2);
        assert_eq!(&clear.as_bytes(0).unwrap()[..4], &[11, 29, 203, 255]);
        assert_eq!(&clear.as_bytes(1).unwrap()[..4], &[199, 31, 7, 255]);
        assert!(concealed.as_bytes(0).unwrap().chunks_exact(4).all(|pixel| {
            pixel[0].abs_diff(11) <= 1
                && pixel[1].abs_diff(29) <= 1
                && pixel[2].abs_diff(203) <= 1
                && pixel[3] == 255
        }));
    }

    #[test]
    fn downsize_and_blur_remove_high_frequency_detail_instead_of_returning_clear_pixels() {
        let checker = RgbaImage::from_fn(512, 256, |x, y| {
            let value = if (x / 2 + y / 2) % 2 == 0 { 0 } else { 255 };
            Rgba([value, value, value, 255])
        });
        let clear = RenderImage::new(vec![Frame::new(checker)]);
        let concealed = blur_first_frame(&clear).unwrap();
        let bytes = concealed.as_bytes(0).unwrap();
        let min = bytes.chunks_exact(4).map(|pixel| pixel[0]).min().unwrap();
        let max = bytes.chunks_exact(4).map(|pixel| pixel[0]).max().unwrap();
        assert!(
            max - min < 20,
            "Original contrast255, blurredcontrast{}",
            max - min
        );
        assert!(min > 100 && max < 155);
        assert_eq!(
            (
                u32::from(concealed.size(0).width),
                u32::from(concealed.size(0).height)
            ),
            (256, 128)
        );
    }

    #[test]
    fn output_stays_bounded_and_preserves_landscape_and_portrait_aspects() {
        for (width, height, expected) in [
            (640, 360, (256, 144)),
            (360, 640, (144, 256)),
            (1, 5000, (1, 256)),
            (5000, 1, (256, 1)),
            (1, 1, (256, 256)),
        ] {
            let clear = RenderImage::new(vec![Frame::new(RgbaImage::from_pixel(
                width,
                height,
                Rgba([1, 2, 3, 255]),
            ))]);
            let concealed = blur_first_frame(&clear).unwrap();
            assert_eq!(
                (
                    u32::from(concealed.size(0).width),
                    u32::from(concealed.size(0).height)
                ),
                expected
            );
            assert_eq!(
                concealed.as_bytes(0).unwrap().len(),
                (expected.0 * expected.1 * 4) as usize
            );
        }
    }

    #[test]
    fn empty_frames_and_zero_dimensions_fail_without_indexing_or_division_panics() {
        let empty = RenderImage::new(Vec::<Frame>::new());
        assert!(blur_first_frame(&empty).is_err());
        let zero = RenderImage::new(vec![Frame::new(RgbaImage::new(0, 0))]);
        assert!(blur_first_frame(&zero).is_err());
        assert_eq!(scaled_dimensions(0, 10, DETAIL_EDGE), None);
        assert_eq!(scaled_dimensions(10, 0, DETAIL_EDGE), None);
        assert_eq!(scaled_dimensions(10, 10, 0), None);
        assert_eq!(scaled_dimensions(u32::MAX, 1, DETAIL_EDGE), Some((24, 1)));
    }
}
