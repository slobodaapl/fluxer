// SPDX-License-Identifier: AGPL-3.0-or-later
//! Own the AVFoundation session on a capture thread. Enumerating devices never
//! opens them, and an outstanding permission prompt does not block call control.
use super::{
    CameraDevice, FIRST_FRAME_TIMEOUT, HEIGHT, MediaError, MediaEvent, NativeVideoSource, Result,
    Session, StopHandle, WIDTH,
};
use livekit::webrtc::{
    native::yuv_helper::nv12_to_i420,
    video_frame::{I420Buffer, VideoFrame, VideoRotation},
};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    ptr::NonNull,
    slice,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc as sync_mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

static CAPTURE_OWNER: Mutex<()> = Mutex::new(());
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(120);

unsafe extern "C" {
    fn fluxer_camera_devices(
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const c_char, *const c_char),
    );
    fn fluxer_camera_authorization() -> i32;
    fn fluxer_camera_request_access(
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void, bool),
    );
    fn fluxer_camera_open(device_id: *const c_char) -> *mut c_void;
    fn fluxer_camera_read(
        handle: *mut c_void,
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize, usize, usize),
    );
    fn fluxer_camera_close(handle: *mut c_void);
}

pub(super) fn devices() -> Vec<CameraDevice> {
    extern "C" fn add_device(context: *mut c_void, id: *const c_char, label: *const c_char) {
        if id.is_null() || label.is_null() {
            return;
        }
        // The bridge borrows the list and strings for this synchronous call.
        let devices = unsafe { &mut *context.cast::<Vec<CameraDevice>>() };
        let id = unsafe { CStr::from_ptr(id) }.to_string_lossy();
        let label = unsafe { CStr::from_ptr(label) }.to_string_lossy();
        if !id.is_empty() && !devices.iter().any(|device| device.id == id) {
            devices.push(CameraDevice {
                id: id.into_owned(),
                label: label.into_owned(),
            });
        }
    }

    let mut devices = Vec::new();
    unsafe { fluxer_camera_devices((&raw mut devices).cast(), add_device) };
    devices
}

pub(super) fn start(
    id: &str,
    source: NativeVideoSource,
    tx: mpsc::Sender<MediaEvent>,
    call_stopped: Arc<AtomicBool>,
    generation: u64,
) -> Result<Session> {
    let id = CString::new(id).map_err(|_| MediaError::UnknownDevice("camera"))?;
    let stopped = Arc::new(AtomicBool::new(false));
    let frame = Arc::new(AtomicBool::new(false));
    let owner = {
        let stopped = stopped.clone();
        let frame = frame.clone();
        thread::Builder::new()
            .name("fluxer-camera-capture".into())
            .spawn(move || {
                let cancelled = || {
                    stopped.load(Ordering::Acquire)
                        || call_stopped.load(Ordering::Acquire)
                        || tx.is_closed()
                };
                let outcome = capture(&id, &source, &tx, &frame, generation, cancelled);
                if !cancelled()
                    && !stopped.swap(true, Ordering::AcqRel)
                    && let Err(error) = outcome
                {
                    let _ = tx.blocking_send(MediaEvent::CameraStopped(generation));
                    let _ = tx.blocking_send(MediaEvent::Error(error.to_string()));
                }
            })
            .map_err(|error| MediaError::Camera(error.to_string()))?
            .thread()
            .clone()
    };
    Ok(Session {
        handle: StopHandle {
            stopped,
            interrupt: Arc::new(move || owner.unpark()),
        },
        frame,
    })
}

fn capture(
    id: &CStr,
    source: &NativeVideoSource,
    tx: &mpsc::Sender<MediaEvent>,
    ready: &AtomicBool,
    generation: u64,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    // A replacement waits here until the old session has released its device.
    let _owner = CAPTURE_OWNER
        .lock()
        .map_err(|_| MediaError::Camera("Camera capture is unavailable".into()))?;
    if cancelled() || !authorize(&cancelled)? || cancelled() {
        return Ok(());
    }
    let native = NativeCapture(
        NonNull::new(unsafe { fluxer_camera_open(id.as_ptr()) }).ok_or_else(|| {
            MediaError::Camera(
                "Could not open the selected camera. Refresh the device list and try again.".into(),
            )
        })?,
    );
    let started = Instant::now();
    let mut last_frame = started;
    while !cancelled() {
        if let Some(buffer) = native.read()
            && !cancelled()
        {
            last_frame = Instant::now();
            source.capture_frame(&VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us: started.elapsed().as_micros() as i64,
                frame_metadata: None,
                buffer,
            });
            if !ready.swap(true, Ordering::AcqRel)
                && tx.try_send(MediaEvent::CameraStarted(generation)).is_err()
            {
                return Err(MediaError::Camera(
                    "Camera readiness could not be delivered".into(),
                ));
            }
        } else if last_frame.elapsed() >= FIRST_FRAME_TIMEOUT {
            return Err(MediaError::Camera(
                "The selected camera stopped sending video".into(),
            ));
        }
        thread::park_timeout(FRAME_INTERVAL);
    }
    Ok(())
}

