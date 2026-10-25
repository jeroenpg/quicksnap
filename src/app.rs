use std::error::Error;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel::{Channel, Event as ChannelEvent, Sender},
            EventLoop,
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use wayland_client::{
    delegate_noop,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use crate::capture::{self, Image, OutputSize};
use crate::deliver::{self, Action};
use crate::editor::{Editor, Sel, Tool, View};
use crate::{browser, render, Msg};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

struct Overlay {
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    pool: Option<SlotPool>,
    buffer: Option<Buffer>,
    img: Image,
    /// Top-left of the output in global logical coordinates.
    origin: (f32, f32),
    /// Physical image pixels per logical surface pixel.
    scale: f32,
    configured: bool,
    dirty: bool,
    frame_pending: bool,
}

impl Overlay {
    fn view(&self) -> View {
        View { w: self.img.width as f32, h: self.img.height as f32, scale: self.scale }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        if let Some(vp) = self.viewport.take() {
            vp.destroy();
        }
    }
}

struct Session {
    id: u64,
    started: Instant,
    pending: usize,
    overlays: Vec<Overlay>,
    errors: Vec<String>,
    editor: Editor,
    /// Screen the pointer is on.
    hover: Option<usize>,
    cursor: Option<CursorIcon>,
    shown: bool,
    /// Releases the capture of the remaining screens once the first overlay is visible.
    release_rest: Option<mpsc::SyncSender<()>>,
    /// Windows in global logical coordinates, topmost first.
    windows: Vec<Win>,
}

pub struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    viewporter: Option<WpViewporter>,
    conn: Connection,
    qh: QueueHandle<App>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
    modifiers: Modifiers,
    dbus: zbus::blocking::Connection,
    tx: Sender<Msg>,
    outputs: Arc<Mutex<Vec<OutputSize>>>,
    next_id: u64,
    session: Option<Session>,
}

pub fn run(
    dbus: zbus::blocking::Connection,
    tx: Sender<Msg>,
    rx: Channel<Msg>,
    outputs: Arc<Mutex<Vec<OutputSize>>>,
) -> Result<(), Box<dyn Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init::<App>(&conn)?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| e.error.to_string())?;
    event_loop
        .handle()
        .insert_source(rx, |event, _, app: &mut App| {
            if let ChannelEvent::Msg(msg) = event {
                app.handle(msg);
            }
        })
        .map_err(|e| e.error.to_string())?;

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh)?,
        layer_shell: LayerShell::bind(&globals, &qh)?,
        shm: Shm::bind(&globals, &qh)?,
        viewporter: globals.bind(&qh, 1..=1, ()).ok(),
        conn,
        qh,
        keyboard: None,
        pointer: None,
        modifiers: Modifiers::default(),
        dbus,
        tx,
        outputs,
        next_id: 0,
        session: None,
    };
    loop {
        event_loop.dispatch(None, &mut app)?;
    }
}

