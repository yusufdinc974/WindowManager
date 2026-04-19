//! Dual-format runtime configuration: TOML (simple) + Lua (advanced).
//!
//! Load order:
//!   1. Deploy compiled-in assets to ~/.config/mywm/
//!   2. Parse the deployed config.toml
//!   3. Run the deployed rc.lua on top (can override TOML values)
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
// Compiled-in assets — single source of truth
// -------------------------------------------------------------------------

const ASSET_DEFAULT_CONFIG_TOML: &str = include_str!("../assets/default_config.toml");
const ASSET_DEFAULT_RC_LUA: &str = include_str!("../assets/default_rc.lua");

// -------------------------------------------------------------------------
// Keybinding types
// -------------------------------------------------------------------------

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
    pub terminal: String,
    pub launcher: String,
    pub outer_gaps: i32,
    pub inner_gaps: i32,
    pub border_width: i32,
    pub active_border_color: String,
    pub inactive_border_color: String,
    pub clear_color: String,
    pub workspace_names: Vec<String>,
    #[serde(default = "default_keybinds")]
    pub keybinds: Vec<Keybind>,
    #[serde(default = "default_swipe_threshold")]
    pub swipe_threshold: f64,
}

fn default_swipe_threshold() -> f64 {
    100.0
}

/// Minimal hardcoded fallback — used ONLY by serde's #[serde(default)]
/// to fill missing fields. Must NOT call toml::from_str to avoid
/// infinite recursion.
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
            swipe_threshold: default_swipe_threshold(),
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
        Keybind { modifiers: super_only.clone(), key: "w".into(), action: "toggle_wallpaper_menu".into() },
        Keybind { modifiers: super_only.clone(), key: "Left".into(), action: "focus_left".into() },
        Keybind { modifiers: super_only.clone(), key: "Right".into(), action: "focus_right".into() },
        Keybind { modifiers: super_shift.clone(), key: "Left".into(), action: "move_window_left".into() },
        Keybind { modifiers: super_shift.clone(), key: "Right".into(), action: "move_window_right".into() },
        Keybind { modifiers: super_only.clone(), key: "1".into(), action: "workspace_1".into() },
        Keybind { modifiers: super_only.clone(), key: "2".into(), action: "workspace_2".into() },
        Keybind { modifiers: super_only.clone(), key: "3".into(), action: "workspace_3".into() },
        Keybind { modifiers: super_only.clone(), key: "4".into(), action: "workspace_4".into() },
        Keybind { modifiers: super_only.clone(), key: "5".into(), action: "workspace_5".into() },
        Keybind { modifiers: super_only.clone(), key: "6".into(), action: "workspace_6".into() },
        Keybind { modifiers: super_only.clone(), key: "7".into(), action: "workspace_7".into() },
        Keybind { modifiers: super_only.clone(), key: "8".into(), action: "workspace_8".into() },
        Keybind { modifiers: super_only.clone(), key: "9".into(), action: "workspace_9".into() },
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
    /// Parse the compiled-in asset TOML to produce the canonical defaults.
    /// Falls back to Config::default() (hardcoded) if parsing fails.
    fn from_asset() -> Self {
        match toml::from_str::<Config>(ASSET_DEFAULT_CONFIG_TOML) {
            Ok(config) => config,
            Err(err) => {
                eprintln!(
                    "BUG: failed to parse compiled-in default_config.toml: {err}. \
                     Using minimal hardcoded defaults."
                );
                Self::default()
            }
        }
    }

    pub fn workspace_count(&self) -> usize {
        self.workspace_names.len()
    }

    pub fn clear_color_f32(&self) -> [f32; 4] {
        parse_hex_color(&self.clear_color)
    }

    /// Deploy compiled-in asset files to the config directory.
    fn deploy_assets() {
        let Some(dir) = config_dir() else {
            warn!("config: cannot resolve config dir — asset deploy skipped");
            return;
        };

        if let Err(err) = fs::create_dir_all(&dir) {
            warn!(?err, ?dir, "config: failed to create config dir");
            return;
        }

        // ── Config files ──
        let toml_dest = dir.join("config.toml");
        match fs::write(&toml_dest, ASSET_DEFAULT_CONFIG_TOML) {
            Ok(()) => info!(?toml_dest, "config: deployed config.toml from asset"),
            Err(err) => warn!(?err, ?toml_dest, "config: failed to write config.toml"),
        }

        let rc_dest = dir.join("rc.lua");
        match fs::write(&rc_dest, ASSET_DEFAULT_RC_LUA) {
            Ok(()) => info!(?rc_dest, "config: deployed rc.lua from asset"),
            Err(err) => warn!(?err, ?rc_dest, "config: failed to write rc.lua"),
        }

        // ── Scripts directory ──
        let script_dir = dir.join("scripts");
        let _ = fs::create_dir_all(&script_dir);

        // mywm-workspaces.sh
        let script_dest = script_dir.join("mywm-workspaces.sh");
        match fs::write(&script_dest, include_str!("../assets/mywm-workspaces.sh")) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(
                        &script_dest,
                        fs::Permissions::from_mode(0o755),
                    );
                }
                info!(?script_dest, "config: deployed mywm-workspaces.sh from asset");
            }
            Err(err) => warn!(?err, ?script_dest, "config: failed to write workspace script"),
        }

        // wallpaper-picker.sh
        let picker_dest = script_dir.join("wallpaper-picker.sh");
        match fs::write(&picker_dest, include_str!("../assets/wallpaper-picker.sh")) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(
                        &picker_dest,
                        fs::Permissions::from_mode(0o755),
                    );
                }
                info!(?picker_dest, "config: deployed wallpaper-picker.sh from asset");
            }
            Err(err) => warn!(?err, ?picker_dest, "config: failed to write wallpaper picker script"),
        }

        // wallpaper-restore.sh
        let restore_dest = script_dir.join("wallpaper-restore.sh");
        match fs::write(&restore_dest, include_str!("../assets/wallpaper-restore.sh")) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(
                        &restore_dest,
                        fs::Permissions::from_mode(0o755),
                    );
                }
                info!(?restore_dest, "config: deployed wallpaper-restore.sh from asset");
            }
            Err(err) => warn!(?err, ?restore_dest, "config: failed to write wallpaper restore script"),
        }

        // ── Wallpaper directories (one per theme) ──
        let wallpapers_dir = dir.join("wallpapers");
        for theme_name in &[
            "tokyonight", "gruvbox", "everforest", "rosepine",
            "kanagawa", "catppuccin", "dracula", "nord",
        ] {
            let theme_wp_dir = wallpapers_dir.join(theme_name);
            if let Err(err) = fs::create_dir_all(&theme_wp_dir) {
                warn!(?err, ?theme_wp_dir, "config: failed to create wallpaper theme dir");
            }
        }
        info!(?wallpapers_dir, "config: ensured wallpaper directories exist");
    }

    /// Load configuration: deploy assets, parse TOML, then run Lua on top.
    pub fn load_from_lua() -> (Lua, Self) {
        Self::deploy_assets();

        let mut config = Self::from_asset();

        if let Some(toml_path) = toml_path() {
            if toml_path.exists() {
                match fs::read_to_string(&toml_path) {
                    Ok(contents) => match toml::from_str::<Config>(&contents) {
                        Ok(toml_config) => {
                            info!(?toml_path, "config: loaded config.toml");
                            config = toml_config;
                        }
                        Err(err) => {
                            warn!(
                                ?toml_path, %err,
                                "config: config.toml parse error, using asset defaults"
                            );
                        }
                    },
                    Err(err) => {
                        warn!(?toml_path, ?err, "config: failed to read config.toml");
                    }
                }
            }
        }

        let lua = unsafe { Lua::unsafe_new() };

        if let Err(err) = seed_wm_table(&lua, &config) {
            warn!(?err, "config: failed to seed `wm` table");
            return (lua, config);
        }

        if let Some(path) = rc_path() {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(source) => {
                        if let Err(err) = lua.load(&source).set_name("rc.lua").exec() {
                            warn!(?path, error = %err, "config: rc.lua execution failed");
                        } else {
                            info!(?path, "config: rc.lua loaded");
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

    pub fn refresh_from_lua(&mut self, lua: &Lua) {
        match read_config_from_lua(lua) {
            Ok(new) => *self = new,
            Err(err) => warn!(?err, "config: refresh from Lua failed"),
        }
    }

    pub fn reload(&mut self, lua: &Lua) {
        Self::deploy_assets();

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
// Hex color parser
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
    wm.set("swipe_threshold", config.swipe_threshold)?;

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
        &wm, "active_border_color", defaults.active_border_color.clone(),
    );
    let inactive_border_color: String = get_or(
        &wm, "inactive_border_color", defaults.inactive_border_color.clone(),
    );
    let clear_color: String = get_or(
        &wm, "clear_color", defaults.clear_color.clone(),
    );
    let swipe_threshold: f64 = get_or(&wm, "swipe_threshold", defaults.swipe_threshold);

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
            if out.is_empty() { defaults.workspace_names.clone() } else { out }
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
        keybinds: defaults.keybinds,
        swipe_threshold,
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