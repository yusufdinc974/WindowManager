//! Phase 6b: bare-metal DRM/udev/GBM rendering.

use std::{
    collections::HashSet,
    ffi::OsString,
    io::Read,
    os::unix::net::UnixListener,
    process::Command,
    sync::Arc,
    time::Duration,
};

use serde::Deserialize;

use smithay::{
    backend::{
        allocator::{
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{EGLContext, EGLDisplay},
        input::{
            ButtonState, Event as _, InputEvent, KeyState, KeyboardKeyEvent,
            PointerButtonEvent, PointerMotionEvent,
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer,
            utils::on_commit_buffer_handler,
            Color32F,
        },
        session::{
            libseat::LibSeatSession, Event as SessionEvent, Session,
        },
        udev::{primary_gpu, UdevBackend},
    },
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        space::{space_render_elements, SpaceRenderElements},
        PopupManager, Space, Window, WindowSurfaceType,
    },
    input::{
        keyboard::{
            FilterResult, KeyboardHandle, KeysymHandle, Keysym, ModifiersState, XkbConfig,
        },
        pointer::{CursorImageStatus, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    output::{Mode as WlOutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            EventLoop, Interest, LoopSignal, Mode as CalloopMode, PostAction,
            RegistrationToken,
        },
        drm::{
            self,
            control::{connector, crtc, Device as _, ModeTypeFlags},
        },
        rustix::fs::OFlags,
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
            Client, Display, DisplayHandle,
        },
        input::Libinput,
    },
    utils::{DeviceFd, Logical, Point, Serial, Transform, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_parent, is_sync_subsurface, with_states, CompositorClientState,
            CompositorHandler, CompositorState,
        },
        dmabuf::{
            DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
        },
        output::{OutputHandler, OutputManagerState},
        shell::xdg::{
            decoration::{XdgDecorationHandler, XdgDecorationState},
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};
use tracing::{debug, info, trace, warn};

const CLEAR_COLOR: [f32; 4] = [0.08, 0.05, 0.14, 1.0];

// -------------------------------------------------------------------------
// Compositor state
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum KeyAction {
    Quit,
    ReloadConfig,
    SpawnTerminal,
    SpawnLauncher,
    CloseFocused,
    ToggleFullscreen,
    ToggleFloating,
    FocusLeft,
    FocusRight,
    MoveWindowLeft,
    MoveWindowRight,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum IpcCommand {
    Quit,
    SpawnTerminal,
    CloseFocused,
}

pub struct State {
    pub start_time: std::time::Instant,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub socket_name: OsString,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub dmabuf_state: DmabufState,

    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub pointer_location: Point<f64, Logical>,

    pub space: Space<Window>,
    pub windows: Vec<Window>,
    pub output: Output,
    pub popups: PopupManager,

    /// Set to true whenever the scene changes and a new frame should
    /// be rendered. The periodic calloop callback checks this and
    /// kicks off a render when the VBlank-driven loop isn't running.
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

pub struct CalloopData {
    pub state: State,
    pub backend: DrmBackend,
}

type WmDrmCompositor = DrmCompositor<
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
            if let Some(window) = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            }
        }

        handle_initial_configure(surface, &self.space);

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
        info!("new xdg toplevel");

        let window = Window::new_wayland_window(surface);
        self.windows.push(window.clone());
        self.recalculate_layout();

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
        self.windows
            .retain(|w| w.toplevel().map(|t| t.wl_surface()) != Some(&dying));

        let dead = self
            .space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&dying))
            .cloned();
        if let Some(window) = dead {
            self.space.unmap_elem(&window);
        }

        let focus = self.windows.first().and_then(|w| w.toplevel()).map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, focus, serial);

        self.recalculate_layout();
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
// Tiling layout
// -------------------------------------------------------------------------

