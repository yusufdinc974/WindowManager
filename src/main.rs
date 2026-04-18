//! Phase 10: bare-metal Wayland compositor entry point.

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
    CalloopData, ClientState, DrmBackend, State, Workspace,
};

// -------------------------------------------------------------------------
// Session environment isolation
// -------------------------------------------------------------------------
/// Forcibly configure the process environment so that every child process
/// (terminals, browsers, launchers …) connects to OUR Wayland display and
/// never falls back to an X11/Wayland session on another TTY.
///
/// This MUST run:
///   1. AFTER the listening socket is created (so we know the socket name),
///   2. BEFORE any `std::process::Command` spawn (autostart, terminals …).
fn isolate_session_environment(socket_name: &std::ffi::OsStr) {
    // ── Core Wayland identity ──
    std::env::set_var("WAYLAND_DISPLAY", socket_name);
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
    std::env::set_var("XDG_CURRENT_DESKTOP", "mywm");
    std::env::set_var("XDG_SESSION_DESKTOP", "mywm");

    // ── Kill any X11 fallback path ──
    std::env::remove_var("DISPLAY");

    // ── Per-toolkit Wayland enforcement ──
    std::env::set_var("MOZ_ENABLE_WAYLAND", "1");
    std::env::set_var("GDK_BACKEND", "wayland");
    std::env::set_var("QT_QPA_PLATFORM", "wayland");
    std::env::set_var("QT_WAYLAND_DISABLE_WINDOWDECORATION", "1");
    std::env::set_var("SDL_VIDEODRIVER", "wayland");
    std::env::set_var("WINIT_UNIX_BACKEND", "wayland");
    std::env::set_var("CLUTTER_BACKEND", "wayland");
    std::env::set_var("ELECTRON_OZONE_PLATFORM_HINT", "wayland");

    // ── Accessibility bus suppression ──
    std::env::set_var("NO_AT_BRIDGE", "1");
    std::env::set_var("GTK_A11Y", "none");

    // ── Session-unique identifier ──
    // Used to isolate profile directories, DBus, etc.
    let session_id = format!("mywm-{}", std::process::id());

    // ══════════════════════════════════════════════════════════════
    //  FIREFOX / THUNDERBIRD ISOLATION
    //
    //  Firefox discovers running instances via a LOCK FILE inside
    //  the profile directory (~/.mozilla/firefox/<profile>/lock).
    //  Neither --no-remote nor MOZ_NO_REMOTE prevents the new
    //  process from acting as a *client* that hands off to the
    //  existing instance and exits.
    //
    //  The ONLY reliable fix: give this session its own profile
    //  root so there is no shared lock file to discover.
    // ══════════════════════════════════════════════════════════════
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        // Per-session Firefox profile root.
        // Firefox will create a fresh "default" profile here on first launch.
        let moz_dir = format!("{}/{}/mozilla", runtime_dir, session_id);
        let tb_dir = format!("{}/{}/thunderbird", runtime_dir, session_id);

        // Create the directories so Firefox doesn't error out.
        let _ = std::fs::create_dir_all(&moz_dir);
        let _ = std::fs::create_dir_all(&tb_dir);

        // HOME/.mozilla → overridden by these env vars.
        // Firefox checks these BEFORE falling back to ~/.mozilla.
        std::env::set_var("MOZ_LEGACY_PROFILES", "0");
        std::env::set_var("MOZ_NO_REMOTE", "1");
        std::env::set_var("MOZ_DBUS_REMOTE", "0");

        // The actual isolation: point Firefox's profile root elsewhere.
        // Firefox respects the -profile flag but NOT an env var for the
        // profile *root*. However, we can override HOME for child
        // processes so ~/.mozilla resolves to our isolated directory.
        //
        // We use a session-private HOME overlay approach:
        let isolated_home = format!("{}/{}/home", runtime_dir, session_id);
        let real_home = std::env::var("HOME").unwrap_or_default();

        let _ = std::fs::create_dir_all(&isolated_home);

        // Symlink everything from real HOME except .mozilla and .thunderbird
        setup_isolated_home(&real_home, &isolated_home, &moz_dir, &tb_dir);

        std::env::set_var("HOME", &isolated_home);

        // Preserve the real home for apps that need it (e.g. file dialogs).
        std::env::set_var("MYWM_REAL_HOME", &real_home);

        info!(
            isolated_home = %isolated_home,
            moz_dir = %moz_dir,
            "session: HOME isolated for single-instance app separation"
        );
    }

    // ── DBus session isolation ──
    match launch_private_dbus_session() {
        Ok(addr) => {
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);
            info!(bus = %addr, "session: launched private DBus session bus");
        }
        Err(err) => {
            if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok() {
                warn!(
                    ?err,
                    "session: failed to launch private DBus — keeping inherited bus address"
                );
            } else {
                warn!(
                    ?err,
                    "session: no DBus session bus available — Waybar and other clients may fail to start"
                );
            }
        }
    }

    info!(
        wayland_display = ?socket_name,
        "session: environment variables locked to this compositor"
    );
}

