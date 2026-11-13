// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded background work for saving cached media and preparing clipboard PNGs.
use image::{ImageDecoder, ImageEncoder};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
};

const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 80 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const WORKERS: usize = 2;
const QUEUED: usize = 4;
type Task = Box<dyn FnOnce() + Send + 'static>;
struct Pool {
    sender: SyncSender<Task>,
}
static POOL: OnceLock<Result<Pool, String>> = OnceLock::new();
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A cancellable operation. Dropping the handle requests cancellation; it never joins a worker.
pub struct ExportJob<T> {
    receiver: Receiver<Result<T, String>>,
    cancelled: Arc<AtomicBool>,
}
impl<T> ExportJob<T> {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    /// `Ok(None)` means pending. The inner result is the completed operation.
    pub fn try_recv(&self) -> Result<Option<Result<T, String>>, String> {
        match self.receiver.try_recv() {
            Ok(value) => Ok(Some(value)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("Media export worker stopped".into()),
        }
    }
}
impl<T> Drop for ExportJob<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn pool() -> Result<&'static Pool, String> {
    POOL.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Task>(QUEUED);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..WORKERS {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("media-export-{index}"))
                .spawn(move || {
                    loop {
                        let task = match receiver.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => break,
                        };
                        match task {
                            Ok(task) => task(),
                            Err(_) => break,
                        }
                    }
                })
                .map_err(|e| format!("Unable to start media export: {e}"))?;
        }
        Ok(Pool { sender })
    })
    .as_ref()
    .map_err(Clone::clone)
}
fn submit<T: Send + 'static>(
    operation: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
) -> Result<ExportJob<T>, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancelled.clone();
    let task = Box::new(move || {
        let result = if worker_cancel.load(Ordering::Acquire) {
            Err("Cancelled".into())
        } else {
            operation(&worker_cancel)
        };
        let _ = sender.send(result);
    });
    match pool()?.sender.try_send(task) {
        Ok(()) => Ok(ExportJob {
            receiver,
            cancelled,
        }),
        Err(TrySendError::Full(_)) => Err("Media export is busy; try again".into()),
        Err(TrySendError::Disconnected(_)) => Err("Media export worker stopped".into()),
    }
}
fn check_cancel(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("Cancelled".into())
    } else {
        Ok(())
    }
}

/// Produce a safe suggested basename; the file picker still owns the destination.
pub fn safe_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let base = base.strip_prefix("SPOILER_").unwrap_or(base);
    let mut output = String::new();
    for ch in base.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            continue;
        }
        if output.len() + ch.len_utf8() > 240 {
            break;
        }
        output.push(ch);
    }
    let output = output.trim().trim_matches('.').trim();
    if output.is_empty() {
        "download".into()
    } else {
        output.to_owned()
    }
}

/// Copy a cached local file to the selected path; no destination is changed until
/// the complete temporary file has been flushed and atomically renamed.
pub fn export_file(source: PathBuf, destination: PathBuf) -> Result<ExportJob<()>, String> {
    submit(move |cancelled| copy_file(&source, &destination, cancelled))
}
struct TempFile {
    path: PathBuf,
    armed: bool,
}
impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn temp_file(parent: &Path) -> Result<(TempFile, File), String> {
    for _ in 0..32 {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".fluxer-save-{}-{id:x}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((TempFile { path, armed: true }, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Unable to create temporary file: {e}")),
        }
    }
    Err("Unable to allocate a temporary filename".into())
}
fn copy_file(source: &Path, destination: &Path, cancelled: &AtomicBool) -> Result<(), String> {
    check_cancel(cancelled)?;
    if destination.file_name().is_none() {
        return Err("Invalid destination".into());
    }
    let source = File::open(source).map_err(|e| format!("Unable to open cached media: {e}"))?;
    copy_source(source, destination, cancelled)
}
fn copy_source(
    mut source: impl Read,
    destination: &Path,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (mut temporary, mut output) = temp_file(parent)?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        check_cancel(cancelled)?;
        let count = source
            .read(&mut buffer)
            .map_err(|e| format!("Unable to read cached media: {e}"))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|e| format!("Unable to save media: {e}"))?;
    }
    output
        .sync_all()
        .map_err(|e| format!("Unable to save media: {e}"))?;
    drop(output);
    check_cancel(cancelled)?;
    // Cancellation is best effort once the atomic commit starts.
    fs::rename(&temporary.path, destination)
        .map_err(|e| format!("Unable to replace destination: {e}"))?;
    temporary.armed = false;
    Ok(())
}

