use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use std::io::Write;
use std::os::unix::net::UnixStream;

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
    desktop::{layer_map_for_output, PopupManager, Space, Window, WindowSurfaceType},
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
            DisplayHandle, Resource,
        },
    },
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::{
        compositor::{get_parent, CompositorClientState, CompositorState},
        dmabuf::DmabufState,
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
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

pub const DEFAULT_CLEAR_COLOR: [f32; 4] = [0.08, 0.05, 0.14, 1.0];
pub const ANIMATION_DURATION: Duration = Duration::from_millis(200);
pub const ANIMATION_START_SCALE: f32 = 0.8;


// -------------------------------------------------------------------------
// Workspace transition animation
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionDirection {
    Left,  // Moving to a higher-index workspace (content slides left)
    Right, // Moving to a lower-index workspace (content slides right)
}

#[derive(Debug, Clone)]
pub struct WorkspaceTransition {
    pub active: bool,
    pub from_workspace: usize,
    pub to_workspace: usize,
    pub direction: TransitionDirection,
    pub start_time: Instant,
    pub duration: Duration,
    pub progress: f64,
}

impl Default for WorkspaceTransition {
    fn default() -> Self {
        Self {
            active: false,
            from_workspace: 0,
            to_workspace: 0,
            direction: TransitionDirection::Left,
            start_time: Instant::now(),
            duration: Duration::from_millis(250),
            progress: 0.0,
        }
    }
}

impl WorkspaceTransition {
    /// Start a new transition animation.
    pub fn begin(&mut self, from: usize, to: usize) {
        self.active = true;
        self.from_workspace = from;
        self.to_workspace = to;
        self.direction = if to > from {
            TransitionDirection::Left
        } else {
            TransitionDirection::Right
        };
        self.start_time = Instant::now();
        self.progress = 0.0;
    }

    /// Advance the animation. Returns true if still animating.
    pub fn tick(&mut self) -> bool {
        if !self.active {
            return false;
        }

        let elapsed = self.start_time.elapsed();
        let linear = (elapsed.as_secs_f64() / self.duration.as_secs_f64()).min(1.0);

        // Ease-out cubic: 1 - (1 - t)^3
        let t = 1.0 - linear;
        self.progress = 1.0 - (t * t * t);

        if self.progress >= 1.0 {
            self.progress = 1.0;
            self.active = false;
            return false;
        }

        true
    }

    /// Get the X offset for the "from" workspace (sliding out).
    pub fn from_offset(&self, screen_width: i32) -> i32 {
        let w = screen_width as f64;
        match self.direction {
            TransitionDirection::Left => -(self.progress * w) as i32,
            TransitionDirection::Right => (self.progress * w) as i32,
        }
    }

    /// Get the X offset for the "to" workspace (sliding in).
    pub fn to_offset(&self, screen_width: i32) -> i32 {
        let w = screen_width as f64;
        match self.direction {
            TransitionDirection::Left => (w - self.progress * w) as i32,
            TransitionDirection::Right => -(w - self.progress * w) as i32,
        }
    }
}

// -------------------------------------------------------------------------
// Pointer grab state for interactive move / resize
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabMode {
    FloatingMove,
    FloatingResize,
    TiledMove,
    TiledResize,
}

#[derive(Debug, Clone)]
pub struct GrabState {
    pub mode: GrabMode,
    pub window: Window,
    pub start_pointer: Point<f64, Logical>,
    pub start_geo: Rectangle<i32, Logical>,
    pub start_split_ratio: f32,
    pub start_stack_ratios: Vec<f32>,
    pub screen_width: i32,
    pub screen_height: i32,
    pub tiled_index: usize,
    pub tiled_count: usize,
}

// -------------------------------------------------------------------------
// Workspace
// -------------------------------------------------------------------------

pub struct Workspace {
    pub space: Space<Window>,
    pub windows: Vec<Window>,
    pub spawn_times: HashMap<Window, Instant>,
    pub configured_sizes: HashMap<Window, (i32, i32)>,
    pub layout: LayoutType,
    pub floating: HashSet<Window>,
    pub floating_geo: HashMap<Window, Rectangle<i32, Logical>>,
    pub split_ratio: f32,
    pub stack_ratios: Vec<f32>,
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
            floating: HashSet::new(),
            floating_geo: HashMap::new(),
            split_ratio: 0.5,
            stack_ratios: Vec::new(),
        }
    }

    pub fn tiled_windows(&self) -> Vec<Window> {
        self.windows
            .iter()
            .filter(|w| !self.floating.contains(w))
            .cloned()
            .collect()
    }

    pub fn ensure_stack_ratios(&mut self) {
        let tiled = self.tiled_windows();
        let stack_count = if tiled.len() > 1 { tiled.len() - 1 } else { 0 };

        if stack_count == 0 {
            self.stack_ratios.clear();
            return;
        }

        if self.stack_ratios.len() != stack_count {
            let equal = 1.0 / stack_count as f32;
            self.stack_ratios = vec![equal; stack_count];
        }
    }

    pub fn normalise_stack_ratios(&mut self) {
        if self.stack_ratios.is_empty() {
            return;
        }
        let min_ratio = 0.05;
        for r in self.stack_ratios.iter_mut() {
            if *r < min_ratio {
                *r = min_ratio;
            }
        }
        let sum: f32 = self.stack_ratios.iter().sum();
        if sum > 0.0 {
            for r in self.stack_ratios.iter_mut() {
                *r /= sum;
            }
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

pub fn window_current_size(window: &Window) -> Option<Size<i32, Logical>> {
    let geo = window.geometry();
    if geo.size.w > 0 && geo.size.h > 0 {
        return Some(geo.size);
    }
    let toplevel = window.toplevel()?;
    toplevel.with_pending_state(|s| s.size)
}

// -------------------------------------------------------------------------
// Compositor state
// -------------------------------------------------------------------------

pub struct State {
    pub start_time: Instant,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub socket_name: std::ffi::OsString,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub dmabuf_state: DmabufState,
    pub data_device_state: DataDeviceState,

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
    pub renderer: GlesRenderer,

    pub pointer_grab: Option<GrabState>,
    
    // ── Touchpad gesture tracking ──
    pub swipe_active: bool,
    pub swipe_fingers: u32,
    pub swipe_dx: f64,

    // ── Workspace transition animation (Phase 27) ──
    pub workspace_transition: WorkspaceTransition,
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
// Workspace / focus / floating operations
// -------------------------------------------------------------------------

impl State {
     /// Hot-reload config from disk (TOML + Lua).
    pub fn reload_config(&mut self) {
        info!("reloading configuration from disk");
        self.config.reload(&self.lua);

        // Re-tile everything with new gaps/borders
        let output = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let focused = self.keyboard.current_focus();
        for ws in self.workspaces.iter_mut() {
            Self::recalculate_layout_for(ws, &output, outer, inner, border, focused.as_ref());
        }
        self.needs_redraw = true;
    }

     pub fn toggle_wallpaper_menu(&mut self) {
        let call = self.lua.load(
            "if type(toggle_wallpaper_menu) == 'function' then toggle_wallpaper_menu() \
             else print('rc.lua: toggle_wallpaper_menu is not defined') end",
        );
        if let Err(err) = call.exec() {
            warn!(error = %err, "toggle_wallpaper_menu: Lua execution failed");
            return;
        }
        self.needs_redraw = true;
    }

    /// Returns true if a layer surface currently holds keyboard focus.
    pub fn layer_has_keyboard_focus(&self) -> bool {
        let Some(focused) = self.keyboard.current_focus() else {
            return false;
        };
        self.layer_surface_of(&focused).is_some()
    }

    pub fn focus_window(&mut self, window: &Window) {
        if self.layer_has_keyboard_focus() {
            return;
        }

        let ws = &mut self.workspaces[self.active_workspace];
        ws.space.raise_element(window, true);

        let surface = window.toplevel().map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, surface, serial);

        if self.workspaces[self.active_workspace].layout == LayoutType::Monocle {
            self.recalculate_layout();
            self.needs_redraw = true;
        }
    }

    pub fn layer_surface_of(
        &self,
        surface: &WlSurface,
    ) -> Option<smithay::desktop::LayerSurface> {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let map = layer_map_for_output(&self.output);
        map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
            .cloned()
    }

    pub fn drop_focus_to_active_window(&mut self) {
        let fallback = self.workspaces[self.active_workspace]
            .windows
            .last()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, fallback, serial);
    }

    pub fn close_focused(&mut self) {
        let Some(focused) = self.keyboard.current_focus() else {
            info!("close_focused: nothing is focused");
            return;
        };

        if let Some(layer) = self.layer_surface_of(&focused) {
            info!(surface = ?focused.id(), "close_focused: closing layer surface");
            layer.layer_surface().send_close();
            self.drop_focus_to_active_window();
            self.needs_redraw = true;
            return;
        }

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
        ws.floating.remove(&window);
        ws.floating_geo.remove(&window);
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        }
        ws.space.unmap_elem(&window);

        ws.stack_ratios.clear();

        let next_focus = self.workspaces[self.active_workspace]
            .windows
            .last()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, next_focus, serial);

        self.recalculate_layout();
        self.needs_redraw = true;
    }

    pub fn focus_relative(&mut self, delta: isize) {
        if self.layer_has_keyboard_focus() {
            return;
        }

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

    pub fn toggle_floating(&mut self) {
        if self.layer_has_keyboard_focus() {
            return;
        }

        let Some(focused) = self.keyboard.current_focus() else {
            info!("toggle_floating: nothing is focused");
            return;
        };

        let ws = &mut self.workspaces[self.active_workspace];
        let Some(window) = ws
            .windows
            .iter()
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&focused))
            .cloned()
        else {
            warn!("toggle_floating: focused surface not tracked");
            return;
        };

        if ws.floating.contains(&window) {
            info!(
                surface = ?focused.id(),
                "toggle_floating: returning to tiled"
            );
            ws.floating.remove(&window);
            ws.floating_geo.remove(&window);
        } else {
            let loc = ws
                .space
                .element_location(&window)
                .unwrap_or_else(|| Point::from((100, 100)));

            let size = window_current_size(&window)
                .unwrap_or_else(|| Size::from((640, 480)));

            let geo = Rectangle::new(loc, size);
            info!(
                surface = ?focused.id(),
                ?geo,
                "toggle_floating: popping out to floating"
            );
            ws.floating.insert(window.clone());
            ws.floating_geo.insert(window.clone(), geo);
        }

        ws.stack_ratios.clear();
        ws.space.raise_element(&window, true);

        self.recalculate_layout();
        self.needs_redraw = true;
    }

    pub fn ensure_floating(&mut self, window: &Window) {
        let ws = &mut self.workspaces[self.active_workspace];
        if ws.floating.contains(window) {
            return;
        }

        let loc = ws
            .space
            .element_location(window)
            .unwrap_or_else(|| Point::from((100, 100)));

        let size = window_current_size(window)
            .unwrap_or_else(|| Size::from((640, 480)));

        let geo = Rectangle::new(loc, size);
        ws.floating.insert(window.clone());
        ws.floating_geo.insert(window.clone(), geo);
        ws.stack_ratios.clear();

        let focused = self.keyboard.current_focus();
        let output = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let ws = &mut self.workspaces[self.active_workspace];
        Self::recalculate_layout_for(ws, &output, outer, inner, border, focused.as_ref());
    }

    pub fn swap_windows(&mut self, idx_a: usize, idx_b: usize) {
        let ws = &mut self.workspaces[self.active_workspace];
        if idx_a == idx_b || idx_a >= ws.windows.len() || idx_b >= ws.windows.len() {
            return;
        }
        info!(
            idx_a,
            idx_b,
            "swap_windows: swapping tiled window positions"
        );
        ws.windows.swap(idx_a, idx_b);
        ws.stack_ratios.clear();
        self.recalculate_layout();
        self.needs_redraw = true;
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

// -------------------------------------------------------------------------
// Workspace IPC — broadcasts workspace state to listeners (Waybar, etc.)
// -------------------------------------------------------------------------

/// JSON representation of workspace state, consumed by Waybar's custom module.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceIpcState {
    pub workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceInfo {
    pub index: usize,
    pub name: String,
    pub active: bool,
    pub occupied: bool,
    pub window_count: usize,
    pub layout: String,
}

impl State {
    /// Build the current workspace state snapshot.
    pub fn workspace_ipc_state(&self) -> WorkspaceIpcState {
        let mut workspaces = Vec::with_capacity(self.workspaces.len());
        for (i, ws) in self.workspaces.iter().enumerate() {
            let name = self
                .config
                .workspace_names
                .get(i)
                .cloned()
                .unwrap_or_else(|| (i + 1).to_string());
            workspaces.push(WorkspaceInfo {
                index: i + 1,
                name,
                active: i == self.active_workspace,
                occupied: !ws.windows.is_empty(),
                window_count: ws.windows.len(),
                layout: format!("{}", ws.layout),
            });
        }
        WorkspaceIpcState { workspaces }
    }

    /// Write current workspace state to the IPC socket file so Waybar
    /// (and other tools) can read it.
    pub fn broadcast_workspace_state(&self) {
        let state = self.workspace_ipc_state();
        let json = match serde_json::to_string(&state) {
            Ok(j) => j,
            Err(err) => {
                warn!(?err, "workspace IPC: failed to serialize state");
                return;
            }
        };

        let ipc_path = workspace_ipc_path();

        // Write atomically: write to .tmp then rename
        let tmp_path = format!("{}.tmp", ipc_path);
        match std::fs::write(&tmp_path, &json) {
            Ok(()) => {
                if let Err(err) = std::fs::rename(&tmp_path, &ipc_path) {
                    warn!(?err, "workspace IPC: rename failed");
                }
            }
            Err(err) => {
                warn!(?err, "workspace IPC: write failed");
            }
        }

        // Also notify any connected stream listeners
        self.notify_workspace_listeners(&json);
    }

    /// Notify connected Waybar listener sockets.
    fn notify_workspace_listeners(&self, json: &str) {
        let sock_path = workspace_ipc_stream_path();
        // Best-effort: connect and write, don't block if nobody is listening
        if let Ok(mut stream) = UnixStream::connect(&sock_path) {
            let _ = stream.set_nonblocking(true);
            let _ = stream.write_all(json.as_bytes());
            let _ = stream.write_all(b"\n");
        }
    }

    // Update existing methods to broadcast on workspace change:

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
            "switching workspace (animated)"
        );

        let from = self.active_workspace;

        // Start the cinematic transition
        self.workspace_transition.begin(from, idx);

        // Switch the active workspace immediately so input goes to the right place
        self.active_workspace = idx;

        let focus = self.workspaces[self.active_workspace]
            .windows
            .last()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, focus, serial);

        self.recalculate_layout();
        self.needs_redraw = true;

        // ── Broadcast workspace change ──
        self.broadcast_workspace_state();
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

        if self.layer_has_keyboard_focus() {
            return;
        }

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
        let was_floating = src_ws.floating.remove(&window);
        let float_geo = src_ws.floating_geo.remove(&window);
        src_ws.space.unmap_elem(&window);
        src_ws.stack_ratios.clear();

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
        if was_floating {
            dst_ws.floating.insert(window.clone());
        }
        if let Some(geo) = float_geo {
            dst_ws.floating_geo.insert(window.clone(), geo);
        }
        dst_ws.windows.push(window);
        dst_ws.stack_ratios.clear();

        let next_focus = self.workspaces[src_idx]
            .windows
            .last()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, next_focus.clone(), serial);

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
            next_focus.as_ref(),
        );
        Self::recalculate_layout_for(
            &mut self.workspaces[target_idx],
            &output,
            outer,
            inner,
            border,
            None,
        );

        self.needs_redraw = true;

        // ── Broadcast workspace change ──
        self.broadcast_workspace_state();
    }
}

// -------------------------------------------------------------------------
// IPC path helpers
// -------------------------------------------------------------------------

pub fn workspace_ipc_path() -> String {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{}/mywm-workspaces.json", runtime)
    } else {
        "/tmp/mywm-workspaces.json".to_string()
    }
}

pub fn workspace_ipc_stream_path() -> String {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{}/mywm-workspaces.sock", runtime)
    } else {
        "/tmp/mywm-workspaces.sock".to_string()
    }
}