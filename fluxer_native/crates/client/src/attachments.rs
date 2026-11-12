//! Per-channel attachment staging and background preparation/upload lifecycle.
use std::{fmt, sync::Arc};

pub type AttachmentId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentStatus {
    Preparing,
    Staged,
    Uploading,
    Ready,
    Queued,
    Sending,
    Failed,
    Cancelled,
}

#[derive(Clone)]
pub struct AttachmentPreview {
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Arc<Vec<u8>>,
}
impl fmt::Debug for AttachmentPreview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentPreview")
            .field("revision", &self.revision)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct StagedAttachment {
    pub id: AttachmentId,
    pub filename: String,
    pub size: u64,
    pub mime_type: String,
    pub spoiler: bool,
    pub status: AttachmentStatus,
    pub progress: f32,
    pub error: Option<String>,
    pub preview: Option<AttachmentPreview>,
}

#[derive(Clone)]
pub(crate) enum AttachmentInput {
    Disk(std::path::PathBuf),
    Clipboard {
        filename: String,
        mime_type: String,
        bytes: Arc<Vec<u8>>,
    },
}
impl AttachmentInput {
    pub(crate) fn filename(&self) -> String {
        let name = match self {
            Self::Disk(path) => path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("attachment"),
            Self::Clipboard { filename, .. } => filename,
        };
        std::path::Path::new(name)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment")
            .to_owned()
    }
}
pub(crate) struct PreparedAttachment {
    pub path: std::path::PathBuf,
    pub size: u64,
    pub mime_type: String,
    pub preview: Option<AttachmentPreview>,
    modified: Option<std::time::SystemTime>,
    // Clipboard data is an isolated 0600 temporary file for streaming uploads.
    // All references drop on removal/reset/task cancellation; no persistent copy.
    _temporary: Option<tempfile::NamedTempFile>,
}
impl PreparedAttachment {
    /// Browser File objects preserve staged bytes. Reject mutable disk sources
    /// that changed since preparation before creating any upload request.
    pub async fn validate_source(&self) -> Result<(), String> {
        let metadata = tokio::fs::metadata(&self.path)
            .await
            .map_err(|_| "The staged file is no longer available")?;
        if !metadata.is_file()
            || metadata.len() != self.size
            || metadata.modified().ok() != self.modified
        {
            return Err("The staged file changed; remove it and attach it again".into());
        }
        Ok(())
    }
}

pub(crate) fn prepare(
    input: AttachmentInput,
    max_size: u64,
    revision: u64,
    cancel: &fluxer_protocol::UploadCancellation,
) -> Result<Arc<PreparedAttachment>, String> {
    use std::io::Write;
    let (path, temporary, declared) = match input {
        AttachmentInput::Disk(path) => (path, None, None),
        AttachmentInput::Clipboard {
            bytes, mime_type, ..
        } => {
            if bytes.len() as u64 > max_size {
                return Err("Image exceeds this account's upload limit".into());
            }
            let mut file = tempfile::NamedTempFile::new()
                .map_err(|_| "Could not prepare the clipboard image")?;
            for chunk in bytes.chunks(64 * 1024) {
                if cancel.is_cancelled() {
                    return Err("Attachment preparation cancelled".into());
                }
                file.write_all(chunk)
                    .map_err(|_| "Could not prepare the clipboard image")?;
            }
            (file.path().to_owned(), Some(file), Some(mime_type))
        }
    };
    let metadata = std::fs::metadata(&path).map_err(|_| "Could not read the selected file")?;
    if !metadata.is_file() {
        return Err("Select a regular file".into());
    }
    if metadata.len() > max_size {
        return Err("File exceeds this account's upload limit".into());
    }
    let mut mime_type = declared.unwrap_or_else(|| {
        mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string()
    });
    if cancel.is_cancelled() {
        return Err("Attachment preparation cancelled".into());
    }
    let preview = (|| {
        let mut reader = image::ImageReader::open(&path)
            .ok()?
            .with_guessed_format()
            .ok()?;
        if let Some(format) = reader.format() {
            mime_type = format.to_mime_type().into();
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().ok()?.thumbnail(256, 256).into_rgba8();
        let (width, height) = image.dimensions();
        let mut bgra = image.into_raw();
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Some(AttachmentPreview {
            revision,
            width,
            height,
            bgra: Arc::new(bgra),
        })
    })();
    Ok(Arc::new(PreparedAttachment {
        path,
        size: metadata.len(),
        mime_type,
        preview,
        modified: metadata.modified().ok(),
        _temporary: temporary,
    }))
}
