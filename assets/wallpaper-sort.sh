#!/usr/bin/env bash
# ============================================================
#  lumie — Automatic Wallpaper Color Classifier
#  Usage: wallpaper-sort.sh <source_directory>
#
#  Analyzes dominant colors of images and copies them
#  to the best-matching theme wallpaper directory.
#
#  Requires: imagemagick (convert command)
#  Install:  sudo apt install imagemagick
# ============================================================

set -euo pipefail

SOURCE_DIR="${1:-}"
WALLPAPER_BASE="$HOME/.config/lumie/wallpapers"

if [[ -z "$SOURCE_DIR" || ! -d "$SOURCE_DIR" ]]; then
    echo "Usage: wallpaper-sort.sh <source_directory>"
    echo "  Scans all images in <source_directory> and copies them"
    echo "  to ~/.config/lumie/wallpapers/<best_matching_theme>/"
    exit 1
fi

# Check imagemagick
if ! command -v convert &>/dev/null; then
    echo "ERROR: imagemagick is required. Install with:"
    echo "  sudo apt install imagemagick"
    exit 1
fi

# ── Theme dominant color definitions (hue ranges + saturation/value hints) ──
# Each theme is defined by its primary accent hue (0-360), plus bg luminance.
# Format: name:hue:hue_tolerance:min_sat:bg_luminance
#
# Hue wheel: 0=Red, 30=Orange, 60=Yellow, 120=Green, 180=Cyan, 240=Blue, 270=Purple, 330=Pink

declare -A THEME_HUE
declare -A THEME_HUE_TOL
declare -A THEME_MIN_SAT
declare -A THEME_BG_LUM

# Tokyo Night — Blue accent, very dark bg
THEME_HUE[tokyonight]=225
THEME_HUE_TOL[tokyonight]=30
THEME_MIN_SAT[tokyonight]=20
THEME_BG_LUM[tokyonight]=12

# Gruvbox — Yellow/amber accent, warm dark bg
THEME_HUE[gruvbox]=45
THEME_HUE_TOL[gruvbox]=25
THEME_MIN_SAT[gruvbox]=30
THEME_BG_LUM[gruvbox]=16

# Everforest — Green accent, forest dark bg
THEME_HUE[everforest]=100
THEME_HUE_TOL[everforest]=35
THEME_MIN_SAT[everforest]=20
THEME_BG_LUM[everforest]=17

# Rosé Pine — Red/rose accent, deep purple bg
THEME_HUE[rosepine]=345
THEME_HUE_TOL[rosepine]=30
THEME_MIN_SAT[rosepine]=30
THEME_BG_LUM[rosepine]=10

# Kanagawa — Orange accent, dark indigo bg
THEME_HUE[kanagawa]=25
THEME_HUE_TOL[kanagawa]=20
THEME_MIN_SAT[kanagawa]=40
THEME_BG_LUM[kanagawa]=13

# Catppuccin — Pink/lavender accent, deep blue bg
THEME_HUE[catppuccin]=310
THEME_HUE_TOL[catppuccin]=35
THEME_MIN_SAT[catppuccin]=20
THEME_BG_LUM[catppuccin]=14

# Dracula — Purple accent, dark gray-blue bg
THEME_HUE[dracula]=265
THEME_HUE_TOL[dracula]=25
THEME_MIN_SAT[dracula]=30
THEME_BG_LUM[dracula]=17

# Nord — Cyan/teal accent, dark slate bg
THEME_HUE[nord]=193
THEME_HUE_TOL[nord]=25
THEME_MIN_SAT[nord]=25
THEME_BG_LUM[nord]=20

THEMES=(tokyonight gruvbox everforest rosepine kanagawa catppuccin dracula nord)

# Ensure all theme directories exist
for t in "${THEMES[@]}"; do
    mkdir -p "${WALLPAPER_BASE}/${t}"
done

