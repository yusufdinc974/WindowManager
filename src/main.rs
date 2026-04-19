//! Phase 10+30: bare-metal Wayland compositor entry point.

use std::{
    collections::HashSet,
    io::Read,
    os::unix::net::UnixListener,
    sync::Arc,
    time::{Duration, Instant},
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
            element::{
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::WaylandSurfaceRenderElement,
                Kind,
            },
            gles::GlesRenderer,
            Color32F,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{primary_gpu, UdevBackend},
    },
    desktop::{
        space::{space_render_elements, SpaceRenderElements},
        layer_map_for_output,
        PopupManager, Space, Window, WindowSurfaceType,
    },
    input::{keyboard::XkbConfig, pointer::CursorImageStatus, Seat, SeatState},
    output::{Mode as WlOutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            timer::{TimeoutAction, Timer},
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
    utils::{DeviceFd, Logical, Point, Transform, Physical},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};
use tracing::{info, trace, warn};

mod config;
mod handlers;
mod input;
mod layout;
mod state;

use config::Config;
use input::{handle_ipc_command, handle_libinput_event, IpcCommand};
use state::{
    animation_alpha, animation_progress, CalloopData, ClientState, DrmBackend, State, Workspace,
};

// -------------------------------------------------------------------------
// Session environment isolation
// -------------------------------------------------------------------------

fn isolate_session_environment(socket_name: &std::ffi::OsStr) {
    std::env::set_var("WAYLAND_DISPLAY", socket_name);
    std::env::remove_var("WAYLAND_DEBUG");
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
    std::env::set_var("XDG_CURRENT_DESKTOP", "mywm");
    std::env::set_var("XDG_SESSION_DESKTOP", "mywm");
    std::env::remove_var("DISPLAY");
    std::env::set_var("MOZ_ENABLE_WAYLAND", "1");
    std::env::set_var("GDK_BACKEND", "wayland");
    std::env::set_var("QT_QPA_PLATFORM", "wayland");
    std::env::set_var("QT_WAYLAND_DISABLE_WINDOWDECORATION", "1");
    std::env::set_var("SDL_VIDEODRIVER", "wayland");
    std::env::set_var("WINIT_UNIX_BACKEND", "wayland");
    std::env::set_var("CLUTTER_BACKEND", "wayland");
    std::env::set_var("ELECTRON_OZONE_PLATFORM_HINT", "wayland");
    std::env::set_var("NO_AT_BRIDGE", "1");
    std::env::set_var("GTK_A11Y", "none");

    let session_id = format!("mywm-{}", std::process::id());

    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let moz_dir = format!("{}/{}/mozilla", runtime_dir, session_id);
        let tb_dir = format!("{}/{}/thunderbird", runtime_dir, session_id);
        let _ = std::fs::create_dir_all(&moz_dir);
        let _ = std::fs::create_dir_all(&tb_dir);
        std::env::set_var("MOZ_LEGACY_PROFILES", "0");
        std::env::set_var("MOZ_NO_REMOTE", "1");
        std::env::set_var("MOZ_DBUS_REMOTE", "0");

        let isolated_home = format!("{}/{}/home", runtime_dir, session_id);
        let real_home = std::env::var("HOME").unwrap_or_default();
        let _ = std::fs::create_dir_all(&isolated_home);
        setup_isolated_home(&real_home, &isolated_home, &moz_dir, &tb_dir);
        std::env::set_var("HOME", &isolated_home);
        std::env::set_var("MYWM_REAL_HOME", &real_home);

        info!(
            isolated_home = %isolated_home,
            moz_dir = %moz_dir,
            "session: HOME isolated for single-instance app separation"
        );
    }

    match launch_private_dbus_session() {
        Ok(addr) => {
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);
            info!(bus = %addr, "session: launched private DBus session bus");
        }
        Err(err) => {
            if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok() {
                warn!(?err, "session: failed to launch private DBus — keeping inherited bus address");
            } else {
                warn!(?err, "session: no DBus session bus available");
            }
        }
    }

    info!(wayland_display = ?socket_name, "session: environment variables locked to this compositor");
}

