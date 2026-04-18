//! Dual-format runtime configuration: TOML (simple) + Lua (advanced).
//!
//! Load order:
//!   1. Built-in defaults
//!   2. If `~/.config/mywm/config.toml` exists → load TOML overrides
//!   3. If `~/.config/mywm/rc.lua` exists → run Lua (can override TOML)
//!   4. Any missing field keeps its default
//!
//! The Lua VM stays alive for the session so callbacks (cycle_theme,
//! toggle_navbar) remain callable from keybinds.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
};

use mlua::{Lua, Table, Value};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// -------------------------------------------------------------------------
// Keybinding types
// -------------------------------------------------------------------------

/// A modifier set for keybindings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Modifiers {
    #[serde(default)]
    pub logo: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
}

impl Default for Modifiers {
    fn default() -> Self {
        Self { logo: true, shift: false, ctrl: false, alt: false }
    }
}

/// A single keybinding definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keybind {
    pub modifiers: Modifiers,
    pub key: String,
    pub action: String,
}

// -------------------------------------------------------------------------
// Config
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // ── Programs ──
    pub terminal: String,
    pub launcher: String,

    // ── Appearance ──
    pub outer_gaps: i32,
    pub inner_gaps: i32,
    pub border_width: i32,
    pub active_border_color: String,
    pub inactive_border_color: String,
    pub clear_color: String,

    // ── Workspaces ──
    pub workspace_names: Vec<String>,

    // ── Keybindings ──
    #[serde(default = "default_keybinds")]
    pub keybinds: Vec<Keybind>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            terminal: "alacritty".to_string(),
            launcher: "fuzzel".to_string(),
            outer_gaps: 15,
            inner_gaps: 10,
            border_width: 2,
            active_border_color: "#7aa2f7".to_string(),
            inactive_border_color: "#1a1b26".to_string(),
            clear_color: "#14101e".to_string(),
            workspace_names: vec![
                "1".into(), "2".into(), "3".into(),
                "4".into(), "5".into(), "6".into(),
                "7".into(), "8".into(), "9".into(),
            ],
            keybinds: default_keybinds(),
        }
    }
}

fn default_keybinds() -> Vec<Keybind> {
    let super_only = Modifiers { logo: true, shift: false, ctrl: false, alt: false };
    let super_shift = Modifiers { logo: true, shift: true, ctrl: false, alt: false };

    vec![
        Keybind { modifiers: super_shift.clone(), key: "Escape".into(), action: "quit".into() },
        Keybind { modifiers: super_shift.clone(), key: "r".into(), action: "reload_config".into() },
        Keybind { modifiers: super_only.clone(), key: "Return".into(), action: "spawn_terminal".into() },
        Keybind { modifiers: super_only.clone(), key: "d".into(), action: "spawn_launcher".into() },
        Keybind { modifiers: super_only.clone(), key: "q".into(), action: "close_focused".into() },
        Keybind { modifiers: super_only.clone(), key: "f".into(), action: "toggle_fullscreen".into() },
        Keybind { modifiers: super_only.clone(), key: "space".into(), action: "cycle_layout".into() },
        Keybind { modifiers: super_shift.clone(), key: "space".into(), action: "toggle_floating".into() },
        Keybind { modifiers: super_only.clone(), key: "t".into(), action: "cycle_theme".into() },
        Keybind { modifiers: super_only.clone(), key: "b".into(), action: "toggle_navbar".into() },
        Keybind { modifiers: super_only.clone(), key: "Left".into(), action: "focus_left".into() },
        Keybind { modifiers: super_only.clone(), key: "Right".into(), action: "focus_right".into() },
        Keybind { modifiers: super_shift.clone(), key: "Left".into(), action: "move_window_left".into() },
        Keybind { modifiers: super_shift.clone(), key: "Right".into(), action: "move_window_right".into() },
        // Workspace switching: Super+1..9
        Keybind { modifiers: super_only.clone(), key: "1".into(), action: "workspace_1".into() },
        Keybind { modifiers: super_only.clone(), key: "2".into(), action: "workspace_2".into() },
        Keybind { modifiers: super_only.clone(), key: "3".into(), action: "workspace_3".into() },
        Keybind { modifiers: super_only.clone(), key: "4".into(), action: "workspace_4".into() },
        Keybind { modifiers: super_only.clone(), key: "5".into(), action: "workspace_5".into() },
        Keybind { modifiers: super_only.clone(), key: "6".into(), action: "workspace_6".into() },
        Keybind { modifiers: super_only.clone(), key: "7".into(), action: "workspace_7".into() },
        Keybind { modifiers: super_only.clone(), key: "8".into(), action: "workspace_8".into() },
        Keybind { modifiers: super_only.clone(), key: "9".into(), action: "workspace_9".into() },
        // Move to workspace: Super+Shift+1..9
        Keybind { modifiers: super_shift.clone(), key: "1".into(), action: "move_to_workspace_1".into() },
        Keybind { modifiers: super_shift.clone(), key: "2".into(), action: "move_to_workspace_2".into() },
        Keybind { modifiers: super_shift.clone(), key: "3".into(), action: "move_to_workspace_3".into() },
        Keybind { modifiers: super_shift.clone(), key: "4".into(), action: "move_to_workspace_4".into() },
        Keybind { modifiers: super_shift.clone(), key: "5".into(), action: "move_to_workspace_5".into() },
        Keybind { modifiers: super_shift.clone(), key: "6".into(), action: "move_to_workspace_6".into() },
        Keybind { modifiers: super_shift.clone(), key: "7".into(), action: "move_to_workspace_7".into() },
        Keybind { modifiers: super_shift.clone(), key: "8".into(), action: "move_to_workspace_8".into() },
        Keybind { modifiers: super_shift.clone(), key: "9".into(), action: "move_to_workspace_9".into() },
    ]
}

