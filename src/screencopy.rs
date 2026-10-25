//! Screen capture through the ext-image-copy-capture Wayland protocol, for compositors without
//! KWin's D-Bus API (COSMIC). Every screen is captured at once on a private connection: the
//! compositor copies each output on its own render pass, so there is no one screen to favour.

use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use crate::capture::{self, Image, OutputSize};

/// Give up on screens the compositor hasn't delivered by then.
const TIMEOUT: Duration = Duration::from_secs(3);

enum Status {
    /// Waiting for the session's buffer constraints.
    Constraints,
    Capturing,
    Ready,
    Failed(String),
    Delivered,
}

struct Shot {
    name: String,
    started: Instant,
    size: (u32, u32),
    formats: Vec<wl_shm::Format>,
    /// memfd backing the wl_buffer, row stride and whether the pixels are RGBA.
    buffer: Option<(File, u32, bool)>,
    status: Status,
}

struct State {
    shm: wl_shm::WlShm,
    outputs: Vec<(wl_output::WlOutput, Option<String>)>,
    shots: Vec<Shot>,
}

/// Capture `outputs` (by name) at once, calling `done` for each as it arrives.
/// `before_rest(false)` runs on another thread meanwhile, as nothing waits for a first screen.
pub fn capture_all(
    outputs: Vec<OutputSize>,
    done: impl Fn(String, Result<Image, String>),
    before_rest: impl FnOnce(bool) -> bool + Send,
) {
    let names: Vec<String> = outputs.into_iter().map(|(name, _)| name).collect();
    std::thread::scope(|s| {
        s.spawn(|| before_rest(false));
        let mut delivered = Vec::new();
        let result = capture(&names, |name, img| {
            delivered.push(name.clone());
            done(name, img);
        });
        // Every screen must be answered, or the app keeps waiting for it.
        let why = result.err().unwrap_or_else(|| "not captured".into());
        for name in names.iter().filter(|n| !delivered.contains(n)) {
            done(name.clone(), Err(format!("{name}: {why}")));
        }
    });
}

fn capture(names: &[String], mut done: impl FnMut(String, Result<Image, String>)) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| format!("wayland: {e}"))?;
    let qh = queue.handle();
    let missing = |what| format!("compositor lacks {what}");
    let sources: ExtOutputImageCaptureSourceManagerV1 =
        globals.bind(&qh, 1..=1, ()).map_err(|_| missing("ext_output_image_capture_source_manager_v1"))?;
    let copier: ExtImageCopyCaptureManagerV1 =
        globals.bind(&qh, 1..=1, ()).map_err(|_| missing("ext_image_copy_capture_manager_v1"))?;
    let shm = globals.bind(&qh, 1..=1, ()).map_err(|_| missing("wl_shm"))?;
    // wl_output v4 sends the connector name ("DP-1") the rest of the app uses.
    let outputs = globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|g| g.interface == "wl_output" && g.version >= 4)
        .enumerate()
        .map(|(i, g)| (globals.registry().bind(g.name, 4, &qh, i), None))
        .collect();
    let mut state = State { shm, outputs, shots: Vec::new() };
    queue.roundtrip(&mut state).map_err(|e| format!("wayland: {e}"))?;

    for name in names {
        let Some((output, _)) = state.outputs.iter().find(|(_, n)| n.as_ref() == Some(name)) else { continue };
        let source = sources.create_source(output, &qh, ());
        // No cursor in the picture (paint_cursors not set).
        copier.create_session(&source, Options::empty(), &qh, state.shots.len());
        state.shots.push(Shot {
            name: name.clone(),
            started: Instant::now(),
            size: (0, 0),
            formats: Vec::new(),
            buffer: None,
            status: Status::Constraints,
        });
    }

    let deadline = Instant::now() + TIMEOUT;
    while state.shots.iter().any(|s| !matches!(s.status, Status::Delivered)) {
        dispatch_until(&conn, &mut queue, &mut state, deadline)?;
        for shot in &mut state.shots {
            let result = match std::mem::replace(&mut shot.status, Status::Delivered) {
                Status::Ready => read(shot),
                Status::Failed(e) => Err(format!("{}: {e}", shot.name)),
                other => {
                    shot.status = other;
                    continue;
                }
            };
            if std::env::var_os("QUICKSNAP_DEBUG").is_some() {
                eprintln!("quicksnap: {}: ready {:?}", shot.name, shot.started.elapsed());
            }
            done(shot.name.clone(), result);
        }
    }
    Ok(())
}