fn setup_isolated_home(real_home: &str, isolated_home: &str, _moz_dir: &str, _tb_dir: &str) {
    let isolated_dirs = [".mozilla", ".thunderbird"];
    let real = std::path::Path::new(real_home);
    let iso = std::path::Path::new(isolated_home);

    let entries = match std::fs::read_dir(real) {
        Ok(e) => e,
        Err(err) => {
            warn!(?err, "cannot read real HOME for isolation");
            return;
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let target = iso.join(&name);

        if target.exists() || target.symlink_metadata().is_ok() {
            continue;
        }
        if isolated_dirs.iter().any(|d| name_str == *d) {
            continue;
        }
        if name_str == ".config" {
            let real_config = entry.path();
            let iso_config = iso.join(".config");
            let _ = std::fs::create_dir_all(&iso_config);
            if let Ok(config_entries) = std::fs::read_dir(&real_config) {
                for ce in config_entries.flatten() {
                    let ce_name = ce.file_name();
                    let ce_target = iso_config.join(&ce_name);
                    if !ce_target.exists() && ce_target.symlink_metadata().is_err() {
                        let _ = std::os::unix::fs::symlink(ce.path(), &ce_target);
                    }
                }
            }
            continue;
        }
        if let Err(err) = std::os::unix::fs::symlink(entry.path(), &target) {
            tracing::trace!(?err, entry = %name_str, "isolated home: failed to symlink");
        }
    }

    let iso_moz_ff = iso.join(".mozilla").join("firefox");
    let _ = std::fs::create_dir_all(&iso_moz_ff);
    let iso_tb = iso.join(".thunderbird");
    let _ = std::fs::create_dir_all(&iso_tb);
    info!(real_home, isolated_home, "session: isolated HOME set up");
}

fn launch_private_dbus_session() -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("dbus-launch")
        .arg("--sh-syntax")
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "dbus-launch exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("DBUS_SESSION_BUS_ADDRESS=") {
            let addr = rest.trim_end_matches(';').trim_matches('\'').trim_matches('"');
            if !addr.is_empty() {
                for pid_line in stdout.lines() {
                    let pid_line = pid_line.trim();
                    if let Some(pid_str) = pid_line.strip_prefix("DBUS_SESSION_BUS_PID=") {
                        let pid_str = pid_str.trim_end_matches(';');
                        if let Ok(pid) = pid_str.parse::<u32>() {
                            DBUS_DAEMON_PID.store(pid, std::sync::atomic::Ordering::SeqCst);
                            info!(pid, "session: dbus-daemon PID recorded for cleanup");
                        }
                    }
                }
                return Ok(addr.to_string());
            }
        }
    }

    Err(format!("could not parse DBUS_SESSION_BUS_ADDRESS from dbus-launch output: {stdout}").into())
}

static DBUS_DAEMON_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn kill_private_dbus_session() {
    let pid = DBUS_DAEMON_PID.load(std::sync::atomic::Ordering::SeqCst);
    if pid != 0 {
        info!(pid, "session: killing private dbus-daemon");
        unsafe { libc::kill(pid as i32, libc::SIGTERM); }
    }
}

fn cleanup_isolated_home() {
    if let (Ok(runtime_dir), Ok(real_home)) = (
        std::env::var("XDG_RUNTIME_DIR"),
        std::env::var("MYWM_REAL_HOME"),
    ) {
        std::env::set_var("HOME", &real_home);
        let session_dir = format!("{}/mywm-{}", runtime_dir, std::process::id());
        if std::path::Path::new(&session_dir).exists() {
            if let Err(err) = std::fs::remove_dir_all(&session_dir) {
                warn!(?err, dir = %session_dir, "failed to clean up session directory");
            } else {
                info!(dir = %session_dir, "session: cleaned up isolated directory");
            }
        }
    }
}

// -------------------------------------------------------------------------
// Helper: set file executable
// -------------------------------------------------------------------------

