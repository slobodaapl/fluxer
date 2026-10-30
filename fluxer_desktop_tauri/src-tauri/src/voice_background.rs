// SPDX-License-Identifier: AGPL-3.0-or-later

//! The voice background media cache, the port of
//! fluxer_desktop/src/main/VoiceBackgroundMediaCache.ts. Images and videos picked as a voice
//! call background are kept in `voice-background-media` under the app data directory, named
//! by the SHA-256 of their id, so no id can reach outside that folder. At most 16 are kept,
//! newest first by modification time. Reads return the file as a `data:` URL.
//!
//! Electron left the channels ungated; here they take the privileged document gate
//! (`capture_for_read`). The renderer sends the media bytes as the raw request body and the id,
//! type and file name percent-encoded in headers.
//!
//! Kept from Electron: caching an id again under another type leaves the old file, and a read
//! takes whichever of them the folder lists first (the renderer mints a fresh id per upload).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use percent_encoding::percent_decode_str;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Manager, State, Webview};

use crate::atomic_file;
use crate::documents::{Caller, DocumentRegistry};
use crate::error::BridgeError;
use crate::js::{js_trim, utf16_length};

const CACHE_DIR_NAME: &str = "voice-background-media";
const MAX_BACKGROUND_MEDIA_BYTES: usize = 10 * 1024 * 1024;
const MAX_BACKGROUND_MEDIA_CACHE_FILES: usize = 16;
const MAX_BACKGROUND_MEDIA_ID_LENGTH: usize = 128;
const MAX_BACKGROUND_MEDIA_TEMP_AGE: Duration = Duration::from_secs(60 * 60);
const WEBP_HEADER_BYTES: usize = 1024;
const ERROR: &str = "Error";
const SIZE_INVALID: &str = "Voice background media size is invalid";

const ID_HEADER: &str = "x-fluxer-media-id";
const TYPE_HEADER: &str = "x-fluxer-media-type";
const NAME_HEADER: &str = "x-fluxer-media-name";
const DOCUMENT_TOKEN_HEADER: &str = "x-fluxer-document-token";

/// `VoiceBackgroundMediaKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Static,
    Animated,
    Video,
}

struct MediaType {
    mime_type: &'static str,
    extension: &'static str,
    kind: MediaKind,
}

const MEDIA_TYPES: [MediaType; 6] = [
    MediaType {
        mime_type: "image/jpeg",
        extension: "jpg",
        kind: MediaKind::Static,
    },
    MediaType {
        mime_type: "image/png",
        extension: "png",
        kind: MediaKind::Static,
    },
    MediaType {
        mime_type: "image/gif",
        extension: "gif",
        kind: MediaKind::Animated,
    },
    MediaType {
        mime_type: "image/webp",
        extension: "webp",
        kind: MediaKind::Static,
    },
    MediaType {
        mime_type: "video/mp4",
        extension: "mp4",
        kind: MediaKind::Video,
    },
    MediaType {
        mime_type: "video/webm",
        extension: "webm",
        kind: MediaKind::Video,
    },
];

fn by_extension(extension: &str) -> Option<&'static MediaType> {
    MEDIA_TYPES
        .iter()
        .find(|media| media.extension == extension)
}

/// `path.extname(name).slice(1)`.
fn extension_of(name: &Path) -> String {
    name.extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `VoiceBackgroundMediaCacheResult`.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheResult {
    path: String,
    media_kind: MediaKind,
}

/// `VoiceBackgroundMediaReadResult`.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    path: String,
    media_kind: MediaKind,
    data_url: String,
}

fn failure(message: impl Into<String>) -> BridgeError {
    BridgeError::new(ERROR, message)
}

fn io_failure(error: std::io::Error) -> BridgeError {
    failure(error.to_string())
}

