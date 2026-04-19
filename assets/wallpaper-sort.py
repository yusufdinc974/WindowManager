#!/usr/bin/env python3
"""
mywm — Automatic Wallpaper Color Classifier

Usage: python3 wallpaper-sort.py <source_directory>

Analyzes dominant colors and sorts wallpapers into theme directories.
Requires: python3-pillow (PIL)
"""

import sys
import os
import shutil
import colorsys
import random
from pathlib import Path
from collections import Counter

try:
    from PIL import Image
except ImportError:
    print("ERROR: python3-pillow is required.")
    print("  sudo apt install python3-pillow")
    sys.exit(1)

# ── Theme definitions ──
# Each theme: (name, accent_hue_0_360, hue_tolerance, min_saturation_0_1, bg_lightness_0_1)
THEMES = [
    # name           hue   tol   min_sat  bg_light
    ("tokyonight",   225,  30,   0.20,    0.12),
    ("gruvbox",       45,  25,   0.30,    0.16),
    ("everforest",   100,  35,   0.20,    0.17),
    ("rosepine",     345,  30,   0.30,    0.10),
    ("kanagawa",      25,  20,   0.40,    0.13),
    ("catppuccin",   310,  35,   0.20,    0.14),
    ("dracula",      265,  25,   0.30,    0.17),
    ("nord",         193,  25,   0.25,    0.20),
]

WALLPAPER_BASE = Path.home() / ".config" / "mywm" / "wallpapers"
IMAGE_EXTENSIONS = {".png", ".jpg", ".jpeg", ".webp", ".bmp"}


def ensure_dirs():
    """Create all theme wallpaper directories."""
    for name, *_ in THEMES:
        (WALLPAPER_BASE / name).mkdir(parents=True, exist_ok=True)


def get_dominant_colors(img_path, num_colors=8, resize=80):
    """Extract dominant colors from an image as list of (h, s, l) tuples."""
    try:
        img = Image.open(img_path).convert("RGB")
        img = img.resize((resize, resize), Image.LANCZOS)
    except Exception as e:
        print(f"  WARNING: cannot open {img_path}: {e}")
        return []

    pixels = list(img.getdata())

    # Quantize: count similar colors
    # Round RGB to reduce unique colors
    quantized = []
    for r, g, b in pixels:
        qr = (r // 16) * 16
        qg = (g // 16) * 16
        qb = (b // 16) * 16
        quantized.append((qr, qg, qb))

    counter = Counter(quantized)
    top_colors = counter.most_common(num_colors)

    hsl_colors = []
    for (r, g, b), count in top_colors:
        h, l, s = colorsys.rgb_to_hls(r / 255.0, g / 255.0, b / 255.0)
        hsl_colors.append((h * 360, s, l, count))

    return hsl_colors


def hue_distance(h1, h2):
    """Circular hue distance (0-180)."""
    d = abs(h1 - h2) % 360
    return d if d <= 180 else 360 - d


def score_image_for_theme(colors, theme):
    """Score how well image colors match a theme. Higher = better."""
    name, target_hue, hue_tol, min_sat, target_lum = theme

    if not colors:
        return 0

    total_score = 0
    total_weight = 0

    for hue, sat, lum, count in colors:
        weight = count  # More common colors matter more

        # Hue match (0-100)
        hdist = hue_distance(hue, target_hue)
        if hdist <= hue_tol:
            hue_score = 100 * (1.0 - hdist / max(hue_tol, 1))
        elif hdist <= hue_tol * 2:
            hue_score = 30 * (1.0 - (hdist - hue_tol) / max(hue_tol, 1))
        else:
            hue_score = 0

        # Saturation bonus (saturated colors are more meaningful)
        sat_score = 0
        if sat >= min_sat:
            sat_score = min(30, sat * 30)
        elif sat < 0.08:
            # Very desaturated (gray/black/white) — neutral, slight penalty
            sat_score = -5

        # Luminance match (dark themes prefer dark images)
        lum_diff = abs(lum - target_lum)
        if lum_diff < 0.25:
            lum_score = 20 * (1.0 - lum_diff / 0.25)
        else:
            lum_score = 0

        total_score += (hue_score + sat_score + lum_score) * weight
        total_weight += weight

    if total_weight > 0:
        return total_score / total_weight
    return 0


def classify_image(img_path):
    """Classify a single image. Returns (best_theme_name, score)."""
    colors = get_dominant_colors(img_path)

    best_theme = None
    best_score = 0

    for theme in THEMES:
        score = score_image_for_theme(colors, theme)
        if score > best_score:
            best_score = score
            best_theme = theme[0]

    return best_theme, best_score


def main():
    if len(sys.argv) < 2:
        print("Usage: python3 wallpaper-sort.py <source_directory>")
        print("  Scans images and copies them to themed wallpaper directories.")
        sys.exit(1)

    source_dir = Path(sys.argv[1])
    if not source_dir.is_dir():
        print(f"ERROR: not a directory: {source_dir}")
        sys.exit(1)

    ensure_dirs()

    print("╔══════════════════════════════════════════════════════════╗")
    print("║  mywm Wallpaper Classifier (Python)                     ║")
    print(f"║  Scanning: {source_dir}")
    print("╚══════════════════════════════════════════════════════════╝")
    print()

    images = sorted(
        p for p in source_dir.iterdir()
        if p.is_file() and p.suffix.lower() in IMAGE_EXTENSIONS
    )

    if not images:
        print("No images found in source directory.")
        sys.exit(0)

    classified = 0
    sent_to_all = 0
    theme_counts = {t[0]: 0 for t in THEMES}

    for img_path in images:
        best_theme, score = classify_image(img_path)
        filename = img_path.name

        if best_theme and score > 15:
            dest = WALLPAPER_BASE / best_theme / filename
            shutil.copy2(img_path, dest)
            theme_counts[best_theme] += 1
            classified += 1
            print(f"  {filename:<45s} → {best_theme:<15s} (score: {score:.1f})")
        else:
            # Low confidence — copy to all themes
            for t_name, *_ in THEMES:
                dest = WALLPAPER_BASE / t_name / filename
                shutil.copy2(img_path, dest)
                theme_counts[t_name] += 1
            sent_to_all += 1
            print(f"  {filename:<45s} → ALL themes      (score: {score:.1f})")

    print()
    print(f"Done! Classified: {classified} | Sent to all: {sent_to_all}")
    print()
    print("Wallpaper counts per theme (including previously existing):")
    for name, *_ in THEMES:
        d = WALLPAPER_BASE / name
        total = len([f for f in d.iterdir() if f.is_file()]) if d.exists() else 0
        print(f"  {name:<15s} {total} wallpapers")


if __name__ == "__main__":
    main()