fn set_executable(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut perms = metadata.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

// -------------------------------------------------------------------------
// main
// -------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Phase 30: Kill libwayland wire protocol spam ──
    std::env::remove_var("WAYLAND_DEBUG");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "info,\
                         smithay=warn,\
                         calloop=warn,\
                         wayland_server=warn,\
                         wayland_commons=warn,\
                         drm=warn,\
                         gbm=warn,\
                         input=warn"
                    )
                }),
        )
        .compact()
        .with_target(false)
        .init();

    info!("bootstrapping bare-metal Wayland compositor (Phase 10+30)");

    let (lua, config) = Config::load_from_lua();
    info!(
        terminal = %config.terminal,
        outer = config.outer_gaps,
        inner = config.inner_gaps,
        border = config.border_width,
        active_border = %config.active_border_color,
        inactive_border = %config.inactive_border_color,
        workspaces = config.workspace_count(),
        "config: active values"
    );

    let mut event_loop: EventLoop<CalloopData> = EventLoop::try_new()?;
    let loop_signal = event_loop.get_signal();

    let display: Display<State> = Display::new()?;
    let display_handle = display.handle();

    let (mut session, session_notifier) = LibSeatSession::new()
        .map_err(|e| format!("failed to open libseat session: {e:?}"))?;
    info!(seat = %session.seat(), "libseat session opened");

    let (drm_backend, renderer, output, drm_notifier) =
        init_drm_backend(&mut session, &display_handle)?;
    info!("DRM backend ready");

    let render_node = drm_backend
        .drm_node
        .node_with_type(NodeType::Render)
        .and_then(|n| n.ok())
        .unwrap_or(drm_backend.drm_node);
    info!(?render_node, "resolved render node for dmabuf feedback");

    let dmabuf_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let default_feedback = DmabufFeedbackBuilder::new(render_node.dev_id(), dmabuf_formats)
        .build()
        .map_err(|e| format!("failed to build dmabuf feedback: {e:?}"))?;

    let mut dmabuf_state = DmabufState::new();
    let _dmabuf_global = dmabuf_state
        .create_global_with_default_feedback::<State>(&display_handle, &default_feedback);
    info!("linux_dmabuf global registered with render node feedback");

    let compositor_state = CompositorState::new::<State>(&display_handle);
    let xdg_shell_state = XdgShellState::new::<State>(&display_handle);
    let xdg_decoration_state = XdgDecorationState::new::<State>(&display_handle);
    let layer_shell_state = WlrLayerShellState::new::<State>(&display_handle);
    let shm_state = ShmState::new::<State>(&display_handle, vec![]);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&display_handle);
    let data_device_state = DataDeviceState::new::<State>(&display_handle);

    let mut seat_state: SeatState<State> = SeatState::new();
    let mut seat: Seat<State> = seat_state.new_wl_seat(&display_handle, "seat0");
    let keyboard = seat
        .add_keyboard(XkbConfig::default(), 200, 25)
        .map_err(|e| format!("failed to initialise keyboard: {e:?}"))?;
    let pointer = seat.add_pointer();
    info!("seat 'seat0' initialised with keyboard + pointer capabilities");

    let workspace_count = config.workspace_count().max(1);
    let workspaces: Vec<Workspace> = (0..workspace_count)
        .map(|_| Workspace::new(&output))
        .collect();
    info!(count = workspace_count, "workspaces initialised");

    let listening_socket = ListeningSocketSource::new_auto()?;
    let socket_name = listening_socket.socket_name().to_os_string();
    info!(?socket_name, "listening for wayland clients");

    isolate_session_environment(&socket_name);
    ensure_waybar_config();

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
                    warn!("dispatch_clients: BrokenPipe (client disconnected)");
                    Ok(PostAction::Continue)
                }
                Err(err) => {
                    warn!(?err, "dispatch_clients: fatal error");
                    Err(std::io::Error::other(err))
                }
            }
        },
    )?;

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
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                        let mut buf = [0u8; 1024];
                        let n = match stream.read(&mut buf) {
                            Ok(n) => n,
                            Err(err) => {
                                warn!(?err, "failed to read IPC payload");
                                continue;
                            }
                        };
                        if n == 0 { continue; }
                        let payload = match std::str::from_utf8(&buf[..n]) {
                            Ok(s) => s.trim(),
                            Err(err) => {
                                warn!(?err, "IPC payload was not valid UTF-8");
                                continue;
                            }
                        };
                        if payload.is_empty() { continue; }
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
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(err) => {
                        warn!(?err, "accept() on IPC socket failed");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    )?;

    let seat_name = session.seat();
    let mut libinput_context = Libinput::new_with_udev::<
        LibinputSessionInterface<LibSeatSession>,
    >(session.clone().into());
    match libinput_context.udev_assign_seat(&seat_name) {
        Ok(()) => info!(seat = %seat_name, "libinput: seat assigned"),
        Err(()) => warn!(seat = %seat_name, "libinput: udev_assign_seat failed"),
    }
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());

    event_loop.handle().insert_source(libinput_backend, |event, _, data| {
        handle_libinput_event(&mut data.state, event);
    })?;
    info!("libinput event source registered");

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

    let udev_backend = UdevBackend::new(&seat_name)
        .map_err(|e| format!("failed to create udev backend: {e:?}"))?;
    event_loop.handle().insert_source(udev_backend, |event, _, _data| {
        match event {
            smithay::backend::udev::UdevEvent::Added { device_id, path } => {
                info!(?device_id, ?path, "udev: GPU added (ignored)");
            }
            smithay::backend::udev::UdevEvent::Changed { device_id } => {
                info!(?device_id, "udev: GPU changed");
            }
            smithay::backend::udev::UdevEvent::Removed { device_id } => {
                warn!(?device_id, "udev: GPU removed");
            }
        }
    })?;

    let drm_token: RegistrationToken = event_loop.handle().insert_source(
        drm_notifier,
        |event, _meta, data| { handle_drm_event(event, data); },
    )?;

    // ── Autostart ──
    {
        let autostart_result: Result<mlua::Value, _> = lua.load("return wm.autostart").eval();
        if let Ok(mlua::Value::Table(cmds)) = autostart_result {
            for cmd in cmds.sequence_values::<String>().flatten() {
                info!(command = %cmd, "autostart: launching");
                match std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(child) => info!(pid = child.id(), "autostart: process spawned"),
                    Err(err) => warn!(?err, "autostart: failed to spawn"),
                }
            }
        }
    }

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
        data_device_state,
        seat,
        keyboard,
        pointer,
        pointer_location: Point::from((0.0, 0.0)),
        cursor_status: CursorImageStatus::default_named(),
        cursor_buffer: SolidColorBuffer::new((12, 12), [1.0, 1.0, 1.0, 1.0]),
        workspaces,
        active_workspace: 0,
        output,
        popups: PopupManager::default(),
        config,
        lua,
        needs_redraw: true,
        renderer,
        pointer_grab: None,
        swipe_active: false,
        swipe_fingers: 0,
        swipe_dx: 0.0,
        workspace_transition: state::WorkspaceTransition::default(),
        window_opacity: 1.0,
    };

    let mut data = CalloopData {
        state,
        backend: drm_backend,
    };

    // Write initial opacity state for Waybar
    data.state.broadcast_opacity_state();

    event_loop.handle().insert_idle(|data| {
        info!("broadcasting initial workspace state for waybar");
        data.state.broadcast_workspace_state();
    });

    let broadcast_timer = Timer::from_duration(Duration::from_millis(500));
    event_loop.handle().insert_source(broadcast_timer, |_, _, data| {
        data.state.broadcast_workspace_state();
        TimeoutAction::ToDuration(Duration::from_secs(2))
    })?;

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

            // ── Tick workspace transition animation ──
            if data.state.workspace_transition.active {
                data.state.workspace_transition.tick();
                data.state.needs_redraw = true;
            }

            // ── Reap completed dying window animations ──
            for ws in data.state.workspaces.iter_mut() {
                ws.reap_dying_windows();
            }

            if data.state.any_animating() {
                data.state.recalculate_layout();
                data.state.needs_redraw = true;
            }

            if data.state.needs_redraw && !data.backend.pending_frame {
                render_frame(data);
            }
        },
    )?;

    info!("event loop exited, shutting down");

    // Kill waybar first to stop broken pipe errors from helper scripts
    let _ = std::process::Command::new("pkill").args(["-x", "waybar"]).status();
    let _ = std::process::Command::new("pkill").args(["-f", "mywm-workspaces.sh"]).status();
    let _ = std::process::Command::new("pkill").args(["-f", "mywm-opacity.sh"]).status();
    // Small delay for processes to exit
    std::thread::sleep(Duration::from_millis(100));

    drop(data.backend);
    event_loop.handle().remove(drm_token);
    drop(data.state);
    drop(event_loop);
    drop(session);

    let _ = std::fs::remove_file(ipc_socket_path);
    let _ = std::fs::remove_file(crate::state::workspace_ipc_path());
    let _ = std::fs::remove_file(crate::state::workspace_ipc_stream_path());
    let _ = std::fs::remove_file(crate::state::opacity_ipc_path());

    // Kill orphaned Waybar and helper scripts
    let _ = std::process::Command::new("pkill")
        .args(["-f", "mywm-workspaces.sh"])
        .status();
    let _ = std::process::Command::new("pkill")
        .args(["-f", "mywm-opacity.sh"])
        .status();
    let _ = std::process::Command::new("pkill")
        .args(["-x", "waybar"])
        .status();

    kill_private_dbus_session();
    cleanup_isolated_home();

    info!("clean shutdown complete");
    Ok(())
}

