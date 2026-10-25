//! Window frames on COSMIC, for right-click "select window". There is no KWin script to ask,
//! but cosmic-comp tells privileged clients each toplevel's geometry per output through
//! zcosmic_toplevel_info_v1. It does not report stacking order, so the focused window counts
//! as topmost and smaller windows as above larger ones (dialogs over their parents).

use std::time::{Duration, Instant};

use wayland_client::{
    event_created_child,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_output, wl_registry},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};

use crate::screencopy::dispatch_until;

use self::protocol::toplevel_info::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
};

/// Generated from the protocol XML vendored from pop-os/cosmic-protocols (MIT-style licence;
/// the crate itself is GPL). The toplevel protocol still names the old workspace interface.
#[allow(dead_code, non_camel_case_types, non_upper_case_globals, unused_imports, clippy::all)]
mod protocol {
    pub mod workspace {
        use wayland_client;
        use wayland_client::protocol::*;
        pub mod __interfaces {
            use wayland_client::backend as wayland_backend;
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/cosmic-workspace-unstable-v1.xml");
        }
        use self::__interfaces::*;
        wayland_scanner::generate_client_code!("protocols/cosmic-workspace-unstable-v1.xml");
    }

    pub mod toplevel_info {
        use super::workspace::*;
        use wayland_client;
        use wayland_client::protocol::*;
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::*;
        use wayland_protocols::ext::workspace::v1::client::*;
        pub mod __interfaces {
            use super::super::workspace::__interfaces::*;
            use wayland_client::backend as wayland_backend;
            use wayland_client::protocol::__interfaces::*;
            use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
            use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/cosmic-toplevel-info-unstable-v1.xml");
        }
        use self::__interfaces::*;
        wayland_scanner::generate_client_code!("protocols/cosmic-toplevel-info-unstable-v1.xml");
    }
}

/// zcosmic_toplevel_handle_v1 state values.
const MINIMIZED: u32 = 1;
const ACTIVATED: u32 = 2;

/// How long to wait for the compositor's toplevel details.
const TIMEOUT: Duration = Duration::from_millis(500);

struct Toplevel {
    handle: ExtForeignToplevelHandleV1,
    app_id: String,
    /// Position and size relative to each output it is on, in logical pixels.
    geometry: Vec<(wl_output::WlOutput, [i32; 4])>,
    /// None until the compositor sent the details.
    states: Option<Vec<u32>>,
    workspaces: Vec<ExtWorkspaceHandleV1>,
}

struct State {
    info: ZcosmicToplevelInfoV1,
    outputs: Vec<(wl_output::WlOutput, Option<String>)>,
    toplevels: Vec<Toplevel>,
    active_workspaces: Vec<ExtWorkspaceHandleV1>,
}

/// Visible windows as "x,y,w,h,app_id;..." in global logical coordinates, topmost first (the
/// KWin script's format). `origins` maps output names to their logical top-left.
pub fn query(origins: &[(String, (i32, i32))]) -> Result<String, String> {
    let err = |e: &dyn std::fmt::Display| format!("toplevels: {e}");
    let conn = Connection::connect_to_env().map_err(|e| err(&e))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| err(&e))?;
    let qh = queue.handle();
    // Version 2 added per-output geometry, 3 the ext-workspace membership.
    let info = globals.bind(&qh, 2..=3, ()).map_err(|_| "compositor lacks zcosmic_toplevel_info_v1 v2")?;
    let _list: ExtForeignToplevelListV1 =
        globals.bind(&qh, 1..=1, ()).map_err(|_| "compositor lacks ext_foreign_toplevel_list_v1")?;
    let _workspaces: Option<ExtWorkspaceManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let outputs = globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|g| g.interface == "wl_output" && g.version >= 4)
        .enumerate()
        .map(|(i, g)| (globals.registry().bind(g.name, 4, &qh, i), None))
        .collect();
    let mut state = State { info, outputs, toplevels: Vec::new(), active_workspaces: Vec::new() };
    // First the toplevel and workspace lists. cosmic-comp sends the details requested for each
    // on its next refresh, which can take a frame or more; every handle starts with its state.
    queue.roundtrip(&mut state).map_err(|e| err(&e))?;
    let deadline = Instant::now() + TIMEOUT;
    while state.toplevels.iter().any(|t| t.states.is_none()) {
        dispatch_until(&conn, &mut queue, &mut state, deadline).map_err(|e| err(&e))?;
    }

    let mut windows: Vec<(bool, i32, [i32; 4], &str)> = Vec::new();
    for t in &state.toplevels {
        let states = t.states.as_deref().unwrap_or_default();
        if states.contains(&MINIMIZED) {
            continue;
        }
        if !t.workspaces.is_empty() && !t.workspaces.iter().any(|w| state.active_workspaces.contains(w)) {
            continue;
        }
        // A window spanning screens reports the same frame once per screen; any one will do.
        let frame = t.geometry.iter().find_map(|(output, [x, y, w, h])| {
            let name = state.outputs.iter().find(|(o, _)| o == output)?.1.as_ref()?;
            let (ox, oy) = origins.iter().find(|(n, _)| n == name)?.1;
            Some([ox + x, oy + y, *w, *h])
        });
        let Some(frame) = frame.filter(|f| f[2] >= 10 && f[3] >= 10) else { continue };
        windows.push((!states.contains(&ACTIVATED), frame[2] * frame[3], frame, &t.app_id));
    }
    windows.sort_by_key(|&(inactive, area, _, _)| (inactive, area));
    let list: Vec<String> = windows
        .iter()
        .map(|(_, _, [x, y, w, h], class)| format!("{x},{y},{w},{h},{class}"))
        .collect();
    Ok(list.join(";"))
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.info.get_cosmic_toplevel(&toplevel, qh, state.toplevels.len());
            state.toplevels.push(Toplevel {
                handle: toplevel,
                app_id: String::new(),
                geometry: Vec::new(),
                states: None,
                workspaces: Vec::new(),
            });
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_handle_v1::Event::AppId { app_id } = event {
            if let Some(t) = state.toplevels.iter_mut().find(|t| t.handle == *handle) {
                t.app_id = app_id;
            }
        }
    }
}

impl Dispatch<ZcosmicToplevelHandleV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        &idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zcosmic_toplevel_handle_v1::Event;
        let t = &mut state.toplevels[idx];
        match event {
            Event::Geometry { output, x, y, width, height } => {
                t.geometry.retain(|(o, _)| *o != output);
                t.geometry.push((output, [x, y, width, height]));
            }
            Event::State { state } => {
                t.states = Some(state.chunks_exact(4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]])).collect());
            }
            Event::ExtWorkspaceEnter { workspace } => t.workspaces.push(workspace),
            Event::ExtWorkspaceLeave { workspace } => t.workspaces.retain(|w| *w != workspace),
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtWorkspaceManagerV1,
        _: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_workspace_handle_v1::Event::State { state: ws } = event {
            let active = ws.into_result().is_ok_and(|s| s.contains(ext_workspace_handle_v1::State::Active));
            state.active_workspaces.retain(|w| w != handle);
            if active {
                state.active_workspaces.push(handle.clone());
            }
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

wayland_client::delegate_noop!(State: ignore ZcosmicToplevelInfoV1);
wayland_client::delegate_noop!(State: ignore ExtWorkspaceGroupHandleV1);
