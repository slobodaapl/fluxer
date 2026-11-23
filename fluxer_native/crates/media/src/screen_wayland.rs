// SPDX-License-Identifier: AGPL-3.0-or-later
//! Enumerate and capture named outputs and toplevels on the same compositor.
use crate::screen::{ScreenSource, ScreenSourceKind};
use std::{
    collections::HashMap,
    error::Error,
    os::fd::{AsFd, AsRawFd},
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::ObjectId,
    delegate_noop, event_created_child,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1 as handle, ext_foreign_toplevel_list_v1 as list,
    },
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1 as sources,
        ext_image_capture_source_v1 as source,
        ext_output_image_capture_source_manager_v1 as output_sources,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1 as frame, ext_image_copy_capture_manager_v1 as capture,
        ext_image_copy_capture_session_v1 as session,
    },
};

type CaptureResult<T> = Result<T, Box<dyn Error>>;

#[derive(Default)]
struct Window {
    identifier: String,
    title: String,
    app_id: String,
    ready: bool,
}

#[derive(Default)]
struct Output {
    name: String,
    description: String,
}

#[derive(Default)]
struct State {
    windows: HashMap<ObjectId, (handle::ExtForeignToplevelHandleV1, Window)>,
    outputs: HashMap<ObjectId, (wl_output::WlOutput, Output)>,
    pending_size: (u32, u32),
    pending_format: Option<wl_shm::Format>,
    constraints: Option<BufferConstraints>,
    stopped: bool,
    frame: Option<bool>,
    transform: Option<wl_output::Transform>,
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, output)) = state.outputs.get_mut(&proxy.id()) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => output.name = name,
            wl_output::Event::Description { description } => output.description = description,
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BufferConstraints {
    width: u32,
    height: u32,
    format: wl_shm::Format,
}

impl Dispatch<list::ExtForeignToplevelListV1, ()> for State {
    event_created_child!(State, list::ExtForeignToplevelListV1, [
        list::EVT_TOPLEVEL_OPCODE => (handle::ExtForeignToplevelHandleV1, ())
    ]);

    fn event(
        state: &mut Self,
        _: &list::ExtForeignToplevelListV1,
        event: list::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let list::Event::Toplevel { toplevel } = event {
            state
                .windows
                .insert(toplevel.id(), (toplevel, Window::default()));
        }
    }
}

impl Dispatch<handle::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &handle::ExtForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, handle::Event::Closed) {
            state.windows.remove(&proxy.id());
            proxy.destroy();
            return;
        }
        let Some((_, window)) = state.windows.get_mut(&proxy.id()) else {
            return;
        };
        match event {
            handle::Event::Identifier { identifier } => window.identifier = identifier,
            handle::Event::Title { title } => window.title = title,
            handle::Event::AppId { app_id } => window.app_id = app_id,
            handle::Event::Done => window.ready = true,
            _ => {}
        }
    }
}

impl Dispatch<session::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &session::ExtImageCopyCaptureSessionV1,
        event: session::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            session::Event::BufferSize { width, height } => state.pending_size = (width, height),
            session::Event::ShmFormat {
                format: WEnum::Value(format),
            } if matches!(format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888) => {
                state.pending_format = Some(format);
            }
            session::Event::Done => {
                let (width, height) = state.pending_size;
                state.constraints = state.pending_format.take().map(|format| BufferConstraints {
                    width,
                    height,
                    format,
                });
            }
            session::Event::Stopped => state.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<frame::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &frame::ExtImageCopyCaptureFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            frame::Event::Transform {
                transform: WEnum::Value(transform),
            } => state.transform = Some(transform),
            frame::Event::Ready => state.frame = Some(true),
            frame::Event::Failed {
                reason: WEnum::Value(frame::FailureReason::Stopped),
            } => {
                state.stopped = true;
            }
            frame::Event::Failed { .. } => state.frame = Some(false),
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore sources::ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(State: ignore output_sources::ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ignore source::ExtImageCaptureSourceV1);
delegate_noop!(State: ignore capture::ExtImageCopyCaptureManagerV1);