// -------------------------------------------------------------------------
// ensure_waybar_config — deploys Waybar + mywm scripts to isolated HOME
// -------------------------------------------------------------------------

fn ensure_waybar_config() {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        warn!("ensure_waybar_config: HOME is empty, skipping");
        return;
    }

    // ── Waybar config directory ──
    let waybar_dir = format!("{}/.config/waybar", home);
    let waybar_path = std::path::Path::new(&waybar_dir);
    if waybar_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        info!(path = %waybar_dir, "removing waybar config symlink");
        let _ = std::fs::remove_file(&waybar_dir);
    }
    if waybar_path.is_dir() {
        info!(path = %waybar_dir, "removing existing waybar config dir");
        let _ = std::fs::remove_dir_all(&waybar_dir);
    }
    if let Err(err) = std::fs::create_dir_all(&waybar_dir) {
        warn!(?err, dir = %waybar_dir, "failed to create waybar config dir");
        return;
    }

    let config_path = format!("{}/config", waybar_dir);
    info!(path = %config_path, "writing waybar config from asset");
    let _ = std::fs::write(&config_path, include_str!("../assets/waybar-config.json"));

    let style_path = format!("{}/style.css", waybar_dir);
    info!(path = %style_path, "writing waybar style.css from asset");
    let _ = std::fs::write(&style_path, include_str!("../assets/waybar-style.css"));

    let colors_path = format!("{}/colors.css", waybar_dir);
    info!(path = %colors_path, "writing bootstrap colors.css");
    let _ = std::fs::write(&colors_path, r#"/* Bootstrap colors — overwritten by rc.lua write_theme_css() */
@define-color bg_color #1a1b26;
@define-color bg_alt_color #24283b;
@define-color bg_surface_color #292e42;
@define-color fg_color #c0caf5;
@define-color fg_dim_color #565f89;
@define-color fg_bright_color #e0e6ff;
@define-color accent_color #7aa2f7;
@define-color accent2_color #bb9af7;
@define-color accent3_color #ff007c;
@define-color green_color #73daca;
@define-color red_color #f7768e;
@define-color orange_color #ff9e64;
@define-color yellow_color #e0af68;
@define-color cyan_color #7dcfff;
@define-color teal_color #2ac3de;
@define-color magenta_color #c678dd;
@define-color pink_color #ff79c6;
@define-color urgent_color #db4b4b;
@define-color success_color #9ece6a;
@define-color warning_color #e0af68;
@define-color active_border #7aa2f7;
@define-color inactive_border #1a1b26;
@define-color bar_bg_color rgba(26, 27, 38, 0.92);
@define-color accent_hover rgba(122, 162, 247, 0.15);
@define-color accent_subtle rgba(122, 162, 247, 0.10);
@define-color accent_border rgba(122, 162, 247, 0.30);
@define-color red_hover rgba(247, 118, 142, 0.20);
@define-color red_subtle rgba(247, 118, 142, 0.10);
@define-color orange_hover rgba(255, 158, 100, 0.18);
@define-color green_subtle rgba(115, 218, 202, 0.10);
@define-color separator_color rgba(59, 66, 97, 0.50);
@define-color border_glow rgba(122, 162, 247, 0.35);
"#);

    // ── Deploy mywm scripts ──
    let scripts_dir = format!("{}/.config/mywm/scripts", home);
    if let Err(err) = std::fs::create_dir_all(&scripts_dir) {
        warn!(?err, dir = %scripts_dir, "failed to create mywm scripts dir");
        return;
    }

    let opacity_sh = format!("{}/mywm-opacity.sh", scripts_dir);
    info!(path = %opacity_sh, "writing mywm-opacity.sh");
    let _ = std::fs::write(&opacity_sh, include_str!("../assets/mywm-opacity.sh"));
    set_executable(&opacity_sh);

    let opacity_control_sh = format!("{}/mywm-opacity-control.sh", scripts_dir);
    info!(path = %opacity_control_sh, "writing mywm-opacity-control.sh");
    let _ = std::fs::write(&opacity_control_sh, include_str!("../assets/mywm-opacity-control.sh"));
    set_executable(&opacity_control_sh);

    let workspaces_sh = format!("{}/mywm-workspaces.sh", scripts_dir);
    info!(path = %workspaces_sh, "writing mywm-workspaces.sh");
    let _ = std::fs::write(&workspaces_sh, include_str!("../assets/mywm-workspaces.sh"));
    set_executable(&workspaces_sh);

    // ── Clean stale waybar config from real HOME ──
    if let Ok(real_home) = std::env::var("MYWM_REAL_HOME") {
        let real_waybar_dir = format!("{}/.config/waybar", real_home);
        let real_waybar_path = std::path::Path::new(&real_waybar_dir);
        if real_waybar_path.is_dir() {
            info!(path = %real_waybar_dir, "cleaning stale waybar config from real HOME");
            let _ = std::fs::remove_dir_all(&real_waybar_dir);
        }
    }

    info!(waybar_dir = %waybar_dir, "waybar config deployed");
}

