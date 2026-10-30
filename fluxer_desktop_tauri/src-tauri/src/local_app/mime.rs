// SPDX-License-Identifier: AGPL-3.0-or-later

const OCTET_STREAM_CONTENT_TYPE: &str = "application/octet-stream";

const NO_STORE_CACHE_CONTROL: &str = "no-store";
const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const REVALIDATED_CACHE_CONTROL: &str = "public, max-age=3600, must-revalidate";

const REWRITTEN_FILE_NAMES: [&str; 3] = ["index.html", "manifest.json", "browserconfig.xml"];
const HASHED_ASSET_DIRECTORY: &str = "assets";
const CONTENT_HASH_MIN_LENGTH: usize = 8;

fn is_separator(byte: u8) -> bool {
    byte == b'/' || byte == b'\\'
}

/// Port of Node `path.extname` (separators `/` and `\`).
fn extname(path: &str) -> &str {
    let bytes = path.as_bytes();
    let mut start_dot: Option<usize> = None;
    let mut start_part = 0;
    let mut end: Option<usize> = None;
    let mut matched_slash = true;
    let mut pre_dot_state: i8 = 0;
    for index in (0..bytes.len()).rev() {
        let byte = bytes[index];
        if is_separator(byte) {
            if !matched_slash {
                start_part = index + 1;
                break;
            }
            continue;
        }
        if end.is_none() {
            matched_slash = false;
            end = Some(index + 1);
        }
        if byte == b'.' {
            if start_dot.is_none() {
                start_dot = Some(index);
            } else if pre_dot_state != 1 {
                pre_dot_state = 1;
            }
        } else if start_dot.is_some() {
            pre_dot_state = -1;
        }
    }
    let (Some(start_dot), Some(end)) = (start_dot, end) else {
        return "";
    };
    if pre_dot_state == 0
        || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1)
    {
        return "";
    }
    &path[start_dot..end]
}

/// Port of Node `path.basename`.
fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(is_separator_char);
    trimmed.rsplit(is_separator_char).next().unwrap_or("")
}

fn is_separator_char(character: char) -> bool {
    character == '/' || character == '\\'
}

fn content_type_for_extension(extension: &str) -> Option<&'static str> {
    Some(match extension {
        ".html" | ".htm" => "text/html; charset=utf-8",
        ".js" | ".mjs" => "application/javascript; charset=utf-8",
        ".css" => "text/css; charset=utf-8",
        ".json" => "application/json; charset=utf-8",
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".avif" => "image/avif",
        ".svg" => "image/svg+xml",
        ".ico" => "image/x-icon",
        ".woff" => "font/woff",
        ".woff2" => "font/woff2",
        ".ttf" => "font/ttf",
        ".otf" => "font/otf",
        ".mp3" => "audio/mpeg",
        ".mp4" => "video/mp4",
        ".webm" => "video/webm",
        ".ogg" => "audio/ogg",
        ".txt" => "text/plain; charset=utf-8",
        ".xml" => "application/xml; charset=utf-8",
        ".webmanifest" => "application/manifest+json",
        ".map" => "application/json",
        ".onnx" => OCTET_STREAM_CONTENT_TYPE,
        ".wasm" => "application/wasm",
        _ => return None,
    })
}

pub fn content_type(path: &str) -> &'static str {
    content_type_for_extension(&extname(path).to_ascii_lowercase())
        .unwrap_or(OCTET_STREAM_CONTENT_TYPE)
}

pub fn is_served_extension(path: &str) -> bool {
    content_type_for_extension(&extname(path).to_ascii_lowercase()).is_some()
}

