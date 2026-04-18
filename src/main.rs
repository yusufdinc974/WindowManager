//! Phase 10: bare-metal Wayland compositor entry point.
//!
//! After the Grand Refactoring `main.rs` only hosts:
//!   * module declarations,
//!   * the DRM/udev/libseat backend bringup,
//!   * `render_frame` / `handle_drm_event`,
//!   * the `main()` function and its calloop event loop.

use std::{
    collections::HashSet,
    io::Read,
    os::unix::net::UnixListener,
    sync::Arc,
    time::Duration,
};

use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer,
            Color32F,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{primary_gpu, UdevBackend},
    },
    desktop::{
        space::{space_render_elements, SpaceRenderElements},
        PopupManager, Space, Window, WindowSurfaceType,
    },
    input::{keyboard::XkbConfig, Seat, SeatState},
    output::{Mode as WlOutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            EventLoop, Interest, Mode as CalloopMode, PostAction,
            RegistrationToken,
        },
        drm::{
            self,
            control::{connector, Device as _, ModeTypeFlags},
        },
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{
            protocol::wl_surface::WlSurface, Display, DisplayHandle,
        },
    },
    utils::{DeviceFd, Logical, Point, Transform},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        output::OutputManagerState,
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};
use tracing::{info, trace, warn};

// -------------------------------------------------------------------------
// Extracted modules (Phase 10)
// -------------------------------------------------------------------------

mod handlers;
mod input;
mod layout;
mod state;

use input::{handle_ipc_command, handle_libinput_event, IpcCommand};
use state::{
    CalloopData, ClientState, DrmBackend, State, Workspace, CLEAR_COLOR, NUM_WORKSPACES,
};

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

    info!("bootstrapping bare-metal Wayland compositor (Phase 10)");

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
    let layer_shell_state = WlrLayerShellState::new::<State>(&display_handle);
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

    // ---- Workspaces ---------------------------------------------------
    let workspaces: Vec<Workspace> = (0..NUM_WORKSPACES)
        .map(|_| Workspace::new(&output))
        .collect();
    info!(count = NUM_WORKSPACES, "workspaces initialised");

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
        layer_shell_state,
        shm_state,
        output_manager_state,
        seat_state,
        dmabuf_state,
        seat,
        keyboard,
        pointer,
        pointer_location: Point::from((0.0, 0.0)),
        workspaces,
        active_workspace: 0,
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

            let ws = &mut data.state.workspaces[data.state.active_workspace];
            ws.space.refresh();
            data.state.popups.cleanup();

            // If the VBlank-driven render loop has stopped (no pending
            // frame) and something has changed, kick off a new render.
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

    let active_space = &state.workspaces[state.active_workspace].space;
    let spaces = [active_space];
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

    // Send frame callbacks to windows on ALL workspaces so that
    // background clients don't stall waiting for a
    // wl_callback.done that never arrives.
    for ws in state.workspaces.iter() {
        ws.space.elements().for_each(|window| {
            window.send_frame(&output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        });
    }

    state.workspaces[state.active_workspace].space.refresh();
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