impl State {
    pub fn recalculate_layout(&mut self) {
        let Some(geo) = self.space.output_geometry(&self.output) else {
            return;
        };
        let origin = geo.loc;
        let (screen_w, screen_h) = (geo.size.w, geo.size.h);

        match self.windows.len() {
            0 => {}
            1 => {
                place_tile(
                    &mut self.space,
                    &self.windows[0],
                    origin,
                    (screen_w, screen_h),
                );
            }
            n => {
                let master_w = screen_w / 2;
                let stack_w = screen_w - master_w;
                let stack_x = origin.x + master_w;
                let stack_count = (n - 1) as i32;
                let slice_h = screen_h / stack_count;

                place_tile(
                    &mut self.space,
                    &self.windows[0],
                    origin,
                    (master_w, screen_h),
                );

                for (i, window) in self.windows.iter().skip(1).enumerate() {
                    let i = i as i32;
                    let y = origin.y + i * slice_h;
                    let h = if i == stack_count - 1 {
                        origin.y + screen_h - y
                    } else {
                        slice_h
                    };
                    place_tile(&mut self.space, window, (stack_x, y).into(), (stack_w, h));
                }
            }
        }
    }

    pub fn focus_window(&mut self, window: &Window) {
        self.space.raise_element(window, true);
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
        let Some(idx) = self
            .windows
            .iter()
            .position(|w| w.toplevel().map(|t| t.wl_surface()) == Some(&focused))
        else {
            warn!("close_focused: focused surface does not belong to a tracked window");
            return;
        };
        let window = self.windows.remove(idx);
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        }
        self.space.unmap_elem(&window);

        let next_focus = self
            .windows
            .first()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, next_focus, serial);

        self.recalculate_layout();
        self.needs_redraw = true;
    }

    pub fn focus_relative(&mut self, delta: isize) {
        if self.windows.is_empty() {
            return;
        }
        let len = self.windows.len() as isize;
        let current = self.keyboard.current_focus();
        let current_idx = current.as_ref().and_then(|focused| {
            self.windows
                .iter()
                .position(|w| w.toplevel().map(|t| t.wl_surface()) == Some(focused))
        });
        let next_idx = match current_idx {
            Some(i) => (i as isize + delta).rem_euclid(len) as usize,
            None => 0,
        };
        let next = self.windows[next_idx].clone();
        self.focus_window(&next);
    }
}

