//! Smithay protocol handler implementations for `State`.
use smithay::reexports::wayland_server::Resource;


use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{
            utils::{on_commit_buffer_handler, RendererSurfaceStateUserData},
            ImportDma,
        },
    },
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_layer_shell,
    delegate_output, delegate_seat, delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{layer_map_for_output, LayerSurface, Window, WindowSurfaceType},
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
        selection::{
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::{
            wlr_layer::{
                KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface,
                WlrLayerShellHandler, WlrLayerShellState,
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
use std::time::Instant;

use tracing::{debug, info, warn};

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
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        self.needs_redraw = true;
    }
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
        debug!(surface = ?surface.id(), "commit() called");

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
                debug!(surface = ?root.id(), "calling window.on_commit()");
                window.on_commit();
            }
        }

        handle_initial_configure_all(surface, &self.workspaces);

        // ── Layer surface commit handling ──
        let output = self.output.clone();

        // Snapshot the usable area BEFORE the layer commit processes.
        let old_non_exclusive = layer_map_for_output(&output).non_exclusive_zone();

        let (layer_commit, focus_target) = handle_layer_commit(surface, &output);

        if layer_commit {
            // Only retile if the exclusive zone actually changed
            // (bar appeared, disappeared, or resized its exclusive area).
            // Content-only repaints (clock, workspace highlight) skip this.
            let new_non_exclusive = layer_map_for_output(&output).non_exclusive_zone();

            if old_non_exclusive != new_non_exclusive {
                debug!(
                    ?old_non_exclusive,
                    ?new_non_exclusive,
                    "layer commit: exclusive zone changed — retiling all workspaces"
                );
                let outer = self.config.outer_gaps;
                let inner = self.config.inner_gaps;
                let border = self.config.border_width;
                let focused = self.keyboard.current_focus();
                for ws in self.workspaces.iter_mut() {
                    Self::recalculate_layout_for(
                        ws, &output, outer, inner, border, focused.as_ref(),
                    );
                }
            }
        }

        if let Some(target) = focus_target {
            let keyboard = self.keyboard.clone();
            if keyboard.current_focus().as_ref() != Some(&target) {
                info!(
                    surface = ?target.id(),
                    "focusing layer surface (Exclusive/OnDemand)"
                );
                let serial = SERIAL_COUNTER.next_serial();
                keyboard.set_focus(self, Some(target), serial);
            }
        }

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
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        match self.renderer.import_dmabuf(&dmabuf, None) {
            Ok(_) => {
                debug!("dmabuf imported successfully");
                let _ = notifier.successful::<State>();
            }
            Err(err) => {
                warn!(?err, "dmabuf import into renderer failed");
                notifier.failed();
            }
        }
    }
}

delegate_dmabuf!(State);

// -------------------------------------------------------------------------
// SelectionHandler / DataDeviceHandler
// -------------------------------------------------------------------------

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl WaylandDndGrabHandler for State {}

delegate_data_device!(State);