struct Picker {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    shm: wl_shm::WlShm,
    source_manager: sources::ExtForeignToplevelImageCaptureSourceManagerV1,
    output_manager: output_sources::ExtOutputImageCaptureSourceManagerV1,
    manager: capture::ExtImageCopyCaptureManagerV1,
}

impl Picker {
    fn open() -> CaptureResult<Self> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
        let qh = queue.handle();
        let shm = globals.bind(&qh, 1..=1, ())?;
        let source_manager = globals.bind(&qh, 1..=1, ())?;
        let output_manager = globals.bind(&qh, 1..=1, ())?;
        let manager = globals.bind(&qh, 1..=1, ())?;
        let _list: list::ExtForeignToplevelListV1 = globals.bind(&qh, 1..=1, ())?;
        let mut state = State::default();
        globals.contents().with_list(|announced| {
            for global in announced {
                if global.interface == "wl_output" && global.version >= 4 {
                    let output: wl_output::WlOutput =
                        globals.registry().bind(global.name, 4, &qh, ());
                    state
                        .outputs
                        .insert(output.id(), (output, Output::default()));
                }
            }
        });
        queue.roundtrip(&mut state)?;
        Ok(Self {
            connection,
            queue,
            state,
            shm,
            source_manager,
            output_manager,
            manager,
        })
    }

    fn dispatch(&mut self, wait: Duration) -> CaptureResult<()> {
        self.queue.dispatch_pending(&mut self.state)?;
        self.queue.flush()?;
        if let Some(guard) = self.queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: self.connection.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // The connection outlives the poll call and no other thread reads it.
            let ready = unsafe { libc::poll(&mut fd, 1, wait.as_millis() as i32) };
            if ready > 0 {
                guard.read()?;
                self.queue.dispatch_pending(&mut self.state)?;
            } else if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn sources() -> CaptureResult<Vec<ScreenSource>> {
    let picker = Picker::open()?;
    let windows = picker
        .state
        .windows
        .into_values()
        .map(|(_, window)| window)
        .filter(|window| window.ready && !window.identifier.is_empty())
        .map(|window| {
            let label = match (window.title.is_empty(), window.app_id.is_empty()) {
                (false, false) if window.title != window.app_id => {
                    format!("{} · {}", window.title, window.app_id)
                }
                (false, _) => window.title,
                (true, false) => window.app_id,
                (true, true) => "Untitled window".into(),
            };
            ScreenSource {
                id: 0,
                label,
                kind: ScreenSourceKind::Window,
                wayland_id: Some(window.identifier),
            }
        });
    let mut displays: Vec<_> = picker
        .state
        .outputs
        .into_values()
        .map(|(_, output)| output)
        .filter(|output| !output.name.is_empty())
        .map(|output| {
            let label = if output.description.is_empty() || output.description == output.name {
                format!("Display · {}", output.name)
            } else {
                format!("Display · {} ({})", output.description, output.name)
            };
            ScreenSource {
                id: 0,
                label,
                kind: ScreenSourceKind::Screen,
                wayland_id: Some(output.name),
            }
        })
        .collect();
    if displays.is_empty() {
        return Err("The compositor did not expose any named displays".into());
    }
    displays.extend(windows);
    displays.sort_unstable_by(|a, b| {
        (a.kind != ScreenSourceKind::Screen)
            .cmp(&(b.kind != ScreenSourceKind::Screen))
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.wayland_id.cmp(&b.wayland_id))
    });
    Ok(displays)
}

struct Buffer {
    constraints: BufferConstraints,
    memory: memmap2::MmapMut,
    buffer: wl_buffer::WlBuffer,
}

