#!/usr/bin/env python3
"""
Sort wallpapers by dominant color into lumie theme directories.

Themes and their dominant hues:
  tokyonight  → Blue    (#7aa2f7)
  gruvbox     → Yellow  (#fabd2f)
  everforest  → Green   (#a7c080)
  rosepine    → Red     (#eb6f92)
  kanagawa    → Orange  (#ffa066)
  catppuccin  → Pink    (#f5c2e7)
  dracula     → Purple  (#bd93f9)
  nord        → Cyan    (#88c0d0)

Usage:
  python3 wallpaper-sort.py /path/to/dharmx-walls
"""

import sys
import os
import shutil
import colorsys
from pathlib import Path
from collections import Counter

try:
    from PIL import Image
except ImportError:
    print("Installing Pillow...")
    os.system(f"{sys.executable} -m pip install Pillow")
    from PIL import Image

# ── Theme definitions: name → (hue_center, hue_range, sat_min) ──
# Hue is 0–360. Each theme claims a hue wedge.
THEMES = {
    #              hue_center  hue_tolerance  min_saturation
    "rosepine":   (350,        30,            0.15),   # Red
    "kanagawa":   (25,         20,            0.15),   # Orange
    "gruvbox":    (45,         20,            0.15),   # Yellow
    "everforest": (100,        40,            0.10),   # Green
    "nord":       (190,        30,            0.10),   # Cyan
    "tokyonight": (225,        30,            0.10),   # Blue
    "dracula":    (265,        25,            0.10),   # Purple
    "catppuccin": (310,        30,            0.10),   # Pink
}

DEST_BASE = Path.home() / ".config" / "lumie" / "wallpapers"
IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".webp", ".bmp", ".gif"}


def get_dominant_colors(img_path: str, n_colors: int = 8, sample_size: int = 150) -> list:
    """Extract dominant colors by quantizing a downscaled image."""
    try:
        img = Image.open(img_path).convert("RGB")
        # Downsample for speed
        img = img.resize((sample_size, sample_size), Image.LANCZOS)
        # Quantize to palette
        quantized = img.quantize(colors=n_colors, method=Image.Quantize.MEDIANCUT)
        palette = quantized.getpalette()
        # Count pixel occurrences per palette index
        pixel_counts = Counter(quantized.getdata())
        
        colors = []
        for idx, count in pixel_counts.most_common(n_colors):
            r = palette[idx * 3]
            g = palette[idx * 3 + 1]
            b = palette[idx * 3 + 2]
            colors.append((r, g, b, count))
        return colors
    except Exception as e:
        print(f"  ⚠ Failed to analyze {img_path}: {e}")
        return []


def rgb_to_hsv(r: int, g: int, b: int) -> tuple:
    """Convert RGB (0-255) to HSV (0-360, 0-1, 0-1)."""
    h, s, v = colorsys.rgb_to_hsv(r / 255.0, g / 255.0, b / 255.0)
    return h * 360, s, v


def hue_distance(h1: float, h2: float) -> float:
    """Circular distance between two hues (0-360)."""
    d = abs(h1 - h2) % 360
    return min(d, 360 - d)


def classify_image(img_path: str) -> str:
    """Determine which theme an image belongs to based on dominant colors."""
    colors = get_dominant_colors(img_path)
    if not colors:
        return "tokyonight"  # fallback

    total_pixels = sum(c[3] for c in colors)
    
    # Score each theme
    theme_scores = {name: 0.0 for name in THEMES}
    
    for r, g, b, count in colors:
        h, s, v = rgb_to_hsv(r, g, b)
        weight = count / total_pixels
        
        # Skip very dark or very desaturated pixels (backgrounds)
        if v < 0.12:
            continue
        if s < 0.05:
            continue
            
        # Boost weight for saturated, mid-brightness colors
        # (these are the "character" colors of a wallpaper)
        color_importance = s * min(v, 1.0 - v * 0.3) * weight
        
        for theme_name, (hue_center, hue_tol, sat_min) in THEMES.items():
            dist = hue_distance(h, hue_center)
            if dist <= hue_tol and s >= sat_min:
                # Closer to center = higher score
                proximity = 1.0 - (dist / hue_tol)
                theme_scores[theme_name] += proximity * color_importance * s
    
    # Pick the theme with highest score
    best = max(theme_scores, key=theme_scores.get)
    
    # If no theme scored above threshold, use a fallback based on
    # the single most dominant chromatic color
    if theme_scores[best] < 0.001:
        # Find the most saturated prominent color
        best_color = None
        best_sat = 0
        for r, g, b, count in colors:
            h, s, v = rgb_to_hsv(r, g, b)
            if s > best_sat and v > 0.1:
                best_sat = s
                best_color = (h, s, v)
        
        if best_color and best_sat > 0.05:
            h = best_color[0]
            min_dist = 999
            for theme_name, (hue_center, _, _) in THEMES.items():
                d = hue_distance(h, hue_center)
                if d < min_dist:
                    min_dist = d
                    best = theme_name
        else:
            # Truly achromatic/dark image → tokyonight (dark blue theme)
            best = "tokyonight"
    
    return best


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <source_directory>")
        sys.exit(1)
    
    source = Path(sys.argv[1]).expanduser().resolve()
    if not source.is_dir():
        print(f"Error: {source} is not a directory")
        sys.exit(1)

    # Collect image files
    images = sorted([
        f for f in source.rglob("*")
        if f.is_file() and f.suffix.lower() in IMAGE_EXTS
    ])
    
    print(f"Found {len(images)} images in {source}")
    print(f"Destination: {DEST_BASE}")
    print()

    # Clean existing wallpapers
    print("── Cleaning existing wallpapers ──")
    for theme_name in THEMES:
        theme_dir = DEST_BASE / theme_name
        if theme_dir.exists():
            count = len(list(theme_dir.iterdir()))
            if count > 0:
                for f in theme_dir.iterdir():
                    f.unlink()
                print(f"  🗑  {theme_name}: removed {count} files")
        theme_dir.mkdir(parents=True, exist_ok=True)
    print()

    # Sort images
    print("── Sorting wallpapers by color ──")
    counts = {name: 0 for name in THEMES}
    
    for i, img_path in enumerate(images, 1):
        theme = classify_image(str(img_path))
        dest_dir = DEST_BASE / theme
        dest_file = dest_dir / img_path.name
        
        shutil.copy2(str(img_path), str(dest_file))
        counts[theme] += 1
        
        print(f"  [{i:3d}/{len(images)}] {img_path.name:40s} → {theme}")
    
    # Summary
    print()
    print("── Summary ──")
    theme_icons = {
        "rosepine": "🔴", "kanagawa": "🟠", "gruvbox": "🟡",
        "everforest": "🟢", "nord": "🩵", "tokyonight": "🔵",
        "dracula": "🟣", "catppuccin": "🩷",
    }
    for theme_name in THEMES:
        icon = theme_icons.get(theme_name, "  ")
        print(f"  {icon} {theme_name:12s}: {counts[theme_name]:3d} wallpapers")
    print(f"\n  Total: {sum(counts.values())} wallpapers sorted")


if __name__ == "__main__":
    main()