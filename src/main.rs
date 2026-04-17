//! Phase 3: The Spawning (xdg-shell + rendering real clients).
//!
//! What's new compared to Phase 2:
//!   * We now own a Wayland `Display` and expose a listening socket so
//!     real clients (alacritty, weston-terminal, …) can connect.
//!   * Three protocol globals are initialised: `wl_compositor` /
//!     `wl_subcompositor` (via `CompositorState`), `wl_shm` (via
//!     `ShmState`), and `xdg_wm_base` (via `XdgShellState`). Together
//!     these are the absolute minimum a toolkit needs to render.
//!   * An `Output` is created + mapped into a `smithay::desktop::Space`,
//!     giving us a 2D plane onto which toplevels are laid out.
//!   * `XdgShellHandler::new_toplevel` wraps each incoming surface in a
//!     `Window` and drops it into the Space at (100, 100).
//!   * `Ctrl + Shift + Enter` spawns an `alacritty` terminal. The child
//!     inherits `WAYLAND_DISPLAY` pointing at *our* socket, not the host.
//!   * The render path runs `space::render_output` on top of the clear
//!     colour, so client surfaces are composited into the framebuffer.

use std::{ffi::OsString, process::Command, sync::Arc, time::Duration};

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Event as _, InputEvent, KeyState, KeyboardKeyEvent,
        },
        renderer::{
            damage::OutputDamageTracker,
            element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer,
            utils::on_commit_buffer_handler,
            Color32F,
        },
        winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend, WinitInput},
    },
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{
        keyboard::{
            FilterResult, KeyboardHandle, KeysymHandle, Keysym, ModifiersState, XkbConfig,
        },
        pointer::{CursorImageStatus, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopSignal, Mode as CalloopMode, PostAction,
        },
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
            Client, Display, DisplayHandle,
        },
        winit::platform::pump_events::PumpStatus,
    },
    utils::{Logical, Point, Rectangle, Serial, Transform, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_parent, is_sync_subsurface, with_states, CompositorClientState,
            CompositorHandler, CompositorState,
        },
        output::{OutputHandler, OutputManagerState},
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};
use tracing::{debug, info, trace, warn};

/// Clear colour drawn *under* all client surfaces. RGBA, linear 0..=1.
const CLEAR_COLOR: [f32; 4] = [0.08, 0.05, 0.14, 1.0];

// -------------------------------------------------------------------------
// Compositor state
// -------------------------------------------------------------------------

/// Actions returned from the keyboard filter.
///
/// Every compositor-level keybinding gets a variant here; the filter
/// returns `FilterResult::Intercept(KeyAction::_)` and the dispatcher
/// above translates that into real side-effects (spawning, quitting,
/// later: focus cycling, workspace switch, …).
#[derive(Debug, Clone, Copy)]
pub enum KeyAction {
    /// `Ctrl + Shift + Escape` — tear everything down.
    Quit,
    /// `Ctrl + Shift + Enter` — spawn alacritty as a new Wayland client.
    SpawnTerminal,
}

/// The compositor's entire mutable world.
pub struct State {
    pub start_time: std::time::Instant,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    /// The name of the Wayland socket we're listening on (e.g.
    /// `wayland-1`). Exported into `WAYLAND_DISPLAY` before children
    /// are spawned so they connect to us rather than the host.
    pub socket_name: OsString,

    // ---- Wayland protocol globals ------------------------------------
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,

    // ---- Seat (clonable handles cached for convenience) -------------
    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub pointer_location: Point<f64, Logical>,

    // ---- Desktop plane ----------------------------------------------
    /// 2D layout plane. Windows map into it at (x, y) positions and are
    /// rendered bottom-to-top in insertion order.
    pub space: Space<Window>,
    /// The single `wl_output` we advertise. Cloneable (Arc-backed).
    pub output: Output,
    /// Tracks per-frame damage so the renderer can do partial repaints
    /// later. For now we just drive it through `render_output`.
    pub damage_tracker: OutputDamageTracker,
    /// Track xdg popups (right-click menus, combo boxes, …). Unused in
    /// practice until we start handling popup grabs.
    pub popups: PopupManager,
}

