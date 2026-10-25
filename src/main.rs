mod app;
mod browser;
mod capture;
mod dbus;
mod deliver;
mod editor;
mod render;
mod screencopy;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use smithay_client_toolkit::reexports::calloop::channel;

pub enum Msg {
    /// The shortcut was pressed (or `quicksnap capture` was run).
    Trigger,
    /// Close the overlay without doing anything (`quicksnap cancel`).
    Cancel,
    /// Window frames reported by the KWin script: "x,y,w,h,class;...", topmost first.
    Windows(String),
    /// A screen finished capturing for session `id`.
    Captured(u64, String, Result<capture::Image, String>),
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        None | Some("daemon") => daemon(),
        Some("capture") => client("Capture", &()),
        Some("cancel") => client("Cancel", &()),
        Some("bench") => client("Bench", &(std::env::args().skip(2).collect::<Vec<_>>(),)),
        Some(_) => {
            eprintln!("usage: quicksnap [daemon|capture|cancel|bench [OUTPUT...]]");
            ExitCode::FAILURE
        }
    }
}

fn daemon() -> ExitCode {
    let (tx, rx) = channel::channel();
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let conn = match dbus::start(tx.clone(), outputs.clone()) {
        Ok(conn) => conn,
        // Already running: behave like a launcher and take a screenshot.
        Err(zbus::Error::NameTaken) => return client("Capture", &()),
        Err(e) => {
            eprintln!("quicksnap: cannot claim {}: {e}", dbus::BUS_NAME);
            return ExitCode::FAILURE;
        }
    };
    if capture::is_kwin(&conn) {
        dbus::listen_shortcut(conn.clone(), tx.clone());
        match dbus::register_shortcut(&conn) {
            Ok(keys) if keys.iter().any(|&k| k != 0) => eprintln!("quicksnap: shortcut active: {keys:?}"),
            Ok(_) => eprintln!("quicksnap: no shortcut bound (is Print taken by another app?)"),
            Err(e) => eprintln!("quicksnap: cannot register shortcut: {e}"),
        }
    } else {
        // No kglobalaccel here: the desktop's own shortcut runs `quicksnap capture` (install.sh).
        eprintln!("quicksnap: not on KWin; capturing via ext-image-copy-capture");
    }
    match app::run(conn, tx, rx, outputs) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("quicksnap: {e}");
            ExitCode::FAILURE
        }
    }
}

fn client<B>(method: &str, body: &B) -> ExitCode
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    match dbus::call_daemon(method, body) {
        Ok(reply) => {
            if let Ok(text) = reply.body().deserialize::<String>() {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("quicksnap: daemon not reachable: {e}");
            ExitCode::FAILURE
        }
    }
}
