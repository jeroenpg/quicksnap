use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::OnceLock;
use std::time::Instant;

use zbus::blocking::Connection;
use zbus::zvariant::{Fd, OwnedValue, Value};

/// A captured screen in BGRA byte order (what wl_shm XRGB8888 expects on little endian),
/// plus a pre-dimmed copy used as the overlay background. The alpha byte is ignored.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub dimmed: Vec<u8>,
}

/// Brightness of the area outside the selection, out of 256.
const DIM: u32 = 120;

/// Linux caps unprivileged pipes at /proc/sys/fs/pipe-max-size (1 MiB by default).
const PIPE_SIZE: i32 = 1 << 20;

/// Whether KWin runs this session. Otherwise (COSMIC, other wlroots-style compositors) screens
/// are captured with the ext-image-copy-capture Wayland protocol instead.
pub fn is_kwin(conn: &Connection) -> bool {
    static KWIN: OnceLock<bool> = OnceLock::new();
    *KWIN.get_or_init(|| {
        conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &("org.kde.KWin",),
        )
        .and_then(|reply| reply.body().deserialize())
        .unwrap_or(false)
    })
}

/// Name of the output under the mouse (KWin's active screen follows the mouse by default).
pub fn active_output(conn: &Connection) -> Option<String> {
    let reply = conn
        .call_method(Some("org.kde.KWin"), "/KWin", Some("org.kde.KWin"), "activeOutputName", &())
        .ok()?;
    reply.body().deserialize().ok()
}

/// An output name and the byte size its capture will likely have.
pub type OutputSize = (String, usize);

/// Capture every output, the one under the mouse first and alone. KWin does the readback
/// on its main thread, so concurrent requests are slow and delay our overlay from mapping.
/// Then `before_rest(shown_first)` runs (the app waits there until that overlay is on
/// screen; `false` aborts); the rest are then taken one by one so KWin is never blocked long.
/// Without KWin all screens are captured at once and `before_rest(false)` runs alongside.
pub fn capture_all(
    conn: &Connection,
    outputs: Vec<OutputSize>,
    done: impl Fn(String, Result<Image, String>),
    before_rest: impl FnOnce(bool) -> bool + Send,
) {
    if !is_kwin(conn) {
        return crate::screencopy::capture_all(outputs, done, before_rest);
    }
    let active = active_output(conn);
    let (first, rest): (Vec<_>, Vec<_>) =
        outputs.into_iter().partition(|(name, _)| Some(name) == active.as_ref());
    let had_first = !first.is_empty();
    for (name, expected) in first {
        let result = capture_screen(conn, &name, expected);
        done(name, result);
    }
    if !before_rest(had_first) {
        return;
    }
    for (name, expected) in rest {
        let result = capture_screen(conn, &name, expected);
        done(name, result);
    }
}

/// Grab one output straight from KWin as raw pixels (no PNG round trip).
/// `expected` is the likely byte size, used to size the buffer up front.
pub fn capture_screen(conn: &Connection, output: &str, expected: usize) -> Result<Image, String> {
    let started = Instant::now();
    let (mut reader, writer) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
    // A bigger pipe means far fewer wakeups while KWin streams tens of megabytes.
    unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETPIPE_SZ, PIPE_SIZE) };
    // KWin writes into the pipe; read concurrently so a full pipe never stalls it.
    let reader_thread = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(expected + 1);
        reader.read_to_end(&mut buf).map(|_| buf)
    });

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("include-cursor", Value::from(false));
    options.insert("native-resolution", Value::from(true));

    let writer: OwnedFd = writer.into();
    let reply = conn.call_method(
        Some("org.kde.KWin"),
        "/org/kde/KWin/ScreenShot2",
        Some("org.kde.KWin.ScreenShot2"),
        "CaptureScreen",
        &(output, options, Fd::from(writer.as_fd())),
    );
    let replied = started.elapsed();
    // Drop our write end so the reader sees EOF once KWin closes its copy.
    drop(writer);
    let raw = reader_thread
        .join()
        .map_err(|_| "pipe reader panicked".to_string())?
        .map_err(|e| format!("read pipe: {e}"))?;
    let fetched = started.elapsed();

    let reply = reply.map_err(|e| format!("KWin CaptureScreen({output}): {e}"))?;
    let meta: HashMap<String, OwnedValue> =
        reply.body().deserialize().map_err(|e| format!("KWin reply: {e}"))?;
    let get = |key: &str| -> Result<u32, String> {
        meta.get(key)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("KWin reply is missing {key}"))
    };
    // QImage formats: RGB32/ARGB32(_Premultiplied) are BGRA in memory, RGBX/RGBA8888 are RGBA.
    let swap = match get("format")? {
        4 | 5 | 6 => false,
        16 | 17 | 18 => true,
        other => return Err(format!("unsupported QImage format {other}")),
    };
    let img = convert(raw, get("width")?, get("height")?, get("stride")?, swap)?;
    if std::env::var_os("QUICKSNAP_DEBUG").is_some() {
        eprintln!(
            "quicksnap: {output}: kwin reply {replied:?}, pixels {fetched:?}, ready {:?}",
            started.elapsed()
        );
    }
    Ok(img)
}

/// Turn raw 4-byte pixels into an `Image`; `swap` means they are RGBA rather than BGRA.
pub fn convert(raw: Vec<u8>, width: u32, height: u32, stride: u32, swap: bool) -> Result<Image, String> {
    let (w, h, stride) = (width as usize, height as usize, stride as usize);
    if stride < w * 4 || raw.len() < stride * h {
        return Err(format!("short image: {} bytes for {width}x{height}", raw.len()));
    }

    // Reuse the captured buffer when it is already tightly packed (the usual case).
    let mut data = if stride == w * 4 {
        let mut raw = raw;
        raw.truncate(w * h * 4);
        raw
    } else {
        (0..h).flat_map(|y| &raw[y * stride..y * stride + w * 4]).copied().collect()
    };
    if swap {
        data.chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
    }

    // Dimming is a straight per-byte multiply; split it across cores.
    let mut dimmed = vec![0u8; data.len()];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let chunk = data.len().div_ceil(threads).next_multiple_of(4).max(4);
    std::thread::scope(|s| {
        for (src, dst) in data.chunks(chunk).zip(dimmed.chunks_mut(chunk)) {
            s.spawn(move || {
                for (d, &c) in dst.iter_mut().zip(src) {
                    *d = ((c as u32 * DIM) >> 8) as u8;
                }
            });
        }
    });
    Ok(Image { width, height, data, dimmed })
}