fn place_tile(
    space: &mut Space<Window>,
    window: &Window,
    location: Point<i32, Logical>,
    size: (i32, i32),
) {
    if let Some(toplevel) = window.toplevel() {
        toplevel.with_pending_state(|s| {
            s.size = Some(size.into());
        });
        toplevel.send_configure();
    }
    space.map_element(window.clone(), location, false);
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

// -------------------------------------------------------------------------
// OutputHandler
// -------------------------------------------------------------------------

impl OutputHandler for State {}
delegate_output!(State);

// -------------------------------------------------------------------------
// Entry point
// -------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("bootstrapping bare-metal Wayland compositor (Phase 6b: DRM/udev)");

    let mut event_loop: EventLoop<CalloopData> = EventLoop::try_new()?;
    let loop_signal = event_loop.get_signal();

    let display: Display<State> = Display::new()?;
    let display_handle = display.handle();

    // ---- Libseat session ----------------------------------------------
    let (mut session, session_notifier) = LibSeatSession::new()
        .map_err(|e| format!("failed to open libseat session: {e:?}"))?;
    info!(seat = %session.seat(), "libseat session opened");

    // ---- DRM backend ---------------------------------------------------
    let (drm_backend, output, drm_notifier) =
        init_drm_backend(&mut session, &display_handle)?;
    info!("DRM backend ready");

    // ---- linux_dmabuf global with device feedback ---------------------
    let render_node = drm_backend
        .drm_node
        .node_with_type(NodeType::Render)
        .and_then(|n| n.ok())
        .unwrap_or(drm_backend.drm_node);
    info!(?render_node, "resolved render node for dmabuf feedback");

    let dmabuf_formats = drm_backend
        .renderer
        .egl_context()
        .dmabuf_render_formats()
        .clone();

    let default_feedback = DmabufFeedbackBuilder::new(
        render_node.dev_id(),
        dmabuf_formats,
    )
    .build()
    .map_err(|e| format!("failed to build dmabuf feedback: {e:?}"))?;

    let mut dmabuf_state = DmabufState::new();
    let _dmabuf_global = dmabuf_state.create_global_with_default_feedback::<State>(
        &display_handle,
        &default_feedback,
    );
    info!("linux_dmabuf global registered with render node feedback");

    // ---- Protocol globals ---------------------------------------------
    let compositor_state = CompositorState::new::<State>(&display_handle);
    let xdg_shell_state = XdgShellState::new::<State>(&display_handle);
    let xdg_decoration_state = XdgDecorationState::new::<State>(&display_handle);
    let shm_state = ShmState::new::<State>(&display_handle, vec![]);
    let output_manager_state =
        OutputManagerState::new_with_xdg_output::<State>(&display_handle);

    // ---- Seat ---------------------------------------------------------
    let mut seat_state: SeatState<State> = SeatState::new();
    let mut seat: Seat<State> =
        seat_state.new_wl_seat(&display_handle, "seat0");
    let keyboard = seat
        .add_keyboard(XkbConfig::default(), 200, 25)
        .map_err(|e| format!("failed to initialise keyboard: {e:?}"))?;
    let pointer = seat.add_pointer();

    info!("seat 'seat0' initialised with keyboard + pointer capabilities");

    // ---- Space + map output -------------------------------------------
    let mut space: Space<Window> = Space::default();
    space.map_output(&output, (0, 0));

    // ---- Wayland listening socket ------------------------------------
    let listening_socket = ListeningSocketSource::new_auto()?;
    let socket_name = listening_socket.socket_name().to_os_string();
    info!(?socket_name, "listening for wayland clients");

    event_loop
        .handle()
        .insert_source(listening_socket, |stream, _meta, data| {
            if let Err(err) = data
                .state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                warn!(?err, "failed to accept new wayland client");
            }
        })?;

    event_loop.handle().insert_source(
        Generic::new(display, Interest::READ, CalloopMode::Level),
        |_, display, data| {
            match unsafe { display.get_mut().dispatch_clients(&mut data.state) } {
                Ok(_) => Ok(PostAction::Continue),
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => {
                    Ok(PostAction::Continue)
                }
                Err(err) => Err(std::io::Error::other(err)),
            }
        },
    )?;

    // ---- IPC socket ---------------------------------------------------
    let ipc_socket_path = "/tmp/mywm.sock";
    let _ = std::fs::remove_file(ipc_socket_path);
    let ipc_listener = UnixListener::bind(ipc_socket_path)?;
    ipc_listener.set_nonblocking(true)?;
    info!(path = ipc_socket_path, "listening for IPC commands");

    event_loop.handle().insert_source(
        Generic::new(ipc_listener, Interest::READ, CalloopMode::Level),
        |_, listener, data| {
            let listener = unsafe { listener.get_mut() };
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream
                            .set_read_timeout(Some(Duration::from_millis(50)));
                        let mut buf = [0u8; 1024];
                        let n = match stream.read(&mut buf) {
                            Ok(n) => n,
                            Err(err) => {
                                warn!(?err, "failed to read IPC payload");
                                continue;
                            }
                        };
                        if n == 0 {
                            continue;
                        }
                        let payload = match std::str::from_utf8(&buf[..n]) {
                            Ok(s) => s.trim(),
                            Err(err) => {
                                warn!(?err, "IPC payload was not valid UTF-8");
                                continue;
                            }
                        };
                        if payload.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<IpcCommand>(payload) {
                            Ok(cmd) => {
                                info!(?cmd, "IPC command received");
                                handle_ipc_command(&mut data.state, cmd);
                            }
                            Err(err) => {
                                warn!(?err, %payload, "failed to parse IPC command");
                            }
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        break;
                    }
                    Err(err) => {
                        warn!(?err, "accept() on IPC socket failed");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    )?;

    // ---- Libinput input backend ---------------------------------------
    let seat_name = session.seat();
    let mut libinput_context = Libinput::new_with_udev::<
        LibinputSessionInterface<LibSeatSession>,
    >(session.clone().into());
    match libinput_context.udev_assign_seat(&seat_name) {
        Ok(()) => info!(seat = %seat_name, "libinput: seat assigned, enumerating devices"),
        Err(()) => warn!(seat = %seat_name, "libinput: udev_assign_seat failed"),
    }
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());

    event_loop.handle().insert_source(libinput_backend, |event, _, data| {
        handle_libinput_event(&mut data.state, event);
    })?;
    info!("libinput event source registered");

    // ---- Session notifier ---------------------------------------------
    event_loop.handle().insert_source(session_notifier, {
        let mut libinput_context = libinput_context.clone();
        move |event, &mut (), data: &mut CalloopData| match event {
            SessionEvent::PauseSession => {
                info!("session paused (VT switch out); suspending libinput");
                libinput_context.suspend();
            }
            SessionEvent::ActivateSession => {
                info!("session activated (VT switch in); resuming libinput + DRM");
                if let Err(err) = libinput_context.resume() {
                    warn!(?err, "libinput resume failed");
                }
                if let Err(err) = data.backend.compositor.reset_state() {
                    warn!(?err, "DRM: reset_state on activate failed");
                }
                data.backend.pending_frame = false;
                data.state.needs_redraw = true;
                render_frame(data);
            }
        }
    })?;

    // ---- udev hotplug watcher ----------------------------------------
    let udev_backend = UdevBackend::new(&seat_name)
        .map_err(|e| format!("failed to create udev backend: {e:?}"))?;
    event_loop.handle().insert_source(udev_backend, |event, _, _data| {
        match event {
            smithay::backend::udev::UdevEvent::Added { device_id, path } => {
                info!(?device_id, ?path, "udev: GPU added (ignored — single-GPU phase)");
            }
            smithay::backend::udev::UdevEvent::Changed { device_id } => {
                info!(?device_id, "udev: GPU changed");
            }
            smithay::backend::udev::UdevEvent::Removed { device_id } => {
                warn!(?device_id, "udev: GPU removed — we don't handle this yet");
            }
        }
    })?;

    // ---- DRM notifier (VBlank → render) -------------------------------
    let drm_token: RegistrationToken = event_loop.handle().insert_source(
        drm_notifier,
        |event, _meta, data| {
            handle_drm_event(event, data);
        },
    )?;

    // ---- Environment for spawned children ----------------------------
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
    std::env::remove_var("DISPLAY");
    std::env::set_var("GDK_BACKEND", "wayland");
    std::env::set_var("QT_QPA_PLATFORM", "wayland");
    std::env::set_var("WINIT_UNIX_BACKEND", "wayland");
    info!(
        wayland_display = ?socket_name,
        "environment variables set for child processes"
    );

    // ---- Assemble state ----------------------------------------------
    let state = State {
        start_time: std::time::Instant::now(),
        display_handle,
        loop_signal,
        socket_name,
        compositor_state,
        xdg_shell_state,
        xdg_decoration_state,
        shm_state,
        output_manager_state,
        seat_state,
        dmabuf_state,
        seat,
        keyboard,
        pointer,
        pointer_location: Point::from((0.0, 0.0)),
        space,
        windows: Vec::new(),
        output,
        popups: PopupManager::default(),
        needs_redraw: true,
    };

    let mut data = CalloopData {
        state,
        backend: drm_backend,
    };

    event_loop.handle().insert_idle(|data| {
        info!("DRM: rendering first frame to bootstrap flip loop");
        render_frame(data);
    });

    info!("entering calloop event loop");

    event_loop.run(
        Some(Duration::from_millis(16)),
        &mut data,
        |data| {
            if let Err(err) = data.state.display_handle.flush_clients() {
                if err.kind() != std::io::ErrorKind::BrokenPipe {
                    warn!(?err, "periodic flush_clients failed");
                }
            }
            data.state.space.refresh();
            data.state.popups.cleanup();

            // If the VBlank-driven render loop has stopped (no pending
            // frame) and something has changed, kick off a new render.
            // This is the mechanism that restarts rendering after the
            // loop dies due to no-damage frames.
            if data.state.needs_redraw && !data.backend.pending_frame {
                render_frame(data);
            }
        },
    )?;

    info!("event loop exited, shutting down");

    // ---- Graceful shutdown with correct drop ordering -----------------
    drop(data.backend);
    event_loop.handle().remove(drm_token);
    drop(data.state);
    drop(event_loop);
    drop(session);

    let _ = std::fs::remove_file(ipc_socket_path);

    info!("clean shutdown complete");
    Ok(())
}