impl Config {
    pub fn workspace_count(&self) -> usize {
        self.workspace_names.len()
    }

    /// Parse clear_color into [f32; 4].
    pub fn clear_color_f32(&self) -> [f32; 4] {
        parse_hex_color(&self.clear_color)
    }

    /// Load configuration: TOML first, then Lua on top.
    /// Returns the live Lua VM + parsed config.
    pub fn load_from_lua() -> (Lua, Self) {
        // Step 1: Start with defaults
        let mut config = Self::default();

        // Step 2: Try loading TOML
        if let Some(toml_path) = toml_path() {
            if toml_path.exists() {
                match fs::read_to_string(&toml_path) {
                    Ok(contents) => match toml::from_str::<Config>(&contents) {
                        Ok(toml_config) => {
                            info!(?toml_path, "config: loaded config.toml");
                            config = toml_config;
                        }
                        Err(err) => {
                            warn!(?toml_path, %err, "config: config.toml parse error, using defaults");
                        }
                    },
                    Err(err) => {
                        warn!(?toml_path, ?err, "config: failed to read config.toml");
                    }
                }
            } else {
                // Write a default config.toml on first boot
                if let Some(dir) = toml_path.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                match toml::to_string_pretty(&config) {
                    Ok(toml_str) => {
                        if let Err(err) = fs::write(&toml_path, &toml_str) {
                            warn!(?toml_path, ?err, "config: failed to write default config.toml");
                        } else {
                            info!(?toml_path, "config: wrote default config.toml");
                        }
                    }
                    Err(err) => {
                        warn!(%err, "config: failed to serialize default config to TOML");
                    }
                }
            }
        }

        // Step 3: Lua VM (always created — needed for callbacks)
        let lua = unsafe { Lua::unsafe_new() };

        if let Err(err) = seed_wm_table(&lua, &config) {
            warn!(?err, "config: failed to seed `wm` table");
            return (lua, config);
        }

        // Step 4: Try loading rc.lua
        if let Some(path) = rc_path() {
            if !path.exists() {
                if let Some(dir) = path.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                if let Err(err) = fs::write(&path, DEFAULT_RC_LUA) {
                    warn!(?path, ?err, "config: failed to write default rc.lua");
                } else {
                    info!(?path, "config: wrote default rc.lua");
                }
            }

            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(source) => {
                        if let Err(err) = lua.load(&source).set_name("rc.lua").exec() {
                            warn!(?path, error = %err, "config: rc.lua failed");
                        } else {
                            info!(?path, "config: rc.lua loaded");
                            // Read back any Lua overrides
                            match read_config_from_lua(&lua) {
                                Ok(lua_config) => config = lua_config,
                                Err(err) => {
                                    warn!(?err, "config: could not read back wm table");
                                }
                            }
                        }
                    }
                    Err(err) => {
                        warn!(?path, ?err, "config: failed to read rc.lua");
                    }
                }
            }
        }

        (lua, config)
    }

    /// Re-read `wm.*` out of a live Lua VM.
    pub fn refresh_from_lua(&mut self, lua: &Lua) {
        match read_config_from_lua(lua) {
            Ok(new) => *self = new,
            Err(err) => warn!(?err, "config: refresh from Lua failed"),
        }
    }

    /// Reload both TOML and Lua from disk.
    pub fn reload(&mut self, lua: &Lua) {
        // Re-read TOML
        if let Some(toml_path) = toml_path() {
            if toml_path.exists() {
                match fs::read_to_string(&toml_path) {
                    Ok(contents) => match toml::from_str::<Config>(&contents) {
                        Ok(toml_config) => {
                            info!(?toml_path, "config: reloaded config.toml");
                            *self = toml_config;
                        }
                        Err(err) => {
                            warn!(?toml_path, %err, "config: reload config.toml parse error");
                        }
                    },
                    Err(err) => {
                        warn!(?toml_path, ?err, "config: reload failed to read config.toml");
                    }
                }
            }
        }

        // Re-seed Lua with current config, then re-run rc.lua
        if let Err(err) = seed_wm_table(lua, self) {
            warn!(?err, "config: reload failed to seed wm table");
            return;
        }

        if let Some(path) = rc_path() {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(source) => {
                        if let Err(err) = lua.load(&source).set_name("rc.lua").exec() {
                            warn!(?path, error = %err, "config: reload rc.lua failed");
                        } else {
                            info!(?path, "config: reloaded rc.lua");
                            self.refresh_from_lua(lua);
                        }
                    }
                    Err(err) => {
                        warn!(?path, ?err, "config: reload failed to read rc.lua");
                    }
                }
            }
        }

        info!(
            terminal = %self.terminal,
            border = self.border_width,
            active = %self.active_border_color,
            inactive = %self.inactive_border_color,
            gaps = ?(self.outer_gaps, self.inner_gaps),
            workspaces = self.workspace_count(),
            "config: reload complete"
        );
    }

    /// Build a lookup table from (modifiers, key_name) → action string
    /// for fast keybinding dispatch.
    pub fn keybind_map(&self) -> HashMap<(bool, bool, bool, bool, String), String> {
        let mut map = HashMap::new();
        for kb in &self.keybinds {
            let key = (
                kb.modifiers.logo,
                kb.modifiers.shift,
                kb.modifiers.ctrl,
                kb.modifiers.alt,
                kb.key.to_lowercase(),
            );
            map.insert(key, kb.action.clone());
        }
        map
    }
}