/// Create an isolated HOME directory that symlinks everything from the
/// real HOME *except* browser profile directories, which get their own
/// fresh copies so profile lock files don't collide across sessions.
fn setup_isolated_home(
    real_home: &str,
    isolated_home: &str,
    _moz_dir: &str,
    _tb_dir: &str,
) {
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

        // For .config, make it a REAL directory and symlink its contents
        // so we can write new files (like waybar config) into it
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
            tracing::trace!(
                ?err,
                entry = %name_str,
                "isolated home: failed to symlink"
            );
        }
    }

    // Ensure isolated .mozilla and .thunderbird exist as real dirs
    let iso_moz_ff = iso.join(".mozilla").join("firefox");
    let _ = std::fs::create_dir_all(&iso_moz_ff);

    let iso_tb = iso.join(".thunderbird");
    let _ = std::fs::create_dir_all(&iso_tb);

    info!(
        real_home,
        isolated_home,
        "session: isolated HOME set up"
    );
}

/// Spawn a private `dbus-daemon --session` and return its bus address.
fn launch_private_dbus_session() -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("dbus-launch")
        .arg("--sh-syntax")
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "dbus-launch exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("DBUS_SESSION_BUS_ADDRESS=") {
            let addr = rest
                .trim_end_matches(';')
                .trim_matches('\'')
                .trim_matches('"');
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

    Err(format!(
        "could not parse DBUS_SESSION_BUS_ADDRESS from dbus-launch output: {stdout}"
    )
    .into())
}

static DBUS_DAEMON_PID: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