// -------------------------------------------------------------------------
// Keyboard filter
// -------------------------------------------------------------------------

fn handle_keybinding(
    _state: &mut State,
    mods: &ModifiersState,
    keysym_handle: KeysymHandle<'_>,
    key_state: KeyState,
) -> FilterResult<KeyAction> {
    if key_state != KeyState::Pressed {
        return FilterResult::Forward;
    }

    // We only handle chords that include Super, with no Ctrl or Alt.
    if !mods.logo || mods.ctrl || mods.alt {
        return FilterResult::Forward;
    }

    let sym = keysym_handle.modified_sym();

    let action = if mods.shift {
        // ── Super + Shift ───────────────────────────────────────────
        debug!(?mods, sym = ?sym, "chord pressed (Super+Shift)");
        match sym {
            Keysym::Escape                          => KeyAction::Quit,
            s if s == Keysym::r || s == Keysym::R   => KeyAction::ReloadConfig,
            Keysym::Left                             => KeyAction::MoveWindowLeft,
            Keysym::Right                            => KeyAction::MoveWindowRight,
            _ => return FilterResult::Forward,
        }
    } else {
        // ── Super only ──────────────────────────────────────────────
        debug!(?mods, sym = ?sym, "chord pressed (Super)");
        match sym {
            Keysym::Return                           => KeyAction::SpawnTerminal,
            s if s == Keysym::d || s == Keysym::D    => KeyAction::SpawnLauncher,
            s if s == Keysym::q || s == Keysym::Q    => KeyAction::CloseFocused,
            s if s == Keysym::f || s == Keysym::F    => KeyAction::ToggleFullscreen,
            Keysym::space                            => KeyAction::ToggleFloating,
            Keysym::Left                             => KeyAction::FocusLeft,
            Keysym::Right                            => KeyAction::FocusRight,
            _ => return FilterResult::Forward,
        }
    };

    FilterResult::Intercept(action)
}