fn is_content_hash(value: &str) -> bool {
    value.len() >= CONTENT_HASH_MIN_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_hashed_asset_file_name(file_name: &str) -> bool {
    let Some(last_dot) = file_name.rfind('.') else {
        return false;
    };
    let stem = &file_name[..last_dot];
    is_content_hash(stem) || stem.split(['.', '-']).any(is_content_hash)
}

fn is_hashed_asset_path(path: &str) -> bool {
    let segments: Vec<&str> = path.split(is_separator_char).collect();
    let Some((file_name, directories)) = segments.split_last() else {
        return false;
    };
    directories.contains(&HASHED_ASSET_DIRECTORY) && is_hashed_asset_file_name(file_name)
}

pub fn cache_control(path: &str) -> &'static str {
    if REWRITTEN_FILE_NAMES.contains(&basename(path)) {
        return NO_STORE_CACHE_CONTROL;
    }
    if is_hashed_asset_path(path) {
        return IMMUTABLE_CACHE_CONTROL;
    }
    REVALIDATED_CACHE_CONTROL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extname_matches_node() {
        assert_eq!(extname("a.js"), ".js");
        assert_eq!(extname("dir/a.tar.gz"), ".gz");
        assert_eq!(extname(".bashrc"), "");
        assert_eq!(extname("dir/.bashrc"), "");
        assert_eq!(extname(".a.b"), ".b");
        assert_eq!(extname("a."), ".");
        assert_eq!(extname("a.."), ".");
        assert_eq!(extname("."), "");
        assert_eq!(extname(".."), "");
        assert_eq!(extname("..."), ".");
        assert_eq!(extname("noext"), "");
        assert_eq!(extname("a.js/"), ".js");
        assert_eq!(extname("a.b/c"), "");
        assert_eq!(extname("a\\b.css"), ".css");
        assert_eq!(extname(""), "");
    }

    #[test]
    fn content_type_is_case_insensitive_with_fallback() {
        assert_eq!(content_type("index.HTML"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/a.MJS"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(content_type("model.onnx"), "application/octet-stream");
        assert_eq!(content_type("x.wasm"), "application/wasm");
        assert_eq!(content_type("x.unknown"), "application/octet-stream");
        assert_eq!(content_type(".bashrc"), "application/octet-stream");
    }

    #[test]
    fn served_extension_includes_onnx_but_not_unknown() {
        assert!(is_served_extension("model.onnx"));
        assert!(is_served_extension("a.PNG"));
        assert!(!is_served_extension("a.unknown"));
        assert!(!is_served_extension(".js"));
        assert!(!is_served_extension("channels/1"));
        assert!(!is_served_extension("a."));
    }

    #[test]
    fn rewritten_files_are_never_stored() {
        assert_eq!(cache_control("index.html"), "no-store");
        assert_eq!(cache_control("/root/index.html"), "no-store");
        assert_eq!(cache_control("manifest.json"), "no-store");
        assert_eq!(cache_control("assets/browserconfig.xml"), "no-store");
        assert_eq!(cache_control("assets/index.html"), "no-store");
    }

    #[test]
    fn hashed_assets_are_immutable() {
        const IMMUTABLE: &str = "public, max-age=31536000, immutable";
        assert_eq!(cache_control("assets/abcdef12.js"), IMMUTABLE);
        assert_eq!(cache_control("assets/ABCDEF12.js"), IMMUTABLE);
        assert_eq!(cache_control("assets/app.abcdef12.js"), IMMUTABLE);
        assert_eq!(cache_control("assets/app-abcdef1234.css"), IMMUTABLE);
        assert_eq!(cache_control("/root/assets/app-abcdef1234.css"), IMMUTABLE);
        assert_eq!(cache_control("x\\assets\\app-abcdef1234.css"), IMMUTABLE);
        assert_eq!(cache_control("assets/sub/app-abcdef1234.css"), IMMUTABLE);
    }

    #[test]
    fn unhashed_or_outside_assets_revalidate() {
        const REVALIDATED: &str = "public, max-age=3600, must-revalidate";
        assert_eq!(cache_control("assets/abcdef1.js"), REVALIDATED);
        assert_eq!(cache_control("assets/app.js"), REVALIDATED);
        assert_eq!(cache_control("assets/abcdef12"), REVALIDATED);
        assert_eq!(cache_control("other/app-abcdef1234.css"), REVALIDATED);
        assert_eq!(cache_control("abcdef12.js"), REVALIDATED);
        assert_eq!(cache_control("assets"), REVALIDATED);
        assert_eq!(cache_control("assets/ghijklmn.js"), REVALIDATED);
        assert_eq!(cache_control("assets/.abcdef12"), REVALIDATED);
    }
}