/// Per-client user data. One instance exists for every Wayland client
/// that connects to our socket. Smithay's compositor delegate threads
/// `CompositorClientState` through to track per-client surface state.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// The userdata threaded through calloop callbacks.
pub struct CalloopData {
    pub state: State,
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub winit: WinitEventLoop,
}

// -------------------------------------------------------------------------
// SeatHandler
// -------------------------------------------------------------------------
//
// With `wayland_frontend` enabled our focus type must implement
// `WaylandFocus`. `WlSurface` does — and is in fact the natural choice:
// *all* routed keyboard / pointer events in Wayland terminate at a
// surface, not a higher-level window object.

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
//
// A raw Wayland surface's lifecycle:
//   1. Client creates a `wl_surface` (CompositorState handles this via
//      delegate_compositor!).
//   2. Client calls `xdg_wm_base.get_xdg_surface` + `.get_toplevel` to
//      promote it — triggers `XdgShellHandler::new_toplevel` below.
//   3. Client attaches buffers with `wl_surface.attach` and commits.
//      Every commit lands here in `CompositorHandler::commit`.
//   4. We call `on_commit_buffer_handler` (imports the buffer into the
//      renderer) and `window.on_commit` (advances the desktop-layer
//      state machine), then the surface is ready to be rendered.

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Let Smithay's renderer layer hook any buffer-import bookkeeping
        // (ShmBuffer → GlesTexture upload, etc.) before we do anything
        // desktop-layer-specific.
        on_commit_buffer_handler::<Self>(surface);

        // Sub-surfaces commit atomically with their root; only act on
        // the outermost surface of a commit tree.
        if !is_sync_subsurface(surface) {
            // Walk up to the toplevel's root WlSurface.
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            // If the root belongs to a window we manage, tick its
            // internal "current state ↔ pending state" machine.
            if let Some(window) = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            }
        }

        // The very first commit on a new xdg_toplevel must be answered
        // with a configure event, or the client will never draw.
        handle_initial_configure(surface, &self.space);
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
// XdgShellHandler
// -------------------------------------------------------------------------
//
// `xdg_shell` is the protocol that turns a dumb `wl_surface` into
// something with window semantics — a title, geometry, interactive
// move/resize, minimise/maximise. `XdgShellState` dispatches the raw
// protocol; we only have to decide what to *do* when each request
// arrives.
//
// The trait hooks that interest us in Phase 3:
//   * `new_toplevel`: a new window appeared. Wrap + map it.
//   * `new_popup` / `grab` / `reposition_request`: popups (menus, combo
//     boxes). Stub for now — terminals don't use them.
//   * `move_request` / `resize_request`: interactive drag / edge-grab.
//     Stubbed until Phase 4 when we wire pointer grabs.

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    /// A new toplevel surface was created by a client.
    ///
    /// At this point the client has a `wl_surface` + `xdg_surface` +
    /// `xdg_toplevel` triple, but no buffer attached yet. Our job is to:
    ///   1. Wrap the protocol handle in a `desktop::Window`, which
    ///      centralises hit-testing, render-element generation, and
    ///      surface-tree traversal.
    ///   2. Map the window into the space at a floating position so it
    ///      participates in layout and rendering.
    ///   3. Hand it keyboard focus so typed input actually reaches it.
    ///
    /// The *initial configure* (telling the client "here's how big you
    /// should be") is sent lazily on the first commit — see
    /// `handle_initial_configure` below. That's the idiomatic smithay
    /// pattern: let the client's first commit drive the handshake.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!("new xdg toplevel");

        let window = Window::new_wayland_window(surface);
        // Map at a fixed floating offset. Tiling math belongs in a
        // later phase; for now every window lands at (100, 100).
        self.space.map_element(window.clone(), (100, 100), true);

        // Give the new window keyboard focus. Without this the terminal
        // starts but ignores typing.
        let wl_surface = window.toplevel().unwrap().wl_surface().clone();
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, Some(wl_surface), serial);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // We don't constrain popups yet; track them so their commit/frame
        // plumbing works if an app pops one (e.g. alacritty's font-size
        // confirm dialog). Layout correctness comes in a later phase.
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

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // Popup grabs land in Phase 4 with pointer grabs.
    }

    fn move_request(&mut self, _surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        // Interactive move lands in Phase 4.
    }

    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
        // Interactive resize lands in Phase 4.
    }
}

