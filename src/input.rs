//! Keyboard / pointer routing and IPC command dispatch.
//!
//! Extracted from `main.rs` during Phase 10 (The Grand Refactoring).
//! `KeyAction` and `IpcCommand` live here (moved from `state.rs` — they're
//! command types consumed exclusively by this module's dispatchers).

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
    input::keyboard::{FilterResult, Keysym, KeysymHandle, ModifiersState},
    utils::SERIAL_COUNTER,
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

/// Map a keysym _1 .. _9 to a 0-based workspace index.
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
            _ => {
                // Super+Shift+[1-9] → MoveToWorkspace
                // When shift is held, xkb gives us the shifted symbol
                // (e.g. '!' for Shift+1 on US layout). We need the
                // *raw* (unshifted) keysym to detect number keys.
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
            _ => {
                // Super+[1-9] → SwitchWorkspace
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
        KeyAction::SwitchWorkspace(idx) => {
            state.switch_workspace(idx);
        }
        KeyAction::MoveToWorkspace(idx) => {
            state.move_to_workspace(idx);
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
        }

        InputEvent::PointerButton { event } => {
            if event.state() != ButtonState::Pressed {
                return;
            }
            let ws = &state.workspaces[state.active_workspace];
            let hit = ws
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