// -------------------------------------------------------------------------
// DRM backend init
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
) -> Result<(DrmBackend, GlesRenderer, Output, smithay::backend::drm::DrmDeviceNotifier), BackendError> {
    let seat_name = session.seat();
    let path = primary_gpu(&seat_name).ok().flatten().ok_or(BackendError::NoPrimaryGpu)?;
    info!(?path, "DRM: primary GPU path resolved");

    let drm_node = DrmNode::from_path(&path)
        .map_err(|e| BackendError::OpenDevice(path.clone(), e.to_string()))?;
    info!(?drm_node, node_type = ?drm_node.ty(), "DRM: node opened");

    let owned_fd = session
        .open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
        .map_err(|e| BackendError::OpenDevice(path.clone(), format!("{e:?}")))?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(owned_fd));
    info!("DRM: session opened device fd");

    let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), true)?;
    info!(atomic = true, "DRM: DrmDevice initialised");

    let gbm = GbmDevice::new(drm_fd.clone())?;
    info!("GBM: device created");

    let egl_display = unsafe { EGLDisplay::new(gbm.clone())? };
    info!("EGL: display created");

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
            connector = ?info.interface(), id = ?info.handle(),
            state = ?info.state(), modes = info.modes().len(),
            "DRM: probed connector"
        );
        if info.state() == connector::State::Connected && !info.modes().is_empty() {
            let mode = info.modes().iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .copied()
                .unwrap_or_else(|| info.modes()[0]);
            info!(?mode, "DRM: selected connector + mode");
            picked = Some((info, mode));
            break;
        }
    }
    let (connector_info, mode) = picked.ok_or(BackendError::NoConnector)?;

    let crtc = connector_info.encoders().iter()
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
        Some(wl_mode), Some(Transform::Normal),
        Some(smithay::output::Scale::Integer(1)), Some((0, 0).into()),
    );
    output.set_preferred(wl_mode);
    info!(size = ?mode_size, refresh = mode.vrefresh(), "DRM: wl_output advertised");

    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), smithay::backend::drm::exporter::gbm::NodeFilter::All);
    let renderer_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let raw_cursor_size = drm_device.cursor_size();
    let cursor_size = if raw_cursor_size.w > 0 && raw_cursor_size.h > 0 {
        raw_cursor_size
    } else {
        (64u32, 64u32).into()
    };
    let cursor_size = cursor_size.to_logical(1, Transform::Normal).to_buffer(1, Transform::Normal);

    let compositor = DrmCompositor::new(
        smithay::output::OutputModeSource::Auto(output.downgrade()),
        surface, None, allocator, framebuffer_exporter,
        [Fourcc::Abgr8888, Fourcc::Argb8888],
        renderer_formats, cursor_size, Some(gbm),
    ).map_err(|e| BackendError::Compositor(format!("{e:?}")))?;
    info!("DRM: compositor ready");

    let backend = DrmBackend {
        drm_node, compositor, crtc,
        frame_sent: HashSet::new(),
        pending_frame: false,
    };

    Ok((backend, renderer, output, drm_notifier))
}

// -------------------------------------------------------------------------
// Render elements declaration
// -------------------------------------------------------------------------

use smithay::backend::renderer::element::utils::{RelocateRenderElement, Relocate};

smithay::backend::renderer::element::render_elements! {
    OutputRenderElements<=GlesRenderer>;
    Space = SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Relocated = RelocateRenderElement<SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>>,
    Cursor = SolidColorRenderElement,
}

// -------------------------------------------------------------------------
// Render frame
// -------------------------------------------------------------------------