impl App {
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Trigger => self.start_session(),
            Msg::Cancel => self.close(),
            Msg::Windows(list) => {
                if let Some(sess) = self.session.as_mut() {
                    sess.windows = parse_windows(&list);
                }
            }
            Msg::Captured(id, name, result) => self.on_captured(id, name, result),
        }
    }

    fn output_names(&self) -> Vec<(String, wl_output::WlOutput)> {
        self.output_state
            .outputs()
            .filter_map(|o| Some((self.output_state.info(&o)?.name?, o)))
            .collect()
    }

    fn start_session(&mut self) {
        if self.session.is_some() {
            return;
        }
        let outputs = self.output_names();
        if outputs.is_empty() {
            return;
        }
        self.next_id += 1;
        let id = self.next_id;
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        self.session = Some(Session {
            id,
            started: Instant::now(),
            pending: outputs.len(),
            overlays: Vec::new(),
            errors: Vec::new(),
            editor: Editor::default(),
            hover: None,
            cursor: None,
            shown: false,
            release_rest: Some(release_tx),
            windows: Vec::new(),
        });
        let sizes = outputs.iter().map(|(name, output)| (name.clone(), self.expected_size(output))).collect();
        let (conn, tx) = (self.dbus.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let windows_conn = conn.clone();
            capture::capture_all(
                &conn,
                sizes,
                |name, result| {
                    let _ = tx.send(Msg::Captured(id, name, result));
                },
                move |shown_first| {
                    if shown_first {
                        let released = release_rx.recv_timeout(Duration::from_millis(500));
                        // Disconnected means the session ended (Esc): skip the other screens.
                        if matches!(released, Err(mpsc::RecvTimeoutError::Disconnected)) {
                            return false;
                        }
                    }
                    // Only now, so it never delays the first overlay.
                    // Without KWin there is no window list: right-click selects the whole screen.
                    if capture::is_kwin(&windows_conn) {
                        crate::dbus::query_windows(&windows_conn);
                    }
                    true
                },
            );
        });
    }

    fn on_captured(&mut self, id: u64, name: String, result: Result<Image, String>) {
        let output = self.output_names().into_iter().find(|(n, _)| *n == name).map(|(_, o)| o);
        let origin = output
            .as_ref()
            .and_then(|o| self.output_state.info(o)?.logical_position)
            .map_or((0.0, 0.0), |(x, y)| (x as f32, y as f32));
        let Some(sess) = self.session.as_mut().filter(|s| s.id == id) else { return };
        sess.pending -= 1;
        match (result, output) {
            (Ok(img), Some(output)) => {
                let surface = self.compositor.create_surface(&self.qh);
                let layer = self.layer_shell.create_layer_surface(
                    &self.qh,
                    surface,
                    Layer::Overlay,
                    Some("quicksnap"),
                    Some(&output),
                );
                layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                layer.set_exclusive_zone(-1);
                layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                layer.set_size(0, 0);
                let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(layer.wl_surface(), &self.qh, ()));
                layer.commit();
                sess.overlays.push(Overlay {
                    layer,
                    viewport,
                    pool: None,
                    buffer: None,
                    img,
                    origin,
                    scale: 1.0,
                    configured: false,
                    dirty: true,
                    frame_pending: false,
                });
            }
            (Ok(_), None) => sess.errors.push(format!("{name}: output disappeared")),
            (Err(e), _) => {
                sess.errors.push(e);
                // Nothing will be shown for this screen; don't hold up the others.
                if let Some(release) = sess.release_rest.take() {
                    let _ = release.send(());
                }
            }
        }
        if sess.pending == 0 {
            eprintln!(
                "quicksnap: captured {} screen(s) in {} ms",
                sess.overlays.len(),
                sess.started.elapsed().as_millis()
            );
            for e in &sess.errors {
                eprintln!("quicksnap: {e}");
            }
            if sess.overlays.is_empty() {
                let msg = sess.errors.join("\n");
                self.session = None;
                crate::dbus::notify(&self.dbus, "Screenshot failed", &msg);
            }
        }
    }

    fn close(&mut self) {
        self.session = None;
        let _ = self.conn.flush();
    }

    fn finish(&mut self, action: Action) {
        let Some(sess) = self.session.as_ref() else { return };
        let ed = &sess.editor;
        let idx = ed.screen.filter(|_| ed.sel.is_some()).or(sess.hover).unwrap_or(0);
        let Some(ov) = sess.overlays.get(idx) else { return };
        let (w, h) = (ov.img.width, ov.img.height);
        let rect = match ed.sel.filter(|_| ed.screen == Some(idx)) {
            Some(sel) => sel.pixels(w, h),
            None => (0, 0, w, h),
        };
        if rect.2 <= rect.0 || rect.3 <= rect.1 {
            return;
        }
        let (w, h, rgb) = render::export(&ov.img, ed, rect, ov.scale);
        // Hide first; encoding and saving happen in the background.
        self.close();
        let dbus = self.dbus.clone();
        std::thread::spawn(move || deliver::deliver(action, w, h, rgb, &dbus));
    }

    fn overlay_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.session.as_ref()?.overlays.iter().position(|o| o.layer.wl_surface() == surface)
    }
}

struct Win {
    /// x, y, width, height
    rect: [f32; 4],
    class: String,
}

fn parse_windows(list: &str) -> Vec<Win> {
    list.split(';')
        .filter_map(|w| {
            let mut parts = w.splitn(5, ',');
            let mut rect = [0.0; 4];
            for v in &mut rect {
                *v = parts.next()?.trim().parse().ok()?;
            }
            Some(Win { rect, class: parts.next().unwrap_or("").to_string() })
        })
        .collect()
}

/// Selection (physical pixels of `ov`) for a right-click: the topmost window under the
/// pointer, clipped to this screen, or the whole screen over the desktop. For browsers it is
/// just the page, unless the pointer is on the tabs/toolbar.
fn window_under(windows: &[Win], ov: &Overlay, local: (f64, f64)) -> Sel {
    let (w, h) = (ov.img.width, ov.img.height);
    let local = (local.0 as f32, local.1 as f32);
    let (sel, win) = window_sel(windows, ov.origin, ov.scale, (w as f32, h as f32), local);
    if !win.is_some_and(|win| browser::is_browser(&win.class)) {
        return sel;
    }
    let Some((x0, y0, x1, y1)) = browser::content_rect(&ov.img.data, w, sel.pixels(w, h), ov.scale) else {
        return sel;
    };
    if local.1 * ov.scale < y0 as f32 {
        return sel; // pointer on the tabs/toolbar: whole window
    }
    Sel { x0: x0 as f32, y0: y0 as f32, x1: x1 as f32, y1: y1 as f32 }
}