# ── Extract dominant colors from an image ──
# Returns: "hue saturation luminance" of the top 5 dominant colors
get_dominant_colors() {
    local img="\$1"
    # Resize to small for speed, extract top 5 colors
    convert "$img" -resize 100x100! -colors 5 -depth 8 \
        -format '%c' histogram:info:- 2>/dev/null | \
    while IFS= read -r line; do
        # Extract RGB values from histogram output
        local rgb
        rgb=$(echo "$line" | grep -oP '#[0-9A-Fa-f]{6}' | head -1)
        if [[ -n "$rgb" ]]; then
            local r g b
            r=$((16#${rgb:1:2}))
            g=$((16#${rgb:3:2}))
            b=$((16#${rgb:5:2}))
            # Convert RGB to HSL
            rgb_to_hsl "$r" "$g" "$b"
        fi
    done
}

# ── RGB to HSL conversion in bash ──
rgb_to_hsl() {
    local r=\$1 g=\$2 b=\$3

    # Normalize to 0-1000 range (avoid floating point)
    local r1=$(( r * 1000 / 255 ))
    local g1=$(( g * 1000 / 255 ))
    local b1=$(( b * 1000 / 255 ))

    local max=$r1 min=$r1
    [[ $g1 -gt $max ]] && max=$g1
    [[ $b1 -gt $max ]] && max=$b1
    [[ $g1 -lt $min ]] && min=$g1
    [[ $b1 -lt $min ]] && min=$b1

    local lum=$(( (max + min) / 2 ))
    local sat=0
    local hue=0

    if [[ $max -ne $min ]]; then
        local diff=$(( max - min ))
        if [[ $lum -gt 500 ]]; then
            sat=$(( diff * 1000 / (2000 - max - min) ))
        else
            sat=$(( diff * 1000 / (max + min) ))
        fi

        if [[ $max -eq $r1 ]]; then
            hue=$(( (g1 - b1) * 60 / diff ))
        elif [[ $max -eq $g1 ]]; then
            hue=$(( 120 + (b1 - r1) * 60 / diff ))
        else
            hue=$(( 240 + (r1 - g1) * 60 / diff ))
        fi

        [[ $hue -lt 0 ]] && hue=$(( hue + 360 ))
    fi

    # Output: hue(0-360) sat(0-100) lum(0-100)
    echo "$hue $(( sat / 10 )) $(( lum / 10 ))"
}

# ── Score an image against a theme ──
score_theme() {
    local img="\$1"
    local theme="\$2"

    local target_hue=${THEME_HUE[$theme]}
    local hue_tol=${THEME_HUE_TOL[$theme]}
    local min_sat=${THEME_MIN_SAT[$theme]}
    local target_lum=${THEME_BG_LUM[$theme]}

    local total_score=0
    local color_count=0

    while IFS=' ' read -r hue sat lum; do
        [[ -z "$hue" ]] && continue
        color_count=$((color_count + 1))

        # Hue distance (circular, 0-180)
        local hue_diff=$(( (hue - target_hue + 360) % 360 ))
        [[ $hue_diff -gt 180 ]] && hue_diff=$(( 360 - hue_diff ))

        # Hue score: closer = better (max 100)
        local hue_score=0
        if [[ $hue_diff -le $hue_tol ]]; then
            hue_score=$(( 100 - (hue_diff * 100 / hue_tol) ))
        fi

        # Saturation bonus (saturated colors match better)
        local sat_score=0
        if [[ $sat -ge $min_sat ]]; then
            sat_score=$(( sat > 80 ? 30 : sat * 30 / 80 ))
        fi

        # Luminance match for dark themes (dark images match dark themes)
        local lum_diff=$(( lum - target_lum ))
        [[ $lum_diff -lt 0 ]] && lum_diff=$(( -lum_diff ))
        local lum_score=$(( lum_diff < 30 ? 20 : 0 ))

        total_score=$(( total_score + hue_score + sat_score + lum_score ))
    done < <(get_dominant_colors "$img")

    if [[ $color_count -gt 0 ]]; then
        echo $(( total_score / color_count ))
    else
        echo 0
    fi
}

# ── Main: classify each image ──
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  lumie Wallpaper Classifier                              ║"
echo "║  Scanning: $SOURCE_DIR"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""

classified=0
skipped=0

find "$SOURCE_DIR" -maxdepth 1 -type f \
    \( -iname '*.png' -o -iname '*.jpg' -o -iname '*.jpeg' \
       -o -iname '*.webp' -o -iname '*.bmp' \) \
    -print0 | \
while IFS= read -r -d '' img; do
    filename=$(basename "$img")
    
    best_theme=""
    best_score=0

    for theme in "${THEMES[@]}"; do
        score=$(score_theme "$img" "$theme")
        if [[ $score -gt $best_score ]]; then
            best_score=$score
            best_theme=$theme
        fi
    done

    if [[ -n "$best_theme" && $best_score -gt 10 ]]; then
        cp "$img" "${WALLPAPER_BASE}/${best_theme}/${filename}"
        printf "  %-40s → %-15s (score: %d)\n" "$filename" "$best_theme" "$best_score"
        classified=$((classified + 1))
    else
        # Low confidence — copy to ALL themes
        for theme in "${THEMES[@]}"; do
            cp "$img" "${WALLPAPER_BASE}/${theme}/${filename}"
        done
        printf "  %-40s → ALL themes     (score too low: %d)\n" "$filename" "$best_score"
        skipped=$((skipped + 1))
    fi
done

echo ""
echo "Done! Classified: $classified | Sent to all: $skipped"
echo ""
echo "Wallpaper counts per theme:"
for t in "${THEMES[@]}"; do
    count=$(find "${WALLPAPER_BASE}/${t}" -maxdepth 1 -type f 2>/dev/null | wc -l)
    printf "  %-15s %d wallpapers\n" "$t" "$count"
done