fn orient_pixels(
    pixels: &[u8],
    width: u32,
    height: u32,
    transform: wl_output::Transform,
    oriented: &mut Vec<u8>,
) -> (u32, u32) {
    use wl_output::Transform;
    let (turns, flip): (u8, bool) = match transform {
        Transform::_90 => (1, false),
        Transform::_180 => (2, false),
        Transform::_270 => (3, false),
        Transform::Flipped => (0, true),
        Transform::Flipped90 => (1, true),
        Transform::Flipped180 => (2, true),
        Transform::Flipped270 => (3, true),
        _ => (0, false),
    };
    let (w, h) = (width as usize, height as usize);
    let (out_w, out_h) = if turns.is_multiple_of(2) {
        (w, h)
    } else {
        (h, w)
    };
    oriented.resize(pixels.len(), 0);
    let destination = oriented.as_chunks_mut::<4>().0;
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let (mut x, y) = (index % w, index / w);
        if flip {
            x = w - 1 - x;
        }
        let (x, y) = match turns {
            1 => (y, w - 1 - x),
            2 => (w - 1 - x, h - 1 - y),
            3 => (h - 1 - y, x),
            _ => (x, y),
        };
        destination[y * out_w + x] = *pixel;
    }
    (out_w as u32, out_h as u32)
}

impl Buffer {
    fn new(
        shm: &wl_shm::WlShm,
        constraints: BufferConstraints,
        queue: &QueueHandle<State>,
    ) -> CaptureResult<Self> {
        let BufferConstraints {
            width,
            height,
            format,
        } = constraints;
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err("Invalid window capture dimensions".into());
        }
        let length = u64::from(width) * u64::from(height) * 4;
        if length > 128 * 1024 * 1024 {
            return Err("Window dimensions exceed the capture buffer limit".into());
        }
        let file = tempfile::tempfile()?;
        file.set_len(length)?;
        // The compositor writes the buffer before Ready. The capture loop
        // reads it only after Ready and before sending another frame request.
        let memory = unsafe { memmap2::MmapMut::map_mut(&file)? };
        let pool = shm.create_pool(file.as_fd(), length as i32, queue, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            (width * 4) as i32,
            format,
            queue,
            (),
        );
        pool.destroy();
        Ok(Self {
            constraints,
            memory,
            buffer,
        })
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

pub(crate) fn capture_source(
    kind: ScreenSourceKind,
    identifier: &str,
    interval: Duration,
    mut running: impl FnMut() -> bool,
    set_owner: impl FnOnce(&str, &str),
    mut publish: impl FnMut(&[u8], u32, u32),
) -> CaptureResult<()> {
    let mut picker = Picker::open()?;
    let qh = picker.queue.handle();
    let (source, handle_id) = if kind == ScreenSourceKind::Window {
        let (handle, window) = picker
            .state
            .windows
            .values()
            .find(|(_, window)| window.ready && window.identifier == identifier)
            .ok_or("The selected window closed")?;
        set_owner(&window.app_id, &window.title);
        (
            picker.source_manager.create_source(handle, &qh, ()),
            Some(handle.id()),
        )
    } else {
        let (output, _) = picker
            .state
            .outputs
            .values()
            .find(|(_, output)| output.name == identifier)
            .ok_or("The selected display was disconnected")?;
        (picker.output_manager.create_source(output, &qh, ()), None)
    };
    let session = picker
        .manager
        .create_session(&source, capture::Options::PaintCursors, &qh, ());
    source.destroy();
    let mut buffer: Option<Buffer> = None;
    let mut oriented_pixels = Vec::new();
    let mut pending: Option<frame::ExtImageCopyCaptureFrameV1> = None;
    let mut next_frame = Instant::now();
    let mut first_frame_deadline = Some(next_frame + Duration::from_secs(5));
    while running() {
        if picker.state.stopped
            || handle_id
                .as_ref()
                .is_some_and(|id| !picker.state.windows.contains_key(id))
        {
            return Err("The selected source closed or capture was stopped".into());
        }
        if let Some(ready) = picker.state.frame.take() {
            if let Some(frame) = pending.take() {
                frame.destroy();
            }
            if ready && let Some(buffer) = &buffer {
                let BufferConstraints { width, height, .. } = buffer.constraints;
                match picker.state.transform.take() {
                    Some(transform) if transform != wl_output::Transform::Normal => {
                        let (width, height) = orient_pixels(
                            &buffer.memory,
                            width,
                            height,
                            transform,
                            &mut oriented_pixels,
                        );
                        publish(&oriented_pixels, width, height);
                    }
                    _ => publish(&buffer.memory, width, height),
                }
                first_frame_deadline = None;
            }
        }
        let now = Instant::now();
        if first_frame_deadline.is_some_and(|deadline| now >= deadline) {
            return Err("The compositor did not provide a capture frame".into());
        }
        if pending.is_none()
            && now >= next_frame
            && let Some(constraints) = picker.state.constraints
        {
            if buffer
                .as_ref()
                .is_none_or(|buffer| buffer.constraints != constraints)
            {
                buffer = Some(Buffer::new(&picker.shm, constraints, &qh)?);
            }
            let buffer = buffer.as_ref().expect("capture buffer prepared");
            picker.state.transform = None;
            let frame = session.create_frame(&qh, ());
            frame.attach_buffer(&buffer.buffer);
            frame.damage_buffer(0, 0, constraints.width as i32, constraints.height as i32);
            frame.capture();
            pending = Some(frame);
            next_frame = now + interval;
        }
        let wait = if pending.is_some() || picker.state.constraints.is_none() {
            Duration::from_millis(25)
        } else {
            next_frame
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25))
        };
        picker.dispatch(wait)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn rotated_display_retains_all_pixels_in_the_reported_orientation() {
        let frame = [1u8, 2, 3, 4, 5, 6].map(|pixel| [pixel; 4]).concat();
        let mut oriented = Vec::new();
        assert_eq!(
            orient_pixels(&frame, 3, 2, wl_output::Transform::_90, &mut oriented),
            (2, 3)
        );
        assert_eq!(
            oriented,
            [3u8, 6, 2, 5, 1, 4].map(|pixel| [pixel; 4]).concat()
        );
    }