fn window_sel<'a>(
    windows: &'a [Win],
    origin: (f32, f32),
    s: f32,
    (w, h): (f32, f32),
    local: (f32, f32),
) -> (Sel, Option<&'a Win>) {
    let (gx, gy) = (origin.0 + local.0, origin.1 + local.1);
    let full = Sel { x0: 0.0, y0: 0.0, x1: w, y1: h };
    let Some(win) = windows.iter().find(|win| {
        let [x, y, ww, wh] = win.rect;
        gx >= x && gx < x + ww && gy >= y && gy < y + wh
    }) else {
        return (full, None);
    };
    let [x, y, ww, wh] = win.rect;
    let clip = |v: f32, max: f32| v.clamp(0.0, max);
    let sel = Sel {
        x0: clip((x - origin.0) * s, w),
        y0: clip((y - origin.1) * s, h),
        x1: clip((x + ww - origin.0) * s, w),
        y1: clip((y + wh - origin.1) * s, h),
    };
    (sel, Some(win))
}

fn mark_dirty(sess: &mut Session, idx: usize, shm: &Shm, qh: &QueueHandle<App>) {
    let Some(ov) = sess.overlays.get_mut(idx) else { return };
    ov.dirty = true;
    if !ov.frame_pending {
        draw(ov, &sess.editor, idx, shm, qh);
    }
}

fn draw(ov: &mut Overlay, editor: &Editor, idx: usize, shm: &Shm, qh: &QueueHandle<App>) {
    if !ov.configured {
        return;
    }
    let (w, h) = (ov.img.width as i32, ov.img.height as i32);
    let stride = w * 4;
    let pool = ov
        .pool
        .get_or_insert_with(|| SlotPool::new((stride * h) as usize, shm).expect("shm pool"));
    let new_buffer =
        |pool: &mut SlotPool| pool.create_buffer(w, h, stride, wl_shm::Format::Xrgb8888).expect("shm buffer").0;
    let buffer = ov.buffer.get_or_insert_with(|| new_buffer(pool));
    // The compositor may still be reading the previous frame; use a fresh buffer then.
    if buffer.canvas(pool).is_none() {
        *buffer = new_buffer(pool);
    }
    let canvas = buffer.canvas(pool).expect("fresh buffer is free");
    render::compose(canvas, &ov.img, editor, idx, ov.scale);

    let surface = ov.layer.wl_surface();
    surface.damage_buffer(0, 0, w, h);
    surface.frame(qh, surface.clone());
    buffer.attach_to(surface).expect("attach buffer");
    ov.layer.commit();
    ov.frame_pending = true;
    ov.dirty = false;
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        let Some(idx) = self.overlay_index(surface) else { return };
        let sess = self.session.as_mut().unwrap();
        let ov = &mut sess.overlays[idx];
        ov.frame_pending = false;
        if ov.dirty {
            draw(ov, &sess.editor, idx, &self.shm, qh);
        }
        // The first overlay is on screen: now let KWin capture the other screens.
        if let Some(release) = sess.release_rest.take() {
            eprintln!("quicksnap: overlay visible {} ms after trigger", sess.started.elapsed().as_millis());
            let _ = release.send(());
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.sync_outputs();
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.sync_outputs();
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.sync_outputs();
        self.close();
    }
}

impl App {
    fn sync_outputs(&mut self) {
        let sizes = self.output_names().into_iter().map(|(n, o)| (n, self.expected_size(&o))).collect();
        *self.outputs.lock().unwrap() = sizes;
    }

