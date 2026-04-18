//! Core compositor state types.
//!
//! Extracted from `main.rs` during Phase 10 (The Grand Refactoring).
//! This module contains only type definitions and trivial constructors —
//! the actual wiring (protocol globals, event sources, DRM bringup) still
//! lives in `main.rs` and `backend.rs`.

use std::{collections::HashSet, ffi::OsString, time::Instant};

use smithay::{
    backend::{
        allocator::gbm::GbmAllocator,
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDeviceFd,
            DrmNode,
        },
        renderer::gles::GlesRenderer,
    },
    desktop::{PopupManager, Space, Window},
    input::{keyboard::KeyboardHandle, pointer::PointerHandle, Seat, SeatState},
    output::Output,
    reexports::{
        calloop::LoopSignal,
        drm::control::crtc,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
            DisplayHandle,
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        dmabuf::DmabufState,
        output::OutputManagerState,
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
    },
};
use tracing::{debug, info, warn};

pub const CLEAR_COLOR: [f32; 4] = [0.08, 0.05, 0.14, 1.0];
pub const NUM_WORKSPACES: usize = 9;

// -------------------------------------------------------------------------
// Workspace
// -------------------------------------------------------------------------

pub struct Workspace {
    pub space: Space<Window>,
    pub windows: Vec<Window>,
}

impl Workspace {
    pub fn new(output: &Output) -> Self {
        let mut space = Space::default();
        space.map_output(output, (0, 0));
        Self {
            space,
            windows: Vec::new(),
        }
    }
}

// -------------------------------------------------------------------------
// Compositor state
// -------------------------------------------------------------------------

pub struct State {
    pub start_time: Instant,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub socket_name: OsString,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub dmabuf_state: DmabufState,

    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub pointer_location: Point<f64, Logical>,

    pub workspaces: Vec<Workspace>,
    pub active_workspace: usize,
    pub output: Output,
    pub popups: PopupManager,

    /// Set to true whenever the scene changes and a new frame should
    /// be rendered.
    pub needs_redraw: bool,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

// -------------------------------------------------------------------------
// DRM backend
// -------------------------------------------------------------------------

pub type WmDrmCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

pub struct DrmBackend {
    #[allow(dead_code)]
    pub drm_node: DrmNode,
    pub renderer: GlesRenderer,
    pub compositor: WmDrmCompositor,
    pub crtc: crtc::Handle,
    pub frame_sent: HashSet<WlSurface>,
    /// True when a frame has been queued but VBlank hasn't fired yet.
    pub pending_frame: bool,
}

// -------------------------------------------------------------------------
// Calloop shared data
// -------------------------------------------------------------------------

pub struct CalloopData {
    pub state: State,
    pub backend: DrmBackend,
}

// -------------------------------------------------------------------------
// Workspace / focus operations (inherent methods on State)
// -------------------------------------------------------------------------

impl State {
    pub fn focus_window(&mut self, window: &Window) {
        let ws = &mut self.workspaces[self.active_workspace];
        ws.space.raise_element(window, true);
        let surface = window
            .toplevel()
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, surface, serial);
    }

    pub fn close_focused(&mut self) {
        let Some(focused) = self.keyboard.current_focus() else {
            info!("close_focused: nothing is focused");
            return;
        };

        let ws = &mut self.workspaces[self.active_workspace];
        let Some(idx) = ws
            .windows
            .iter()
            .position(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&focused))
        else {
            warn!("close_focused: focused surface does not belong to a tracked window");
            return;
        };
        let window = ws.windows.remove(idx);
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        }
        ws.space.unmap_elem(&window);

        let output = self.output.clone();
        Self::recalculate_layout_for(
            &mut self.workspaces[self.active_workspace],
            &output,
        );

        let next_focus = self.workspaces[self.active_workspace]
            .windows
            .first()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, next_focus, serial);

        self.needs_redraw = true;
    }

    pub fn focus_relative(&mut self, delta: isize) {
        let ws = &self.workspaces[self.active_workspace];
        if ws.windows.is_empty() {
            return;
        }
        let len = ws.windows.len() as isize;
        let current = self.keyboard.current_focus();
        let current_idx = current.as_ref().and_then(|focused| {
            ws.windows
                .iter()
                .position(|w| w.toplevel().map(|t| t.wl_surface()) == Some(focused))
        });
        let next_idx = match current_idx {
            Some(i) => (i as isize + delta).rem_euclid(len) as usize,
            None => 0,
        };
        let next = ws.windows[next_idx].clone();
        self.focus_window(&next);
    }

    // ── Workspace operations ────────────────────────────────────────

    pub fn switch_workspace(&mut self, idx: usize) {
        if idx >= self.workspaces.len() {
            warn!(idx, "switch_workspace: index out of range");
            return;
        }
        if idx == self.active_workspace {
            debug!(workspace = idx + 1, "already on this workspace");
            return;
        }

        info!(
            from = self.active_workspace + 1,
            to = idx + 1,
            "switching workspace"
        );

        self.active_workspace = idx;

        // Update keyboard focus to whatever is on top of the new workspace.
        let focus = self.workspaces[self.active_workspace]
            .windows
            .last()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, focus, serial);

        self.needs_redraw = true;
    }

    pub fn move_to_workspace(&mut self, target_idx: usize) {
        if target_idx >= self.workspaces.len() {
            warn!(target_idx, "move_to_workspace: index out of range");
            return;
        }
        if target_idx == self.active_workspace {
            debug!(
                workspace = target_idx + 1,
                "move_to_workspace: window is already on this workspace"
            );
            return;
        }

        let Some(focused) = self.keyboard.current_focus() else {
            info!("move_to_workspace: nothing is focused");
            return;
        };

        let src_idx = self.active_workspace;

        // Find the window in the source workspace.
        let src_ws = &mut self.workspaces[src_idx];
        let Some(win_idx) = src_ws
            .windows
            .iter()
            .position(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&focused))
        else {
            warn!("move_to_workspace: focused surface not found in active workspace");
            return;
        };

        let window = src_ws.windows.remove(win_idx);
        src_ws.space.unmap_elem(&window);

        info!(
            from = src_idx + 1,
            to = target_idx + 1,
            "moving window to workspace"
        );

        // Add to target workspace.
        let dst_ws = &mut self.workspaces[target_idx];
        dst_ws.windows.push(window);

        // Recalculate layout for both workspaces.
        let output = self.output.clone();
        Self::recalculate_layout_for(
            &mut self.workspaces[src_idx],
            &output,
        );
        Self::recalculate_layout_for(
            &mut self.workspaces[target_idx],
            &output,
        );

        // Update focus on the source (active) workspace.
        let next_focus = self.workspaces[src_idx]
            .windows
            .last()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, next_focus, serial);

        self.needs_redraw = true;
    }
}
