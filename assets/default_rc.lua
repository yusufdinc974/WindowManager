-- ============================================================
--  lumie — default rc.lua
--  ~/.config/lumie/rc.lua
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

-- ── Helper: resolve the REAL home directory ──
-- lumie sandboxes HOME under /run/user/<uid>/lumie-<id>/home/
-- We detect this and resolve to the actual passwd home.
local function real_home_dir()
    local h = os.getenv("HOME") or ""
    if h:match("/run/user/%d+/lumie%-.+/home") then
        local handle = io.popen("getent passwd $(id -u) | cut -d: -f6 2>/dev/null")
        if handle then
            local real = handle:read("*l")
            handle:close()
            if real and real ~= "" then
                return real
            end
        end
    end
    return h
end

-- ── Helper: get the session HOME (possibly sandboxed) ──
-- Used for waybar config which lives in the sandbox
local function session_home_dir()
    return os.getenv("HOME") or ""
end

-- Waybar lives in the SANDBOXED home (compositor deploys it there)
local function waybar_config_dir()
    return session_home_dir() .. "/.config/waybar"
end

-- Wallpapers and lumie config live in the REAL home
local function lumie_config_dir()
    return real_home_dir() .. "/.config/lumie"
end

local function lumie_cache_dir()
    return real_home_dir() .. "/.cache/lumie"
end

-- ── Helper: find theme index by name ──
local function theme_index_by_name(name)
    for i, t in ipairs(themes) do
        if t.name == name then return i end
    end
    return nil
end

-- ── Restore last theme from cache, or default to 1 ──
local function restore_theme_index()
    local cache = lumie_cache_dir() .. "/current_theme.txt"
    local f = io.open(cache, "r")
    if f then
        local name = f:read("*l")
        f:close()
        if name and name ~= "" then
            local idx = theme_index_by_name(name)
            if idx then
                print("theme: restored '" .. name .. "' (index " .. idx .. ") from cache")
                return idx
            end
        end
    end
    return 1
end

local function save_theme_index(t)
    local cache_dir = lumie_cache_dir()
    os.execute('mkdir -p "' .. cache_dir .. '"')
    local f = io.open(cache_dir .. "/current_theme.txt", "w")
    if f then
        f:write(t.name .. "\n")
        f:close()
    end
end

-- Now override the initial theme index with the restored one
w.__theme_index = restore_theme_index()
w.active_border_color   = themes[w.__theme_index].active
w.inactive_border_color = themes[w.__theme_index].inactive

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

-- ── Helper: check if a file exists ──
local function file_exists(path)
    local f = io.open(path, "r")
    if f then
        f:close()
        return true
    end
    return false
end

-- ── Helper: read first line from a file ──
local function read_file_line(path)
    local f = io.open(path, "r")
    if not f then return nil end
    local line = f:read("*l")
    f:close()
    if line and line ~= "" then
        return line
    end
    return nil
end

-- ── Write colors.css for the given theme ──
local function write_theme_css(t)
    local dir = waybar_config_dir()
    local path = dir .. "/colors.css"

    local css = string.format([[
/* Auto-generated by lumie rc.lua — do not edit manually */
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
        hex_rgba(t.red, 0.20),          -- red_hover
        hex_rgba(t.red, 0.10),          -- red_subtle
        hex_rgba(t.orange, 0.18),       -- orange_hover
        hex_rgba(t.green, 0.10),        -- green_subtle
        hex_rgba(t.separator, 0.50),    -- separator_color
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

-- ============================================================
--  Phase 34: SwayOSD theme CSS writer
-- ============================================================

local function write_swayosd_css(t)
    local dir = session_home_dir() .. "/.config/swayosd"
    os.execute('mkdir -p "' .. dir .. '"')

    local css = string.format([[
/* Auto-generated by lumie rc.lua — theme: %s */

window#osd {
    padding: 12px 20px;
    border-radius: 999px;
    background: %s;
    border: 2px solid %s;
}

#osd .progressbar {
    border-radius: 999px;
    background: %s;
    min-height: 6px;
}

#osd .progressbar:disabled {
    background: %s;
}

#osd .progressbar progress {
    border-radius: 999px;
    background: %s;
    min-height: 6px;
}

#osd .icon {
    color: %s;
    padding-right: 8px;
}

#osd .label {
    color: %s;
}
]],
        t.name,
        t.bg,                           -- window background
        t.accent,                       -- window border
        hex_rgba(t.accent, 0.15),       -- progressbar trough
        hex_rgba(t.fg_dim, 0.08),       -- progressbar disabled
        t.accent,                       -- progressbar fill
        t.accent,                       -- icon color
        t.fg                            -- label color
    )

    local path = dir .. "/style.css"
    local f = io.open(path, "w")
    if f then
        f:write(css)
        f:close()
        print(string.format("swayosd: wrote %s (%s)", path, t.name))
    else
        print(string.format("swayosd: ERROR — could not write %s", path))
    end