fn read(shot: &mut Shot) -> Result<Image, String> {
    let (file, stride, swap) = shot.buffer.take().ok_or("no buffer")?;
    let (w, h) = shot.size;
    let mut raw = vec![0u8; stride as usize * h as usize];
    file.read_exact_at(&mut raw, 0).map_err(|e| format!("{}: read buffer: {e}", shot.name))?;
    capture::convert(raw, w, h, stride, swap)
}

/// Flush, wait for events up to `deadline` and dispatch them.
pub fn dispatch_until<S>(
    conn: &Connection,
    queue: &mut EventQueue<S>,
    state: &mut S,
    deadline: Instant,
) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("wayland: {e}");
    conn.flush().map_err(|e| err(&e))?;
    if let Some(guard) = queue.prepare_read() {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut fd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
        match unsafe { libc::poll(&mut fd, 1, left.as_millis() as i32) } {
            0 => return Err("timed out".into()),
            n if n > 0 => {
                // WouldBlock just means another reader got there first.
                let _ = guard.read();
            }
            _ => {}
        }
    }
    queue.dispatch_pending(state).map_err(|e| err(&e))?;
    Ok(())
}

/// Allocate a shm buffer matching the session's constraints and start the copy.
fn start_frame(
    state: &mut State,
    idx: usize,
    session: &ExtImageCopyCaptureSessionV1,
    qh: &QueueHandle<State>,
) -> Result<(), String> {
    use wl_shm::Format;
    let shot = &mut state.shots[idx];
    let (w, h) = shot.size;
    // XRGB/ARGB8888 are BGRA in memory on little endian, XBGR/ABGR8888 are RGBA.
    let pick = |f: Format| shot.formats.contains(&f).then_some(f);
    let (format, swap) = pick(Format::Xrgb8888)
        .or(pick(Format::Argb8888))
        .map(|f| (f, false))
        .or(pick(Format::Xbgr8888).or(pick(Format::Abgr8888)).map(|f| (f, true)))
        .ok_or_else(|| format!("no supported shm format in {:?}", shot.formats))?;
    let stride = w * 4;
    let len = stride as usize * h as usize;
    if len == 0 {
        return Err("empty buffer size".into());
    }

    let fd = unsafe { libc::memfd_create(c"quicksnap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd: {}", std::io::Error::last_os_error()));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(len as u64).map_err(|e| format!("memfd: {e}"))?;
    let pool = state.shm.create_pool(file.as_fd(), len as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, qh, ());
    pool.destroy();

    let frame = session.create_frame(qh, idx);
    frame.attach_buffer(&buffer);
    frame.damage_buffer(0, 0, w as i32, h as i32);
    frame.capture();
    shot.buffer = Some((file, stride, swap));
    shot.status = Status::Capturing;
    Ok(())
}

impl Dispatch<ExtImageCopyCaptureSessionV1, usize> for State {
    fn event(
        state: &mut Self,
        session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        &idx: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let shot = &mut state.shots[idx];
        match event {
            Event::BufferSize { width, height } => shot.size = (width, height),
            Event::ShmFormat { format: WEnum::Value(f) } => shot.formats.push(f),
            // Constraints are complete; later updates don't matter for a single frame.
            Event::Done if matches!(shot.status, Status::Constraints) => {
                if let Err(e) = start_frame(state, idx, session, qh) {
                    state.shots[idx].status = Status::Failed(e);
                }
            }
            Event::Stopped if !matches!(shot.status, Status::Ready | Status::Delivered) => {
                shot.status = Status::Failed("capture session stopped".into());
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        &idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let shot = &mut state.shots[idx];
        match event {
            Event::Ready => shot.status = Status::Ready,
            Event::Failed { reason } => shot.status = Status::Failed(format!("capture failed: {reason:?}")),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        &idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.outputs[idx].1 = Some(name);
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
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ExtImageCaptureSourceV1);
delegate_noop!(State: ExtImageCopyCaptureManagerV1);