/// `getCacheKey`: ids are 1 to 128 UTF-16 code units.
fn cache_key(id: &str) -> Result<String, BridgeError> {
    let length = utf16_length(id);
    if length == 0 || length > MAX_BACKGROUND_MEDIA_ID_LENGTH {
        return Err(failure("Invalid voice background media id"));
    }
    Ok(format!("{:x}", Sha256::digest(id.as_bytes())))
}

/// `getMediaType`: by the MIME type, else by the file name's extension.
fn media_type(mime_type: &str, file_name: Option<&str>) -> Result<&'static MediaType, BridgeError> {
    let normalized = js_trim(mime_type).to_lowercase();
    if let Some(media) = MEDIA_TYPES
        .iter()
        .find(|media| media.mime_type == normalized)
    {
        return Ok(media);
    }
    let extension = extension_of(Path::new(file_name.unwrap_or_default())).to_lowercase();
    by_extension(&extension).ok_or_else(|| failure("Unsupported voice background media type"))
}

/// `isAnimatedWebp`: an extended (`VP8X`) header's animation flag, else an `ANMF` chunk in the
/// first 1024 bytes.
fn is_animated_webp(data: &[u8]) -> bool {
    if data.len() < 21 || &data[0..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return false;
    }
    if &data[12..16] == b"VP8X" {
        return data[20] & 0b0000_0010 != 0;
    }
    data[..data.len().min(WEBP_HEADER_BYTES)]
        .windows(4)
        .any(|window| window == b"ANMF")
}

fn classify(media: &MediaType, data: &[u8]) -> MediaKind {
    if media.extension == "webp" && is_animated_webp(data) {
        MediaKind::Animated
    } else {
        media.kind
    }
}

/// `readMediaHeader`.
fn read_header(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut header = Vec::with_capacity(WEBP_HEADER_BYTES);
    std::fs::File::open(path)
        .ok()?
        .take(WEBP_HEADER_BYTES as u64)
        .read_to_end(&mut header)
        .ok()?;
    Some(header)
}

/// The regular files of `directory` (symlinks are not), in listing order; none when it cannot
/// be listed.
fn regular_files(directory: &Path) -> Vec<(String, PathBuf)> {
    let Ok(listing) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    listing
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect()
}

pub struct VoiceBackgroundMedia {
    directory: PathBuf,
}