fn kill_private_dbus_session() {
    let pid = DBUS_DAEMON_PID.load(std::sync::atomic::Ordering::SeqCst);
    if pid != 0 {
        info!(pid, "session: killing private dbus-daemon");
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
}

/// Clean up the isolated home directory on shutdown.
fn cleanup_isolated_home() {
    if let (Ok(runtime_dir), Ok(real_home)) = (
        std::env::var("XDG_RUNTIME_DIR"),
        std::env::var("MYWM_REAL_HOME"),
    ) {
        // Restore HOME so nothing writes to the isolated dir during shutdown.
        std::env::set_var("HOME", &real_home);

        // Remove the per-session directory tree.
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("bootstrapping bare-metal Wayland compositor (Phase 10)");

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

    let dmabuf_formats = renderer
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

    let compositor_state = CompositorState::new::<State>(&display_handle);
    let xdg_shell_state = XdgShellState::new::<State>(&display_handle);
    let xdg_decoration_state = XdgDecorationState::new::<State>(&display_handle);
    let layer_shell_state = WlrLayerShellState::new::<State>(&display_handle);
    let shm_state = ShmState::new::<State>(&display_handle, vec![]);
    let output_manager_state =
        OutputManagerState::new_with_xdg_output::<State>(&display_handle);
    let data_device_state = DataDeviceState::new::<State>(&display_handle);

    let mut seat_state: SeatState<State> = SeatState::new();

    let mut seat: Seat<State> =
        seat_state.new_wl_seat(&display_handle, "seat0");

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

    // ── Create the listening socket ──
    let listening_socket = ListeningSocketSource::new_auto()?;
    let socket_name = listening_socket.socket_name().to_os_string();
    info!(?socket_name, "listening for wayland clients");

    // ══════════════════════════════════════════════════════════════════
    //  CRITICAL: Lock the environment to THIS compositor BEFORE any
    //  child process can be spawned (event sources, autostart, etc.).
    // ══════════════════════════════════════════════════════════════════
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

    let drm_token: RegistrationToken = event_loop.handle().insert_source(
        drm_notifier,
        |event, _meta, data| {
            handle_drm_event(event, data);
        },
    )?;

    // ── Autostart (runs AFTER env isolation) ──
      {
        let autostart_result: Result<mlua::Value, _> =
            lua.load("return wm.autostart").eval();
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


    /// Write the default Waybar config and CSS if they don't exist yet.
    /// Must be called AFTER isolate_session_environment() so HOME points
    /// to the correct (possibly isolated) directory.
    fn ensure_waybar_config() {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            warn!("ensure_waybar_config: HOME is empty, skipping");
            return;
        }

        let waybar_dir = format!("{}/.config/waybar", home);
        
        // If waybar dir is a symlink (from setup_isolated_home), remove it
        // and create a real directory so we control the config
        let waybar_path = std::path::Path::new(&waybar_dir);
        if waybar_path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            info!(path = %waybar_dir, "removing waybar config symlink to write our own");
            let _ = std::fs::remove_file(&waybar_dir);
        }
        
        if let Err(err) = std::fs::create_dir_all(&waybar_dir) {
            warn!(?err, dir = %waybar_dir, "failed to create waybar config dir");
            return;
        }

        // Always overwrite config to ensure it matches our compositor
        let config_path = format!("{}/config", waybar_dir);
        info!(path = %config_path, "writing waybar config");
        let _ = std::fs::write(&config_path, include_str!("../assets/waybar-config.json"));

        let style_path = format!("{}/style.css", waybar_dir);
        info!(path = %style_path, "writing waybar style.css");
        let _ = std::fs::write(&style_path, include_str!("../assets/waybar-style.css"));

        let script_dir = format!("{}/.config/mywm/scripts", home);
        let _ = std::fs::create_dir_all(&script_dir);

        let script_path = format!("{}/mywm-workspaces.sh", script_dir);
        info!(path = %script_path, "writing workspace script for waybar");
        let _ = std::fs::write(&script_path, include_str!("../assets/mywm-workspaces.sh"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &script_path,
                std::fs::Permissions::from_mode(0o755),
            );
        }

        info!(waybar_dir = %waybar_dir, "waybar config ensured");
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
    };

    let mut data = CalloopData {
        state,
        backend: drm_backend,
    };

     event_loop.handle().insert_idle(|data| {
        info!("broadcasting initial workspace state for waybar");
        data.state.broadcast_workspace_state();
    });

    // Re-broadcast workspace state periodically so waybar picks it up
    // once it finishes connecting (it starts via autostart with a slight delay).
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

    drop(data.backend);
    event_loop.handle().remove(drm_token);
    drop(data.state);
    drop(event_loop);
    drop(session);

    let _ = std::fs::remove_file(ipc_socket_path);
    let _ = std::fs::remove_file(crate::state::workspace_ipc_path());
    let _ = std::fs::remove_file(crate::state::workspace_ipc_stream_path());

    // Kill our private dbus-daemon so it doesn't linger.
    kill_private_dbus_session();
    cleanup_isolated_home();

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
        GlesRenderer,
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
        compositor,
        crtc,
        frame_sent: HashSet::new(),
        pending_frame: false,
    };

    Ok((backend, renderer, output, drm_notifier))
}

smithay::backend::renderer::element::render_elements! {
    OutputRenderElements<=GlesRenderer>;
    Space = SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Cursor = SolidColorRenderElement,
}

