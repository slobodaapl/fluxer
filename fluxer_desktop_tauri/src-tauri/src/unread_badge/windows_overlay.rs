// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Windows taskbar overlay of fluxer_desktop/src/main/WindowsBadge.ts. `badge-N.ico`
//! (`badge-dark-N.ico` with the dark taskbar badge setting) shows counts 1 to 9, `badge-10` is
//! "9+" and `badge-11` the dot; the icons are the build channel's, embedded. Each icon holds PNG
//! frames, and the one picked is the smallest at least 16 px times the window's scale factor.
//!
//! tao's overlay call takes no description, so the overlay's accessible text
//! (`describeUnreadBadge`, else "No Notifications") is lost; the overlay is re-applied only when
//! the icon changes.

use std::collections::BTreeMap;

use crate::build_channel::BuildChannel;

const BADGE_COUNT: usize = 11;
/// `badge-11.ico`.
const DOT_INDEX: usize = 10;
const OVERLAY_BASE_SIZE: f64 = 16.0;
const ICON_HEADER_BYTES: usize = 6;
const ICON_DIRECTORY_ENTRY_BYTES: usize = 16;
const PNG_SIGNATURE: [u8; 4] = [0x89, 0x50, 0x4e, 0x47];

macro_rules! badge {
    ($channel:literal, $prefix:literal, $number:literal) => {
        include_bytes!(concat!(
            "../../../../fluxer_desktop/build_resources/icons-",
            $channel,
            "/badges/",
            $prefix,
            $number,
            ".ico"
        )) as &[u8]
    };
}

macro_rules! badges {
    ($channel:literal, $prefix:literal) => {
        [
            badge!($channel, $prefix, "1"),
            badge!($channel, $prefix, "2"),
            badge!($channel, $prefix, "3"),
            badge!($channel, $prefix, "4"),
            badge!($channel, $prefix, "5"),
            badge!($channel, $prefix, "6"),
            badge!($channel, $prefix, "7"),
            badge!($channel, $prefix, "8"),
            badge!($channel, $prefix, "9"),
            badge!($channel, $prefix, "10"),
            badge!($channel, $prefix, "11"),
        ]
    };
}

const BADGES: [&[u8]; BADGE_COUNT] = match BuildChannel::current() {
    BuildChannel::Stable => badges!("stable", "badge-"),
    BuildChannel::Canary => badges!("canary", "badge-"),
    BuildChannel::Development => badges!("development", "badge-"),
};
const DARK_BADGES: [&[u8]; BADGE_COUNT] = match BuildChannel::current() {
    BuildChannel::Stable => badges!("stable", "badge-dark-"),
    BuildChannel::Canary => badges!("canary", "badge-dark-"),
    BuildChannel::Development => badges!("development", "badge-dark-"),
};

/// `readIconPngFrames`: the PNG frames of an ICO file by size (a size byte of 0 is 256).
fn read_icon_png_frames(data: &[u8]) -> BTreeMap<u32, &[u8]> {
    let mut frames = BTreeMap::new();
    let u16_at = |offset: usize| u16::from_le_bytes([data[offset], data[offset + 1]]);
    let u32_at = |offset: usize| {
        u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize
    };
    if data.len() < ICON_HEADER_BYTES || u16_at(2) != 1 {
        return frames;
    }
    for index in 0..usize::from(u16_at(4)) {
        let entry = ICON_HEADER_BYTES + index * ICON_DIRECTORY_ENTRY_BYTES;
        if entry + ICON_DIRECTORY_ENTRY_BYTES > data.len() {
            break;
        }
        let size = match data[entry] {
            0 => 256,
            size => u32::from(size),
        };
        let (length, offset) = (u32_at(entry + 8), u32_at(entry + 12));
        if let Some(frame) = offset
            .checked_add(length)
            .and_then(|end| data.get(offset..end))
            && frame.starts_with(&PNG_SIGNATURE)
        {
            frames.insert(size, frame);
        }
    }
    frames
}

/// `pickBadgeFrameSize`.
fn pick_frame_size(sizes: impl Iterator<Item = u32>, scale_factor: f64) -> Option<u32> {
    let target = (OVERLAY_BASE_SIZE * scale_factor).round();
    let mut sorted: Vec<u32> = sizes.collect();
    sorted.sort_unstable();
    sorted
        .iter()
        .copied()
        .find(|size| f64::from(*size) >= target)
        .or_else(|| sorted.last().copied())
}

/// The icon of `getOverlayIconData`: none for no unread messages.
fn overlay_index(count: i64) -> Option<usize> {
    match count {
        super::UNREAD_DOT => Some(DOT_INDEX),
        ..=0 => None,
        count => Some(count.min(10) as usize - 1),
    }
}

#[cfg(windows)]
pub use self::apply::{Overlay, refresh, set};

#[cfg(windows)]
mod apply {
    use std::sync::{Arc, Mutex};

    use tauri::{AppHandle, Manager};

    use super::*;
    use crate::config::DesktopConfig;
    use crate::png_image;
    use crate::window::MAIN_WINDOW_LABEL;

    /// Which frame is on the taskbar: dark variant, icon index, frame size.
    type Applied = Option<(bool, usize, u32)>;

