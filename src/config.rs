//! Lua-driven runtime configuration.
//!
//! Phase 13 introduced an embedded Lua 5.4 interpreter (via `mlua`); Phase 14
//! keeps the interpreter alive for the whole session so the user's rc.lua
//! can define callbacks (`cycle_theme`, `toggle_navbar`) that the
//! compositor invokes when keys are pressed.
//!
//! Startup sequence:
//!
//!  1. spin up a Lua VM with the full stdlib (including `os`/`io`, needed
//!     by `toggle_navbar`'s shell-out),
//!  2. seed a global `wm` table with the built-in defaults,
//!  3. execute the user's `rc.lua`,
//!  4. read `wm.*` back out into our strongly-typed [`Config`].
//!
//! Any failure at any stage — missing file, IO error, Lua syntax error,
//! runtime error, type mismatch — downgrades gracefully to the built-in
//! defaults so the compositor always boots.

use std::{
    fs,
    path::PathBuf,
};

use mlua::{Lua, Table, Value};
use tracing::{info, warn};

// -------------------------------------------------------------------------
// Config
// -------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Config {
    pub terminal: String,
    pub launcher: String,
    pub outer_gaps: i32,
    pub inner_gaps: i32,
    pub border_width: i32,
    pub active_border_color: String,
    pub inactive_border_color: String,
    pub workspace_names: Vec<String>,
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
            workspace_names: vec![
                "1".into(), "2".into(), "3".into(),
                "4".into(), "5".into(), "6".into(),
                "7".into(), "8".into(), "9".into(),
            ],
        }
    }
}

impl Config {
    /// Number of workspaces — derived from `workspace_names` so the name
    /// list is the single source of truth.
    pub fn workspace_count(&self) -> usize {
        self.workspace_names.len()
    }

    /// Boot the Lua VM, run `rc.lua`, and return the live VM alongside
    /// the parsed configuration. The VM is kept alive for the whole
    /// session so callbacks like `cycle_theme()` can be invoked later.
    ///
    /// On any failure we still return a live VM (possibly with only the
    /// defaults seeded) plus a default [`Config`] so the compositor boots.
    pub fn load_from_lua() -> (Lua, Self) {
        // SAFETY: we use the *unsafe* constructor because our default
        // rc.lua calls `os.execute("pkill waybar; waybar &")`, and the
        // "safe" stdlib subset strips `os.execute`. The user's rc.lua is
        // already trusted code (we run it in-process), so giving it the
        // full stdlib is the right call.
        let lua = unsafe { Lua::unsafe_new() };
        let defaults = Self::default();

        if let Err(err) = seed_wm_table(&lua, &defaults) {
            warn!(?err, "config: failed to seed `wm` table, using defaults");
            return (lua, defaults);
        }

        let Some(path) = rc_path() else {
            warn!("config: could not resolve config directory, using defaults");
            return (lua, defaults);
        };

        // First run: drop the default rc.lua on disk.
        if !path.exists() {
            if let Some(dir) = path.parent() {
                if let Err(err) = fs::create_dir_all(dir) {
                    warn!(?dir, ?err, "config: failed to create directory");
                    return (lua, defaults);
                }
            }
            if let Err(err) = fs::write(&path, DEFAULT_RC_LUA) {
                warn!(?path, ?err, "config: failed to write default rc.lua");
                return (lua, defaults);
            }
            info!(?path, "config: wrote default rc.lua");
        }

        let source = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => {
                warn!(?path, ?err, "config: failed to read rc.lua, using defaults");
                return (lua, defaults);
            }
        };

        if let Err(err) = lua.load(&source).set_name("rc.lua").exec() {
            warn!(?path, error = %err, "config: rc.lua failed, using defaults");
            return (lua, defaults);
        }
        info!(?path, "config: rc.lua loaded");

        let cfg = read_config_from_lua(&lua).unwrap_or_else(|err| {
            warn!(?err, "config: could not read back wm table, using defaults");
            Self::default()
        });

        (lua, cfg)
    }

    /// Re-read `wm.*` out of a live Lua VM. Used after `cycle_theme()` /
    /// `toggle_navbar()` have mutated the table.
    pub fn refresh_from_lua(&mut self, lua: &Lua) {
        match read_config_from_lua(lua) {
            Ok(new) => *self = new,
            Err(err) => warn!(?err, "config: refresh from Lua failed"),
        }
    }
}

// -------------------------------------------------------------------------
// Lua plumbing
// -------------------------------------------------------------------------

fn seed_wm_table(lua: &Lua, defaults: &Config) -> mlua::Result<()> {
    let wm = lua.create_table()?;
    wm.set("terminal", defaults.terminal.clone())?;
    wm.set("launcher", defaults.launcher.clone())?;
    wm.set("outer_gaps", defaults.outer_gaps)?;
    wm.set("inner_gaps", defaults.inner_gaps)?;
    wm.set("border_width", defaults.border_width)?;
    wm.set("active_border_color", defaults.active_border_color.clone())?;
    wm.set("inactive_border_color", defaults.inactive_border_color.clone())?;

    let names = lua.create_table()?;
    for (i, n) in defaults.workspace_names.iter().enumerate() {
        // Lua tables are 1-indexed — keep that idiom visible to the user.
        names.set(i + 1, n.clone())?;
    }
    wm.set("workspace_names", names)?;

    // Seed an empty autostart table so rc.lua can append to it.
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
        workspace_names,    })
}

/// Pull a field out of a Lua table, swapping in `fallback` (with a warning)
/// on any type / lookup error.
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