delegate_xdg_shell!(State);

/// Send the initial configure exactly once, on the first commit after a
/// toplevel is created. Before this event clients refuse to draw.
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
    // ---- Logging -------------------------------------------------------
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("bootstrapping nested Wayland compositor (Phase 3: xdg-shell)");

    // ---- calloop event loop -------------------------------------------
    let mut event_loop: EventLoop<CalloopData> = EventLoop::try_new()?;
    let loop_signal = event_loop.get_signal();

    // ---- Wayland display ----------------------------------------------
    //
    // `Display<D>` is the core of a Smithay compositor: it owns all
    // client connections, dispatches incoming protocol messages to the
    // right handler (via the `delegate_*!` macros above), and exposes a
    // `DisplayHandle` used to create globals.
    let display: Display<State> = Display::new()?;
    let display_handle = display.handle();

    // ---- winit backend ------------------------------------------------
    let (backend, winit) = winit::init::<GlesRenderer>()
        .map_err(|e| format!("failed to initialize winit backend: {e:?}"))?;

    let window_size = backend.window_size();
    info!(?window_size, "winit backend initialized");

    // ---- Output (advertised to clients as wl_output) -----------------
    //
    // A `wl_output` tells clients "there is a display of size N×M at
    // position (X, Y) in the compositor's global space". We only have
    // the winit window, so: one output, same size as the window, at
    // origin. `Transform::Flipped180` matches the GL → winit Y-flip.
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "WindowManager".into(),
            serial_number: "00000000".into(),
        },
    );
    let _output_global = output.create_global::<State>(&display_handle);
    let output_mode = OutputMode {
        size: window_size,
        refresh: 60_000,
    };
    output.change_current_state(
        Some(output_mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(output_mode);

    let damage_tracker = OutputDamageTracker::from_output(&output);

    // ---- Protocol globals ---------------------------------------------
    //
    // Each `*State::new::<State>(&dh)` registers a Wayland global (a
    // "service" clients can bind to). The `State` type parameter ties
    // dispatch back to our `impl *Handler for State` blocks above.
    let compositor_state = CompositorState::new::<State>(&display_handle);
    let xdg_shell_state = XdgShellState::new::<State>(&display_handle);
    let shm_state = ShmState::new::<State>(&display_handle, vec![]);
    let output_manager_state =
        OutputManagerState::new_with_xdg_output::<State>(&display_handle);

    // ---- Seat ---------------------------------------------------------
    //
    // `new_wl_seat` (vs `new_seat` in Phase 2) also registers the
    // `wl_seat` global — so clients can see our keyboard/pointer and
    // we can route events to them.
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
    //
    // `ListeningSocketSource::new_auto()` finds the next free
    // `wayland-N` name in `$XDG_RUNTIME_DIR`. We export it via
    // `WAYLAND_DISPLAY` so child processes we spawn (alacritty)
    // connect to *us* instead of the host compositor.
    let listening_socket = ListeningSocketSource::new_auto()?;
    let socket_name = listening_socket.socket_name().to_os_string();
    info!(?socket_name, "listening for wayland clients");

    event_loop
        .handle()
        .insert_source(listening_socket, |stream, _meta, data| {
            // A new client just connected; register it with the display
            // so its messages start dispatching through our handlers.
            if let Err(err) = data
                .state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                warn!(?err, "failed to accept new wayland client");
            }
        })?;

    // Also insert the Display itself so calloop drives protocol
    // dispatch whenever the Wayland FD becomes readable.
    event_loop.handle().insert_source(
        Generic::new(display, Interest::READ, CalloopMode::Level),
        |_, display, data| {
            // SAFETY: we never drop the Display here (Generic keeps it
            // alive), and dispatch_clients synchronously calls into
            // handlers on `data.state` and returns before we exit.
            unsafe { display.get_mut().dispatch_clients(&mut data.state) }
                .map_err(std::io::Error::other)?;
            Ok(PostAction::Continue)
        },
    )?;

    // Spawned children need to see our socket, not the host's.
    // SAFETY: set_var is !Send/!Sync in edition 2024; we're single-threaded
    // at this point.
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    // ---- Assemble state ----------------------------------------------
    let state = State {
        start_time: std::time::Instant::now(),
        display_handle,
        loop_signal,
        socket_name,
        compositor_state,
        xdg_shell_state,
        shm_state,
        output_manager_state,
        seat_state,
        seat,
        keyboard,
        pointer,
        pointer_location: Point::from((0.0, 0.0)),
        space,
        output,
        damage_tracker,
        popups: PopupManager::default(),
    };

    let mut data = CalloopData {
        state,
        backend,
        winit,
    };

    // ---- Frame pump ---------------------------------------------------
    //
    // One timer tick per ~16 ms: drain winit, render, flush. Not yet
    // driven by VBlank — that's a udev-era concern.
    event_loop
        .handle()
        .insert_source(Timer::immediate(), |_instant, _meta, data| {
            dispatch_winit(data);
            redraw(data);
            TimeoutAction::ToDuration(Duration::from_millis(16))
        })?;

    info!("entering calloop event loop");
    event_loop.run(None, &mut data, |_data| {})?;

    info!("event loop exited, shutting down");
    Ok(())
}