fn dispatch_action(state: &mut State, action: Option<KeyAction>) {
    let Some(action) = action else { return };

    match action {
        KeyAction::Quit => {
            info!("kill switch triggered (Super+Shift+Escape) — stopping");
            state.loop_signal.stop();
        }
        KeyAction::SpawnTerminal => spawn_terminal(),
        KeyAction::CloseFocused => state.close_focused(),
        KeyAction::FocusRight => state.focus_relative(1),
        KeyAction::FocusLeft => state.focus_relative(-1),
        KeyAction::ReloadConfig => {
            info!("Action ReloadConfig not yet implemented");
        }
        KeyAction::SpawnLauncher => {
            info!("Action SpawnLauncher not yet implemented");
        }
        KeyAction::ToggleFullscreen => {
            info!("Action ToggleFullscreen not yet implemented");
        }
        KeyAction::ToggleFloating => {
            info!("Action ToggleFloating not yet implemented");
        }
        KeyAction::MoveWindowLeft => {
            info!("Action MoveWindow not yet implemented");
        }
        KeyAction::MoveWindowRight => {
            info!("Action MoveWindow not yet implemented");
        }
    }
}

fn handle_libinput_event(state: &mut State, event: InputEvent<LibinputInputBackend>) {
    match event {
        InputEvent::Keyboard { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let time = event.time_msec();
            let keycode = event.key_code();
            let key_state = event.state();

            let keyboard = state.keyboard.clone();
            let action = keyboard.input::<KeyAction, _>(
                state,
                keycode,
                key_state,
                serial,
                time,
                |state, mods, keysym_handle| {
                    handle_keybinding(state, mods, keysym_handle, key_state)
                },
            );
            dispatch_action(state, action);
        }

        InputEvent::PointerMotion { event } => {
            state.pointer_location.x += event.delta_x();
            state.pointer_location.y += event.delta_y();
            if let Some(geo) = state.space.output_geometry(&state.output) {
                state.pointer_location.x =
                    state.pointer_location.x.clamp(0.0, geo.size.w as f64);
                state.pointer_location.y =
                    state.pointer_location.y.clamp(0.0, geo.size.h as f64);
            }
            trace!(
                x = state.pointer_location.x,
                y = state.pointer_location.y,
                "libinput pointer moved"
            );
        }

        InputEvent::PointerButton { event } => {
            if event.state() != ButtonState::Pressed {
                return;
            }
            let hit = state
                .space
                .element_under(state.pointer_location)
                .map(|(w, _)| w.clone());
            if let Some(window) = hit {
                state.focus_window(&window);
            }
        }

        InputEvent::DeviceAdded { device } => {
            info!(name = %device.name(), "libinput: device added");
        }
        InputEvent::DeviceRemoved { device } => {
            info!(name = %device.name(), "libinput: device removed");
        }

        _ => {}
    }
}

fn handle_ipc_command(state: &mut State, cmd: IpcCommand) {
    match cmd {
        IpcCommand::Quit => {
            info!("IPC: quit");
            state.loop_signal.stop();
        }
        IpcCommand::SpawnTerminal => {
            info!("IPC: spawn terminal");
            spawn_terminal();
        }
        IpcCommand::CloseFocused => {
            info!("IPC: close focused");
            state.close_focused();
        }
    }
}