    #[derive(Default)]
    pub struct OverlayState {
        icon: Applied,
        /// `lastCount`.
        count: Option<i64>,
    }

    pub type Overlay = Mutex<OverlayState>;

    fn apply(app: &AppHandle, count: i64, force: bool) {
        let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
            return;
        };
        let dark = app
            .state::<Arc<DesktopConfig>>()
            .window_behavior()
            .dark_taskbar_badge;
        let scale_factor = window.scale_factor().unwrap_or(1.0);
        let frame = overlay_index(count).and_then(|index| {
            let frames = read_icon_png_frames(if dark { DARK_BADGES } else { BADGES }[index]);
            let size = pick_frame_size(frames.keys().copied(), scale_factor)?;
            Some(((dark, index, size), frames[&size]))
        });
        let overlay = app.state::<Overlay>();
        let mut state = overlay.lock().expect("taskbar overlay lock");
        let key = frame.map(|(key, _)| key);
        if force || state.icon != key {
            let image = frame.and_then(|(_, png)| match png_image::decode(png) {
                Ok(image) => Some(image),
                Err(error) => {
                    tracing::warn!(%error, "The taskbar badge frame does not decode");
                    None
                }
            });
            if let Err(error) = window.set_overlay_icon(image.as_ref().map(|image| image.as_image())) {
                tracing::warn!(%error, "Failed to set the taskbar overlay icon");
            }
            state.icon = key;
        }
        state.count = Some(count);
    }

    /// `setWindowsBadgeOverlay`.
    pub fn set(app: &AppHandle, count: i64) {
        apply(app, count, false);
    }

    /// `refreshWindowsBadgeOverlay`: re-applies the last count, if there was one.
    pub fn refresh(app: &AppHandle) {
        let last = app
            .state::<Overlay>()
            .lock()
            .expect("taskbar overlay lock")
            .count;
        if let Some(count) = last {
            apply(app, count, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::png_image::{self, RgbaImage};

    /// An ICO of PNG frames, with an extra entry that is not PNG data.
    fn icon_file(frames: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let entries = frames.len() + 1;
        let mut header = vec![0, 0, 1, 0];
        header.extend_from_slice(&(entries as u16).to_le_bytes());
        let mut body = Vec::new();
        let mut directory = Vec::new();
        let data_start = ICON_HEADER_BYTES + entries * ICON_DIRECTORY_ENTRY_BYTES;
        let not_png = vec![0x28, 0, 0, 0, 1, 2, 3, 4];
        for (size, data) in frames.iter().map(|(size, data)| (*size, data)).chain([(64, &not_png)]) {
            let mut entry = [0u8; ICON_DIRECTORY_ENTRY_BYTES];
            entry[0] = size;
            entry[8..12].copy_from_slice(&(data.len() as u32).to_le_bytes());
            entry[12..16].copy_from_slice(&((data_start + body.len()) as u32).to_le_bytes());
            directory.extend_from_slice(&entry);
            body.extend_from_slice(data);
        }
        [header, directory, body].concat()
    }

    fn png(size: u32) -> Vec<u8> {
        png_image::encode(&RgbaImage {
            width: size,
            height: size,
            pixels: vec![255; (size * size * 4) as usize],
        })
    }

    #[test]
    fn png_frames_are_read_by_size_and_other_frames_skipped() {
        let file = icon_file(&[(16, png(16)), (0, png(2)), (32, png(32))]);
        let frames = read_icon_png_frames(&file);
        assert_eq!(frames.keys().copied().collect::<Vec<_>>(), [16, 32, 256]);
        assert_eq!(png_image::decode(frames[&32]).unwrap().width, 32);
        // Truncated files and non-icon resources yield nothing.
        assert!(read_icon_png_frames(&file[..4]).is_empty());
        let mut cursor = file.clone();
        cursor[2] = 2;
        assert!(read_icon_png_frames(&cursor).is_empty());
    }

    #[test]
    fn the_frame_is_the_smallest_covering_the_scaled_size() {
        let sizes = [48, 16, 24, 20, 40, 32];
        assert_eq!(pick_frame_size(sizes.into_iter(), 1.0), Some(16));
        assert_eq!(pick_frame_size(sizes.into_iter(), 1.25), Some(20));
        assert_eq!(pick_frame_size(sizes.into_iter(), 1.75), Some(32));
        assert_eq!(pick_frame_size(sizes.into_iter(), 4.0), Some(48));
        assert_eq!(pick_frame_size(std::iter::empty(), 1.0), None);
    }

    #[test]
    fn counts_map_to_their_icon_with_nine_plus_and_the_dot() {
        assert_eq!(overlay_index(0), None);
        assert_eq!(overlay_index(1), Some(0));
        assert_eq!(overlay_index(9), Some(8));
        assert_eq!(overlay_index(10), Some(9));
        assert_eq!(overlay_index(250), Some(9));
        assert_eq!(overlay_index(super::super::UNREAD_DOT), Some(DOT_INDEX));
    }

    #[test]
    fn every_bundled_badge_has_png_frames() {
        for icon in BADGES.iter().chain(DARK_BADGES.iter()) {
            let frames = read_icon_png_frames(icon);
            assert!(!frames.is_empty());
            for png in frames.values() {
                png_image::decode(png).unwrap();
            }
        }
    }
}