// -------------------------------------------------------------------------
// winit → compositor event routing
// -------------------------------------------------------------------------

fn dispatch_winit(data: &mut CalloopData) {
    let status = data.winit.dispatch_new_events(|event| match event {
        WinitEvent::CloseRequested => {
            info!("host window close requested; stopping event loop");
            data.state.loop_signal.stop();
        }
        WinitEvent::Resized { size, scale_factor } => {
            info!(?size, scale_factor, "nested window resized");
            // Keep the advertised wl_output in sync so clients re-layout.
            let mode = OutputMode {
                size,
                refresh: 60_000,
            };
            data.state.output.change_current_state(
                Some(mode),
                None,
                None,
                None,
            );
        }
        WinitEvent::Input(input_event) => {
            handle_input_event(&mut data.state, &data.backend, input_event);
        }
        WinitEvent::Redraw => { /* we redraw unconditionally each tick */ }
        WinitEvent::Focus(_) => {}
    });

    if let PumpStatus::Exit(code) = status {
        info!(code, "winit pump exited; stopping event loop");
        data.state.loop_signal.stop();
    }
}

fn handle_input_event(
    state: &mut State,
    backend: &WinitGraphicsBackend<GlesRenderer>,
    event: InputEvent<WinitInput>,
) {
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

            match action {
                Some(KeyAction::Quit) => {
                    info!("kill switch triggered (Ctrl+Shift+Escape) — stopping");
                    state.loop_signal.stop();
                }
                Some(KeyAction::SpawnTerminal) => {
                    spawn_terminal();
                }
                None => {}
            }
        }

        InputEvent::PointerMotionAbsolute { event } => {
            let size = backend.window_size();
            let x = event.x_transformed(size.w);
            let y = event.y_transformed(size.h);
            state.pointer_location = Point::from((x, y));
            trace!(x, y, "pointer moved");
        }

        _ => {}
    }
}