// -------------------------------------------------------------------------
// XdgShellHandler
// -------------------------------------------------------------------------

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let ws_idx = self.active_workspace;
        let win_count_before = self.workspaces[ws_idx].windows.len();

        info!(
            workspace = ws_idx + 1,
            existing_windows = win_count_before,
            surface = ?surface.wl_surface().id(),
            "new xdg toplevel on workspace"
        );

        let window = Window::new_wayland_window(surface.clone());
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;

        let layer_has_focus = self.layer_has_keyboard_focus();
        let new_surface = surface.wl_surface().clone();

        let ws = &mut self.workspaces[ws_idx];
        ws.spawn_times.insert(window.clone(), Instant::now());
        ws.windows.push(window.clone());

        debug!(
            window_count = ws.windows.len(),
            "about to recalculate_layout_for"
        );

        Self::recalculate_layout_for(
            ws,
            &self.output,
            outer,
            inner,
            border,
            if layer_has_focus { None } else { Some(&new_surface) },
        );

        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|s| {
                debug!(
                    pending_size = ?s.size,
                    pending_decoration = ?s.decoration_mode,
                    "pending state before send_configure"
                );
            });
        }

        debug!(
            surface = ?surface.wl_surface().id(),
            "sending initial configure"
        );
        surface.send_configure();

        if !layer_has_focus {
            let wl_surface = surface.wl_surface().clone();
            let serial = SERIAL_COUNTER.next_serial();
            let keyboard = self.keyboard.clone();
            keyboard.set_focus(self, Some(wl_surface), serial);
        }

        self.needs_redraw = true;

        if let Err(err) = self.display_handle.flush_clients() {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                warn!(?err, "flush_clients failed after new_toplevel");
            }
        }

        self.broadcast_workspace_state();
        debug!("new_toplevel complete");
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let dying_surface = surface.wl_surface().clone();
        info!(
            surface = ?dying_surface.id(),
            "xdg toplevel destroyed"
        );

        if let Some(ref grab) = self.pointer_grab {
            let grab_matches = grab
                .window
                .toplevel()
                .map(|t| t.wl_surface() == &dying_surface)
                .unwrap_or(false);
            if grab_matches {
                self.pointer_grab = None;
            }
        }

        for (i, ws) in self.workspaces.iter_mut().enumerate() {
            let had_window = ws.windows.len();
            ws.windows
                .retain(|w| w.toplevel().map(|t| t.wl_surface()) != Some(&dying_surface));
            ws.spawn_times
                .retain(|w, _| w.toplevel().map(|t| t.wl_surface()) != Some(&dying_surface));
            ws.configured_sizes
                .retain(|w, _| w.toplevel().map(|t| t.wl_surface()) != Some(&dying_surface));
            ws.floating
                .retain(|w| w.toplevel().map(|t| t.wl_surface()) != Some(&dying_surface));
            ws.floating_geo
                .retain(|w, _| w.toplevel().map(|t| t.wl_surface()) != Some(&dying_surface));

            if ws.windows.len() != had_window {
                let dead = ws
                    .space
                    .elements()
                    .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&dying_surface))
                    .cloned();
                if let Some(window) = dead {
                    ws.space.unmap_elem(&window);
                }
                info!(
                    workspace = i + 1,
                    remaining_windows = ws.windows.len(),
                    "removed destroyed toplevel from workspace"
                );
                break;
            }
        }

        if !self.layer_has_keyboard_focus() {
            let focus = self.workspaces[self.active_workspace]
                .windows
                .last()
                .and_then(|w| w.toplevel())
                .map(|t| t.wl_surface().clone());
            let serial = SERIAL_COUNTER.next_serial();
            let keyboard = self.keyboard.clone();
            keyboard.set_focus(self, focus.clone(), serial);
        }

        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let focus = self.keyboard.current_focus();
        for ws in self.workspaces.iter_mut() {
            Self::recalculate_layout_for(ws, &self.output, outer, inner, border, focus.as_ref());
        }

        self.needs_redraw = true;

        if let Err(err) = self.display_handle.flush_clients() {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                warn!(?err, "flush_clients failed after toplevel_destroyed");
            }
        }
        self.broadcast_workspace_state();
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
        debug!(
            surface = ?toplevel.wl_surface().id(),
            "new_decoration called"
        );
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });

        let initial_sent = with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().unwrap().initial_configure_sent)
                .unwrap_or(false)
        });
        if initial_sent {
            toplevel.send_configure();
        }
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        mode: zxdg_toplevel_decoration_v1::Mode,
    ) {
        debug!(
            surface = ?toplevel.wl_surface().id(),
            ?mode,
            "request_mode called"
        );
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        let initial_sent = with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().unwrap().initial_configure_sent)
                .unwrap_or(false)
        });
        debug!(
            initial_sent,
            surface = ?toplevel.wl_surface().id(),
            "request_mode: will send configure?"
        );
        if initial_sent {
            toplevel.send_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        debug!(
            surface = ?toplevel.wl_surface().id(),
            "unset_mode called"
        );
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        let initial_sent = with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().unwrap().initial_configure_sent)
                .unwrap_or(false)
        });
        if initial_sent {
            toplevel.send_configure();
        }
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
            map.arrange();
        }

        let out = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let focused = self.keyboard.current_focus();
        for ws in self.workspaces.iter_mut() {
            Self::recalculate_layout_for(ws, &out, outer, inner, border, focused.as_ref());
        }
        self.needs_redraw = true;
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let output = self.output.clone();

        let dying_wl = surface.wl_surface().clone();

        {
            let mut map = layer_map_for_output(&output);
            let found = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = found {
                map.unmap_layer(&layer);
            }
            map.arrange();
        }

        let focus_on_dying = self.keyboard.current_focus().as_ref() == Some(&dying_wl);
        if focus_on_dying {
            info!("focused layer surface destroyed — dropping focus to active window");
            self.drop_focus_to_active_window();
        }

        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let focused = self.keyboard.current_focus();
        for ws in self.workspaces.iter_mut() {
            Self::recalculate_layout_for(ws, &output, outer, inner, border, focused.as_ref());
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
// Private helpers
// -------------------------------------------------------------------------

fn handle_layer_commit(
    surface: &WlSurface,
    output: &Output,
) -> (bool, Option<WlSurface>) {
    let layer = {
        let map = layer_map_for_output(output);
        match map
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .cloned()
        {
            Some(l) => l,
            None => return (false, None),
        }
    };

    // Only call arrange() if there's a pending configure to process.
    // Pure content repaints (clock, workspace indicator) don't need it.
    let configure_sent = layer.layer_surface().send_pending_configure().is_some();
    if configure_sent {
        debug!(
            surface = ?surface.id(),
            "layer commit: sent configure — rearranging layer map"
        );
        let mut map = layer_map_for_output(output);
        map.arrange();
    }

    let has_buffer = with_states(surface, |states| {
        states
            .data_map
            .get::<RendererSurfaceStateUserData>()
            .map(|data| data.lock().unwrap().buffer().is_some())
            .unwrap_or(false)
    });

    if !has_buffer {
        return (true, None);
    }

    let interactivity = layer.cached_state().keyboard_interactivity;
    let focus_target = match interactivity {
        KeyboardInteractivity::Exclusive | KeyboardInteractivity::OnDemand => {
            Some(layer.wl_surface().clone())
        }
        KeyboardInteractivity::None => {
            if layer.can_receive_keyboard_focus() {
                debug!(
                    surface = ?surface.id(),
                    "layer commit: cached_state says None but can_receive_keyboard_focus is true"
                );
                Some(layer.wl_surface().clone())
            } else {
                None
            }
        }
    };

    (true, focus_target)
}

fn handle_initial_configure_all(surface: &WlSurface, workspaces: &[crate::state::Workspace]) {
    for ws in workspaces {
        if let Some(window) = ws
            .space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface() == surface).unwrap_or(false))
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
                debug!(
                    surface = ?surface.id(),
                    "handle_initial_configure_all: sending initial configure"
                );
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_configure();
                }
            }
            return;
        }
    }
}