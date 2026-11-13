// SPDX-License-Identifier: Apache-2.0
//! MIME negotiation shared by Wayland clipboard and primary selection.

pub(super) const TEXT_MIME_TYPES: [&str; 3] =
    ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

pub(super) enum Entry<'a> {
    Text(&'a str),
    Image { mime: &'static str, bytes: &'a [u8] },
}

pub(super) fn offered(entries: &[Entry<'_>]) -> Vec<&'static str> {
    let mut types = Vec::new();
    for entry in entries {
        match entry {
            Entry::Text(_) => {
                for mime in TEXT_MIME_TYPES {
                    if !types.contains(&mime) {
                        types.push(mime);
                    }
                }
            }
            Entry::Image { mime, .. } => {
                if !types.contains(mime) {
                    types.push(mime);
                }
            }
        }
    }
    types
}

pub(super) fn payload(entries: &[Entry<'_>], mime: &str, self_mime: &str) -> Option<Vec<u8>> {
    if TEXT_MIME_TYPES.contains(&mime) || mime == self_mime {
        let mut data = Vec::new();
        let mut found = false;
        for entry in entries {
            if let Entry::Text(text) = entry {
                data.extend_from_slice(text.as_bytes());
                found = true;
            }
        }
        return found.then_some(data);
    }
    entries.iter().find_map(|entry| match entry {
        Entry::Image { mime: kind, bytes } if *kind == mime => Some(bytes.to_vec()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_mime_offers_and_concatenation_preserve_self_marker() {
        let entries = [Entry::Text("alpha"), Entry::Text("β")];
        assert_eq!(offered(&entries), TEXT_MIME_TYPES);
        for mime in TEXT_MIME_TYPES {
            assert_eq!(
                payload(&entries, mime, "pid/1"),
                Some("alphaβ".as_bytes().to_vec())
            );
        }
        assert_eq!(
            payload(&entries, "pid/1", "pid/1"),
            Some("alphaβ".as_bytes().to_vec())
        );
        assert_eq!(payload(&entries, "image/png", "pid/1"), None);
    }
    #[test]
    fn image_mime_and_bytes_are_exact_without_fabricated_text() {
        let entries = [Entry::Image {
            mime: "image/png",
            bytes: b"\x89PNG\r\n",
        }];
        assert_eq!(offered(&entries), ["image/png"]);
        assert_eq!(
            payload(&entries, "image/png", "pid/1"),
            Some(b"\x89PNG\r\n".to_vec())
        );
        assert_eq!(payload(&entries, "text/plain", "pid/1"), None);
        assert_eq!(payload(&entries, "image/jpeg", "pid/1"), None);
        assert_eq!(payload(&entries, "pid/1", "pid/1"), None);
    }
    #[test]
    fn mixed_entries_offer_each_type_once_and_select_matching_image() {
        let entries = [
            Entry::Text("x"),
            Entry::Image {
                mime: "image/png",
                bytes: b"png",
            },
            Entry::Image {
                mime: "image/jpeg",
                bytes: b"jpeg",
            },
            Entry::Text("y"),
            Entry::Image {
                mime: "image/png",
                bytes: b"second",
            },
        ];
        assert_eq!(
            offered(&entries),
            [
                "text/plain;charset=utf-8",
                "UTF8_STRING",
                "text/plain",
                "image/png",
                "image/jpeg"
            ]
        );
        assert_eq!(
            payload(&entries, "image/png", "pid/1"),
            Some(b"png".to_vec())
        );
        assert_eq!(
            payload(&entries, "image/jpeg", "pid/1"),
            Some(b"jpeg".to_vec())
        );
        assert_eq!(
            payload(&entries, "text/plain", "pid/1"),
            Some(b"xy".to_vec())
        );
        assert_eq!(payload(&entries, "application/octet-stream", "pid/1"), None);
    }
}