fn render_frame(data: &mut CalloopData) {
    let CalloopData { state, backend } = data;

    if backend.pending_frame {
        return;
    }

    let now = Instant::now();
    let border_width = state.config.border_width.max(0);
    let focused_surface = state.keyboard.current_focus();
    let active_color = crate::config::parse_hex_color(&state.config.active_border_color);
    let inactive_color = crate::config::parse_hex_color(&state.config.inactive_border_color);
    let global_opacity = state.window_opacity;

    let screen_width = state.workspaces[state.active_workspace]
        .space
        .output_geometry(&state.output)
        .map(|g| g.size.w)
        .unwrap_or(1920);

    let mut all_elements: Vec<OutputRenderElements> = Vec::new();

    // ── Cursor on top of everything ──
    let cursor_loc = state.pointer_location.to_i32_round().to_physical(1);
    let cursor_elem = SolidColorRenderElement::from_buffer(
        &state.cursor_buffer, cursor_loc, 1.0, 1.0, Kind::Cursor,
    );
    all_elements.push(OutputRenderElements::Cursor(cursor_elem));

    if state.workspace_transition.active {
        let transition = state.workspace_transition.clone();
        let from_offset = transition.from_offset(screen_width);
        let to_offset = transition.to_offset(screen_width);
        let from_ws = transition.from_workspace;
        let to_ws = transition.to_workspace;

        // "from" workspace
                let from_space = &state.workspaces[from_ws].space;
        let from_spaces = [from_space];
        if let Ok(from_elements) = space_render_elements(
            &mut state.renderer, from_spaces, &state.output, global_opacity,
        ) {
            let from_borders = build_border_and_glow_elements(
                &state.workspaces[from_ws], border_width, &focused_surface,
                &active_color, &inactive_color, from_offset, now, global_opacity,
            );
            for elem in from_elements {
                all_elements.push(relocate_space_element(elem, from_offset));
            }
            all_elements.extend(from_borders);
        }

        all_elements.extend(build_dying_window_elements(
            &state.workspaces[from_ws], now, from_offset, global_opacity,
        ));

        // "to" workspace
        let to_space = &state.workspaces[to_ws].space;
        let to_spaces = [to_space];
        if let Ok(to_elements) = space_render_elements(
            &mut state.renderer, to_spaces, &state.output, global_opacity,
        ) {
            let to_borders = build_border_and_glow_elements(
                &state.workspaces[to_ws], border_width, &focused_surface,
                &active_color, &inactive_color, to_offset, now, global_opacity,
            );
            for elem in to_elements {
                all_elements.push(relocate_space_element(elem, to_offset));
            }
            all_elements.extend(to_borders);
        }

        all_elements.extend(build_dying_window_elements(
            &state.workspaces[to_ws], now, to_offset, global_opacity,
        ));
        } else {
        let active_ws_idx = state.active_workspace;
        let active_space = &state.workspaces[active_ws_idx].space;
        let spaces = [active_space];
        match space_render_elements(&mut state.renderer, spaces, &state.output, global_opacity) {
            Ok(space_elements) => {
                let border_elements = build_border_and_glow_elements(
                    &state.workspaces[active_ws_idx], border_width,
                    &focused_surface, &active_color, &inactive_color, 0, now,
                    global_opacity,
                );

                all_elements.extend(border_elements);

                let fade_in_overlays = build_fade_in_overlays(
                    &state.workspaces[active_ws_idx], now, global_opacity,
                    &state.config.clear_color,
                );
                all_elements.extend(fade_in_overlays);

                all_elements.extend(
                    space_elements.into_iter().map(OutputRenderElements::Space),
                );
            }
            Err(err) => {
                warn!(?err, "space_render_elements failed");
                return;
            }
        }

        all_elements.extend(build_dying_window_elements(
            &state.workspaces[active_ws_idx], now, 0, global_opacity,
        ));
    }

    let _ = &state.cursor_status;

    match backend.compositor.render_frame::<_, _>(
        &mut state.renderer, &all_elements,
        Color32F::from(state.config.clear_color_f32()),
        FrameFlags::DEFAULT,
    ) {
        Ok(frame) => {
            if frame.is_empty
                && !state.workspace_transition.active
                && !state.any_animating()
            {
                trace!("no damage — VBlank loop paused");
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

    // ── Send frame callbacks ──
    let elapsed = state.start_time.elapsed();
    let output = state.output.clone();

    state.workspaces[state.active_workspace]
        .space
        .elements()
        .for_each(|window| {
            window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        });

    if state.workspace_transition.active {
        let from_ws = state.workspace_transition.from_workspace;
        if from_ws != state.active_workspace {
            state.workspaces[from_ws].space.elements().for_each(|window| {
                window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            });
        }
    }

    {
        let map = layer_map_for_output(&output);
        for layer in map.layers() {
            layer.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
    }

    state.workspaces[state.active_workspace].space.refresh();
    state.popups.cleanup();

    if let Err(err) = state.display_handle.flush_clients() {
        if err.kind() != std::io::ErrorKind::BrokenPipe {
            warn!(?err, "failed to flush wayland clients");
        }
    }
}

// -------------------------------------------------------------------------
// Border + Glow rendering
// -------------------------------------------------------------------------

const GLOW_LAYERS: &[(i32, f32)] = &[
    (2, 0.35),
    (4, 0.18),
    (6, 0.07),
];

fn build_border_and_glow_elements(
    ws: &Workspace,
    border_width: i32,
    focused_surface: &Option<WlSurface>,
    active_color: &[f32; 4],
    inactive_color: &[f32; 4],
    x_offset: i32,
    now: Instant,
    global_opacity: f32,
) -> Vec<OutputRenderElements> {
    let mut elements = Vec::new();
    let bw = border_width;

    if bw <= 0 {
        return elements;
    }

    for window in ws.space.elements() {
        let Some(loc) = ws.space.element_location(window) else {
            continue;
        };

        let geo = window.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            continue;
        }

        let is_focused = window
            .toplevel()
            .map(|t| focused_surface.as_ref() == Some(t.wl_surface()))
            .unwrap_or(false);

        let spawn_alpha = ws.spawn_times.get(window)
            .and_then(|t| animation_progress(now, *t))
            .map(animation_alpha)
            .unwrap_or(1.0);
        let window_alpha = spawn_alpha * global_opacity;

        if is_focused {
            for &(expansion, layer_alpha) in GLOW_LAYERS.iter().rev() {
                let glow_alpha = layer_alpha * window_alpha;
                let color = [
                    active_color[0] * glow_alpha,
                    active_color[1] * glow_alpha,
                    active_color[2] * glow_alpha,
                    glow_alpha,
                ];

                let gx = loc.x - bw - expansion + x_offset;
                let gy = loc.y - bw - expansion;
                let gw = geo.size.w + 2 * bw + 2 * expansion;
                let gh = geo.size.h + 2 * bw + 2 * expansion;

                let buf = SolidColorBuffer::new((gw, expansion + bw), color);
                let elem = SolidColorRenderElement::from_buffer(
                    &buf, Point::<i32, Physical>::from((gx, gy)),
                    1.0, 1.0, Kind::Unspecified,
                );
                elements.push(OutputRenderElements::Cursor(elem));

                let buf = SolidColorBuffer::new((gw, expansion + bw), color);
                let elem = SolidColorRenderElement::from_buffer(
                    &buf,
                    Point::<i32, Physical>::from((gx, gy + gh - expansion - bw)),
                    1.0, 1.0, Kind::Unspecified,
                );
                elements.push(OutputRenderElements::Cursor(elem));

                let inner_h = gh - 2 * (expansion + bw);
                if inner_h > 0 {
                    let buf = SolidColorBuffer::new((expansion + bw, inner_h), color);
                    let elem = SolidColorRenderElement::from_buffer(
                        &buf,
                        Point::<i32, Physical>::from((gx, gy + expansion + bw)),
                        1.0, 1.0, Kind::Unspecified,
                    );
                    elements.push(OutputRenderElements::Cursor(elem));
                }

                if inner_h > 0 {
                    let buf = SolidColorBuffer::new((expansion + bw, inner_h), color);
                    let elem = SolidColorRenderElement::from_buffer(
                        &buf,
                        Point::<i32, Physical>::from((
                            gx + gw - expansion - bw,
                            gy + expansion + bw,
                        )),
                        1.0, 1.0, Kind::Unspecified,
                    );
                    elements.push(OutputRenderElements::Cursor(elem));
                }
            }

            let a = active_color[3] * window_alpha;
            let border_color = [
                active_color[0] * a,
                active_color[1] * a,
                active_color[2] * a,
                a,
            ];
            render_flat_border(
                &mut elements, loc, geo.size.w, geo.size.h,
                bw, x_offset, &border_color,
            );
        } else {
            let a = inactive_color[3] * window_alpha;
            let border_color = [
                inactive_color[0] * a,
                inactive_color[1] * a,
                inactive_color[2] * a,
                a,
            ];
            render_flat_border(
                &mut elements, loc, geo.size.w, geo.size.h,
                bw, x_offset, &border_color,
            );
        }
    }

    elements
}

fn render_flat_border(
    elements: &mut Vec<OutputRenderElements>,
    loc: Point<i32, Logical>,
    win_w: i32,
    win_h: i32,
    bw: i32,
    x_offset: i32,
    color: &[f32; 4],
) {
    let outer_x = loc.x - bw + x_offset;
    let outer_y = loc.y - bw;
    let outer_w = win_w + 2 * bw;
    let outer_h = win_h + 2 * bw;

    let buf = SolidColorBuffer::new((outer_w, bw), *color);
    let elem = SolidColorRenderElement::from_buffer(
        &buf, Point::<i32, Physical>::from((outer_x, outer_y)),
        1.0, 1.0, Kind::Unspecified,
    );
    elements.push(OutputRenderElements::Cursor(elem));

    let buf = SolidColorBuffer::new((outer_w, bw), *color);
    let elem = SolidColorRenderElement::from_buffer(
        &buf, Point::<i32, Physical>::from((outer_x, outer_y + outer_h - bw)),
        1.0, 1.0, Kind::Unspecified,
    );
    elements.push(OutputRenderElements::Cursor(elem));

    let buf = SolidColorBuffer::new((bw, outer_h - 2 * bw), *color);
    let elem = SolidColorRenderElement::from_buffer(
        &buf, Point::<i32, Physical>::from((outer_x, outer_y + bw)),
        1.0, 1.0, Kind::Unspecified,
    );
    elements.push(OutputRenderElements::Cursor(elem));

    let buf = SolidColorBuffer::new((bw, outer_h - 2 * bw), *color);
    let elem = SolidColorRenderElement::from_buffer(
        &buf,
        Point::<i32, Physical>::from((outer_x + outer_w - bw, outer_y + bw)),
        1.0, 1.0, Kind::Unspecified,
    );
    elements.push(OutputRenderElements::Cursor(elem));
}

// -------------------------------------------------------------------------
// Fade-in overlays
// -------------------------------------------------------------------------

fn build_fade_in_overlays(
    ws: &Workspace,
    now: Instant,
    global_opacity: f32,
    clear_color_hex: &str,
    ) -> Vec<OutputRenderElements> {
    let mut elements = Vec::new();
    let clear = crate::config::parse_hex_color(clear_color_hex);

    for window in ws.space.elements() {
        let Some(spawn_time) = ws.spawn_times.get(window) else {
            continue;
        };
        let Some(progress) = animation_progress(now, *spawn_time) else {
            continue;
        };

        let alpha = animation_alpha(progress);
        let overlay_alpha = (1.0 - alpha) * global_opacity;
        if overlay_alpha < 0.01 {
            continue;
        }

        let Some(loc) = ws.space.element_location(window) else {
            continue;
        };
        let geo = window.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            continue;
        }

        let color = [clear[0], clear[1], clear[2], overlay_alpha];
        let buf = SolidColorBuffer::new((geo.size.w, geo.size.h), color);
        let elem = SolidColorRenderElement::from_buffer(
            &buf,
            Point::<i32, Physical>::from((loc.x, loc.y)),
            1.0,
            1.0,
            Kind::Unspecified,
        );
        elements.push(OutputRenderElements::Cursor(elem));
    }

    elements
}

// -------------------------------------------------------------------------
// Dying window fade-out elements
// -------------------------------------------------------------------------

fn build_dying_window_elements(
    ws: &Workspace,
    now: Instant,
    x_offset: i32,
    global_opacity: f32,
) -> Vec<OutputRenderElements> {
    let mut elements = Vec::new();

    for dw in &ws.dying_windows {
        let Some(fade_alpha) = dw.fade_alpha(now) else {
            continue;
        };

        let alpha = fade_alpha * global_opacity;
        if alpha < 0.01 {
            continue;
        }

        let w = dw.last_size.0.max(1);
        let h = dw.last_size.1.max(1);
        let x = dw.last_location.x + x_offset;
        let y = dw.last_location.y;

        let color = [0.15, 0.12, 0.20, alpha * 0.7];
        let buf = SolidColorBuffer::new((w, h), color);
        let elem = SolidColorRenderElement::from_buffer(
            &buf,
            Point::<i32, Physical>::from((x, y)),
            1.0,
            1.0,
            Kind::Unspecified,
        );
        elements.push(OutputRenderElements::Cursor(elem));

        let border_alpha = alpha * 0.3;
        if border_alpha > 0.01 {
            let border_color = [0.5, 0.5, 0.6, border_alpha];
            let bw = 2;

            let buf = SolidColorBuffer::new((w + 2 * bw, bw), border_color);
            let elem = SolidColorRenderElement::from_buffer(
                &buf,
                Point::<i32, Physical>::from((x - bw, y - bw)),
                1.0, 1.0, Kind::Unspecified,
            );
            elements.push(OutputRenderElements::Cursor(elem));

            let buf = SolidColorBuffer::new((w + 2 * bw, bw), border_color);
            let elem = SolidColorRenderElement::from_buffer(
                &buf,
                Point::<i32, Physical>::from((x - bw, y + h)),
                1.0, 1.0, Kind::Unspecified,
            );
                        elements.push(OutputRenderElements::Cursor(elem));

            let buf = SolidColorBuffer::new((bw, h), border_color);
            let elem = SolidColorRenderElement::from_buffer(
                &buf,
                Point::<i32, Physical>::from((x - bw, y)),
                1.0, 1.0, Kind::Unspecified,
            );
            elements.push(OutputRenderElements::Cursor(elem));

            let buf = SolidColorBuffer::new((bw, h), border_color);
            let elem = SolidColorRenderElement::from_buffer(
                &buf,
                Point::<i32, Physical>::from((x + w, y)),
                1.0, 1.0, Kind::Unspecified,
            );
            elements.push(OutputRenderElements::Cursor(elem));
        }
    }

    elements
}

// -------------------------------------------------------------------------
// Element relocation helper
// -------------------------------------------------------------------------

fn relocate_space_element(
    elem: SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    x_offset: i32,
) -> OutputRenderElements {
    if x_offset == 0 {
        OutputRenderElements::Space(elem)
    } else {
        let relocated = RelocateRenderElement::from_element(
            elem,
            Point::<i32, Physical>::from((x_offset, 0)),
            Relocate::Relative,
        );
        OutputRenderElements::Relocated(relocated)
    }
}

// -------------------------------------------------------------------------
// Hex color parser
// -------------------------------------------------------------------------

fn parse_hex_color(hex: &str) -> [f32; 4] {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return [1.0, 1.0, 1.0, 1.0];
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255) as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255) as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255) as f32 / 255.0;
    let a = if hex.len() >= 8 {
        u8::from_str_radix(&hex[6..8], 16).unwrap_or(255) as f32 / 255.0
    } else {
        1.0
    };
    [r, g, b, a]
}

// -------------------------------------------------------------------------
// DRM event handler
// -------------------------------------------------------------------------

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