    #[test]
    #[ignore = "requires an isolated Wayland compositor and an animated fixture window"]
    fn selected_toplevel_supplies_its_app_identity_and_frames() {
        let expected_app = std::env::var("FLUXER_WAYLAND_TEST_APP").expect("fixture app ID");
        let id = {
            let picker = Picker::open().expect("toplevel protocols on private compositor");
            let window = picker
                .state
                .windows
                .values()
                .find(|(_, window)| window.app_id == expected_app && window.ready)
                .expect("fixture window mapped");
            window.1.identifier.clone()
        };
        let frames = Cell::new(0);
        let deadline = Instant::now() + Duration::from_secs(8);
        capture_source(
            ScreenSourceKind::Window,
            &id,
            Duration::from_millis(33),
            || Instant::now() < deadline && frames.get() < 2,
            |app, _| assert_eq!(app, expected_app),
            |pixels, width, height| {
                assert!(width > 32 && height > 32);
                assert_eq!(pixels.len(), (width * height * 4) as usize);
                assert!(
                    pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|pixel| pixel[..3] != [0, 0, 0])
                );
                frames.set(frames.get() + 1);
            },
        )
        .expect("capture the selected toplevel");
        assert_eq!(frames.get(), 2);
    }

    #[test]
    #[ignore = "requires an isolated Wayland compositor with a visible fixture"]
    fn selected_display_shares_frames_from_the_same_picker() {
        let display = sources()
            .unwrap()
            .into_iter()
            .find(|entry| entry.kind == ScreenSourceKind::Screen)
            .unwrap();
        let rendered = Cell::new(false);
        capture_source(
            ScreenSourceKind::Screen,
            display.wayland_id.as_deref().unwrap(),
            Duration::from_millis(33),
            || !rendered.get(),
            |_, _| panic!("a display has no application owner"),
            |pixels, width, height| {
                assert!(width > 32 && height > 32);
                assert_eq!(pixels.len(), (width * height * 4) as usize);
                rendered.set(true);
            },
        )
        .unwrap();
        assert!(rendered.get());
    }
}