fn spawn_terminal() {
    info!("spawning alacritty");
    match Command::new("alacritty")
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => debug!(pid = child.id(), "alacritty spawned"),
        Err(err) => warn!(?err, "failed to spawn alacritty"),
    }
}

// -------------------------------------------------------------------------
// DRM backend
// -------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
enum BackendError {
    #[error("no primary GPU found for seat")]
    NoPrimaryGpu,
    #[error("failed to open DRM device {0:?}: {1}")]
    OpenDevice(std::path::PathBuf, String),
    #[error("DRM: {0}")]
    Drm(#[from] smithay::backend::drm::DrmError),
    #[error("EGL: {0}")]
    Egl(#[from] smithay::backend::egl::Error),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("GLES: {0}")]
    Gles(String),
    #[error("no connected display found")]
    NoConnector,
    #[error("no free CRTC for connector")]
    NoCrtc,
    #[error("DrmCompositor: {0}")]
    Compositor(String),
}

fn init_drm_backend(
    session: &mut LibSeatSession,
    display_handle: &DisplayHandle,
) -> Result<
    (
        DrmBackend,
        Output,
        smithay::backend::drm::DrmDeviceNotifier,
    ),
    BackendError,
> {
    let seat_name = session.seat();

    let path = primary_gpu(&seat_name)
        .ok()
        .flatten()
        .ok_or(BackendError::NoPrimaryGpu)?;
    info!(?path, "DRM: primary GPU path resolved");

    let drm_node = DrmNode::from_path(&path)
        .map_err(|e| BackendError::OpenDevice(path.clone(), e.to_string()))?;
    info!(?drm_node, node_type = ?drm_node.ty(), "DRM: node opened");

    let owned_fd = session
        .open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| BackendError::OpenDevice(path.clone(), format!("{e:?}")))?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(owned_fd));
    info!("DRM: session opened device fd with master access");

    let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), true)?;
    info!(atomic = true, "DRM: DrmDevice initialised");

    let gbm = GbmDevice::new(drm_fd.clone())?;
    info!("GBM: device created");

    let egl_display = unsafe { EGLDisplay::new(gbm.clone())? };
    info!("EGL: display created from GBM");

    let egl_context = EGLContext::new(&egl_display)?;
    info!("EGL: context created");

    let renderer = unsafe { GlesRenderer::new(egl_context) }
        .map_err(|e| BackendError::Gles(format!("{e:?}")))?;
    info!("GLES: renderer online");

    let res_handles = drm.resource_handles()?;

    let mut picked: Option<(connector::Info, drm::control::Mode)> = None;
    for handle in res_handles.connectors() {
        let info = drm.get_connector(*handle, false)?;
        info!(
            connector = ?info.interface(),
            id = ?info.handle(),
            state = ?info.state(),
            modes = info.modes().len(),
            "DRM: probed connector"
        );
        if info.state() == connector::State::Connected && !info.modes().is_empty() {
            let mode = info
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .copied()
                .unwrap_or_else(|| info.modes()[0]);
            info!(?mode, "DRM: selected connector + mode");
            picked = Some((info, mode));
            break;
        }
    }
    let (connector_info, mode) = picked.ok_or(BackendError::NoConnector)?;

    let crtc = connector_info
        .encoders()
        .iter()
        .filter_map(|eh| drm.get_encoder(*eh).ok())
        .flat_map(|enc| res_handles.filter_crtcs(enc.possible_crtcs()))
        .next()
        .ok_or(BackendError::NoCrtc)?;
    info!(?crtc, "DRM: CRTC picked");

    let mut drm_device = drm;
    let surface = drm_device.create_surface(crtc, mode, &[connector_info.handle()])?;
    info!("DRM: surface created");

    let mode_size = (mode.size().0 as i32, mode.size().1 as i32);

    let (phys_w, phys_h) = connector_info.size().unwrap_or((0, 0));
    let output = Output::new(
        format!("{:?}-{}", connector_info.interface(), connector_info.interface_id()),
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "Unknown".into(),
            model: "WindowManager".into(),
            serial_number: "00000000".into(),
        },
    );
    let _output_global = output.create_global::<State>(display_handle);

    let wl_mode = WlOutputMode {
        size: mode_size.into(),
        refresh: (mode.vrefresh() * 1000) as i32,
    };

    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        Some(smithay::output::Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.set_preferred(wl_mode);

    info!(
        size = ?mode_size,
        refresh = mode.vrefresh(),
        phys_mm = ?(phys_w, phys_h),
        "DRM: wl_output advertised with mode + scale + transform"
    );

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let framebuffer_exporter = GbmFramebufferExporter::new(
        gbm.clone(),
        smithay::backend::drm::exporter::gbm::NodeFilter::All,
    );
    let renderer_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let raw_cursor_size = drm_device.cursor_size();
    let cursor_size = if raw_cursor_size.w > 0 && raw_cursor_size.h > 0 {
        raw_cursor_size
    } else {
        (64u32, 64u32).into()
    };
    let cursor_size = cursor_size
        .to_logical(1, Transform::Normal)
        .to_buffer(1, Transform::Normal);

    let compositor = DrmCompositor::new(
        smithay::output::OutputModeSource::Auto(output.downgrade()),
        surface,
        None,
        allocator,
        framebuffer_exporter,
        [Fourcc::Abgr8888, Fourcc::Argb8888],
        renderer_formats,
        cursor_size,
        Some(gbm),
    )
    .map_err(|e| BackendError::Compositor(format!("{e:?}")))?;
    info!("DRM: compositor ready — first page-flip will kick off the loop");

    let backend = DrmBackend {
        drm_node,
        renderer,
        compositor,
        crtc,
        frame_sent: HashSet::new(),
        pending_frame: false,
    };

    Ok((backend, output, drm_notifier))
}