end

-- ── Write initial theme CSS on startup (using restored theme) ──
local ok, err = pcall(write_theme_css, themes[w.__theme_index])
if not ok then
    print("theme: skipping initial write (waybar dir not ready yet — this is normal)")
end

-- Write initial SwayOSD CSS on startup
local ok2, err2 = pcall(write_swayosd_css, themes[w.__theme_index])
if not ok2 then
    print("swayosd: skipping initial write — " .. tostring(err2))
end

-- ============================================================
--  Wallpaper Engine
-- ============================================================

--- Apply a wallpaper via swaybg and cache the path.
--- @param img_path string  Full path to the image file
--- @param theme_name string  Theme name for per-theme caching
local function apply_wallpaper(img_path, theme_name)
    if not img_path or img_path == "" then return false end
    if not file_exists(img_path) then
        print("wallpaper: file not found — " .. img_path)
        return false
    end

    -- Kill existing swaybg, launch new one
    os.execute("pkill -x swaybg 2>/dev/null; sleep 0.1")
    os.execute(string.format(
        'swaybg -i "%s" -m fill &', img_path
    ))

    -- Write caches
    local cache_dir = lumie_cache_dir()
    os.execute('mkdir -p "' .. cache_dir .. '"')

    -- Global cache
    local gf = io.open(cache_dir .. "/current_wallpaper.txt", "w")
    if gf then gf:write(img_path .. "\n"); gf:close() end

    -- Per-theme cache
    if theme_name and theme_name ~= "" then
        local tf = io.open(cache_dir .. "/wallpaper_" .. theme_name .. ".txt", "w")
        if tf then tf:write(img_path .. "\n"); tf:close() end
    end

    print(string.format("wallpaper: applied %s (theme: %s)", img_path, theme_name or "?"))
    return true
end