fn rc_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            return Some(p.join("mywm").join("rc.lua"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config").join("mywm").join("rc.lua"))
}

// -------------------------------------------------------------------------
// Default rc.lua (written on first boot)
// -------------------------------------------------------------------------

const DEFAULT_RC_LUA: &str = r##"-- ============================================================
--  mywm — default rc.lua
--  ~/.config/mywm/rc.lua
--
--  This file is plain Lua 5.4. It runs once, inside the
--  compositor, at startup. The `wm` table is pre-populated
--  with built-in defaults before this script runs — mutate it
--  and the compositor reads your changes back.
--
--  Functions you define as globals (cycle_theme, toggle_navbar)
--  can be invoked from keybinds; the Rust side calls them by
--  name whenever you press the associated key chord.
--
--  NOTE: Do NOT use os.execute() to launch Wayland clients
--  (waybar, terminals, etc.) directly in this file. When
--  rc.lua runs, the Wayland socket does not exist yet.
--  Instead, add commands to `wm.autostart` — the compositor
--  will spawn them after the socket is live.
-- ============================================================

-- ------------------------------------------------------------
--  1.  Shortcut
-- ------------------------------------------------------------
local w = wm

-- ------------------------------------------------------------
--  2.  Terminal
-- ------------------------------------------------------------
w.terminal = "alacritty"

-- Application launcher. Spawned on Super+D via `sh -c`, so any
-- arguments you pass here are parsed by the shell as usual.
-- `fuzzel` is recommended — it is a native wlr-layer-shell launcher
-- with no GTK3 dependency. `wofi --show drun` also works but its
-- GTK3 seat initialisation is fragile on non-GNOME compositors.
w.launcher = "fuzzel"

-- ------------------------------------------------------------
--  3.  Gaps & borders
-- ------------------------------------------------------------
w.outer_gaps   = 15
w.inner_gaps   = 10
w.border_width = 2

-- ------------------------------------------------------------
--  4.  Workspaces
-- ------------------------------------------------------------
w.workspace_names = {
    "1:web", "2:code", "3:term", "4:chat", "5:media",
    "6", "7", "8", "9:scratch",
}

-- ------------------------------------------------------------
--  5.  Theme palette
--
--      Each entry has `name`, `active` (border on the focused
--      window) and `inactive` (everything else). Add or remove
--      freely — the cycle function below just walks the list.
-- ------------------------------------------------------------
local themes = {
    { name = "tokyonight", active = "#7aa2f7", inactive = "#1a1b26" },
    { name = "gruvbox",    active = "#fabd2f", inactive = "#3c3836" },
    { name = "dracula",    active = "#bd93f9", inactive = "#44475a" },
    { name = "catppuccin", active = "#f5c2e7", inactive = "#313244" },
    { name = "nord",       active = "#88c0d0", inactive = "#2e3440" },
}

-- Internal state for the cycler — prefixed with `__` to
-- signal "Rust doesn't read this".
w.__theme_index = 1
w.active_border_color   = themes[1].active
w.inactive_border_color = themes[1].inactive

-- Invoked from Rust when Super+T is pressed. Must be a global
-- function (not `local`) so the compositor can resolve it.
function cycle_theme()
    w.__theme_index = (w.__theme_index % #themes) + 1
    local t = themes[w.__theme_index]
    w.active_border_color   = t.active
    w.inactive_border_color = t.inactive
    print(string.format("theme -> %s  active=%s inactive=%s",
          t.name, t.active, t.inactive))
end

-- ------------------------------------------------------------
--  6.  Navbar (Waybar) placement
--
--      `toggle_navbar` regenerates ~/.config/waybar/config
--      with the opposite `position` value, then restarts
--      waybar so the change takes effect. Invoked from
--      Super+B.
-- ------------------------------------------------------------
w.__navbar_position = "top"
w.__navbar_visible  = true

local function waybar_config_text(position)
    return string.format([[{
  "position": "%s",
  "height": 30,
  "spacing": 6,
  "modules-left":   ["sway/workspaces"],
  "modules-center": ["clock"],
  "modules-right":  ["pulseaudio", "battery", "tray"]
}
]], position)
end

function toggle_navbar()
    if w.__navbar_visible then
        os.execute("pkill waybar 2>/dev/null")
        w.__navbar_visible = false
        print("navbar -> hidden")
    else
        w.__navbar_position = (w.__navbar_position == "top")
                              and "bottom" or "top"
        local home = os.getenv("HOME") or ""
        local dir  = home .. "/.config/waybar"
        os.execute("mkdir -p '" .. dir .. "'")
        local path = dir .. "/config"
        local f, err = io.open(path, "w")
        if f == nil then
            print("toggle_navbar: open " .. path .. " failed: " .. (err or "?"))
            return
        end
        f:write(waybar_config_text(w.__navbar_position))
        f:close()
        os.execute("pkill waybar 2>/dev/null; (waybar >/dev/null 2>&1 &)")
        w.__navbar_visible = true
        print("navbar -> " .. w.__navbar_position)
    end
end

-- ------------------------------------------------------------
--  7.  Autostart
--
--      Commands listed here are spawned by the compositor
--      AFTER the Wayland socket is live. Do NOT use
--      os.execute() for Wayland clients in this file.
-- ------------------------------------------------------------
w.autostart = {
    "pkill waybar 2>/dev/null; waybar >/dev/null 2>&1 &",
}

-- ------------------------------------------------------------
--  8.  Startup summary
-- ------------------------------------------------------------
print(string.format(
    "rc.lua: %d workspaces, gaps %d/%d, border %dpx %s",
    #w.workspace_names,
    w.outer_gaps, w.inner_gaps,
    w.border_width,
    w.active_border_color
))
"##;