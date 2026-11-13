// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lazy crops of Fluxer's bundled Twemoji sheets; no font fallback dependency.
use crate::chat_helpers::EmojiSprite;
use gpui::RenderImage;
use std::{collections::HashMap, sync::Arc};

impl gpui::Global for SpriteCache {}

static STATIC_ENDPOINT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn configure_instance(document: Option<&serde_json::Value>) {
    *STATIC_ENDPOINT.lock().unwrap() = document
        .and_then(|document| document.pointer("/endpoints/static_cdn"))
        .and_then(serde_json::Value::as_str)
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        .map(|url| url.trim_end_matches('/').to_owned());
}

fn codepoints(text: &str) -> String {
    // EmojiCodepointUtils.ts retains FE0F in ZWJ sequences except eye/speech.
    let bare = text.replace('\u{fe0f}', "");
    let source = if text.contains('\u{200d}') && bare != "👁\u{200d}🗨" {
        text
    } else {
        &bare
    };
    source
        .chars()
        .map(|ch| format!("{:x}", ch as u32))
        .collect::<Vec<_>>()
        .join("-")
}

pub fn image_source(text: &str, high_dpi: bool, cx: &mut gpui::App) -> Option<gpui::ImageSource> {
    if let Some(sprite) = crate::chat_helpers::unicode_sprite(text) {
        if !cx.has_global::<SpriteCache>() {
            cx.set_global(SpriteCache::new());
        }
        let (image, retired) = cx.global_mut::<SpriteCache>().get(sprite, high_dpi);
        if let Some(retired) = retired {
            cx.defer(move |cx| cx.drop_image(retired, None));
        }
        return image.map(Into::into);
    }
    if !crate::chat_helpers::unicode_catalog_contains(text) {
        return None;
    }
    STATIC_ENDPOINT.lock().unwrap().as_ref().map(|endpoint| {
        gpui::SharedString::from(format!("{endpoint}/emoji/{}.svg?v=2", codepoints(text))).into()
    })
}

/// Shared by picker categories, reactions and other compact emoji labels.
pub fn unicode_art(
    text: &str,
    font_size: f32,
    high_dpi: bool,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    use gpui::{prelude::*, *};
    if let Some(source) = image_source(text, high_dpi, cx) {
        img(source)
            .flex_shrink_0()
            .size(px(font_size))
            .object_fit(ObjectFit::Contain)
            .into_any_element()
    } else if crate::chat_helpers::unicode_catalog_contains(text) {
        // Known art awaiting instance configuration must not change to OS art.
        div().flex_shrink_0().size(px(font_size)).into_any_element()
    } else {
        div()
            .text_size(px(font_size))
            .line_height(relative(1.))
            .child(text.to_owned())
            .into_any_element()
    }
}

/// Inline art for compact static labels that do not have a selectable input.
pub fn unicode_text(
    text: &str,
    font_size: f32,
    high_dpi: bool,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    use gpui::{prelude::*, *};
    use unicode_segmentation::UnicodeSegmentation;
    let mut children = Vec::new();
    let mut copied = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        if !crate::chat_helpers::unicode_catalog_contains(grapheme) {
            continue;
        }
        if start > copied {
            children.push(
                div()
                    .child(text[copied..start].to_owned())
                    .into_any_element(),
            );
        }
        children.push(unicode_art(grapheme, font_size, high_dpi, cx));
        copied = start + grapheme.len();
    }
    if copied < text.len() {
        children.push(div().child(text[copied..].to_owned()).into_any_element());
    }
    div()
        .flex()
        .items_center()
        .text_size(px(font_size))
        .children(children)
        .into_any_element()
}

