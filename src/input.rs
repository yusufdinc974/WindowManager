//! Keyboard / pointer routing, IPC command dispatch, and interactive grabs.

use std::process::Command;
use std::time::{Duration, Instant};

use smithay::backend::session::Session;
use serde::Deserialize;

use smithay::{
    backend::{
        input::{
            ButtonState, Event as _, GestureBeginEvent, GestureEndEvent,
            GestureSwipeUpdateEvent, InputEvent, KeyState, KeyboardKeyEvent,
            PointerButtonEvent, PointerMotionEvent,
        },
        libinput::LibinputInputBackend,
    },
    desktop::{layer_map_for_output, WindowSurfaceType},
    wayland::compositor::get_parent,
    input::{
        keyboard::{FilterResult, Keysym, KeysymHandle, ModifiersState},
        pointer::{ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer as WlrLayer,
};

use tracing::{debug, info, trace, warn};

use crate::state::{window_current_size, GrabMode, GrabState, State};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Minimum accumulated horizontal pixels to trigger a workspace switch.
const SWIPE_THRESHOLD: f64 = 100.0;

// -------------------------------------------------------------------------
// Action / IPC command types
// -------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum KeyAction {
    Quit,
    ReloadConfig,
    SpawnTerminal,
    SpawnLauncher,
    CloseFocused,
    ToggleFullscreen,
    CycleLayout,
    ToggleFloating,
    FocusLeft,
    FocusRight,
    MoveWindowLeft,
    MoveWindowRight,
    SwitchWorkspace(usize),
    MoveToWorkspace(usize),
    CycleTheme,
    ToggleNavbar,
    ToggleWallpaperMenu,
    /// VT/TTY switch (Phase 35)
    SwitchVt(i32),
    Exec(String),
    VolumeUp,
    VolumeDown,
    VolumeMute,
    BrightnessUp,
    BrightnessDown,
    /// Intercepted but already handled inline by the keyboard filter.
    NoOp,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum IpcCommand {
    Quit,
    SpawnTerminal,
    CloseFocused,
    /// Set absolute opacity (0.1 – 1.0)
    SetOpacity { value: f32 },
    /// Adjust opacity by delta (e.g. +0.05 or -0.05)
    AdjustOpacity { value: f32 },
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

fn keysym_to_key_name(sym: Keysym) -> Option<String> {
    match sym {
        Keysym::Return => Some("return".into()),
        Keysym::Escape => Some("escape".into()),
        Keysym::space  => Some("space".into()),
        Keysym::Left   => Some("left".into()),
        Keysym::Right  => Some("right".into()),
        Keysym::Up     => Some("up".into()),
        Keysym::Down   => Some("down".into()),
        Keysym::_1     => Some("1".into()),
        Keysym::_2     => Some("2".into()),
        Keysym::_3     => Some("3".into()),
        Keysym::_4     => Some("4".into()),
        Keysym::_5     => Some("5".into()),
        Keysym::_6     => Some("6".into()),
        Keysym::_7     => Some("7".into()),
        Keysym::_8     => Some("8".into()),
        Keysym::_9     => Some("9".into()),

        // ── F-keys (for laptops where Fn-lock sends F1–F12 by default) ──
        Keysym::F1     => Some("f1".into()),
        Keysym::F2     => Some("f2".into()),
        Keysym::F3     => Some("f3".into()),
        Keysym::F4     => Some("f4".into()),
        Keysym::F5     => Some("f5".into()),
        Keysym::F6     => Some("f6".into()),
        Keysym::F7     => Some("f7".into()),
        Keysym::F8     => Some("f8".into()),
        Keysym::F9     => Some("f9".into()),
        Keysym::F10    => Some("f10".into()),
        Keysym::F11    => Some("f11".into()),
        Keysym::F12    => Some("f12".into()),


        Keysym::XF86_AudioRaiseVolume  => Some("xf86audioraisevolume".into()),
        Keysym::XF86_AudioLowerVolume  => Some("xf86audiolowervolume".into()),
        Keysym::XF86_AudioMute         => Some("xf86audiomute".into()),
        Keysym::XF86_AudioMicMute      => Some("xf86audiomicmute".into()),
        Keysym::XF86_AudioPlay         => Some("xf86audioplay".into()),
        Keysym::XF86_AudioPause        => Some("xf86audiopause".into()),
        Keysym::XF86_AudioNext         => Some("xf86audionext".into()),
        Keysym::XF86_AudioPrev         => Some("xf86audioprev".into()),
        Keysym::XF86_MonBrightnessUp   => Some("xf86monbrightnessup".into()),
        Keysym::XF86_MonBrightnessDown => Some("xf86monbrightnessdown".into()),

        _ => {
            // For letter keys, get the lowercase character name
            let raw = sym.key_char();
            raw.map(|c| c.to_lowercase().to_string())
        }
    }
}

fn action_string_to_key_action(action: &str) -> Option<KeyAction> {
    match action {
        "quit" => Some(KeyAction::Quit),
        "reload_config" => Some(KeyAction::ReloadConfig),
        "spawn_terminal" => Some(KeyAction::SpawnTerminal),
        "spawn_launcher" => Some(KeyAction::SpawnLauncher),
        "close_focused" => Some(KeyAction::CloseFocused),
        "toggle_fullscreen" => Some(KeyAction::ToggleFullscreen),
        "cycle_layout" => Some(KeyAction::CycleLayout),
        "toggle_floating" => Some(KeyAction::ToggleFloating),
        "focus_left" => Some(KeyAction::FocusLeft),
        "focus_right" => Some(KeyAction::FocusRight),
        "move_window_left" => Some(KeyAction::MoveWindowLeft),
        "move_window_right" => Some(KeyAction::MoveWindowRight),
        "cycle_theme" => Some(KeyAction::CycleTheme),
        "toggle_navbar" => Some(KeyAction::ToggleNavbar),
        "toggle_wallpaper_menu" => Some(KeyAction::ToggleWallpaperMenu),
        s if s.starts_with("workspace_") => {
            s.strip_prefix("workspace_")
                .and_then(|n| n.parse::<usize>().ok())
                .map(|n| KeyAction::SwitchWorkspace(n.saturating_sub(1)))
        }
        s if s.starts_with("move_to_workspace_") => {
            s.strip_prefix("move_to_workspace_")
                .and_then(|n| n.parse::<usize>().ok())
                .map(|n| KeyAction::MoveToWorkspace(n.saturating_sub(1)))
        }
        "volume_up" => Some(KeyAction::VolumeUp),
        "volume_down" => Some(KeyAction::VolumeDown),
        "volume_mute" => Some(KeyAction::VolumeMute),
        "brightness_up" => Some(KeyAction::BrightnessUp),
        "brightness_down" => Some(KeyAction::BrightnessDown),
        // ── Phase 34: exec:<command> runs an arbitrary shell command ──
        s if s.starts_with("exec:") => {
            let cmd = s.strip_prefix("exec:").unwrap_or("").trim();
            if cmd.is_empty() {
                warn!("exec: keybind with empty command");
                None
            } else {
                Some(KeyAction::Exec(cmd.to_string()))
            }
        }
        _ => {
            warn!(action, "unknown keybind action");
            None
        }
    }
}

fn handle_keybinding(
    state: &mut State,
    mods: &ModifiersState,
    keysym_handle: KeysymHandle<'_>,
    key_state: KeyState,
) -> FilterResult<KeyAction> {
    if key_state != KeyState::Pressed {
        return FilterResult::Forward;
    }

    let sym = keysym_handle.modified_sym();

     // ── Phase 34 DEBUG: log every keypress ──
    let key_name = keysym_to_key_name(sym);
    let raw_key_name = keysym_handle
        .raw_syms()
        .first()
        .and_then(|s| keysym_to_key_name(*s));
    info!(
        ?sym,
        ?key_name,
        ?raw_key_name,
        logo = mods.logo,
        shift = mods.shift,
        ctrl = mods.ctrl,
        alt = mods.alt,
        "DEBUG keybind: key pressed"
    );

    let on_layer = state.layer_has_keyboard_focus();

    // Escape on layer surface → dismiss
        if sym == Keysym::Escape && !mods.logo && !mods.ctrl && !mods.alt && !mods.shift {
        if on_layer {
            if let Some(focused) = state.keyboard.current_focus() {
                if let Some(layer) = state.layer_surface_of(&focused) {
                    // Only dismiss overlay/popup layer surfaces, NOT persistent bars
                    let dominated_layer = matches!(
                        layer.layer(),
                        smithay::wayland::shell::wlr_layer::Layer::Overlay
                    );
                    let has_exclusive_kb = matches!(
                        layer.cached_state().keyboard_interactivity,
                        smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive
                    );
                    if dominated_layer || has_exclusive_kb {
                        info!("Escape pressed on popup layer surface — closing");
                        layer.layer_surface().send_close();
                        state.drop_focus_to_active_window();
                        state.needs_redraw = true;
                        return FilterResult::Intercept(KeyAction::NoOp);
                    } else {
                        // Persistent layer (like Waybar) — just release focus back
                        info!("Escape on persistent layer surface — releasing focus only");
                        state.drop_focus_to_active_window();
                        state.needs_redraw = true;
                        return FilterResult::Intercept(KeyAction::NoOp);
                    }
                }
            }
        }
        return FilterResult::Forward;
    }

    if mods.ctrl && mods.alt && !mods.logo {
        let vt = match sym {
            Keysym::XF86_Switch_VT_1  | Keysym::F1  => Some(1),
            Keysym::XF86_Switch_VT_2  | Keysym::F2  => Some(2),
            Keysym::XF86_Switch_VT_3  | Keysym::F3  => Some(3),
            Keysym::XF86_Switch_VT_4  | Keysym::F4  => Some(4),
            Keysym::XF86_Switch_VT_5  | Keysym::F5  => Some(5),
            Keysym::XF86_Switch_VT_6  | Keysym::F6  => Some(6),
            Keysym::XF86_Switch_VT_7  | Keysym::F7  => Some(7),
            Keysym::XF86_Switch_VT_8  | Keysym::F8  => Some(8),
            Keysym::XF86_Switch_VT_9  | Keysym::F9  => Some(9),
            Keysym::XF86_Switch_VT_10 | Keysym::F10 => Some(10),
            Keysym::XF86_Switch_VT_11 | Keysym::F11 => Some(11),
            Keysym::XF86_Switch_VT_12 | Keysym::F12 => Some(12),
            _ => None,
        };
        if let Some(vt_num) = vt {
            return FilterResult::Intercept(KeyAction::SwitchVt(vt_num));
        }
    }

    // Build the lookup key from current modifier state + key name.
    // Try modified sym first, then raw sym for shifted keys (e.g., Shift+1).
    let key_name = keysym_to_key_name(sym);
    let raw_key_name = keysym_handle
        .raw_syms()
        .first()
        .and_then(|s| keysym_to_key_name(*s));

    let lookup_key = (mods.logo, mods.shift, mods.ctrl, mods.alt);

    let keybind_map = state.config.keybind_map();

    // Try modified sym name first
    let action_str = key_name
        .as_ref()
        .and_then(|name| {
            keybind_map.get(&(lookup_key.0, lookup_key.1, lookup_key.2, lookup_key.3, name.clone()))
        })
        .or_else(|| {
            // Fallback to raw sym (handles Shift+1 where modified sym is '!')
            raw_key_name.as_ref().and_then(|name| {
                keybind_map.get(&(lookup_key.0, lookup_key.1, lookup_key.2, lookup_key.3, name.clone()))
            })
        });

    if let Some(action_str) = action_str {
        debug!(
            action = %action_str,
            key = ?key_name,
            "keybind matched"
        );
        if let Some(action) = action_string_to_key_action(action_str) {
            return FilterResult::Intercept(action);
        }
    }

    // No keybind matched — forward to client if no Super,
    // or swallow if Super is held (prevents random chars in terminals).
    if !mods.logo {
        FilterResult::Forward
    } else {
        FilterResult::Forward // Forward even with Super so clients can use it
    }
}

fn dispatch_action(state: &mut State, action: Option<KeyAction>) {
    let Some(action) = action else { return };

    match action {
        KeyAction::Quit => {
            info!("kill switch triggered — stopping");
            state.loop_signal.stop();
        }
        KeyAction::SpawnTerminal => spawn_terminal(&state.config.terminal),
        KeyAction::CloseFocused => state.close_focused(),
        KeyAction::FocusRight => state.focus_relative(1),
        KeyAction::FocusLeft => state.focus_relative(-1),
        KeyAction::ReloadConfig => {
            info!("hot-reloading configuration");
            state.reload_config();
        }
        KeyAction::SpawnLauncher => spawn_launcher(&state.config.launcher),
        KeyAction::ToggleFullscreen => {
            info!("Action ToggleFullscreen not yet implemented");
        }
        KeyAction::CycleLayout => {
            state.cycle_layout();
        }
        KeyAction::ToggleFloating => {
            state.toggle_floating();
        }
        KeyAction::MoveWindowLeft => {
            info!("Action MoveWindowLeft not yet implemented");
        }
        KeyAction::MoveWindowRight => {
            info!("Action MoveWindowRight not yet implemented");
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
        KeyAction::ToggleWallpaperMenu => {      
            state.toggle_wallpaper_menu();
        }
        KeyAction::SwitchVt(vt) => {
            info!(vt, "switching to VT");
            if let Err(err) = state.session.change_vt(vt) {
                warn!(?err, vt, "failed to switch VT");
            }
        }

                // ── Phase 34: Run arbitrary shell command ──
        KeyAction::Exec(cmd) => {
            info!(command = %cmd, "exec: spawning command");
            match Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(child) => {
                    debug!(pid = child.id(), command = %cmd, "exec: spawned");
                    let cmd_owned = cmd.clone();
                    std::thread::spawn(move || {
                        match child.wait_with_output() {
                            Ok(output) if !output.status.success() => {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                warn!(
                                    command = %cmd_owned,
                                    exit_code = ?output.status.code(),
                                    stderr = %stderr.trim(),
                                    "exec: command failed"
                                );
                            }
                            Ok(_) => {}
                            Err(err) => warn!(?err, command = %cmd_owned, "exec: wait failed"),
                        }
                    });
                }
                Err(err) => warn!(?err, command = %cmd, "exec: failed to spawn"),
            }
        }

        KeyAction::VolumeUp => {
            let _ = Command::new("wpctl")
                .args(["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let (vol, muted) = query_volume();
            state.osd.show(crate::state::OsdKind::Volume, vol, muted);
            state.needs_redraw = true;
        }
        KeyAction::VolumeDown => {
            let _ = Command::new("wpctl")
                .args(["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let (vol, muted) = query_volume();
            state.osd.show(crate::state::OsdKind::Volume, vol, muted);
            state.needs_redraw = true;
        }
        KeyAction::VolumeMute => {
            let _ = Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let (vol, muted) = query_volume();
            state.osd.show(crate::state::OsdKind::Volume, vol, muted);
            state.needs_redraw = true;
        }
        KeyAction::BrightnessUp => {
            let _ = Command::new("brightnessctl")
                .args(["set", "+5%"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let bri = query_brightness();
            state.osd.show(crate::state::OsdKind::Brightness, bri, false);
            state.needs_redraw = true;
        }
        KeyAction::BrightnessDown => {
            let _ = Command::new("brightnessctl")
                .args(["set", "5%-"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let bri = query_brightness();
            state.osd.show(crate::state::OsdKind::Brightness, bri, false);
            state.needs_redraw = true;
        }
        KeyAction::NoOp => {}
        }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn usable_screen_dimensions(state: &State) -> (i32, i32) {
    let ws = &state.workspaces[state.active_workspace];
    let geo = ws
        .space
        .output_geometry(&state.output)
        .unwrap_or_default();
    let non_exclusive = layer_map_for_output(&state.output).non_exclusive_zone();
    let outer = state.config.outer_gaps;
    let w = (non_exclusive.size.w.min(geo.size.w) - 2 * outer).max(1);
    let h = (non_exclusive.size.h.min(geo.size.h) - 2 * outer).max(1);
    (w, h)
}

/// Find the window under the pointer, with a fallback that checks
/// bounding boxes including border areas.
fn hit_test_window(state: &State, pos: Point<f64, Logical>) -> Option<smithay::desktop::Window> {
    let ws = &state.workspaces[state.active_workspace];

    if let Some((w, _)) = ws.space.element_under(pos) {
        return Some(w.clone());
    }

    let bw = state.config.border_width.max(0);
    ws.windows.iter().find(|w| {
        if let Some(loc) = ws.space.element_location(w) {
            let size = window_current_size(w)
                .unwrap_or_else(|| Size::from((0, 0)));
            let rx = loc.x - bw;
            let ry = loc.y - bw;
            let rw = size.w + 2 * bw;
            let rh = size.h + 2 * bw;
            pos.x >= rx as f64
                && pos.x <= (rx + rw) as f64
                && pos.y >= ry as f64
                && pos.y <= (ry + rh) as f64
        } else {
            false
        }
    }).cloned()
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

            // ── Interactive grab handling ──
            if let Some(grab) = state.pointer_grab.clone() {
                let dx = pos.x - grab.start_pointer.x;
                let dy = pos.y - grab.start_pointer.y;

                match grab.mode {
                    GrabMode::FloatingMove => {
                        let new_x = grab.start_geo.loc.x + dx as i32;
                        let new_y = grab.start_geo.loc.y + dy as i32;

                        let ws = &mut state.workspaces[state.active_workspace];
                        if let Some(geo) = ws.floating_geo.get_mut(&grab.window) {
                            geo.loc = Point::from((new_x, new_y));
                        }
                        let bw = state.config.border_width.max(0);
                        ws.space.map_element(
                            grab.window.clone(),
                            Point::from((new_x + bw, new_y + bw)),
                            false,
                        );
                        ws.space.raise_element(&grab.window, false);
                        state.needs_redraw = true;
                    }

                    GrabMode::FloatingResize => {
                        let new_w = (grab.start_geo.size.w + dx as i32).max(64);
                        let new_h = (grab.start_geo.size.h + dy as i32).max(64);

                        let ws = &mut state.workspaces[state.active_workspace];
                        if let Some(geo) = ws.floating_geo.get_mut(&grab.window) {
                            geo.size = Size::from((new_w, new_h));
                        }

                        let bw = state.config.border_width.max(0);
                        let inner_w = (new_w - 2 * bw).max(1);
                        let inner_h = (new_h - 2 * bw).max(1);
                        let final_size = (inner_w, inner_h);

                        if let Some(toplevel) = grab.window.toplevel() {
                            toplevel.with_pending_state(|s| {
                                s.size = Some(final_size.into());
                            });
                            let last = ws.configured_sizes.get(&grab.window).copied();
                            if last != Some(final_size) {
                                toplevel.send_configure();
                                ws.configured_sizes.insert(grab.window.clone(), final_size);
                            }
                        }
                        ws.space.raise_element(&grab.window, false);
                        state.needs_redraw = true;
                    }

                    GrabMode::TiledMove => {
                        state.needs_redraw = true;
                    }

                    GrabMode::TiledResize => {
                        let ws = &mut state.workspaces[state.active_workspace];

                        if grab.screen_width > 0 && grab.tiled_count > 1 {
                            let ratio_dx = dx as f32 / grab.screen_width as f32;
                            let new_ratio =
                                (grab.start_split_ratio + ratio_dx).clamp(0.1, 0.9);
                            ws.split_ratio = new_ratio;
                        }

                        if grab.tiled_count > 2 && grab.tiled_index > 0 {
                            let stack_idx = grab.tiled_index - 1;
                            let stack_count = grab.tiled_count - 1;

                            if stack_idx < stack_count && grab.screen_height > 0 {
                                let ratio_dy = dy as f32 / grab.screen_height as f32;

                                let mut ratios = grab.start_stack_ratios.clone();

                                if ratios.len() != stack_count {
                                    let eq = 1.0 / stack_count as f32;
                                    ratios = vec![eq; stack_count];
                                }

                                if stack_idx + 1 < stack_count {
                                    ratios[stack_idx] =
                                        (grab.start_stack_ratios[stack_idx] + ratio_dy)
                                            .clamp(0.05, 0.95);
                                    ratios[stack_idx + 1] =
                                        (grab.start_stack_ratios[stack_idx + 1] - ratio_dy)
                                            .clamp(0.05, 0.95);
                                } else {
                                    if stack_idx > 0 {
                                        ratios[stack_idx] =
                                            (grab.start_stack_ratios[stack_idx] + ratio_dy)
                                                .clamp(0.05, 0.95);
                                        ratios[stack_idx - 1] =
                                            (grab.start_stack_ratios[stack_idx - 1] - ratio_dy)
                                                .clamp(0.05, 0.95);
                                    }
                                }

                                let sum: f32 = ratios.iter().sum();
                                if sum > 0.0 {
                                    for r in ratios.iter_mut() {
                                        *r /= sum;
                                    }
                                }

                                ws.stack_ratios = ratios;
                            }
                        }

                        let _ = ws;
                        state.recalculate_layout();
                        state.needs_redraw = true;
                    }
                }

                let under = surface_under_pointer(state, pos);
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
                return;
            }

            // ── Normal (no-grab) pointer motion ──
            // CLICK-TO-FOCUS: We intentionally do NOT change keyboard
            // focus here. Focus changes happen ONLY on button press.
            // We still update the pointer surface so hover cursors
            // (resize handles, text-input I-beams, etc.) work correctly.
            let under = surface_under_pointer(state, pos);

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

            // ── Button release: finalise any active grab ──
            if button_state == ButtonState::Released {
                if let Some(grab) = state.pointer_grab.take() {
                    debug!(mode = ?grab.mode, "pointer grab released");

                    match grab.mode {
                        GrabMode::TiledMove => {
                            let pos = state.pointer_location;
                            let ws = &state.workspaces[state.active_workspace];
                            let drop_target = ws
                                .space
                                .element_under(pos)
                                .map(|(w, _)| w.clone());

                            if let Some(target) = drop_target {
                                if target != grab
                                    .window
                                    && !ws.floating.contains(&target)
                                    && !ws.floating.contains(&grab.window)
                                {
                                    let idx_a = ws
                                        .windows
                                        .iter()
                                        .position(|w| w == &grab.window);
                                    let idx_b = ws
                                        .windows
                                        .iter()
                                        .position(|w| w == &target);
                                    if let (Some(a), Some(b)) = (idx_a, idx_b) {
                                        info!(
                                            from = a,
                                            to = b,
                                            "drag-to-swap: swapping windows"
                                        );
                                        state.swap_windows(a, b);
                                    }
                                }
                            }
                        }
                        GrabMode::TiledResize => {
                            state.recalculate_layout();
                        }
                        GrabMode::FloatingResize => {
                            state.recalculate_layout();
                        }
                        GrabMode::FloatingMove => {}
                    }
                    state.needs_redraw = true;
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
                return;
            }

            // ── Button press ──
            let pos = state.pointer_location;
            let hit = hit_test_window(state, pos);

            let keyboard = state.keyboard.clone();
            let super_held = keyboard.modifier_state().logo;

            if super_held {
                if let Some(window) = hit.clone() {
                    let is_floating = {
                        let ws = &state.workspaces[state.active_workspace];
                        ws.floating.contains(&window)
                    };

                    if is_floating {
                        let grab_mode = match button {
                            BTN_LEFT  => Some(GrabMode::FloatingMove),
                            BTN_RIGHT => Some(GrabMode::FloatingResize),
                            _ => None,
                        };

                        if let Some(mode) = grab_mode {
                            state.focus_window(&window);

                            let ws = &state.workspaces[state.active_workspace];
                            let geo = ws
                                .floating_geo
                                .get(&window)
                                .copied()
                                .unwrap_or_else(|| {
                                    let loc = ws
                                        .space
                                        .element_location(&window)
                                        .unwrap_or_else(|| Point::from((100, 100)));
                                    let size = window.geometry().size;
                                    let size = if size.w > 0 && size.h > 0 {
                                        size
                                    } else {
                                        Size::from((640, 480))
                                    };
                                    Rectangle::new(loc, size)
                                });

                            info!(?mode, ?geo, "starting floating grab");

                            state.pointer_grab = Some(GrabState {
                                mode,
                                window: window.clone(),
                                start_pointer: pos,
                                start_geo: geo,
                                start_split_ratio: 0.5,
                                start_stack_ratios: Vec::new(),
                                screen_width: 0,
                                screen_height: 0,
                                tiled_index: 0,
                                tiled_count: 0,
                            });

                            let pointer = state.pointer.clone();
                            pointer.frame(state);
                            return;
                        }
                    } else {
                        let grab_mode = match button {
                            BTN_LEFT  => Some(GrabMode::TiledMove),
                            BTN_RIGHT => Some(GrabMode::TiledResize),
                            _ => None,
                        };

                        if let Some(mode) = grab_mode {
                            state.focus_window(&window);

                            let ws = &mut state.workspaces[state.active_workspace];

                            ws.ensure_stack_ratios();
                            ws.normalise_stack_ratios();

                            let split_ratio = ws.split_ratio;
                            let stack_ratios = ws.stack_ratios.clone();
                            let tiled = ws.tiled_windows();
                            let tiled_count = tiled.len();
                            let tiled_index = tiled
                                .iter()
                                .position(|w| w == &window)
                                .unwrap_or(0);

                            let (screen_w, screen_h) = usable_screen_dimensions(state);

                            let ws = &state.workspaces[state.active_workspace];
                            let loc = ws
                                .space
                                .element_location(&window)
                                .unwrap_or_else(|| Point::from((0, 0)));
                            let size = window_current_size(&window)
                                .unwrap_or_else(|| Size::from((640, 480)));
                            let geo = Rectangle::new(loc, size);

                            info!(
                                ?mode,
                                ?geo,
                                split_ratio,
                                tiled_index,
                                tiled_count,
                                screen_w,
                                screen_h,
                                "starting tiled grab"
                            );

                            state.pointer_grab = Some(GrabState {
                                mode,
                                window: window.clone(),
                                start_pointer: pos,
                                start_geo: geo,
                                start_split_ratio: split_ratio,
                                start_stack_ratios: stack_ratios,
                                screen_width: screen_w,
                                screen_height: screen_h,
                                tiled_index,
                                tiled_count,
                            });

                            let pointer = state.pointer.clone();
                            pointer.frame(state);
                            return;
                        }
                    }
                }
            }

            // Normal click — focus the window, or an interactive layer surface.
            // Never steal focus from a layer surface on a normal click on a
            // window behind it.
            if state.layer_has_keyboard_focus() {
                // Layer surface has focus — only allow clicks on the layer
                // surface itself to go through, don't change keyboard focus.
                if let Some((clicked, _)) = surface_under_pointer(state, pos) {
                    let mut root = clicked;
                    while let Some(parent) = get_parent(&root) {
                        root = parent;
                    }
                    let is_layer = {
                        let map = layer_map_for_output(&state.output);
                        map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL).is_some()
                    };
                    if !is_layer {
                        // Clicked outside — only CLOSE overlay/exclusive popups,
                        // just release focus for persistent bars like Waybar.
                        if let Some(focused) = state.keyboard.current_focus() {
                            if let Some(layer) = state.layer_surface_of(&focused) {
                                let is_popup = matches!(
                                    layer.layer(),
                                    smithay::wayland::shell::wlr_layer::Layer::Overlay
                                ) || matches!(
                                    layer.cached_state().keyboard_interactivity,
                                    smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive
                                );
                                if is_popup {
                                    info!("click outside popup layer surface — closing it");
                                    layer.layer_surface().send_close();
                                } else {
                                    info!("click outside persistent layer surface — releasing focus only");
                                }
                            }
                        }
                        state.drop_focus_to_active_window();
                        if let Some(window) = hit {
                            state.focus_window(&window);
                        }
                    }
                }
            } else if let Some(window) = hit {
                state.focus_window(&window);
            } else if let Some((clicked, _)) = surface_under_pointer(state, pos) {
                let mut root = clicked;
                while let Some(parent) = get_parent(&root) {
                    root = parent;
                }
                let interactive = {
                    let map = layer_map_for_output(&state.output);
                    map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                        .map(|l| l.can_receive_keyboard_focus())
                        .unwrap_or(false)
                };
                if interactive {
                    let keyboard = state.keyboard.clone();
                    if keyboard.current_focus().as_ref() != Some(&root) {
                        let focus_serial = SERIAL_COUNTER.next_serial();
                        keyboard.set_focus(state, Some(root), focus_serial);
                    }
                }
            } else {
                let keyboard = state.keyboard.clone();
                let focus_is_layer = keyboard.current_focus().map(|s| {
                    let mut root = s;
                    while let Some(parent) = get_parent(&root) {
                        root = parent;
                    }
                    let map = layer_map_for_output(&state.output);
                    map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL).is_some()
                }).unwrap_or(false);
                if focus_is_layer {
                    let fallback = state.workspaces[state.active_workspace]
                        .windows
                        .last()
                        .and_then(|w| w.toplevel())
                        .map(|t| t.wl_surface().clone());
                    let focus_serial = SERIAL_COUNTER.next_serial();
                    keyboard.set_focus(state, fallback, focus_serial);
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

        InputEvent::DeviceAdded { mut device } => {
            info!(name = %device.name(), "libinput: device added");
            if device.config_tap_finger_count() > 0 {
                let _ = device.config_tap_set_enabled(true);
                info!(
                    name = %device.name(),
                    fingers = device.config_tap_finger_count(),
                    "tap-to-click enabled"
                );
            }
        }
        InputEvent::DeviceRemoved { device } => {
            info!(name = %device.name(), "libinput: device removed");
        }

        // ─────────────────────────────────────────────────────────
        // Touchpad 3-finger swipe → workspace switching
        // ─────────────────────────────────────────────────────────

                // ─────────────────────────────────────────────────────────
        // Phase 33: 1-to-1 touchpad workspace gesture
        // ─────────────────────────────────────────────────────────

        InputEvent::GestureSwipeBegin { event } => {
            let fingers = event.fingers();
            debug!(fingers, "gesture swipe begin");
            if fingers == 3 || fingers == 4 {
                // Determine screen width for this gesture
                let screen_w = state.workspaces[state.active_workspace]
                    .space
                    .output_geometry(&state.output)
                    .map(|g| g.size.w as f64)
                    .unwrap_or(1920.0);

                state.gesture_swipe = crate::state::GestureSwipeState {
                    tracking: true,
                    fingers,
                    cumulative_dx: 0.0,
                    origin_workspace: state.active_workspace,
                    screen_width: screen_w,
                    animating: false,
                    anim_start_offset: 0.0,
                    anim_target_offset: 0.0,
                    anim_start_time: Instant::now(),
                    anim_duration: Duration::from_millis(300),
                };
                state.gesture_start_time = Instant::now();
            }
        }

        InputEvent::GestureSwipeUpdate { event } => {
            if state.gesture_swipe.tracking {
                let dx = event.delta_x();
                let new_offset = state.gesture_swipe.cumulative_dx + dx;

                // Clamp: don't scroll past the first or last workspace
                let max_ws = state.workspaces.len().saturating_sub(1);
                let origin = state.gesture_swipe.origin_workspace;
                let screen_w = state.gesture_swipe.screen_width;

                let clamped = if origin == 0 {
                    // Can't go further right (no workspace to the left)
                    new_offset.min(0.0).max(-screen_w)
                } else if origin >= max_ws {
                    // Can't go further left (no workspace to the right)
                    new_offset.max(0.0).min(screen_w)
                } else {
                    new_offset.clamp(-screen_w, screen_w)
                };

                // Apply rubber-band at the edges for a premium feel
                state.gesture_swipe.cumulative_dx = if (origin == 0 && new_offset > 0.0)
                    || (origin >= max_ws && new_offset < 0.0)
                {
                    // Rubber-band: only 20% of the overscroll delta
                    state.gesture_swipe.cumulative_dx + dx * 0.2
                } else {
                    clamped
                };

                trace!(
                    dx,
                    offset = state.gesture_swipe.cumulative_dx,
                    "gesture swipe update (1-to-1)"
                );
                state.needs_redraw = true;
            }
        }

        InputEvent::GestureSwipeEnd { event } => {
            if state.gesture_swipe.tracking {
                let cancelled = event.cancelled();
                let gesture_duration = state.gesture_start_time.elapsed().as_millis() as f64;

                debug!(
                    offset = state.gesture_swipe.cumulative_dx,
                    cancelled,
                    duration_ms = gesture_duration,
                    "gesture swipe end"
                );

                if cancelled {
                    // Cancelled gesture → snap back
                    state.gesture_swipe.tracking = false;
                    state.gesture_swipe.animating = true;
                    state.gesture_swipe.anim_start_offset =
                        state.gesture_swipe.cumulative_dx;
                    state.gesture_swipe.anim_target_offset = 0.0;
                    state.gesture_swipe.anim_start_time = Instant::now();
                    state.gesture_swipe.anim_duration = Duration::from_millis(200);
                    state.gesture_pending_switch = None;
                } else {
                    let max_ws = state.workspaces.len().saturating_sub(1);
                    let outcome = state.gesture_swipe.finish(gesture_duration);
                    match outcome {
                        crate::state::GestureOutcome::SwitchTo(target) => {
                            if target <= max_ws {
                                info!(
                                    from = state.gesture_swipe.origin_workspace + 1,
                                    to = target + 1,
                                    "gesture: committing workspace switch"
                                );
                                state.gesture_pending_switch = Some(target);
                            } else {
                                // Out of bounds, snap back
                                state.gesture_swipe.anim_target_offset = 0.0;
                                state.gesture_pending_switch = None;
                            }
                        }
                        crate::state::GestureOutcome::SnapBack => {
                            info!("gesture: snapping back to origin workspace");
                            state.gesture_pending_switch = None;
                        }
                    }
                }
                state.needs_redraw = true;
            }
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
        IpcCommand::SetOpacity { value } => {
            info!(opacity = value, "IPC: set opacity");
            state.set_window_opacity(value);
            state.broadcast_opacity_state();
        }
        IpcCommand::AdjustOpacity { value } => {
            info!(delta = value, "IPC: adjust opacity");
            state.adjust_window_opacity(value);
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

fn spawn_launcher(command: &str) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        warn!("config: launcher is empty, nothing to spawn");
        return;
    }
    info!(command = trimmed, "spawning launcher");

    match Command::new("sh")
        .arg("-c")
        .arg(trimmed)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            debug!(pid, command = trimmed, "launcher spawned");
            let command_owned = trimmed.to_string();
            std::thread::spawn(move || {
                let t0 = Instant::now();
                match child.wait_with_output() {
                    Ok(output) if !output.status.success() => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let elapsed = t0.elapsed();
                        warn!(
                            pid,
                            command = %command_owned,
                            exit_code = ?output.status.code(),
                            stderr = %stderr.trim(),
                            "launcher exited with error"
                        );

                        if elapsed < Duration::from_millis(500) {
                            let retry_cmd = build_config_bypass(&command_owned);
                            if retry_cmd != command_owned {
                                warn!(
                                    retry = %retry_cmd,
                                    "launcher failed quickly — retrying without user config"
                                );
                                match Command::new("sh")
                                    .arg("-c")
                                    .arg(&retry_cmd)
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn()
                                {
                                    Ok(retry_child) => {
                                        info!(
                                            pid = retry_child.id(),
                                            command = %retry_cmd,
                                            "launcher retry spawned"
                                        );
                                    }
                                    Err(err) => {
                                        warn!(
                                            ?err,
                                            command = %retry_cmd,
                                            "launcher retry also failed to spawn"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Ok(_) => {
                        debug!(pid, command = %command_owned, "launcher exited");
                    }
                    Err(err) => warn!(pid, command = %command_owned, ?err, "wait failed"),
                }
            });
        }
        Err(err) => warn!(?err, command = trimmed, "failed to spawn launcher"),
    }
}

fn build_config_bypass(original: &str) -> String {
    let base = original.split_whitespace().next().unwrap_or("");

    if base.ends_with("fuzzel") || base == "fuzzel" {
        if !original.contains("--config") && !original.contains("-C") {
            return format!("{} --config /dev/null", original);
        }
    }

    if base.ends_with("wofi") || base == "wofi" {
        if !original.contains("--conf") {
            return format!("{} --conf /dev/null", original);
        }
    }

    if base.ends_with("rofi") || base == "rofi" {
        if !original.contains("-no-config") {
            return format!("{} -no-config", original);
        }
    }

    original.to_string()
}

fn spawn_terminal(command: &str) {
    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        warn!("config: terminal_command is empty, nothing to spawn");
        return;
    };
    let args: Vec<&str> = parts.collect();
    info!(program, ?args, "spawning terminal");

    match Command::new(program)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            debug!(pid, program, "terminal spawned");

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

/// Query current volume from wpctl. Returns (volume_0_to_1, is_muted).
fn query_volume() -> (f32, bool) {
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output();

    match output {
        Ok(out) => {
            // Output looks like: "Volume: 0.50" or "Volume: 0.50 [MUTED]"
            let text = String::from_utf8_lossy(&out.stdout);
            let muted = text.contains("[MUTED]");
            let vol = text
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            (vol, muted)
        }
        Err(_) => (0.0, false),
    }
}

/// Query current brightness from brightnessctl. Returns 0.0–1.0.
fn query_brightness() -> f32 {
    let current = Command::new("brightnessctl")
        .args(["get"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f32>().ok())
        .unwrap_or(0.0);

    let max = Command::new("brightnessctl")
        .args(["max"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f32>().ok())
        .unwrap_or(1.0);

    if max > 0.0 { (current / max).clamp(0.0, 1.0) } else { 0.0 }
}