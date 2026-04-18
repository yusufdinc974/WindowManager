//! Smithay protocol handler implementations for `State`.
//!
//! Extracted from `main.rs` during Phase 10 (The Grand Refactoring).
//! Every `impl XxxHandler for State` and its paired `delegate_xxx!(State);`
//! macro call lives here, along with two small private helpers that are
//! only invoked from `CompositorHandler::commit` (layer-shell arrange +
//! xdg-toplevel initial configure).

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::utils::on_commit_buffer_handler,
    },
    delegate_compositor, delegate_dmabuf, delegate_layer_shell, delegate_output, delegate_seat,
    delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{layer_map_for_output, LayerSurface, Space, Window, WindowSurfaceType},
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    output::Output,
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            protocol::{
                wl_buffer::WlBuffer, wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface,
            },
            Client,
        },
    },
    utils::{Serial, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_parent, is_sync_subsurface, with_states, CompositorClientState,
            CompositorHandler, CompositorState,
        },
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        shell::{
            wlr_layer::{
                Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::{
                decoration::XdgDecorationHandler,
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
                XdgToplevelSurfaceData,
            },
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::{info, warn};

use crate::state::{ClientState, State};

// -------------------------------------------------------------------------
// SeatHandler
// -------------------------------------------------------------------------

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}
delegate_seat!(State);

// -------------------------------------------------------------------------
// CompositorHandler / ShmHandler / BufferHandler
// -------------------------------------------------------------------------

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }

            let ws = &self.workspaces[self.active_workspace];
            if let Some(window) = ws
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            }
        }

        handle_initial_configure(surface, &self.workspaces[self.active_workspace].space);

        // If this commit belongs to a layer surface, arrange the layer map
        // (may have changed the exclusive zone) and retile all workspaces.
        if handle_layer_commit(surface, &self.output) {
            let out = self.output.clone();
            for ws in self.workspaces.iter_mut() {
                Self::recalculate_layout_for(ws, &out);
            }
        }

        // A client committed new content — we need to repaint.
        self.needs_redraw = true;

        if let Err(err) = self.display_handle.flush_clients() {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                warn!(?err, "flush_clients failed in commit handler");
            }
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_compositor!(State);
delegate_shm!(State);

// -------------------------------------------------------------------------
// DmabufHandler
// -------------------------------------------------------------------------

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let _ = notifier.successful::<State>();
    }
}

delegate_dmabuf!(State);

// -------------------------------------------------------------------------
// XdgShellHandler
// -------------------------------------------------------------------------

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!(
            workspace = self.active_workspace + 1,
            "new xdg toplevel on workspace"
        );

        let window = Window::new_wayland_window(surface);
        let ws = &mut self.workspaces[self.active_workspace];
        ws.windows.push(window.clone());
        Self::recalculate_layout_for(ws, &self.output);

        let wl_surface = window.toplevel().unwrap().wl_surface().clone();
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, Some(wl_surface), serial);

        self.needs_redraw = true;

        if let Err(err) = self.display_handle.flush_clients() {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                warn!(?err, "flush_clients failed after new_toplevel");
            }
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        info!("xdg toplevel destroyed");

        let dying = surface.wl_surface().clone();

        // Find which workspace owns this surface and clean it up.
        for (i, ws) in self.workspaces.iter_mut().enumerate() {
            let had_window = ws.windows.len();
            ws.windows
                .retain(|w| w.toplevel().map(|t| t.wl_surface()) != Some(&dying));

            if ws.windows.len() != had_window {
                let dead = ws
                    .space
                    .elements()
                    .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&dying))
                    .cloned();
                if let Some(window) = dead {
                    ws.space.unmap_elem(&window);
                }
                // We can't call Self::recalculate_layout_for here because
                // we have a mutable borrow on self.workspaces via the
                // iterator. We'll note the index and do it after.
                info!(workspace = i + 1, "removed destroyed toplevel from workspace");
                break;
            }
        }

        // Recalculate layout for all workspaces that might have changed.
        // In practice only one did, but this is cheap and correct.
        for ws in self.workspaces.iter_mut() {
            Self::recalculate_layout_for(ws, &self.output);
        }

        // Update keyboard focus to whatever is on top of the active workspace.
        let focus = self.workspaces[self.active_workspace]
            .windows
            .first()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, focus, serial);

        self.needs_redraw = true;

        if let Err(err) = self.display_handle.flush_clients() {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                warn!(?err, "flush_clients failed after toplevel_destroyed");
            }
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let _ = self.popups.track_popup(
            smithay::desktop::PopupKind::Xdg(surface),
        );
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}
    fn move_request(&mut self, _surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {}

    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
    }
}

delegate_xdg_shell!(State);

// -------------------------------------------------------------------------
// XdgDecorationHandler
// -------------------------------------------------------------------------

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: zxdg_toplevel_decoration_v1::Mode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }
}

delegate_xdg_decoration!(State);

// -------------------------------------------------------------------------
// WlrLayerShellHandler
// -------------------------------------------------------------------------

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        info!(%namespace, "new wlr layer surface");

        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .unwrap_or_else(|| self.output.clone());

        let layer = LayerSurface::new(surface, namespace);
        {
            let mut map = layer_map_for_output(&output);
            if let Err(err) = map.map_layer(&layer) {
                warn!(?err, "failed to map layer surface");
                return;
            }
        }

        // Exclusive zone may have shifted — retile every workspace so the
        // master/stack layout avoids the newly-reserved strip.
        let out = self.output.clone();
        for ws in self.workspaces.iter_mut() {
            Self::recalculate_layout_for(ws, &out);
        }
        self.needs_redraw = true;
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let output = self.output.clone();
        {
            let mut map = layer_map_for_output(&output);
            let found = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = found {
                map.unmap_layer(&layer);
            }
        }

        // Reclaim the space that used to be reserved for this surface.
        for ws in self.workspaces.iter_mut() {
            Self::recalculate_layout_for(ws, &output);
        }
        self.needs_redraw = true;
    }
}

delegate_layer_shell!(State);

// -------------------------------------------------------------------------
// OutputHandler
// -------------------------------------------------------------------------

impl OutputHandler for State {}
delegate_output!(State);

// -------------------------------------------------------------------------
// Private helpers (only used from CompositorHandler::commit)
// -------------------------------------------------------------------------

/// If `surface` belongs to a layer-shell surface on `output`, arrange the
/// layer map and send the initial configure (if not yet sent). Returns
/// `true` if the surface was a layer surface (so callers can retile).
fn handle_layer_commit(surface: &WlSurface, output: &Output) -> bool {
    let mut map = layer_map_for_output(output);
    let Some(layer) = map
        .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
        .cloned()
    else {
        return false;
    };

    map.arrange();

    let initial_configure_sent = with_states(surface, |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .map(|data| data.lock().unwrap().initial_configure_sent)
            .unwrap_or(false)
    });
    if !initial_configure_sent {
        layer.layer_surface().send_configure();
    }
    true
}

fn handle_initial_configure(surface: &WlSurface, space: &Space<Window>) {
    if let Some(window) = space
        .elements()
        .find(|w| w.toplevel().unwrap().wl_surface() == surface)
    {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });
        if !initial_configure_sent {
            window.toplevel().unwrap().send_configure();
        }
    }
}