fn handle_keybinding(
    _state: &mut State,
    mods: &ModifiersState,
    keysym_handle: KeysymHandle<'_>,
    key_state: KeyState,
) -> FilterResult<KeyAction> {
    if key_state != KeyState::Pressed {
        return FilterResult::Forward;
    }

    let sym = keysym_handle.modified_sym();
    debug!(?mods, sym = ?sym, "key pressed");

    // Ctrl + Shift + Escape: quit.
    if mods.ctrl && mods.shift && !mods.logo && !mods.alt && sym == Keysym::Escape {
        return FilterResult::Intercept(KeyAction::Quit);
    }

    // Ctrl + Shift + Enter: spawn a terminal.
    if mods.ctrl && mods.shift && !mods.logo && !mods.alt && sym == Keysym::Return {
        return FilterResult::Intercept(KeyAction::SpawnTerminal);
    }

    FilterResult::Forward
}

/// Fork-and-exec alacritty. Non-blocking by design — we never `wait()`
/// on the child, calloop keeps ticking, and if alacritty dies the OS
/// reaps the zombie once our process does anything syscall-ish.
fn spawn_terminal() {
    info!("spawning alacritty");
    match Command::new("alacritty").spawn() {
        Ok(child) => debug!(pid = child.id(), "alacritty spawned"),
        Err(err) => warn!(?err, "failed to spawn alacritty"),
    }
}

// -------------------------------------------------------------------------
// Rendering
// -------------------------------------------------------------------------

/// One frame: render the space on top of the clear colour, then flush
/// client-facing bookkeeping (frame callbacks, space refresh, socket
/// writes).
fn redraw(data: &mut CalloopData) {
    let CalloopData { state, backend, .. } = data;

    let size = backend.window_size();
    let damage: Rectangle<i32, smithay::utils::Physical> =
        Rectangle::new((0, 0).into(), size);

    {
        let (renderer, mut framebuffer) = match backend.bind() {
            Ok(pair) => pair,
            Err(err) => {
                warn!(?err, "failed to bind winit backend surface");
                return;
            }
        };

        // `render_output` walks every space, collects their render
        // elements (WlSurface → GPU texture), stacks them above the
        // clear colour, and hands the list to the damage tracker
        // which issues the actual draw commands.
        let render_result = smithay::desktop::space::render_output::<
            _,
            WaylandSurfaceRenderElement<GlesRenderer>,
            _,
            _,
        >(
            &state.output,
            renderer,
            &mut framebuffer,
            1.0,
            0,
            [&state.space],
            &[],
            &mut state.damage_tracker,
            Color32F::from(CLEAR_COLOR),
        );

        if let Err(err) = render_result {
            warn!(?err, "render_output failed");
            return;
        }
    }

    if let Err(err) = backend.submit(Some(&[damage])) {
        warn!(?err, "buffer submission failed");
        return;
    }

    // Tell every mapped window "a frame has been presented" so clients
    // driven by wl_surface.frame callbacks will submit their next
    // buffer. Without this, GTK/Qt apps stutter or freeze.
    let now = state.start_time.elapsed();
    state.space.elements().for_each(|window| {
        window.send_frame(&state.output, now, Some(Duration::ZERO), |_, _| {
            Some(state.output.clone())
        });
    });

    // Space bookkeeping: drop entries whose surface is dead, recompute
    // output overlap, etc.
    state.space.refresh();
    state.popups.cleanup();

    // Drain pending outgoing messages to every client.
    if let Err(err) = state.display_handle.flush_clients() {
        warn!(?err, "failed to flush wayland clients");
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

/// Lookup the surface under a logical point — handy once we wire
/// pointer focus in Phase 4. Unused today but cheap to keep around.
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
