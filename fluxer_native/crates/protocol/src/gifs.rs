// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::Url;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GifFeed {
    Featured,
    Trending,
    Search(String),
}

impl GifFeed {
    pub(crate) fn endpoint(&self) -> &'static str {
        match self {
            Self::Featured => "/gifs/featured",
            Self::Trending => "/gifs/trending",
            Self::Search(_) => "/gifs/search",
        }
    }

    pub fn query(&self) -> &str {
        match self {
            Self::Search(query) => query,
            _ => "",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Gif {
    pub id: String,
    pub slug: String,
    pub provider: String,
    pub title: String,
    pub url: String,
    pub proxy_src: String,
    pub media: BTreeMap<String, GifFormat>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GifFormat {
    pub proxy_src: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GifCategory {
    pub name: String,
    #[serde(default)]
    pub proxy_src: String,
    #[serde(default)]
    pub src: String,
    pub gif: Option<Gif>,
}

#[derive(Clone, Debug)]
pub struct GifProvider {
    pub display_name: String,
    pub attribution_required: bool,
}

#[derive(Clone, Debug)]
pub struct GifResults {
    pub provider: GifProvider,
    pub gifs: Vec<Gif>,
    pub categories: Vec<GifCategory>,
}

#[derive(Clone, Debug)]
pub struct GifShare {
    pub id: String,
    pub url: String,
}

impl GifCategory {
    pub fn image_preview(&self) -> Option<&str> {
        self.gif.as_ref().and_then(Gif::image_preview).or_else(|| {
            [&self.proxy_src, &self.src]
                .into_iter()
                .map(String::as_str)
                .find(|source| {
                    web_url(source).is_some_and(|url| {
                        let path = url.path().to_ascii_lowercase();
                        !path.ends_with(".mp4") && !path.ends_with(".webm")
                    })
                })
        })
    }
}

impl Gif {
    pub fn image_preview(&self) -> Option<&str> {
        [
            "tinywebp",
            "tinygif",
            "mediumwebp",
            "mediumgif",
            "webp",
            "gif",
            "nanowebp",
            "nanogif",
        ]
        .into_iter()
        .filter_map(|key| self.media.get(key).map(|format| format.proxy_src.as_str()))
        .chain(std::iter::once(self.proxy_src.as_str()).filter(|source| {
            Url::parse(source).is_ok_and(|url| {
                let path = url.path().to_ascii_lowercase();
                path.ends_with(".gif") || path.ends_with(".webp")
            })
        }))
        .find(|source| web_url(source).is_some())
    }

    pub fn share(&self) -> Option<GifShare> {
        let raw = self.slug.trim();
        let slug = if web_url(raw).is_some() {
            provider_slug(&self.provider, raw)
        } else {
            (!raw.is_empty()).then(|| raw.to_owned())
        };
        let id = slug
            .or_else(|| provider_slug(&self.provider, &self.url))
            .or_else(|| (!self.id.is_empty()).then(|| self.id.clone()))?;
        let url = match self.provider.as_str() {
            "klipy" => format!(
                "https://klipy.com/gifs/{}",
                utf8_percent_encode(&id, COMPONENT)
            ),
            "tenor" => {
                let token = id.trim_start_matches('/').trim_end_matches('/');
                if token.to_ascii_lowercase().starts_with("view/") {
                    format!("https://tenor.com/{token}")
                } else if !token.contains('/') {
                    format!("https://tenor.com/view/{token}")
                } else {
                    self.url.clone()
                }
            }
            _ => self.url.clone(),
        };
        web_url(&url)?;
        Some(GifShare { id, url })
    }
}

const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

fn web_url(source: &str) -> Option<Url> {
    Url::parse(source).ok().filter(|url| {
        matches!(url.scheme(), "https" | "http")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn provider_slug(provider: &str, source: &str) -> Option<String> {
    let url = web_url(source)?;
    let host = url.host_str()?.trim_start_matches("www.");
    let mut segments = url.path_segments()?;
    let mut kind = segments.next()?.to_ascii_lowercase();
    if provider == "tenor" && kind != "view" {
        let language = kind
            .split_once('-')
            .map_or(kind.as_str(), |(language, _)| language);
        if language.len() != 2 || !language.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return None;
        }
        kind = segments.next()?.to_ascii_lowercase();
    }
    let prefix = match (provider, host, kind.as_str()) {
        ("klipy", "klipy.com", "gif" | "gifs" | "clip" | "clips") => "",
        ("tenor", "tenor.com", "view") => "view/",
        _ => return None,
    };
    let decoded = percent_decode_str(segments.next()?).decode_utf8().ok()?;
    let slug = decoded.trim();
    (!slug.is_empty()).then(|| format!("{prefix}{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn featured_categories_keep_their_preview_and_allow_text_only_categories() {
        let category: GifCategory = serde_json::from_value(json!({
            "name": "high five",
            "src": "https://origin.example/high-five.gif",
            "proxy_src": "https://media.example/full.gif",
            "gif": {
                "id": "1", "slug": "high-five", "provider": "klipy", "title": "High five",
                "url": "https://klipy.com/gifs/high-five", "proxy_src": "https://media.example/full.gif",
                "media": {"tinywebp": {"proxy_src": "https://media.example/tiny.webp"}}
            }
        }))
        .unwrap();
        assert_eq!(category.name, "high five");
        assert_eq!(
            category.image_preview(),
            Some("https://media.example/tiny.webp")
        );

        let category: GifCategory = serde_json::from_value(json!({
            "name": "good luck", "proxy_src": "https://media.example/luck.gif", "gif": null
        }))
        .unwrap();
        assert_eq!(
            category.image_preview(),
            Some("https://media.example/luck.gif")
        );

        let category: GifCategory = serde_json::from_value(json!({
            "name": "hello", "proxy_src": "https://media.example/preview.mp4"
        }))
        .unwrap();
        assert_eq!(category.image_preview(), None);
    }

    #[test]
    fn gif_share_uses_the_provider_slug_instead_of_the_preview_or_numeric_id() {
        let mut gif = Gif {
            id: "12345".into(),
            slug: "https://www.klipy.com/gifs/hello%20world".into(),
            provider: "klipy".into(),
            title: "Hello world".into(),
            url: "https://klipy.com/gifs/other".into(),
            proxy_src: "https://media.example/result.mp4".into(),
            media: BTreeMap::new(),
        };
        let share = gif.share().unwrap();
        assert_eq!(share.id, "hello world");
        assert_eq!(share.url, "https://klipy.com/gifs/hello%20world");

        gif.provider = "tenor".into();
        gif.slug.clear();
        gif.url = "https://tenor.com/es-ES/view/greeting-12345".into();
        let share = gif.share().unwrap();
        assert_eq!(share.id, "view/greeting-12345");
        assert_eq!(share.url, "https://tenor.com/view/greeting-12345");

        assert_eq!(gif.image_preview(), None);
        gif.media.insert(
            "tinygif".into(),
            GifFormat {
                proxy_src: "https://media.example/tiny.gif".into(),
            },
        );
        assert_eq!(gif.image_preview(), Some("https://media.example/tiny.gif"));
    }
}
