-- ============================================================
--  mywm — default rc.lua
--  ~/.config/mywm/rc.lua
-- ============================================================

local w = wm

-- ── Theme palette (8 themes, each with a distinct dominant hue) ──
-- 🔴 Red → 🟠 Orange → 🟡 Yellow → 🟢 Green → 🔵 Cyan → 🔷 Blue → 🟣 Purple → 🩷 Pink
local themes = {
    {
        -- 🔵 BLUE dominant — Tokyo Night
        name        = "tokyonight",
        active      = "#7aa2f7",
        inactive    = "#1a1b26",
        bg          = "#1a1b26",
        bg_alt      = "#24283b",
        bg_surface  = "#292e42",
        fg          = "#c0caf5",
        fg_dim      = "#565f89",
        fg_bright   = "#e0e6ff",
        accent      = "#7aa2f7",
        accent2     = "#bb9af7",
        accent3     = "#ff007c",
        green       = "#73daca",
        red         = "#f7768e",
        orange      = "#ff9e64",
        yellow      = "#e0af68",
        cyan        = "#7dcfff",
        teal        = "#2ac3de",
        magenta     = "#c678dd",
        pink        = "#ff79c6",
        border_glow = "#7aa2f7",
        separator   = "#3b4261",
        urgent      = "#db4b4b",
        success     = "#9ece6a",
        warning     = "#e0af68",
    },
    {
        -- 🟡 YELLOW dominant — Gruvbox
        name        = "gruvbox",
        active      = "#fabd2f",
        inactive    = "#3c3836",
        bg          = "#282828",
        bg_alt      = "#3c3836",
        bg_surface  = "#504945",
        fg          = "#ebdbb2",
        fg_dim      = "#928374",
        fg_bright   = "#fbf1c7",
        accent      = "#fabd2f",
        accent2     = "#fe8019",
        accent3     = "#b8bb26",
        green       = "#b8bb26",
        red         = "#fb4934",
        orange      = "#fe8019",
        yellow      = "#fabd2f",
        cyan        = "#83a598",
        teal        = "#8ec07c",
        magenta     = "#d3869b",
        pink        = "#ea6962",
        border_glow = "#fabd2f",
        separator   = "#665c54",
        urgent      = "#cc241d",
        success     = "#98971a",
        warning     = "#d79921",
    },
    {
        -- 🟢 GREEN dominant — Everforest
        name        = "everforest",
        active      = "#a7c080",
        inactive    = "#272e33",
        bg          = "#272e33",
        bg_alt      = "#2e383c",
        bg_surface  = "#374145",
        fg          = "#d3c6aa",
        fg_dim      = "#7a8478",
        fg_bright   = "#e9e1d4",
        accent      = "#a7c080",
        accent2     = "#83c092",
        accent3     = "#dbbc7f",
        green       = "#a7c080",
        red         = "#e67e80",
        orange      = "#e69875",
        yellow      = "#dbbc7f",
        cyan        = "#7fbbb3",
        teal        = "#83c092",
        magenta     = "#d699b6",
        pink        = "#ea9fc0",
        border_glow = "#a7c080",
        separator   = "#4f5b58",
        urgent      = "#e67e80",
        success     = "#a7c080",
        warning     = "#dbbc7f",
    },
    {
        -- 🔴 RED dominant — Rosé Pine
        name        = "rosepine",
        active      = "#eb6f92",
        inactive    = "#1f1d2e",
        bg          = "#191724",
        bg_alt      = "#1f1d2e",
        bg_surface  = "#26233a",
        fg          = "#e0def4",
        fg_dim      = "#6e6a86",
        fg_bright   = "#f0efff",
        accent      = "#eb6f92",
        accent2     = "#ebbcba",
        accent3     = "#f6c177",
        green       = "#9ccfd8",
        red         = "#eb6f92",
        orange      = "#f6c177",
        yellow      = "#f6c177",
        cyan        = "#9ccfd8",
        teal        = "#56949f",
        magenta     = "#c4a7e7",
        pink        = "#ebbcba",
        border_glow = "#eb6f92",
        separator   = "#524f67",
        urgent      = "#d7345b",
        success     = "#9ccfd8",
        warning     = "#f6c177",
    },
    {
        -- 🟠 ORANGE dominant — Kanagawa
        name        = "kanagawa",
        active      = "#ffa066",
        inactive    = "#1f1f28",
        bg          = "#1f1f28",
        bg_alt      = "#2a2a37",
        bg_surface  = "#363646",
        fg          = "#dcd7ba",
        fg_dim      = "#727169",
        fg_bright   = "#f0ead6",
        accent      = "#ffa066",
        accent2     = "#e6c384",
        accent3     = "#ff5d62",
        green       = "#76946a",
        red         = "#ff5d62",
        orange      = "#ffa066",
        yellow      = "#e6c384",
        cyan        = "#7fb4ca",
        teal        = "#6a9589",
        magenta     = "#957fb8",
        pink        = "#d27e99",
        border_glow = "#ffa066",
        separator   = "#54546d",
        urgent      = "#e82424",
        success     = "#76946a",
        warning     = "#e6c384",
    },
    {
        -- 🩷 PINK dominant — Catppuccin
        name        = "catppuccin",
        active      = "#f5c2e7",
        inactive    = "#313244",
        bg          = "#1e1e2e",
        bg_alt      = "#313244",
        bg_surface  = "#45475a",
        fg          = "#cdd6f4",
        fg_dim      = "#585b70",
        fg_bright   = "#eef0fc",
        accent      = "#f5c2e7",
        accent2     = "#f2cdcd",
        accent3     = "#fab387",
        green       = "#a6e3a1",
        red         = "#f38ba8",
        orange      = "#fab387",
        yellow      = "#f9e2af",
        cyan        = "#89dceb",
        teal        = "#94e2d5",
        magenta     = "#cba6f7",
        pink        = "#f5c2e7",
        border_glow = "#f5c2e7",
        separator   = "#585b70",
        urgent      = "#e64553",
        success     = "#a6e3a1",
        warning     = "#f9e2af",
    },
    {
        -- 🟣 PURPLE dominant — Dracula
        name        = "dracula",
        active      = "#bd93f9",
        inactive    = "#44475a",
        bg          = "#282a36",
        bg_alt      = "#44475a",
        bg_surface  = "#4d5066",
        fg          = "#f8f8f2",
        fg_dim      = "#6272a4",
        fg_bright   = "#ffffff",
        accent      = "#bd93f9",
        accent2     = "#ff79c6",
        accent3     = "#ffb86c",
        green       = "#50fa7b",
        red         = "#ff5555",
        orange      = "#ffb86c",
        yellow      = "#f1fa8c",
        cyan        = "#8be9fd",
        teal        = "#69d2a0",
        magenta     = "#bd93f9",
        pink        = "#ff79c6",
        border_glow = "#bd93f9",
        separator   = "#6272a4",
        urgent      = "#ff2222",
        success     = "#50fa7b",
        warning     = "#ffb86c",
    },
    {
        -- 🩵 CYAN dominant — Nord
        name        = "nord",
        active      = "#88c0d0",
        inactive    = "#2e3440",
        bg          = "#2e3440",
        bg_alt      = "#3b4252",
        bg_surface  = "#434c5e",
        fg          = "#eceff4",
        fg_dim      = "#4c566a",
        fg_bright   = "#ffffff",
        accent      = "#88c0d0",
        accent2     = "#8fbcbb",
        accent3     = "#d08770",
        green       = "#a3be8c",
        red         = "#bf616a",
        orange      = "#d08770",
        yellow      = "#ebcb8b",
        cyan        = "#88c0d0",
        teal        = "#8fbcbb",
        magenta     = "#b48ead",
        pink        = "#c78dab",
        border_glow = "#88c0d0",
        separator   = "#4c566a",
        urgent      = "#bf3b44",
        success     = "#a3be8c",
        warning     = "#ebcb8b",
    },
}

