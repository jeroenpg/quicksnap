use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use crate::capture::{self, OutputSize};
use crate::Msg;

pub const BUS_NAME: &str = "dev.triv.QuickSnap";
const OBJECT_PATH: &str = "/dev/triv/QuickSnap";

const KGA: &str = "org.kde.kglobalaccel";
const KGA_PATH: &str = "/kglobalaccel";
const KGA_IFACE: &str = "org.kde.KGlobalAccel";
const COMPONENT: &str = "quicksnap";
const ACTION: &str = "capture";
/// Qt::Key_Print
const KEY_PRINT: i32 = 0x0100_0009;

struct Service {
    tx: Mutex<Sender<Msg>>,
    outputs: Arc<Mutex<Vec<OutputSize>>>,
}

#[zbus::interface(name = "dev.triv.QuickSnap")]
impl Service {
    fn capture(&self) {
        let _ = self.tx.lock().unwrap().send(Msg::Trigger);
    }

    fn cancel(&self) {
        let _ = self.tx.lock().unwrap().send(Msg::Cancel);
    }

    /// Called back by the KWin script started in `query_windows`.
    fn windows(&self, list: String) {
        let _ = self.tx.lock().unwrap().send(Msg::Windows(list));
    }

    /// Time a capture of every output without showing anything.
    fn bench(&self, only: Vec<String>) -> String {
        let mut outputs = self.outputs.lock().unwrap().clone();
        if !only.is_empty() {
            outputs.retain(|(name, _)| only.contains(name));
        }
        // Own connection and thread: blocking on this one would stall its executor.
        std::thread::spawn(move || bench(outputs))
            .join()
            .unwrap_or_else(|_| "bench panicked".into())
    }
}

/// Claim the bus name. Fails with `zbus::Error::NameTaken` if a daemon already runs.
pub fn start(tx: Sender<Msg>, outputs: Arc<Mutex<Vec<OutputSize>>>) -> zbus::Result<Connection> {
    zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, Service { tx: Mutex::new(tx), outputs })?
        .build()
}

/// Register the global shortcut (Print by default; rebindable in System Settings).
pub fn register_shortcut(conn: &Connection) -> zbus::Result<Vec<i32>> {
    // Vecs, not arrays: zvariant encodes fixed-size arrays as structs.
    let id = vec![COMPONENT, ACTION, "QuickSnap", "Capture screen region"];
    let keys = vec![KEY_PRINT];
    conn.call_method(Some(KGA), KGA_PATH, Some(KGA_IFACE), "doRegister", &(&id,))?;
    // IsDefault = 1: record Print as the default.
    conn.call_method(Some(KGA), KGA_PATH, Some(KGA_IFACE), "setShortcut", &(&id, &keys, 1u32))?;
    // SetPresent = 2 with autoloading: keep whatever the user configured, else the default.
    let reply =
        conn.call_method(Some(KGA), KGA_PATH, Some(KGA_IFACE), "setShortcut", &(&id, &keys, 2u32))?;
    reply.body().deserialize()
}

pub fn listen_shortcut(conn: Connection, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.kde.kglobalaccel.Component")
            .and_then(|r| r.member("globalShortcutPressed"))
            .map(|r| r.build());
        let iter = match rule.and_then(|r| zbus::blocking::MessageIterator::for_match_rule(r, &conn, None)) {
            Ok(iter) => iter,
            Err(e) => return eprintln!("quicksnap: cannot listen for shortcut: {e}"),
        };
        for msg in iter.flatten() {
            if let Ok((component, action, _)) = msg.body().deserialize::<(String, String, i64)>() {
                if component == COMPONENT && action == ACTION && tx.send(Msg::Trigger).is_err() {
                    break;
                }
            }
        }
    });
}

const WINDOWS_SCRIPT: &str = include_str!("windows.js");
const WINDOWS_PLUGIN: &str = "quicksnap-windows";

/// Ask KWin for the window layout; the answer arrives later as `Msg::Windows`.
/// Plain clients can't see other windows on Wayland, but a KWin script can.
pub fn query_windows(conn: &Connection) {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into());
    let path = std::path::Path::new(&dir).join("quicksnap-windows.js");
    let script = WINDOWS_SCRIPT.replace("__PID__", &std::process::id().to_string());
    if std::fs::write(&path, script).is_err() {
        return;
    }
    let (kwin, scripting) = (Some("org.kde.KWin"), Some("org.kde.kwin.Scripting"));
    // The previous run's script is still loaded under the same name; replace it.
    let _ = conn.call_method(kwin, "/Scripting", scripting, "unloadScript", &(WINDOWS_PLUGIN,));
    let path = path.to_string_lossy();
    let loaded = conn.call_method(kwin, "/Scripting", scripting, "loadScript", &(path.as_ref(), WINDOWS_PLUGIN));
    let Ok(reply) = loaded else { return };
    let Ok(id) = reply.body().deserialize::<i32>() else { return };
    let _ = conn.call_method(
        kwin,
        format!("/Scripting/Script{id}").as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    );
}

pub fn notify(conn: &Connection, summary: &str, body: &str) {
    let hints: HashMap<&str, Value> = HashMap::new();
    let _ = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &("QuickSnap", 0u32, "applets-screenshooter", summary, body, Vec::<&str>::new(), hints, 3000i32),
    );
}

/// Client side: ask the running daemon to do something.
pub fn call_daemon<B>(method: &str, body: &B) -> zbus::Result<zbus::Message>
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    let conn = Connection::session()?;
    conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(BUS_NAME), method, body)
}

fn bench(outputs: Vec<OutputSize>) -> String {
    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => return format!("dbus: {e}"),
    };
    let start = Instant::now();
    let lines = Mutex::new(Vec::new());
    capture::capture_all(
        &conn,
        outputs,
        |name, result| {
            let at = start.elapsed().as_millis();
            lines.lock().unwrap().push(match result {
                Ok(img) => format!("{name}: {}x{} ready at {at} ms", img.width, img.height),
                Err(e) => format!("{name}: FAILED {e}"),
            });
        },
        |_| true,
    );
    let mut out = lines.into_inner().unwrap().join("\n");
    out += &format!("\nall screens ready in {} ms", start.elapsed().as_millis());
    out
}