const CAPACITY: usize = 256;
struct Entry {
    image: Arc<RenderImage>,
    touched: u64,
}
pub struct SpriteCache {
    sheets: [Option<image::RgbaImage>; 12],
    crops: HashMap<(EmojiSprite, bool), Entry>,
    clock: u64,
}
impl SpriteCache {
    pub fn new() -> Self {
        Self {
            sheets: std::array::from_fn(|_| None),
            crops: HashMap::new(),
            clock: 0,
        }
    }
    /// Return the crop and, on eviction, the old atlas image to retire in App.
    pub fn get(
        &mut self,
        sprite: EmojiSprite,
        high_dpi: bool,
    ) -> (Option<Arc<RenderImage>>, Option<Arc<RenderImage>>) {
        self.clock += 1;
        let key = (sprite, high_dpi);
        if let Some(entry) = self.crops.get_mut(&key) {
            entry.touched = self.clock;
            return (Some(entry.image.clone()), None);
        }
        let Some(bytes) = sheet_bytes(sprite.sheet, high_dpi) else {
            return (None, None);
        };
        let slot = &mut self.sheets[sprite.sheet as usize + if high_dpi { 6 } else { 0 }];
        if slot.is_none() {
            *slot = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
                .ok()
                .map(image::DynamicImage::into_rgba8);
        }
        let Some(sheet) = slot else {
            return (None, None);
        };
        let Some(crop) = crop_bgra(sheet, sprite, if high_dpi { 64 } else { 32 }) else {
            return (None, None);
        };
        let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            crop
        )]));
        self.crops.insert(
            key,
            Entry {
                image: image.clone(),
                touched: self.clock,
            },
        );
        let evicted = if self.crops.len() > CAPACITY {
            let oldest = self
                .crops
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| *key);
            oldest
                .and_then(|key| self.crops.remove(&key))
                .map(|entry| entry.image)
        } else {
            None
        };
        (Some(image), evicted)
    }
}
fn crop_bgra(sheet: &image::RgbaImage, sprite: EmojiSprite, cell: u32) -> Option<image::RgbaImage> {
    let columns = sprite.columns();
    if sprite.index >= columns * sprite.rows() {
        return None;
    }
    let x = (sprite.index % columns) * cell;
    let y = (sprite.index / columns) * cell;
    if x + cell > sheet.width() || y + cell > sheet.height() {
        return None;
    }
    let mut crop = image::imageops::crop_imm(sheet, x, y, cell, cell).to_image();
    // GPUI RenderImage's public image::Frame container stores BGRA pixels.
    let pixels: &mut [u8] = crop.as_mut();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(crop)
}
fn sheet_bytes(sheet: u8, high_dpi: bool) -> Option<&'static [u8]> {
    Some(match (sheet, high_dpi) {
        (0, true) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-emoji@2x.png"
        ),
        (1, true) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fb@2x.png"
        ),
        (2, true) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fc@2x.png"
        ),
        (3, true) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fd@2x.png"
        ),
        (4, true) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fe@2x.png"
        ),
        (5, true) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3ff@2x.png"
        ),
        (0, false) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-emoji.png"
        ),
        (1, false) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fb.png"
        ),
        (2, false) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fc.png"
        ),
        (3, false) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fd.png"
        ),
        (4, false) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3fe.png"
        ),
        (5, false) => include_bytes!(
            "../../../../fluxer_app/src/media/images/emoji-sprites/spritesheet-1f3ff.png"
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cdn_codepoints_match_source_variation_selector_rules() {
        assert_eq!(codepoints("☺️"), "263a");
        assert_eq!(codepoints("👁️‍🗨️"), "1f441-200d-1f5e8");
        assert_eq!(codepoints("👩‍⚕️"), "1f469-200d-2695-fe0f");
        assert_eq!(codepoints("🫱🏻‍🫲🏼"), "1faf1-1f3fb-200d-1faf2-1f3fc");
        assert!(crate::chat_helpers::unicode_catalog_contains("🫱🏻‍🫲🏼"));
        assert!(!crate::chat_helpers::unicode_catalog_contains("👨‍🦕"));
    }
    #[test]
    fn crop_preserves_asset_pixels_and_converts_rgba_to_gpui_bgra() {
        let sheet = image::load_from_memory(sheet_bytes(0, false).unwrap())
            .unwrap()
            .into_rgba8();
        let sprite = EmojiSprite {
            sheet: 0,
            index: 725,
        }; // source's banana cell (11,17)
        let crop = crop_bgra(&sheet, sprite, 32).unwrap();
        assert_eq!(crop.dimensions(), (32, 32));
        for (x, y) in [(0, 0), (10, 15), (20, 20), (31, 31)] {
            let [r, g, b, a] = sheet.get_pixel(11 * 32 + x, 17 * 32 + y).0;
            assert_eq!(crop.get_pixel(x, y).0, [b, g, r, a]);
        }
        assert!(crop.pixels().any(|pixel| pixel[3] > 0));
        assert!(
            crop_bgra(
                &sheet,
                EmojiSprite {
                    sheet: 0,
                    index: 100_000
                },
                32
            )
            .is_none()
        );
    }
    #[test]
    fn lazy_cache_is_bounded_reuses_images_and_returns_atlas_evictions() {
        let mut cache = SpriteCache::new();
        assert!(cache.sheets.iter().all(Option::is_none));
        let first = cache
            .get(EmojiSprite { sheet: 0, index: 0 }, false)
            .0
            .unwrap();
        let again = cache
            .get(EmojiSprite { sheet: 0, index: 0 }, false)
            .0
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert!(cache.sheets[0].is_some());
        assert!(cache.sheets[1..].iter().all(Option::is_none));
        let mut retired = Vec::new();
        for index in 1..=CAPACITY as u32 {
            if let Some(image) = cache.get(EmojiSprite { sheet: 0, index }, false).1 {
                retired.push(image);
            }
        }
        assert_eq!(cache.crops.len(), CAPACITY);
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].id, first.id);
        assert!(
            !cache
                .crops
                .contains_key(&(EmojiSprite { sheet: 0, index: 0 }, false))
        );
    }
}