    /// Bytes a capture of `output` will take: its current mode in physical pixels, 4 bytes each.
    fn expected_size(&self, output: &wl_output::WlOutput) -> usize {
        self.output_state
            .info(output)
            .and_then(|info| info.modes.iter().find(|m| m.current).map(|m| m.dimensions))
            .map_or(0, |(w, h)| w.max(0) as usize * h.max(0) as usize * 4)
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.close();
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(idx) = self.overlay_index(layer.wl_surface()) else { return };
        let sess = self.session.as_mut().unwrap();
        let ov = &mut sess.overlays[idx];
        let (lw, lh) = configure.new_size;
        if lw > 0 && lh > 0 {
            ov.scale = ov.img.width as f32 / lw as f32;
            match &ov.viewport {
                Some(vp) => vp.set_destination(lw as i32, lh as i32),
                None => ov.layer.wl_surface().set_buffer_scale(ov.scale.round().max(1.0) as i32),
            }
        }
        ov.configured = true;
        draw(ov, &sess.editor, idx, &self.shm, qh);
        if !sess.shown {
            sess.shown = true;
            eprintln!("quicksnap: overlay drawn {} ms after trigger", sess.started.elapsed().as_millis());
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let cursor_surface = self.compositor.create_surface(qh);
            self.pointer = self
                .seat_state
                .get_pointer_with_theme(qh, &seat, self.shm.wl_shm(), cursor_surface, ThemeSpec::default())
                .ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        if self.session.is_none() {
            return;
        }
        match event.keysym {
            Keysym::Escape => return self.close(),
            Keysym::Return | Keysym::KP_Enter => return self.finish(Action::Copy),
            _ => {}
        }
        let ch = event.keysym.key_char().map(|c| c.to_ascii_lowercase());
        let ctrl = self.modifiers.ctrl;
        match (ctrl, ch) {
            (true, Some('c')) => self.finish(Action::Copy),
            (true, Some('s')) => self.finish(Action::Save),
            (true, Some('z')) => {
                let sess = self.session.as_mut().unwrap();
                if sess.editor.undo() {
                    if let Some(idx) = sess.editor.screen {
                        mark_dirty(sess, idx, &self.shm, qh);
                    }
                }
            }
            (false, Some(c)) => {
                let tool = match c {
                    '1' | 'a' => Tool::Arrow,
                    '2' | 'c' | 'o' => Tool::Ellipse,
                    '3' | 'r' => Tool::Rect,
                    '4' | 'p' | 'f' => Tool::Pen,
                    _ => return,
                };
                let sess = self.session.as_mut().unwrap();
                sess.editor.tool = tool;
                if let Some(idx) = sess.editor.screen {
                    mark_dirty(sess, idx, &self.shm, qh);
                }
            }
            _ => {}
        }
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, conn: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            let Some(idx) = self.overlay_index(&event.surface) else { continue };
            let sess = self.session.as_mut().unwrap();
            let view = sess.overlays[idx].view();
            let p = (event.position.0 as f32 * view.scale, event.position.1 as f32 * view.scale);
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    sess.hover = Some(idx);
                    sess.cursor = None;
                }
                PointerEventKind::Motion { .. } => {
                    sess.hover = Some(idx);
                    if sess.editor.motion(idx, p, &view) {
                        mark_dirty(sess, idx, &self.shm, qh);
                    }
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    let prev = sess.editor.press(idx, p, &view);
                    mark_dirty(sess, idx, &self.shm, qh);
                    if let Some(prev) = prev {
                        mark_dirty(sess, prev, &self.shm, qh);
                    }
                }
                PointerEventKind::Press { button: BTN_RIGHT, .. } => {
                    let ov = &sess.overlays[idx];
                    let sel = window_under(&sess.windows, ov, event.position);
                    let prev = sess.editor.select(idx, sel);
                    mark_dirty(sess, idx, &self.shm, qh);
                    if let Some(prev) = prev {
                        mark_dirty(sess, prev, &self.shm, qh);
                    }
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    if sess.editor.release(&view) {
                        mark_dirty(sess, idx, &self.shm, qh);
                    }
                }
                _ => {}
            }
            let icon = sess.editor.cursor(idx, p, &view);
            if sess.cursor != Some(icon) {
                sess.cursor = Some(icon);
                if let Some(pointer) = &self.pointer {
                    let _ = pointer.set_cursor(conn, icon);
                }
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);
delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_click_picks_topmost_window_on_this_screen() {
        // Topmost first, as the KWin script reports them (your layout: laptop at 0,776 @2x).
        let windows = parse_windows("1500,400,800,600,a;1440,387,1920,1036,b;100,800,600,400,c;junk");
        assert_eq!(windows.len(), 3);

        // HDMI-A-1 at 1440,387, scale 1: pointer over the small top window.
        let (sel, _) = window_sel(&windows, (1440.0, 387.0), 1.0, (1920.0, 1080.0), (100.0, 50.0));
        assert_eq!(sel.pixels(1920, 1080), (60, 13, 860, 613));

        // Same screen, outside the small window: the big one behind it.
        let (sel, _) = window_sel(&windows, (1440.0, 387.0), 1.0, (1920.0, 1080.0), (1500.0, 900.0));
        assert_eq!(sel.pixels(1920, 1080), (0, 0, 1920, 1036));

        // Laptop at 0,776 with 2x scale: logical window maps to doubled physical pixels.
        let (sel, _) = window_sel(&windows, (0.0, 776.0), 2.0, (2880.0, 1920.0), (200.0, 100.0));
        assert_eq!(sel.pixels(2880, 1920), (200, 48, 1400, 848));

        // Desktop under the pointer: the whole screen.
        let (sel, _) = window_sel(&windows, (0.0, 776.0), 2.0, (2880.0, 1920.0), (1400.0, 900.0));
        assert_eq!(sel.pixels(2880, 1920), (0, 0, 2880, 1920));
    }
}