fn render_frame(data: &mut CalloopData) {
    let CalloopData { state, backend } = data;

    // Don't try to render if a frame is already in-flight.
    if backend.pending_frame {
        return;
    }

    let spaces = [&state.space];
    let elements = match space_render_elements(
        &mut backend.renderer,
        spaces,
        &state.output,
        1.0,
    ) {
        Ok(v) => v,
        Err(err) => {
            warn!(?err, "space_render_elements failed");
            return;
        }
    };
    let wrapped: Vec<SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>> =
        elements;

    match backend.compositor.render_frame::<_, _>(
        &mut backend.renderer,
        &wrapped,
        Color32F::from(CLEAR_COLOR),
        FrameFlags::DEFAULT,
    ) {
        Ok(frame) => {
            if frame.is_empty {
                // No damage this frame. The DrmCompositor won't accept
                // queue_frame (returns EmptyFrame), so the VBlank loop
                // stops here. That's fine — the periodic calloop callback
                // will call render_frame again when needs_redraw is set.
                trace!("no damage — VBlank loop paused until next redraw");
                state.needs_redraw = false;
            } else if let Err(err) = backend.compositor.queue_frame(()) {
                warn!(?err, "DRM: queue_frame failed");
            } else {
                backend.pending_frame = true;
                state.needs_redraw = false;
            }
        }
        Err(err) => {
            warn!(?err, "DRM: render_frame failed");
        }
    }

    let now = state.start_time.elapsed();
    let output = state.output.clone();
    state.space.elements().for_each(|window| {
        window.send_frame(&output, now, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    });

    state.space.refresh();
    state.popups.cleanup();

    if let Err(err) = state.display_handle.flush_clients() {
        if err.kind() != std::io::ErrorKind::BrokenPipe {
            warn!(?err, "failed to flush wayland clients");
        }
    }
}

fn handle_drm_event(event: DrmEvent, data: &mut CalloopData) {
    match event {
        DrmEvent::VBlank(crtc) => {
            if crtc != data.backend.crtc {
                trace!(?crtc, "VBlank for a CRTC we don't own — ignoring");
                return;
            }
            trace!(?crtc, "DRM: VBlank — advancing frame");
            data.backend.pending_frame = false;
            if let Err(err) = data.backend.compositor.frame_submitted() {
                warn!(?err, "DRM: frame_submitted failed");
            }
            // Render next frame immediately — this keeps the VBlank
            // loop going as long as there's new content.
            render_frame(data);
        }
        DrmEvent::Error(err) => {
            warn!(?err, "DRM: device error event");
        }
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

#[allow(dead_code)]
fn surface_under(
    space: &Space<Window>,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    space.element_under(pos).and_then(|(window, location)| {
        window
            .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
            .map(|(s, p)| (s, (p + location).to_f64()))
    })
}