// -------------------------------------------------------------------------
// Hex color parser (reusable)
// -------------------------------------------------------------------------

pub fn parse_hex_color(hex: &str) -> [f32; 4] {
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
// Lua plumbing
// -------------------------------------------------------------------------

fn seed_wm_table(lua: &Lua, config: &Config) -> mlua::Result<()> {
    let wm = lua.create_table()?;
    wm.set("terminal", config.terminal.clone())?;
    wm.set("launcher", config.launcher.clone())?;
    wm.set("outer_gaps", config.outer_gaps)?;
    wm.set("inner_gaps", config.inner_gaps)?;
    wm.set("border_width", config.border_width)?;
    wm.set("active_border_color", config.active_border_color.clone())?;
    wm.set("inactive_border_color", config.inactive_border_color.clone())?;
    wm.set("clear_color", config.clear_color.clone())?;

    let names = lua.create_table()?;
    for (i, n) in config.workspace_names.iter().enumerate() {
        names.set(i + 1, n.clone())?;
    }
    wm.set("workspace_names", names)?;

    let autostart = lua.create_table()?;
    wm.set("autostart", autostart)?;

    lua.globals().set("wm", wm)?;
    Ok(())
}

fn read_config_from_lua(lua: &Lua) -> mlua::Result<Config> {
    let defaults = Config::default();
    let wm: Table = lua.globals().get("wm")?;

    let terminal: String = get_or(&wm, "terminal", defaults.terminal.clone());
    let launcher: String = get_or(&wm, "launcher", defaults.launcher.clone());
    let outer_gaps: i32 = get_or(&wm, "outer_gaps", defaults.outer_gaps);
    let inner_gaps: i32 = get_or(&wm, "inner_gaps", defaults.inner_gaps);
    let border_width: i32 = get_or(&wm, "border_width", defaults.border_width);
    let active_border_color: String = get_or(
        &wm,
        "active_border_color",
        defaults.active_border_color.clone(),
    );
    let inactive_border_color: String = get_or(
        &wm,
        "inactive_border_color",
        defaults.inactive_border_color.clone(),
    );
    let clear_color: String = get_or(
        &wm,
        "clear_color",
        defaults.clear_color.clone(),
    );

    let workspace_names = match wm.get::<_, Value>("workspace_names") {
        Ok(Value::Table(t)) => {
            let mut out = Vec::new();
            for pair in t.sequence_values::<String>() {
                match pair {
                    Ok(s) => out.push(s),
                    Err(err) => {
                        warn!(?err, "config: workspace_names entry was not a string");
                    }
                }
            }
            if out.is_empty() {
                defaults.workspace_names.clone()
            } else {
                out
            }
        }
        _ => defaults.workspace_names.clone(),
    };

    Ok(Config {
        terminal,
        launcher,
        outer_gaps,
        inner_gaps,
        border_width,
        active_border_color,
        inactive_border_color,
        clear_color,
        workspace_names,
        keybinds: defaults.keybinds, // Keybinds stay from TOML/defaults (Lua doesn't override)
    })
}

fn get_or<V>(tbl: &Table, key: &str, fallback: V) -> V
where
    V: for<'lua> mlua::FromLua<'lua> + Clone,
{
    match tbl.get::<_, V>(key) {
        Ok(v) => v,
        Err(err) => {
            warn!(field = key, ?err, "config: bad value, falling back to default");
            fallback
        }
    }
}

// -------------------------------------------------------------------------
// Paths
// -------------------------------------------------------------------------

fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            return Some(p.join("mywm"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config").join("mywm"))
}

fn rc_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("rc.lua"))
}