/// Decode a cached still image, apply EXIF orientation, and encode clipboard PNG.
pub fn prepare_clipboard_image(source: PathBuf) -> Result<ExportJob<Vec<u8>>, String> {
    submit(move |cancelled| clipboard_image(&source, cancelled))
}
struct BoundedWriter<'a> {
    bytes: Vec<u8>,
    cancelled: &'a AtomicBool,
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::other("Cancelled"));
        }
        if data.len() > MAX_PNG_BYTES.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("Clipboard image is too large"));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn clipboard_image(path: &Path, cancelled: &AtomicBool) -> Result<Vec<u8>, String> {
    check_cancel(cancelled)?;
    let file = File::open(path).map_err(|e| format!("Unable to open cached image: {e}"))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_INPUT_BYTES {
        return Err("Image file is too large to copy".into());
    }
    let mut reader = image::ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|e| format!("Unable to read image: {e}"))?;
    // The clipboard flow handles still images. Animated media uses URL fallback.
    if !matches!(
        reader.format(),
        Some(
            image::ImageFormat::Png
                | image::ImageFormat::Jpeg
                | image::ImageFormat::WebP
                | image::ImageFormat::Bmp
                | image::ImageFormat::Tiff
        )
    ) {
        return Err("Unsupported clipboard image format".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Unable to decode image: {e}"))?;
    let (width, height) = decoder.dimensions();
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > MAX_PIXELS
        || decoder.total_bytes() > 128 * 1024 * 1024
    {
        return Err("Image dimensions are too large to copy".into());
    }
    let orientation = decoder
        .orientation()
        .map_err(|e| format!("Unable to read image orientation: {e}"))?;
    check_cancel(cancelled)?;
    let mut image = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Unable to decode image: {e}"))?;
    check_cancel(cancelled)?;
    image.apply_orientation(orientation);
    let rgba = image.into_rgba8();
    check_cancel(cancelled)?;
    let mut output = BoundedWriter {
        bytes: Vec::new(),
        cancelled,
    };
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("Unable to encode clipboard image: {e}"))?;
    check_cancel(cancelled)?;
    Ok(output.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use std::{
        io::Cursor,
        time::{Duration, Instant},
    };
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "fluxer-export-test-{}-{}",
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn wait<T>(job: &ExportJob<T>) -> Result<T, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = job.try_recv()? {
                return result;
            }
            assert!(Instant::now() < deadline, "worker did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn filenames_are_basenames_without_controls_or_spoiler_prefix() {
        assert_eq!(safe_filename("../../SPOILER_cat.png"), "cat.png");
        assert_eq!(safe_filename("C:\\secret\\SPOILER_foo\n.jpg"), "foo.jpg");
        assert_eq!(safe_filename("../.."), "download");
        assert!(safe_filename(&"🐈".repeat(100)).len() <= 240);
    }
    #[test]
    fn export_atomically_replaces_or_preserves_destination() {
        let tmp = Scratch::new();
        let src = tmp.path("source");
        let dst = tmp.path("target");
        fs::write(&src, vec![42; 512 * 1024 + 123]).unwrap();
        fs::write(&dst, b"previous").unwrap();
        wait(&export_file(src.clone(), dst.clone()).unwrap()).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
        fs::write(&dst, b"previous").unwrap();
        assert!(copy_file(&tmp.path("missing"), &dst, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read(&dst).unwrap(), b"previous");
        assert!(copy_file(&src, &dst, &AtomicBool::new(true)).is_err());
        assert_eq!(fs::read(&dst).unwrap(), b"previous");
        let folder = tmp.path("folder");
        fs::create_dir(&folder).unwrap();
        assert!(copy_file(&src, &folder, &AtomicBool::new(false)).is_err());
        assert!(folder.is_dir());
        assert!(!fs::read_dir(&tmp.0).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".fluxer-save-")
        }));
    }
    #[test]
    fn cancellation_after_partial_copy_preserves_target_and_removes_temp() {
        struct CancellingReader<'a>(&'a AtomicBool);
        impl Read for CancellingReader<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                bytes[..4].copy_from_slice(b"data");
                self.0.store(true, Ordering::Release);
                Ok(4)
            }
        }
        let tmp = Scratch::new();
        let dest = tmp.path("destination");
        fs::write(&dest, b"old content").unwrap();
        let cancelled = AtomicBool::new(false);
        assert!(copy_source(CancellingReader(&cancelled), &dest, &cancelled).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"old content");
        assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 1);
    }
    #[test]
    fn clipboard_encodes_original_pixels_and_rejects_malformed_and_huge_dimensions() {
        let tmp = Scratch::new();
        let path = tmp.path("image.png");
        let pixels = ImageBuffer::from_fn(3, 2, |x, y| Rgba([x as u8 * 50, y as u8 * 80, 31, 255]));
        pixels.save(&path).unwrap();
        let png = wait(&prepare_clipboard_image(path).unwrap()).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(decoded, pixels);
        let bad = tmp.path("bad.png");
        fs::write(&bad, b"invalid").unwrap();
        assert!(clipboard_image(&bad, &AtomicBool::new(false)).is_err());
        let huge = tmp.path("huge.png");
        ImageBuffer::<Rgba<u8>, _>::from_pixel(MAX_DIMENSION + 1, 1, Rgba([0, 0, 0, 255]))
            .save(&huge)
            .unwrap();
        assert!(clipboard_image(&huge, &AtomicBool::new(false)).is_err());
        assert!(clipboard_image(&huge, &AtomicBool::new(true)).is_err());
    }
    #[test]
    fn jpeg_exif_orientation_is_applied() {
        let tmp = Scratch::new();
        let path = tmp.path("rotated.jpg");
        let image = image::DynamicImage::ImageRgb8(ImageBuffer::from_fn(8, 4, |x, _| {
            if x < 4 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        }));
        let mut jpeg = Cursor::new(Vec::new());
        image.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        // APP1 Exif / TIFF orientation=6 (90° clockwise).
        let exif: &[u8] =
            b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(exif);
        bytes.extend_from_slice(&jpeg.get_ref()[2..]);
        fs::write(&path, bytes).unwrap();
        let output = clipboard_image(&path, &AtomicBool::new(false)).unwrap();
        let output = image::load_from_memory(&output).unwrap().into_rgba8();
        assert_eq!(output.dimensions(), (4, 8));
        let top = output.get_pixel(2, 1);
        let bottom = output.get_pixel(2, 6);
        assert!(top[0] > top[2]);
        assert!(bottom[2] > bottom[0]);
    }
}
