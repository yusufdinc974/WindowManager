//! Keyboard / pointer routing and IPC command dispatch.

use std::process::Command;

use serde::Deserialize;



use smithay::{
    backend::{
        input::{
            ButtonState, Event as _, InputEvent, KeyState, KeyboardKeyEvent,
            PointerButtonEvent, PointerMotionEvent,
        },
        libinput::LibinputInputBackend,
    },
    desktop::{layer_map_for_output, WindowSurfaceType},
    input::{
        keyboard::{FilterResult, Keysym, KeysymHandle, ModifiersState},
        pointer::{ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer as WlrLayer,
};
use tracing::{debug, info, trace, warn};

use crate::state::State;

// -------------------------------------------------------------------------
// Action / IPC command types
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
    SwitchWorkspace(usize),
    MoveToWorkspace(usize),
    CycleTheme,
    ToggleNavbar,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum IpcCommand {
    Quit,
    SpawnTerminal,
    CloseFocused,
}

// -------------------------------------------------------------------------
// Keyboard filter
// -------------------------------------------------------------------------

fn keysym_to_workspace_index(sym: Keysym) -> Option<usize> {
    match sym {
        Keysym::_1 => Some(0),
        Keysym::_2 => Some(1),
        Keysym::_3 => Some(2),
        Keysym::_4 => Some(3),
        Keysym::_5 => Some(4),
        Keysym::_6 => Some(5),
        Keysym::_7 => Some(6),
        Keysym::_8 => Some(7),
        Keysym::_9 => Some(8),
        _ => None,
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

    if !mods.logo || mods.ctrl || mods.alt {
        return FilterResult::Forward;
    }

    let sym = keysym_handle.modified_sym();

    let action = if mods.shift {
        debug!(?mods, sym = ?sym, "chord pressed (Super+Shift)");
        match sym {
            Keysym::Escape                          => KeyAction::Quit,
            s if s == Keysym::r || s == Keysym::R   => KeyAction::ReloadConfig,
            Keysym::Left                             => KeyAction::MoveWindowLeft,
            Keysym::Right                            => KeyAction::MoveWindowRight,
            _ => {
                let raw_sym = keysym_handle.raw_syms().first().copied()
                    .unwrap_or(sym);
                if let Some(idx) = keysym_to_workspace_index(raw_sym) {
                    KeyAction::MoveToWorkspace(idx)
                } else {
                    return FilterResult::Forward;
                }
            }
        }
    } else {
        debug!(?mods, sym = ?sym, "chord pressed (Super)");
        match sym {
            Keysym::Return                           => KeyAction::SpawnTerminal,
            s if s == Keysym::d || s == Keysym::D    => KeyAction::SpawnLauncher,
            s if s == Keysym::q || s == Keysym::Q    => KeyAction::CloseFocused,
            s if s == Keysym::f || s == Keysym::F    => KeyAction::ToggleFullscreen,
            s if s == Keysym::t || s == Keysym::T    => KeyAction::CycleTheme,
            s if s == Keysym::b || s == Keysym::B    => KeyAction::ToggleNavbar,
            Keysym::space                            => KeyAction::ToggleFloating,
            Keysym::Left                             => KeyAction::FocusLeft,
            Keysym::Right                            => KeyAction::FocusRight,
            _ => {
                if let Some(idx) = keysym_to_workspace_index(sym) {
                    KeyAction::SwitchWorkspace(idx)
                } else {
                    return FilterResult::Forward;
                }
            }
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
        KeyAction::SpawnTerminal => spawn_terminal(&state.config.terminal),
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
        KeyAction::SwitchWorkspace(idx) => {
            state.switch_workspace(idx);
        }
        KeyAction::MoveToWorkspace(idx) => {
            state.move_to_workspace(idx);
        }
        KeyAction::CycleTheme => {
            state.cycle_theme();
        }
        KeyAction::ToggleNavbar => {
            state.toggle_navbar();
        }
    }
}

// -------------------------------------------------------------------------
// libinput event fan-out
// -------------------------------------------------------------------------

pub fn handle_libinput_event(state: &mut State, event: InputEvent<LibinputInputBackend>) {
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
            if let Some(geo) = state.workspaces[state.active_workspace]
                .space
                .output_geometry(&state.output)
            {
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

            let pos = state.pointer_location;
            let under = surface_under_pointer(state, pos);

            if let Some((surface, _)) = under.as_ref() {
                let ws = &state.workspaces[state.active_workspace];
                let is_window = ws.windows.iter().any(|w| {
                    w.toplevel().map(|t| t.wl_surface()) == Some(surface)
                });
                if is_window {
                    let keyboard = state.keyboard.clone();
                    if keyboard.current_focus().as_ref() != Some(surface) {
                        let serial = SERIAL_COUNTER.next_serial();
                        keyboard.set_focus(state, Some(surface.clone()), serial);
                    }
                }
            }

            let pointer = state.pointer.clone();
            let serial = SERIAL_COUNTER.next_serial();
            let time = event.time_msec();
            pointer.motion(
                state,
                under,
                &MotionEvent {
                    location: pos,
                    serial,
                    time,
                },
            );
            pointer.frame(state);

            state.needs_redraw = true;
        }

        InputEvent::PointerButton { event } => {
            let button = event.button_code();
            let button_state = event.state();
            let serial = SERIAL_COUNTER.next_serial();
            let time = event.time_msec();

            if button_state == ButtonState::Pressed {
                let pos = state.pointer_location;
                let ws = &state.workspaces[state.active_workspace];
                let hit = ws.space.element_under(pos).map(|(w, _)| w.clone());
                if let Some(window) = hit {
                    state.focus_window(&window);
                }
            }

            let pointer = state.pointer.clone();
            pointer.button(
                state,
                &ButtonEvent {
                    button,
                    state: button_state,
                    serial,
                    time,
                },
            );
            pointer.frame(state);
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

// -------------------------------------------------------------------------
// IPC / process spawning
// -------------------------------------------------------------------------

pub fn handle_ipc_command(state: &mut State, cmd: IpcCommand) {
    match cmd {
        IpcCommand::Quit => {
            info!("IPC: quit");
            state.loop_signal.stop();
        }
        IpcCommand::SpawnTerminal => {
            info!("IPC: spawn terminal");
            spawn_terminal(&state.config.terminal);
        }
        IpcCommand::CloseFocused => {
            info!("IPC: close focused");
            state.close_focused();
        }
    }
}

// -------------------------------------------------------------------------
// Pointer focus lookup
// -------------------------------------------------------------------------

fn surface_under_pointer(
    state: &State,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    {
        let map = layer_map_for_output(&state.output);
        if let Some(layer) = map
            .layer_under(WlrLayer::Overlay, pos)
            .or_else(|| map.layer_under(WlrLayer::Top, pos))
        {
            let layer_loc = map.layer_geometry(layer).map(|g| g.loc).unwrap_or_default();
            let local = pos - layer_loc.to_f64();
            if let Some((surface, surface_loc)) =
                layer.surface_under(local, WindowSurfaceType::ALL)
            {
                return Some((surface, (surface_loc + layer_loc).to_f64()));
            }
        }
    }

    let ws = &state.workspaces[state.active_workspace];
    if let Some((window, win_loc)) = ws.space.element_under(pos) {
        let local = pos - win_loc.to_f64();
        if let Some((surface, surface_loc)) =
            window.surface_under(local, WindowSurfaceType::ALL)
        {
            return Some((surface, (surface_loc + win_loc).to_f64()));
        }
    }

    {
        let map = layer_map_for_output(&state.output);
        if let Some(layer) = map
            .layer_under(WlrLayer::Bottom, pos)
            .or_else(|| map.layer_under(WlrLayer::Background, pos))
        {
            let layer_loc = map.layer_geometry(layer).map(|g| g.loc).unwrap_or_default();
            let local = pos - layer_loc.to_f64();
            if let Some((surface, surface_loc)) =
                layer.surface_under(local, WindowSurfaceType::ALL)
            {
                return Some((surface, (surface_loc + layer_loc).to_f64()));
            }
        }
    }

    None
}

fn spawn_terminal(command: &str) {
    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        warn!("config: terminal_command is empty, nothing to spawn");
        return;
    };
    let args: Vec<&str> = parts.collect();
    info!(program, ?args, "spawning terminal");

    // Capture stderr via pipe so we can log why the client crashes
    match Command::new(program)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            debug!(pid, program, "terminal spawned");

            // Spawn a thread to wait for the child and log its exit + stderr
            let program_owned = program.to_string();
            std::thread::spawn(move || {
                match child.wait_with_output() {
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if output.status.success() {
                            debug!(
                                pid,
                                program = %program_owned,
                                "terminal exited successfully"
                            );
                        } else {
                            warn!(
                                pid,
                                program = %program_owned,
                                exit_code = ?output.status.code(),
                                stderr = %stderr.trim(),
                                stdout = %stdout.trim(),
                                "terminal exited with error"
                            );
                        }
                    }
                    Err(err) => {
                        warn!(
                            pid,
                            program = %program_owned,
                            ?err,
                            "failed to wait for terminal process"
                        );
                    }
                }
            });
        }
        Err(err) => warn!(?err, program, "failed to spawn terminal"),
    }
}