impl VoiceBackgroundMedia {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            directory: app_data_dir.join(CACHE_DIR_NAME),
        }
    }

    /// `cacheVoiceBackgroundMedia`.
    pub fn write(
        &self,
        id: &str,
        mime_type: &str,
        file_name: Option<&str>,
        data: &[u8],
    ) -> Result<CacheResult, BridgeError> {
        let key = cache_key(id)?;
        let media = media_type(mime_type, file_name)?;
        if data.is_empty() || data.len() > MAX_BACKGROUND_MEDIA_BYTES {
            return Err(failure(SIZE_INVALID));
        }
        let media_kind = classify(media, data);
        std::fs::create_dir_all(&self.directory).map_err(io_failure)?;
        let path = self.directory.join(format!("{key}.{}", media.extension));
        atomic_file::write(&path, data)?;
        self.trim();
        Ok(CacheResult {
            path: path.to_string_lossy().into_owned(),
            media_kind,
        })
    }

    /// `trimVoiceBackgroundMediaCache`: other files (left-over temporary files) go after an
    /// hour; past 16 media files, the oldest go.
    fn trim(&self) {
        let now = SystemTime::now();
        let mut cached = Vec::new();
        for (name, path) in regular_files(&self.directory) {
            let Ok(modified) = std::fs::metadata(&path).and_then(|metadata| metadata.modified())
            else {
                continue;
            };
            if by_extension(&extension_of(Path::new(&name)).to_lowercase()).is_none() {
                if now
                    .duration_since(modified)
                    .is_ok_and(|age| age > MAX_BACKGROUND_MEDIA_TEMP_AGE)
                {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            cached.push((path, modified));
        }
        if cached.len() <= MAX_BACKGROUND_MEDIA_CACHE_FILES {
            return;
        }
        cached.sort_by_key(|(_, modified)| *modified);
        let excess = cached.len() - MAX_BACKGROUND_MEDIA_CACHE_FILES;
        for (path, _) in &cached[..excess] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// `findCachedVoiceBackgroundMedia`: the first listed `<key>.<media extension>` file.
    fn find(
        &self,
        id: &str,
    ) -> Result<Option<(PathBuf, &'static MediaType, MediaKind)>, BridgeError> {
        let prefix = format!("{}.", cache_key(id)?);
        for (name, path) in regular_files(&self.directory) {
            if !name.starts_with(&prefix) {
                continue;
            }
            let Some(media) = by_extension(&extension_of(Path::new(&name))) else {
                continue;
            };
            let mut kind = media.kind;
            if media.extension == "webp" {
                let Some(header) = read_header(&path) else {
                    continue;
                };
                kind = classify(media, &header);
            }
            return Ok(Some((path, media, kind)));
        }
        Ok(None)
    }

    /// `readVoiceBackgroundMedia`.
    pub fn read(&self, id: &str) -> Result<Option<ReadResult>, BridgeError> {
        let Some((path, media, media_kind)) = self.find(id)? else {
            return Ok(None);
        };
        let data = std::fs::read(&path).map_err(io_failure)?;
        if data.is_empty() || data.len() > MAX_BACKGROUND_MEDIA_BYTES {
            return Err(failure(SIZE_INVALID));
        }
        Ok(Some(ReadResult {
            path: path.to_string_lossy().into_owned(),
            media_kind,
            data_url: format!("data:{};base64,{}", media.mime_type, STANDARD.encode(data)),
        }))
    }

    /// `deleteVoiceBackgroundMedia`: every file of the id, whatever its extension. All removals
    /// are attempted; the first failure is reported.
    pub fn delete(&self, id: &str) -> Result<(), BridgeError> {
        let prefix = format!("{}.", cache_key(id)?);
        let mut first_error = None;
        for (name, path) in regular_files(&self.directory) {
            if !name.starts_with(&prefix) {
                continue;
            }
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    first_error.get_or_insert(error);
                }
                _ => {}
            }
        }
        first_error.map_or(Ok(()), |error| Err(io_failure(error)))
    }
}

type Documents<'a> = State<'a, Arc<DocumentRegistry>>;
type Reply<T> = Result<T, BridgeError>;

fn gate(
    webview: &Webview,
    documents: &DocumentRegistry,
    token: String,
    channel: &str,
) -> Reply<()> {
    documents
        .capture_for_read(&Caller::new(webview, token), channel)
        .map(drop)
        .map_err(|_| {
            BridgeError::new(
                "UntrustedRendererDocumentSenderError",
                format!("{channel} is only reachable from a trusted top-level renderer document"),
            )
        })
}

fn cache(app: &AppHandle) -> Reply<VoiceBackgroundMedia> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| failure(error.to_string()))?;
    Ok(VoiceBackgroundMedia::new(&app_data_dir))
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .expect("voice background media work does not panic")
}

#[tauri::command]
pub async fn voice_background_media_cache_write(
    app: AppHandle,
    webview: Webview,
    documents: Documents<'_>,
    request: Request<'_>,
) -> Reply<CacheResult> {
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| percent_decode_str(value).decode_utf8().ok())
            .map(|value| value.into_owned())
    };
    gate(
        &webview,
        &documents,
        header(DOCUMENT_TOKEN_HEADER).unwrap_or_default(),
        "voice-background-media-cache:write",
    )?;
    let (Some(id), Some(mime_type)) = (header(ID_HEADER), header(TYPE_HEADER)) else {
        return Err(BridgeError::new(
            "TypeError",
            "Voice background media request is malformed",
        ));
    };
    let file_name = header(NAME_HEADER);
    let data = match request.body() {
        InvokeBody::Raw(data) => data.clone(),
        InvokeBody::Json(_) => Vec::new(),
    };
    let cache = cache(&app)?;
    blocking(move || cache.write(&id, &mime_type, file_name.as_deref(), &data)).await
}