--- Pick a random wallpaper from a theme's wallpaper directory.
--- @param theme_name string
--- @return string|nil  Full path to a random image, or nil if none found
local function random_wallpaper_for_theme(theme_name)
    local dir = lumie_config_dir() .. "/wallpapers/" .. theme_name
    local handle = io.popen(
        'find "' .. dir .. '" -maxdepth 1 -type f '
        .. '\\( -iname "*.png" -o -iname "*.jpg" -o -iname "*.jpeg" '
        .. '-o -iname "*.webp" -o -iname "*.bmp" \\) 2>/dev/null'
    )
    if not handle then return nil end

    local images = {}
    for line in handle:lines() do
        if line ~= "" then
            images[#images + 1] = line
        end
    end
    handle:close()

    if #images == 0 then return nil end

    math.randomseed(os.time() + os.clock() * 1000)
    return images[math.random(#images)]
end

--- Restore wallpaper for a given theme (used on boot, reload, and theme switch).
--- Priority: per-theme cache → random pick from theme dir.
--- @param theme_name string
local function restore_wallpaper_for_theme(theme_name)
    if not theme_name or theme_name == "" then return end

    local cache_dir = lumie_cache_dir()

    -- 1. Per-theme cache
    local theme_cache = cache_dir .. "/wallpaper_" .. theme_name .. ".txt"
    local cached = read_file_line(theme_cache)
    if cached and file_exists(cached) then
        apply_wallpaper(cached, theme_name)
        return
    end

    -- 2. Global cache (only if it belongs to this theme's directory)
    local global_cache = cache_dir .. "/current_wallpaper.txt"
    local global_cached = read_file_line(global_cache)
    if global_cached then
        local theme_dir_prefix = lumie_config_dir() .. "/wallpapers/" .. theme_name .. "/"
        if global_cached:sub(1, #theme_dir_prefix) == theme_dir_prefix
           and file_exists(global_cached) then
            apply_wallpaper(global_cached, theme_name)
            return
        end
    end

    -- 3. Random pick
    local random_img = random_wallpaper_for_theme(theme_name)
    if random_img then
        apply_wallpaper(random_img, theme_name)
        return
    end

    print("wallpaper: no wallpapers available for theme '" .. theme_name .. "'")
end

-- ============================================================
--  Wallpaper Picker (Super + W)
-- ============================================================

function current_theme_name()
    return themes[w.__theme_index].name
end


function toggle_wallpaper_menu()
    local theme_name = current_theme_name()
    local script = lumie_config_dir() .. "/scripts/wallpaper-picker.sh"

    if file_exists(script) then
        os.execute(string.format(
            'setsid "%s" "%s" &', script, theme_name
        ))
        print("wallpaper: opened picker for theme " .. theme_name)
    else
        print("wallpaper: picker script not found at " .. script)
    end
end

-- ── Called by compositor after sandbox HOME is ready ──
function rewrite_current_theme_css()
    pcall(write_theme_css, themes[w.__theme_index])
    pcall(write_swayosd_css, themes[w.__theme_index])
end

-- ============================================================
--  Theme Cycling (Super + T) — updated with wallpaper + OSD hooks
-- ============================================================

function cycle_theme()
    w.__theme_index = (w.__theme_index % #themes) + 1
    local t = themes[w.__theme_index]

    -- Update compositor border colors
    w.active_border_color   = t.active
    w.inactive_border_color = t.inactive

    -- Write the new CSS variables file
    write_theme_css(t)

    -- Write SwayOSD theme CSS so the OSD matches
    pcall(write_swayosd_css, t)

    -- Hot-reload Waybar CSS (SIGUSR2 triggers CSS-only reload)
    os.execute("pkill -SIGUSR2 waybar 2>/dev/null")

    -- ── Wallpaper: switch to theme-appropriate wallpaper ──
    restore_wallpaper_for_theme(t.name)

    -- Persist theme choice for next session
    save_theme_index(t)

    print(string.format("theme -> %s  active=%s inactive=%s",
          t.name, t.active, t.inactive))
end

-- ── Navbar position cycler ──
-- Cycles through the four screen edges: top → right → bottom → left → top
local navbar_positions = { "top", "right", "bottom", "left" }
w.__navbar_pos_index = 1
w.__navbar_position  = navbar_positions[w.__navbar_pos_index]

local function rewrite_waybar_position(new_pos)
    -- Pick the right template: horizontal bars use full text + icons,
    -- vertical bars use a stripped icon-only config so the bar fits in
    -- ~38px width instead of growing to 270px to fit horizontal text.
    local is_vertical = (new_pos == "left" or new_pos == "right")
    local template = is_vertical and "config-vertical" or "config-horizontal"
    local src = waybar_config_dir() .. "/" .. template
    local dst = waybar_config_dir() .. "/config"

    local f = io.open(src, "r")
    if not f then
        print("toggle_navbar: cannot read " .. src)
        return false
    end
    local content = f:read("*a")
    f:close()

    -- Patch position to the requested edge (the template defaults to top/right)
    local count
    content, count = content:gsub('"position"%s*:%s*"[^"]*"',
                                  '"position": "' .. new_pos .. '"', 1)
    if count == 0 then
        print("toggle_navbar: no position field in " .. src)
        return false
    end

    f = io.open(dst, "w")
    if not f then
        print("toggle_navbar: cannot write " .. dst)
        return false
    end
    f:write(content)
    f:close()
    return true
end

function toggle_navbar()
    w.__navbar_pos_index = (w.__navbar_pos_index % #navbar_positions) + 1
    local new_pos = navbar_positions[w.__navbar_pos_index]
    w.__navbar_position = new_pos

    if not rewrite_waybar_position(new_pos) then
        return
    end

    os.execute("pkill -x waybar 2>/dev/null; sleep 0.4; "
               .. "setsid /bin/bash -c 'exec waybar > /tmp/waybar-stderr.log 2>&1' &")
    print("navbar -> " .. new_pos)
end

-- ── Autostart ──
-- Restore wallpaper for the initial theme on compositor boot
w.autostart = {
    "/bin/bash -c 'exec waybar > /tmp/waybar-stderr.log 2>&1'",
    string.format(
        '/bin/bash -c "sleep 0.3; swaybg -i \\"$(cat %s/wallpaper_%s.txt 2>/dev/null)\\" -m fill"',
        lumie_cache_dir(), themes[w.__theme_index].name
    ),
}

print(string.format(
    "rc.lua: %d workspaces, gaps %d/%d, border %dpx %s [%s]",
    #w.workspace_names,
    w.outer_gaps, w.inner_gaps,
    w.border_width,
    w.active_border_color,
    themes[w.__theme_index].name
))