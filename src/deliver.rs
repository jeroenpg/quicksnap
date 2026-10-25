use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use zbus::blocking::Connection;

use crate::dbus;

#[derive(Clone, Copy)]
pub enum Action {
    Copy,
    Save,
}

/// Encode and copy/save. Runs on a worker thread after the overlay is already gone.
pub fn deliver(action: Action, w: u32, h: u32, rgb: Vec<u8>, conn: &Connection) {
    let result = encode(w, h, &rgb).and_then(|png| match action {
        Action::Copy => copy(&png),
        Action::Save => save(&png).map(|path| {
            dbus::notify(conn, "Screenshot saved", &path.display().to_string());
        }),
    });
    if let Err(e) = result {
        eprintln!("quicksnap: {e}");
        dbus::notify(conn, "Screenshot failed", &e);
    }
}

fn encode(w: u32, h: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header().map_err(|e| format!("png: {e}"))?;
    writer.write_image_data(rgb).map_err(|e| format!("png: {e}"))?;
    writer.finish().map_err(|e| format!("png: {e}"))?;
    Ok(out)
}

fn copy(png: &[u8]) -> Result<(), String> {
    let mut child = Command::new("wl-copy")
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("wl-copy: {e}"))?;
    child.stdin.take().unwrap().write_all(png).map_err(|e| format!("wl-copy: {e}"))?;
    child.wait().map_err(|e| format!("wl-copy: {e}"))?;
    Ok(())
}

fn save(png: &[u8]) -> Result<PathBuf, String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    let dir = pictures_dir(&home).unwrap_or_else(|| home.join("Pictures"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem = chrono::Local::now().format("Screenshot_%Y%m%d_%H%M%S").to_string();
    let mut path = dir.join(format!("{stem}.png"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.png"));
        n += 1;
    }
    std::fs::write(&path, png).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// XDG_PICTURES_DIR from user-dirs.dirs, which is localised ("~/Afbeeldingen" in Dutch).
fn pictures_dir(home: &std::path::Path) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    let dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    let value = dirs.lines().find_map(|l| l.trim().strip_prefix("XDG_PICTURES_DIR="))?;
    let value = value.trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    // "$HOME/" alone means the feature is disabled.
    (path.is_absolute() && path != home).then_some(path)
}