#[tauri::command]
pub async fn voice_background_media_cache_read(
    app: AppHandle,
    webview: Webview,
    documents: Documents<'_>,
    document_token: String,
    id: String,
) -> Reply<Option<ReadResult>> {
    gate(
        &webview,
        &documents,
        document_token,
        "voice-background-media-cache:read",
    )?;
    let cache = cache(&app)?;
    blocking(move || cache.read(&id)).await
}

#[tauri::command]
pub async fn voice_background_media_cache_delete(
    app: AppHandle,
    webview: Webview,
    documents: Documents<'_>,
    document_token: String,
    id: String,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "voice-background-media-cache:delete",
    )?;
    let cache = cache(&app)?;
    blocking(move || cache.delete(&id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn webp(chunk: &[u8; 4], flags: u8, tail: &[u8]) -> Vec<u8> {
        let mut data = b"RIFF\0\0\0\0WEBP".to_vec();
        data.extend_from_slice(chunk);
        data.extend_from_slice(&[0, 0, 0, 0, flags]);
        data.extend_from_slice(tail);
        data
    }

    fn files(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = regular_files(directory)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_under_the_ids_sha256_and_reads_back_a_data_url() {
        let app_data = tempfile::tempdir().unwrap();
        let cache = VoiceBackgroundMedia::new(app_data.path());
        let written = cache.write("../escape", " IMAGE/PNG ", None, PNG).unwrap();
        let key = format!("{:x}", Sha256::digest(b"../escape"));
        let expected = app_data
            .path()
            .join(CACHE_DIR_NAME)
            .join(format!("{key}.png"));
        assert_eq!(
            written,
            CacheResult {
                path: expected.to_string_lossy().into_owned(),
                media_kind: MediaKind::Static,
            }
        );
        assert_eq!(
            cache.read("../escape").unwrap(),
            Some(ReadResult {
                path: expected.to_string_lossy().into_owned(),
                media_kind: MediaKind::Static,
                data_url: format!("data:image/png;base64,{}", STANDARD.encode(PNG)),
            })
        );
        assert_eq!(cache.read("other").unwrap(), None);
    }

    #[test]
    fn ids_must_be_one_to_128_utf16_units() {
        let app_data = tempfile::tempdir().unwrap();
        let cache = VoiceBackgroundMedia::new(app_data.path());
        let invalid = "Invalid voice background media id";
        assert_eq!(cache.read("").unwrap_err().message(), invalid);
        // 64 astral characters are 128 UTF-16 units; one more is too many.
        let longest = "\u{1f600}".repeat(64);
        assert!(cache.write(&longest, "image/png", None, PNG).is_ok());
        let too_long = format!("{longest}a");
        assert_eq!(cache.delete(&too_long).unwrap_err().message(), invalid);
    }

    #[test]
    fn the_type_falls_back_to_the_file_extension_and_the_size_is_bounded() {
        let app_data = tempfile::tempdir().unwrap();
        let cache = VoiceBackgroundMedia::new(app_data.path());
        assert_eq!(
            cache
                .write(
                    "v",
                    "application/octet-stream",
                    Some("clip.WEBM"),
                    b"\x1a\x45"
                )
                .unwrap()
                .media_kind,
            MediaKind::Video
        );
        assert_eq!(
            cache
                .write("d", "", Some(".png"), PNG)
                .unwrap_err()
                .message(),
            "Unsupported voice background media type"
        );
        assert_eq!(
            cache
                .write("e", "image/png", None, b"")
                .unwrap_err()
                .message(),
            SIZE_INVALID
        );
        let oversized = vec![0u8; MAX_BACKGROUND_MEDIA_BYTES + 1];
        assert_eq!(
            cache
                .write("e", "image/png", None, &oversized)
                .unwrap_err()
                .message(),
            SIZE_INVALID
        );
        assert!(cache.write("e", "image/png", None, &oversized[1..]).is_ok());
    }

    #[test]
    fn animated_webp_is_sniffed_on_write_and_on_read() {
        let app_data = tempfile::tempdir().unwrap();
        let cache = VoiceBackgroundMedia::new(app_data.path());
        let extended = webp(b"VP8X", 0b10, b"");
        assert_eq!(
            cache
                .write("a", "image/webp", None, &extended)
                .unwrap()
                .media_kind,
            MediaKind::Animated
        );
        assert_eq!(
            cache.read("a").unwrap().unwrap().media_kind,
            MediaKind::Animated
        );
        let still = webp(b"VP8X", 0b1000, b"ANMF");
        assert_eq!(
            cache
                .write("b", "image/webp", None, &still)
                .unwrap()
                .media_kind,
            MediaKind::Static
        );
        let simple = webp(b"VP8 ", 0, b"....ANMF");
        assert_eq!(
            cache
                .write("c", "image/webp", None, &simple)
                .unwrap()
                .media_kind,
            MediaKind::Animated
        );
        let mut late = webp(b"VP8 ", 0, &[0; WEBP_HEADER_BYTES]);
        late.extend_from_slice(b"ANMF");
        assert_eq!(
            cache
                .write("d", "image/webp", None, &late)
                .unwrap()
                .media_kind,
            MediaKind::Static
        );
    }

    #[test]
    fn the_cache_keeps_the_sixteen_newest_and_sweeps_old_leftovers() {
        let app_data = tempfile::tempdir().unwrap();
        let cache = VoiceBackgroundMedia::new(app_data.path());
        let directory = app_data.path().join(CACHE_DIR_NAME);
        std::fs::create_dir_all(&directory).unwrap();
        let stale = directory.join("stale.png.0011223344556677.tmp");
        let fresh = directory.join("fresh.tmp");
        std::fs::write(&stale, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        let hours_ago = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(hours_ago)
            .unwrap();
        for index in 0..=MAX_BACKGROUND_MEDIA_CACHE_FILES {
            let id = format!("id-{index}");
            cache.write(&id, "image/png", None, PNG).unwrap();
            // Distinct, increasing modification times.
            let path = directory.join(format!("{}.png", cache_key(&id).unwrap()));
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(hours_ago + Duration::from_secs(index as u64 + 1))
                .unwrap();
        }
        // Each write's trim drops the oldest past sixteen: `id-16` dropped `id-0`, `last`
        // drops `id-1`. The stale leftover went with the first write.
        cache.write("last", "image/png", None, PNG).unwrap();
        assert_eq!(cache.read("id-0").unwrap(), None);
        assert_eq!(cache.read("id-1").unwrap(), None);
        assert!(cache.read("id-2").unwrap().is_some());
        assert!(cache.read("last").unwrap().is_some());
        let names = files(&directory);
        assert!(!names.contains(&"stale.png.0011223344556677.tmp".to_owned()));
        assert!(names.contains(&"fresh.tmp".to_owned()));
        assert_eq!(names.len(), MAX_BACKGROUND_MEDIA_CACHE_FILES + 1);
    }

    #[test]
    fn delete_removes_every_extension_of_the_id_only() {
        let app_data = tempfile::tempdir().unwrap();
        let cache = VoiceBackgroundMedia::new(app_data.path());
        cache.write("x", "image/png", None, PNG).unwrap();
        cache.write("x", "image/gif", None, b"GIF89a").unwrap();
        cache.write("y", "image/png", None, PNG).unwrap();
        cache.delete("x").unwrap();
        assert_eq!(cache.read("x").unwrap(), None);
        assert!(cache.read("y").unwrap().is_some());
        // Deleting from a folder that does not exist is not an error.
        let empty = tempfile::tempdir().unwrap();
        VoiceBackgroundMedia::new(empty.path()).delete("x").unwrap();
    }
}