fn toml_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

// -------------------------------------------------------------------------
// Default rc.lua
// -------------------------------------------------------------------------

const DEFAULT_RC_LUA: &str = r##"-- ============================================================
--  mywm — default rc.lua
--  ~/.config/mywm/rc.lua
-- ============================================================

local w = wm

-- ── Theme palette ──
local themes = {
    { name = "tokyonight", active = "#7aa2f7", inactive = "#1a1b26" },
    { name = "gruvbox",    active = "#fabd2f", inactive = "#3c3836" },
    { name = "dracula",    active = "#bd93f9", inactive = "#44475a" },
    { name = "catppuccin", active = "#f5c2e7", inactive = "#313244" },
    { name = "nord",       active = "#88c0d0", inactive = "#2e3440" },
}

w.__theme_index = 1
w.active_border_color   = themes[1].active
w.inactive_border_color = themes[1].inactive

function cycle_theme()
    w.__theme_index = (w.__theme_index % #themes) + 1
    local t = themes[w.__theme_index]
    w.active_border_color   = t.active
    w.inactive_border_color = t.inactive
    print(string.format("theme -> %s  active=%s inactive=%s",
          t.name, t.active, t.inactive))
end

-- ── Navbar toggle ──
w.__navbar_position = "top"
w.__navbar_visible  = true

function toggle_navbar()
    if w.__navbar_visible then
        os.execute("pkill -x waybar 2>/dev/null")
        w.__navbar_visible = false
        print("navbar -> hidden")
    else
        w.__navbar_position = (w.__navbar_position == "top")
                              and "bottom" or "top"
        os.execute("pkill -x waybar 2>/dev/null; sleep 0.2; setsid waybar &")
        w.__navbar_visible = true
        print("navbar -> " .. w.__navbar_position)
    end
end



-- ── Autostart ──
w.autostart = {
    "setsid waybar &",
}

print(string.format(
    "rc.lua: %d workspaces, gaps %d/%d, border %dpx %s",
    #w.workspace_names,
    w.outer_gaps, w.inner_gaps,
    w.border_width,
    w.active_border_color
))
"##;