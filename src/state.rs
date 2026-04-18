use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    time::{Duration, Instant},
};

use mlua::Lua;

use smithay::{
    backend::{
        allocator::gbm::GbmAllocator,
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDeviceFd,
            DrmNode,
        },
        renderer::{element::solid::SolidColorBuffer, gles::GlesRenderer},
    },
    desktop::{PopupManager, Space, Window},
    input::{
        keyboard::KeyboardHandle,
        pointer::{CursorImageStatus, PointerHandle},
        Seat, SeatState,
    },
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

use crate::config::Config;
use crate::layout::LayoutType;

pub const CLEAR_COLOR: [f32; 4] = [0.08, 0.05, 0.14, 1.0];
pub const ANIMATION_DURATION: Duration = Duration::from_millis(200);
pub const ANIMATION_START_SCALE: f32 = 0.8;

// -------------------------------------------------------------------------
// Workspace
// -------------------------------------------------------------------------

pub struct Workspace {
    pub space: Space<Window>,
    pub windows: Vec<Window>,
    pub spawn_times: HashMap<Window, Instant>,
    /// Tracks the last configure size sent to each window so we only
    /// send a new configure when the size actually changes.
    pub configured_sizes: HashMap<Window, (i32, i32)>,
    /// The active tiling layout for this workspace.
    pub layout: LayoutType,
}

impl Workspace {
    pub fn new(output: &Output) -> Self {
        let mut space = Space::default();
        space.map_output(output, (0, 0));
        Self {
            space,
            windows: Vec::new(),
            spawn_times: HashMap::new(),
            configured_sizes: HashMap::new(),
            layout: LayoutType::default(),
        }
    }
}

pub fn animation_progress(now: Instant, spawn_time: Instant) -> Option<f32> {
    let elapsed = now.saturating_duration_since(spawn_time);
    if elapsed >= ANIMATION_DURATION {
        return None;
    }
    let t = elapsed.as_secs_f32() / ANIMATION_DURATION.as_secs_f32();
    let eased = 1.0 - (1.0 - t).powi(3);
    Some(eased.clamp(0.0, 1.0))
}

pub fn animation_scale(progress: f32) -> f32 {
    ANIMATION_START_SCALE + (1.0 - ANIMATION_START_SCALE) * progress
}

#[allow(dead_code)]
pub fn animation_alpha(progress: f32) -> f32 {
    progress.clamp(0.0, 1.0)
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

    pub cursor_status: CursorImageStatus,
    pub cursor_buffer: SolidColorBuffer,

    pub workspaces: Vec<Workspace>,
    pub active_workspace: usize,
    pub output: Output,
    pub popups: PopupManager,

    pub config: Config,
    pub lua: Lua,

    pub needs_redraw: bool,

    /// The GLES renderer. Lives here so DmabufHandler can import
    /// buffers synchronously.
    pub renderer: GlesRenderer,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        tracing::info!(?client_id, "wayland client initialized");
    }
    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::warn!(?client_id, ?reason, "wayland client DISCONNECTED");
    }
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
    pub compositor: WmDrmCompositor,
    pub crtc: crtc::Handle,
    pub frame_sent: HashSet<WlSurface>,
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
// Workspace / focus operations
// -------------------------------------------------------------------------

impl State {
    pub fn focus_window(&mut self, window: &Window) {
        let ws = &mut self.workspaces[self.active_workspace];
        ws.space.raise_element(window, true);

        // In Monocle mode, move the focused window to the end of the
        // window list so it is the one that gets raised on next retile.
        if ws.layout == LayoutType::Monocle {
            if let Some(pos) = ws.windows.iter().position(|w| w == window) {
                let w = ws.windows.remove(pos);
                ws.windows.push(w);
            }
        }

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
        ws.spawn_times.remove(&window);
        ws.configured_sizes.remove(&window);
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        }
        ws.space.unmap_elem(&window);

        let output = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        Self::recalculate_layout_for(
            &mut self.workspaces[self.active_workspace],
            &output,
            outer,
            inner,
            border,
        );

        let next_focus = self.workspaces[self.active_workspace]
            .windows
            .last()
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

        // In Monocle mode, retile so the newly focused window is raised.
        if self.workspaces[self.active_workspace].layout == LayoutType::Monocle {
            self.recalculate_layout();
        }

        self.needs_redraw = true;
    }

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
            layout = %self.workspaces[idx].layout,
            "switching workspace"
        );

        self.active_workspace = idx;

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
        let spawn_time = src_ws.spawn_times.remove(&window);
        let configured_size = src_ws.configured_sizes.remove(&window);
        src_ws.space.unmap_elem(&window);

        info!(
            from = src_idx + 1,
            to = target_idx + 1,
            "moving window to workspace"
        );

        let dst_ws = &mut self.workspaces[target_idx];
        if let Some(t) = spawn_time {
            dst_ws.spawn_times.insert(window.clone(), t);
        }
        if let Some(sz) = configured_size {
            dst_ws.configured_sizes.insert(window.clone(), sz);
        }
        dst_ws.windows.push(window);

        let output = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        Self::recalculate_layout_for(
            &mut self.workspaces[src_idx],
            &output,
            outer,
            inner,
            border,
        );
        Self::recalculate_layout_for(
            &mut self.workspaces[target_idx],
            &output,
            outer,
            inner,
            border,
        );

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

    pub fn any_animating(&self) -> bool {
        let now = Instant::now();
        self.workspaces.iter().any(|ws| {
            ws.spawn_times
                .values()
                .any(|t| animation_progress(now, *t).is_some())
        })
    }

    pub fn cycle_theme(&mut self) {
        let call = self.lua.load(
            "if type(cycle_theme) == 'function' then cycle_theme() \
             else print('rc.lua: cycle_theme is not defined') end",
        );
        if let Err(err) = call.exec() {
            warn!(error = %err, "cycle_theme: Lua execution failed");
            return;
        }
        self.config.refresh_from_lua(&self.lua);
        info!(
            active = %self.config.active_border_color,
            inactive = %self.config.inactive_border_color,
            "theme refreshed from Lua"
        );
        self.needs_redraw = true;
    }

    pub fn toggle_navbar(&mut self) {
        let call = self.lua.load(
            "if type(toggle_navbar) == 'function' then toggle_navbar() \
             else print('rc.lua: toggle_navbar is not defined') end",
        );
        if let Err(err) = call.exec() {
            warn!(error = %err, "toggle_navbar: Lua execution failed");
            return;
        }
        self.needs_redraw = true;
    }
}