fn authorize(cancelled: &impl Fn() -> bool) -> Result<bool> {
    const NOT_DETERMINED: i32 = 0;
    const AUTHORIZED: i32 = 3;
    let permission_denied =
        || MediaError::Camera("Allow camera access for Fluxer Native in System Settings".into());
    match unsafe { fluxer_camera_authorization() } {
        AUTHORIZED => return Ok(true),
        NOT_DETERMINED => {}
        -1 => {
            return Err(MediaError::Camera(
                "Launch the macOS app bundle to use the camera".into(),
            ));
        }
        _ => return Err(permission_denied()),
    }

    extern "C" fn access_result(context: *mut c_void, granted: bool) {
        // AVFoundation calls this once, possibly after the capture is cancelled.
        let sender = unsafe { Box::from_raw(context.cast::<sync_mpsc::Sender<bool>>()) };
        let _ = sender.send(granted);
    }

    let (sender, receiver) = sync_mpsc::channel();
    unsafe { fluxer_camera_request_access(Box::into_raw(Box::new(sender)).cast(), access_result) };
    let started = Instant::now();
    while !cancelled() && started.elapsed() < PERMISSION_TIMEOUT {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(true) => return Ok(true),
            Ok(false) | Err(sync_mpsc::RecvTimeoutError::Disconnected) => {
                return Err(permission_denied());
            }
            Err(sync_mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    if cancelled() {
        Ok(false)
    } else {
        Err(MediaError::Camera(
            "Camera permission request timed out".into(),
        ))
    }
}

struct NativeCapture(NonNull<c_void>);

impl NativeCapture {
    fn read(&self) -> Option<I420Buffer> {
        let mut frame = None;
        // The bridge calls synchronously while the planes are locked.
        unsafe { fluxer_camera_read(self.0.as_ptr(), (&raw mut frame).cast(), copy_frame) };
        frame
    }
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        unsafe { fluxer_camera_close(self.0.as_ptr()) };
    }
}

extern "C" fn copy_frame(
    context: *mut c_void,
    y: *const u8,
    stride_y: usize,
    uv: *const u8,
    stride_uv: usize,
    width: usize,
    height: usize,
) {
    if !(2..=4096).contains(&width)
        || !(2..=4096).contains(&height)
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || !(width..=i32::MAX as usize).contains(&stride_y)
        || !(width..=i32::MAX as usize).contains(&stride_uv)
        || y.is_null()
        || uv.is_null()
    {
        return;
    }
    let Some(y_len) = stride_y.checked_mul(height) else {
        return;
    };
    let Some(uv_len) = stride_uv.checked_mul(height / 2) else {
        return;
    };
    if y_len > isize::MAX as usize || uv_len > isize::MAX as usize {
        return;
    }
    let y = unsafe { slice::from_raw_parts(y, y_len) };
    let uv = unsafe { slice::from_raw_parts(uv, uv_len) };
    let mut buffer = I420Buffer::new(width as u32, height as u32);
    let (sy, su, sv) = buffer.strides();
    let (out_y, out_u, out_v) = buffer.data_mut();
    nv12_to_i420(
        y,
        stride_y as u32,
        uv,
        stride_uv as u32,
        out_y,
        sy,
        out_u,
        su,
        out_v,
        sv,
        width as i32,
        height as i32,
    );
    let (w, h) = crate::screen::fit_dimensions(width as u32, height as u32, WIDTH, HEIGHT);
    if w != width as u32 || h != height as u32 {
        buffer = buffer.scale(w as i32, h as i32);
    }
    let frame = unsafe { &mut *context.cast::<Option<I420Buffer>>() };
    *frame = Some(buffer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_native_frame_preserves_luma_and_both_chroma_planes() {
        let y = [16, 17, 99, 99, 18, 19, 99, 99];
        let uv = [30, 50, 99, 99];
        let mut buffer: Option<I420Buffer> = None;
        copy_frame(
            (&raw mut buffer).cast(),
            y.as_ptr(),
            4,
            uv.as_ptr(),
            4,
            2,
            2,
        );
        let buffer = buffer.unwrap();
        let (luma, u, v) = buffer.data();
        assert_eq!(luma, [16, 17, 18, 19]);
        assert_eq!(u, [30]);
        assert_eq!(v, [50]);
    }
}
