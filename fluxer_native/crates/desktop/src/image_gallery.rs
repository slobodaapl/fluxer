// SPDX-License-Identifier: AGPL-3.0-or-later
//! A message-scoped image selection. This model never loads or prefetches an
//! image; only the modal's currently displayed item becomes an image element.
use gpui::http_client::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageItem {
    pub context: Option<crate::media_actions::MediaReference>,
    pub source: String,
    pub original: Option<String>,
    pub name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub animated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageGallery {
    pub items: Vec<ImageItem>,
    pub index: usize,
}

pub(super) fn safe_image_url(raw: &str) -> Option<String> {
    // URL parsing otherwise silently removes some control characters.
    if raw.chars().any(char::is_control) {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.into())
}

fn viewer_source(raw: &str, animated: bool) -> Option<String> {
    if animated {
        return crate::media_animation::animated_proxy_url(raw, true);
    }
    let mut url = Url::parse(&safe_image_url(raw)?).ok()?;
    // MediaViewerItemUtils.getBaseProxyURL / MediaProxyUtils.stripMediaProxyParams.
    // Keep other parameters, including signed storage URLs and asset size.
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| {
            !matches!(
                key.as_ref(),
                "width" | "height" | "format" | "quality" | "animated"
            )
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if pairs.len() != url.query_pairs().count() {
        url.set_query(None);
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
    }
    Some(url.into())
}

impl ImageGallery {
    /// Reject the entire invalid gallery rather than dropping an item and
    /// silently shifting the clicked index to an unrelated attachment.
    pub fn new(mut items: Vec<ImageItem>, index: usize) -> Option<Self> {
        if items.is_empty() || index >= items.len() {
            return None;
        }
        for item in &mut items {
            item.source = viewer_source(&item.source, item.animated)?;
            if let Some(original) = &mut item.original {
                *original = safe_image_url(original)?;
            }
            item.width = item.width.filter(|width| *width > 0);
            item.height = item.height.filter(|height| *height > 0);
        }
        Some(Self { items, index })
    }

    pub fn current(&self) -> Option<&ImageItem> {
        self.items.get(self.index)
    }

    /// MediaViewerModal.tsx389–394 wraps at either end. One-item galleries do
    /// not navigate, matching the source's hidden previous/next controls.
    pub fn next(&mut self) -> bool {
        if self.items.len() < 2 || self.current().is_none() {
            return false;
        }
        self.index = (self.index + 1) % self.items.len();
        true
    }

    pub fn previous(&mut self) -> bool {
        if self.items.len() < 2 || self.current().is_none() {
            return false;
        }
        self.index = if self.index == 0 {
            self.items.len() - 1
        } else {
            self.index - 1
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(name: &str) -> ImageItem {
        ImageItem {
            context: None,
            source: format!("https://proxy.example/{name}.png"),
            original: Some(format!("https://cdn.example/{name}.png?signature=x")),
            name: name.into(),
            width: Some(640),
            height: Some(480),
            animated: false,
        }
    }

    #[test]
    fn gallery_starts_at_clicked_image_and_wraps_in_both_directions() {
        let mut gallery =
            ImageGallery::new(vec![item("one"), item("two"), item("three")], 1).unwrap();
        assert_eq!(gallery.current().unwrap().name, "two");
        assert!(gallery.next());
        assert_eq!(gallery.current().unwrap().name, "three");
        assert!(gallery.next());
        assert_eq!(gallery.current().unwrap().name, "one");
        assert!(gallery.previous());
        assert_eq!(gallery.current().unwrap().name, "three");
        assert!(gallery.previous());
        assert_eq!(gallery.current().unwrap().name, "two");
    }

    #[test]
    fn empty_out_of_range_and_unsafe_items_do_not_shift_clicked_index() {
        assert!(ImageGallery::new(vec![], 0).is_none());
        assert!(ImageGallery::new(vec![item("one")], 1).is_none());
        for invalid in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:image/png,x",
            "https://name:password@example.com/image",
            "https://name@example.com/image",
            "//example.com/image",
            "https://exam\nple.com/image",
            "https://example.com/\0image",
        ] {
            let mut unsafe_item = item("bad");
            unsafe_item.source = invalid.into();
            assert!(
                ImageGallery::new(vec![item("one"), unsafe_item], 0).is_none(),
                "{invalid}"
            );
            let mut unsafe_original = item("bad");
            unsafe_original.original = Some(invalid.into());
            assert!(
                ImageGallery::new(vec![unsafe_original], 0).is_none(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn full_view_removes_thumbnail_transforms_and_keeps_original_signature() {
        let mut image = item("signed");
        image.source="https://proxy.example/photo.png?width=400&height=300&quality=low&format=webp&animated=false&signature=a%2Fb&size=128".into();
        image.original = Some("https://cdn.example/photo.png?width=123&signature=a%2Fb".into());
        let gallery = ImageGallery::new(vec![image], 0).unwrap();
        assert_eq!(
            gallery.current().unwrap().source,
            "https://proxy.example/photo.png?signature=a%2Fb&size=128"
        );
        assert_eq!(
            gallery.current().unwrap().original.as_deref(),
            Some("https://cdn.example/photo.png?width=123&signature=a%2Fb")
        );
    }

    #[test]
    fn single_image_and_mutated_invalid_index_are_safe() {
        let mut image = item("one");
        image.width = Some(0);
        image.height = None;
        let mut gallery = ImageGallery::new(vec![image], 0).unwrap();
        assert_eq!(gallery.current().unwrap().width, None);
        assert!(!gallery.next());
        assert!(!gallery.previous());
        gallery.index = usize::MAX;
        assert!(gallery.current().is_none());
        assert!(!gallery.next());
        assert!(!gallery.previous());
    }
}