fn render_frame(data: &mut CalloopData) {
    let CalloopData { state, backend } = data;

    if backend.pending_frame {
        return;
    }

    let active_space = &state.workspaces[state.active_workspace].space;
    let spaces = [active_space];
    let space_elements = match space_render_elements(
        &mut state.renderer,
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

    // ── Build border elements for every mapped window ──
    let border_width = state.config.border_width.max(0);
    let focused_surface = state.keyboard.current_focus();

    let active_color = crate::config::parse_hex_color(&state.config.active_border_color);
    let inactive_color = crate::config::parse_hex_color(&state.config.inactive_border_color);

    let mut border_elements: Vec<OutputRenderElements> = Vec::new();

    let ws = &state.workspaces[state.active_workspace];
    for window in ws.space.elements() {
        if border_width <= 0 {
            break;
        }

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

        let color = if is_focused { active_color } else { inactive_color };

        let bw = border_width;

        let outer_x = loc.x - bw;
        let outer_y = loc.y - bw;
        let outer_w = geo.size.w + 2 * bw;
        let outer_h = geo.size.h + 2 * bw;

        // Top edge
        let top_buf = SolidColorBuffer::new((outer_w, bw), color);
        let top_elem = SolidColorRenderElement::from_buffer(
            &top_buf,
            Point::<i32, Physical>::from((outer_x, outer_y)),
            1.0,
            1.0,
            Kind::Unspecified,
        );
        border_elements.push(OutputRenderElements::Cursor(top_elem));

        // Bottom edge
        let bot_buf = SolidColorBuffer::new((outer_w, bw), color);
        let bot_elem = SolidColorRenderElement::from_buffer(
            &bot_buf,
            Point::<i32, Physical>::from((outer_x, outer_y + outer_h - bw)),
            1.0,
            1.0,
            Kind::Unspecified,
        );
        border_elements.push(OutputRenderElements::Cursor(bot_elem));

        // Left edge
        let left_buf = SolidColorBuffer::new((bw, outer_h - 2 * bw), color);
        let left_elem = SolidColorRenderElement::from_buffer(
            &left_buf,
            Point::<i32, Physical>::from((outer_x, outer_y + bw)),
            1.0,
            1.0,
            Kind::Unspecified,
        );
        border_elements.push(OutputRenderElements::Cursor(left_elem));

        // Right edge
        let right_buf = SolidColorBuffer::new((bw, outer_h - 2 * bw), color);
        let right_elem = SolidColorRenderElement::from_buffer(
            &right_buf,
            Point::<i32, Physical>::from((outer_x + outer_w - bw, outer_y + bw)),
            1.0,
            1.0,
            Kind::Unspecified,
        );
        border_elements.push(OutputRenderElements::Cursor(right_elem));
    }

    // ── Assemble final element list ──
    // Order: cursor (top) → windows → borders (behind windows)
    let mut wrapped: Vec<OutputRenderElements> =
        Vec::with_capacity(space_elements.len() + border_elements.len() + 1);

    // Cursor on top of everything.
    let cursor_loc = state.pointer_location.to_i32_round().to_physical(1);
    let cursor_elem = SolidColorRenderElement::from_buffer(
        &state.cursor_buffer,
        cursor_loc,
        1.0,
        1.0,
        Kind::Cursor,
    );
    wrapped.push(OutputRenderElements::Cursor(cursor_elem));

    // Window surfaces.
    wrapped.extend(space_elements.into_iter().map(OutputRenderElements::Space));

    // Borders behind the windows.
    wrapped.extend(border_elements);

    let _ = &state.cursor_status;

    match backend.compositor.render_frame::<_, _>(
        &mut state.renderer,
        &wrapped,
        Color32F::from(state.config.clear_color_f32()),
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

    // Send frame callbacks to tiled/floating windows.
    for ws in state.workspaces.iter() {
        ws.space.elements().for_each(|window| {
            window.send_frame(&output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        });
    }

    // Send frame callbacks to layer surfaces (waybar, fuzzel, etc.).
    {
        let map = layer_map_for_output(&output);
        for layer in map.layers() {
            layer.send_frame(&output, now, Some(Duration::ZERO), |_, _| {
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

/// Parse a hex color string like "#7aa2f7" into [f32; 4] RGBA.
/// Returns opaque white on parse failure.
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