w.__theme_index = 1
w.active_border_color   = themes[1].active
w.inactive_border_color = themes[1].inactive

-- ── Helper: resolve waybar config directory ──
local function waybar_config_dir()
    local home = os.getenv("HOME") or ""
    return home .. "/.config/waybar"
end

-- ── Helper: hex "#rrggbb" → r, g, b integers ──
local function hex_to_rgb(hex)
    hex = hex:gsub("^#", "")
    local r = tonumber(hex:sub(1, 2), 16) or 0
    local g = tonumber(hex:sub(3, 4), 16) or 0
    local b = tonumber(hex:sub(5, 6), 16) or 0
    return r, g, b
end

-- ── Helper: generate rgba() string from hex + alpha ──
local function hex_rgba(hex, alpha)
    local r, g, b = hex_to_rgb(hex)
    return string.format("rgba(%d, %d, %d, %.2f)", r, g, b, alpha)
end

-- ── Write colors.css for the given theme ──
local function write_theme_css(t)
    local dir = waybar_config_dir()
    local path = dir .. "/colors.css"

    local css = string.format([[
/* Auto-generated by mywm rc.lua — do not edit manually */
/* Theme: %s */

/* ── Background tiers ── */
@define-color bg_color %s;
@define-color bg_alt_color %s;
@define-color bg_surface_color %s;

/* ── Foreground tiers ── */
@define-color fg_color %s;
@define-color fg_dim_color %s;
@define-color fg_bright_color %s;

/* ── Accents ── */
@define-color accent_color %s;
@define-color accent2_color %s;
@define-color accent3_color %s;

/* ── Semantic ── */
@define-color green_color %s;
@define-color red_color %s;
@define-color orange_color %s;
@define-color yellow_color %s;
@define-color cyan_color %s;
@define-color teal_color %s;
@define-color magenta_color %s;
@define-color pink_color %s;

/* ── Status ── */
@define-color urgent_color %s;
@define-color success_color %s;
@define-color warning_color %s;

/* ── Compositor borders ── */
@define-color active_border %s;
@define-color inactive_border %s;

/* ── Computed translucent variants ── */
@define-color bar_bg_color %s;
@define-color accent_hover %s;
@define-color accent_subtle %s;
@define-color accent_border %s;
@define-color red_hover %s;
@define-color red_subtle %s;
@define-color orange_hover %s;
@define-color green_subtle %s;
@define-color separator_color %s;
@define-color border_glow %s;
]],
        t.name,
        -- Background tiers
        t.bg, t.bg_alt, t.bg_surface,
        -- Foreground tiers
        t.fg, t.fg_dim, t.fg_bright,
        -- Accents
        t.accent, t.accent2, t.accent3,
        -- Semantic
        t.green, t.red, t.orange, t.yellow,
        t.cyan, t.teal, t.magenta, t.pink,
        -- Status
        t.urgent, t.success, t.warning,
        -- Compositor borders
        t.active, t.inactive,
        -- Computed translucent variants
        hex_rgba(t.bg, 0.92),           -- bar_bg_color
        hex_rgba(t.accent, 0.15),       -- accent_hover
        hex_rgba(t.accent, 0.10),       -- accent_subtle
        hex_rgba(t.accent, 0.30),       -- accent_border
        hex_rgba(t.red, 0.20),          -- red_hover       (was 0.15, too faint)
        hex_rgba(t.red, 0.10),          -- red_subtle
        hex_rgba(t.orange, 0.18),       -- orange_hover
        hex_rgba(t.green, 0.10),        -- green_subtle
        hex_rgba(t.separator, 0.50),    -- separator_color  (was 0.4, bumped)
        hex_rgba(t.border_glow, 0.35)   -- border_glow
    )

    local f = io.open(path, "w")
    if f then
        f:write(css)
        f:close()
        print(string.format("theme: wrote %s (%s)", path, t.name))
    else
        print(string.format("theme: ERROR — could not write %s", path))
    end
end

-- ── Write initial theme CSS on startup ──
-- This may fail on first boot (waybar dir doesn't exist yet).
-- ensure_waybar_config() in the compositor will write a bootstrap
-- colors.css before waybar starts, so this is safe to skip.
local ok, err = pcall(write_theme_css, themes[1])
if not ok then
    print("theme: skipping initial write (waybar dir not ready yet — this is normal)")
end

function cycle_theme()
    w.__theme_index = (w.__theme_index % #themes) + 1
    local t = themes[w.__theme_index]

    -- Update compositor border colors
    w.active_border_color   = t.active
    w.inactive_border_color = t.inactive

    -- Write the new CSS variables file
    write_theme_css(t)

    -- Hot-reload Waybar CSS (SIGUSR2 triggers CSS-only reload)
    os.execute("pkill -SIGUSR2 waybar 2>/dev/null")

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
    "/bin/bash -c 'exec waybar > /tmp/waybar-stderr.log 2>&1'",
}

print(string.format(
    "rc.lua: %d workspaces, gaps %d/%d, border %dpx %s [%s]",
    #w.workspace_names,
    w.outer_gaps, w.inner_gaps,
    w.border_width,
    w.active_border_color,
    themes[w.__theme_index].name
))