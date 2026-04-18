//! Dynamic tiling layouts: Master/Stack, Monocle, and Grid.
//! Floating windows are excluded from tiling and mapped separately.

use smithay::reexports::wayland_server::Resource;

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

use smithay::{
    desktop::{layer_map_for_output, Space, Window},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
    wayland::compositor::with_states,
    wayland::shell::xdg::XdgToplevelSurfaceData,
};
use tracing::{debug, info, trace};

use crate::state::{animation_progress, animation_scale, State, Workspace};

// -------------------------------------------------------------------------
// LayoutType enum
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutType {
    MasterStack,
    Monocle,
    Grid,
}

impl Default for LayoutType {
    fn default() -> Self {
        Self::MasterStack
    }
}

impl LayoutType {
    pub fn cycle(&mut self) {
        *self = match self {
            Self::MasterStack => Self::Monocle,
            Self::Monocle => Self::Grid,
            Self::Grid => Self::MasterStack,
        };
    }
}

impl fmt::Display for LayoutType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MasterStack => write!(f, "Master/Stack"),
            Self::Monocle => write!(f, "Monocle"),
            Self::Grid => write!(f, "Grid"),
        }
    }
}

// -------------------------------------------------------------------------
// Layout dispatch
// -------------------------------------------------------------------------

impl State {
    pub fn recalculate_layout_for(
        ws: &mut Workspace,
        output: &Output,
        outer_gap: i32,
        inner_gap: i32,
        border_width: i32,
        focused: Option<&WlSurface>,
    ) {
        trace!(
            layout = %ws.layout,
            outer_gap,
            inner_gap,
            border_width,
            total = ws.windows.len(),
            floating = ws.floating.len(),
            split_ratio = ws.split_ratio,
            "layout: recalculating"
        );

        let Some(geo) = ws.space.output_geometry(output) else {
            return;
        };

        let non_exclusive = layer_map_for_output(output).non_exclusive_zone();

        debug!(
            output_geo = ?geo,
            non_exclusive = ?non_exclusive,
            layout = %ws.layout,
            "layout: geometry"
        );

        let origin = Point::<i32, Logical>::from((
            geo.loc.x + non_exclusive.loc.x + outer_gap,
            geo.loc.y + non_exclusive.loc.y + outer_gap,
        ));
        let screen_w = (non_exclusive.size.w - 2 * outer_gap).max(0);
        let screen_h = (non_exclusive.size.h - 2 * outer_gap).max(0);

        if screen_w <= 0 || screen_h <= 0 {
            return;
        }

        let tiled: Vec<Window> = ws.tiled_windows();

        // Ensure stack ratios are in sync with tiled window count.
        ws.ensure_stack_ratios();

        if !tiled.is_empty() {
            match ws.layout {
                LayoutType::MasterStack => {
                    layout_master_stack(
                        &mut ws.space,
                        &tiled,
                        &ws.spawn_times,
                        &mut ws.configured_sizes,
                        origin,
                        screen_w,
                        screen_h,
                        inner_gap,
                        border_width,
                        ws.split_ratio,
                        &ws.stack_ratios,
                    );
                }
                LayoutType::Monocle => {
                    layout_monocle(
                        &mut ws.space,
                        &tiled,
                        &ws.spawn_times,
                        &mut ws.configured_sizes,
                        origin,
                        screen_w,
                        screen_h,
                        border_width,
                        focused,
                    );
                }
                LayoutType::Grid => {
                    layout_grid(
                        &mut ws.space,
                        &tiled,
                        &ws.spawn_times,
                        &mut ws.configured_sizes,
                        origin,
                        screen_w,
                        screen_h,
                        inner_gap,
                        border_width,
                    );
                }
            }
        }

        // Map floating windows at their stored geometry, above the tiled layer.
        let floating_list: Vec<Window> = ws
            .windows
            .iter()
            .filter(|w| ws.floating.contains(w))
            .cloned()
            .collect();

        for window in &floating_list {
            if let Some(rect) = ws.floating_geo.get(window).copied() {
                let bw = border_width.max(0);
                let inner_w = (rect.size.w - 2 * bw).max(1);
                let inner_h = (rect.size.h - 2 * bw).max(1);

                let final_loc = Point::<i32, Logical>::from((
                    rect.loc.x + bw,
                    rect.loc.y + bw,
                ));
                let final_size = (inner_w, inner_h);

                if let Some(toplevel) = window.toplevel() {
                    let initial_sent = with_states(toplevel.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .map(|d| d.lock().unwrap().initial_configure_sent)
                            .unwrap_or(false)
                    });

                    toplevel.with_pending_state(|s| {
                        s.size = Some(final_size.into());
                    });

                    let last = ws.configured_sizes.get(window).copied();
                    if initial_sent && last != Some(final_size) {
                        debug!(
                            surface = ?toplevel.wl_surface().id(),
                            ?final_loc,
                            ?final_size,
                            "floating: configure (size changed)"
                        );
                        toplevel.send_configure();
                        ws.configured_sizes.insert(window.clone(), final_size);
                    }
                }
                ws.space.map_element(window.clone(), final_loc, false);
                ws.space.raise_element(window, false);
            }
        }
    }

    pub fn recalculate_layout(&mut self) {
        let output = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let focused = self.keyboard.current_focus();
        let ws = &mut self.workspaces[self.active_workspace];
        Self::recalculate_layout_for(ws, &output, outer, inner, border, focused.as_ref());
    }

    pub fn cycle_layout(&mut self) {
        let ws_idx = self.active_workspace;
        self.workspaces[ws_idx].layout.cycle();
        let layout = self.workspaces[ws_idx].layout;
        info!(
            workspace = ws_idx + 1,
            layout = %layout,
            "layout cycled"
        );
        self.recalculate_layout();
        self.needs_redraw = true;
    }
}

// -------------------------------------------------------------------------
// Master/Stack layout — uses split_ratio + stack_ratios
// -------------------------------------------------------------------------

fn layout_master_stack(
    space: &mut Space<Window>,
    tiled: &[Window],
    spawn_times: &HashMap<Window, Instant>,
    configured_sizes: &mut HashMap<Window, (i32, i32)>,
    origin: Point<i32, Logical>,
    screen_w: i32,
    screen_h: i32,
    inner_gap: i32,
    border_width: i32,
    split_ratio: f32,
    stack_ratios: &[f32],
) {
    let now = Instant::now();

    match tiled.len() {
        0 => {}
        1 => {
            let (loc, size) = animate_slot(
                &tiled[0],
                spawn_times,
                now,
                origin,
                (screen_w, screen_h),
            );
            debug!(idx = 0, ?loc, ?size, "layout[master/stack]: single window");
            place_tile(space, &tiled[0], loc, size, border_width, configured_sizes);
        }
        n => {
            let ratio = split_ratio.clamp(0.1, 0.9);
            let master_w = ((screen_w as f32 - inner_gap as f32) * ratio).round() as i32;
            let master_w = master_w.max(1);
            let stack_w = (screen_w - master_w - inner_gap).max(1);
            let stack_x = origin.x + master_w + inner_gap;
            let stack_count = (n - 1) as i32;

            debug!(
                master_w,
                stack_w,
                stack_count,
                screen_w,
                screen_h,
                split_ratio = ratio,
                "layout[master/stack]: params"
            );

            // Master window.
            let (m_loc, m_size) = animate_slot(
                &tiled[0],
                spawn_times,
                now,
                origin,
                (master_w, screen_h),
            );
            debug!(idx = 0, ?m_loc, ?m_size, "layout[master/stack]: master slot");
            place_tile(space, &tiled[0], m_loc, m_size, border_width, configured_sizes);

            // Stack windows — use stack_ratios for vertical distribution.
            let total_inner = inner_gap * (stack_count - 1).max(0);
            let usable_h = (screen_h - total_inner).max(0);

            // Compute per-window heights from ratios.
            let heights: Vec<i32> = if stack_ratios.len() == stack_count as usize {
                let mut h_list: Vec<i32> = stack_ratios
                    .iter()
                    .map(|r| (usable_h as f32 * r).round() as i32)
                    .collect();
                // Fix rounding: adjust last window to absorb remainder.
                let sum: i32 = h_list.iter().sum();
                if let Some(last) = h_list.last_mut() {
                    *last += usable_h - sum;
                }
                h_list
            } else {
                // Equal distribution fallback.
                let slice_h = usable_h / stack_count.max(1);
                let mut h_list: Vec<i32> = (0..stack_count).map(|_| slice_h).collect();
                let sum: i32 = h_list.iter().sum();
                if let Some(last) = h_list.last_mut() {
                    *last += usable_h - sum;
                }
                h_list
            };

            let mut y = origin.y;
            for (i, window) in tiled.iter().skip(1).enumerate() {
                let h = heights[i].max(1);
                let (loc, size) = animate_slot(
                    window,
                    spawn_times,
                    now,
                    (stack_x, y).into(),
                    (stack_w, h),
                );
                debug!(idx = i + 1, ?loc, ?size, "layout[master/stack]: stack slot");
                place_tile(space, window, loc, size, border_width, configured_sizes);
                y += h + inner_gap;
            }
        }
    }
}

// -------------------------------------------------------------------------
// Monocle layout
// -------------------------------------------------------------------------

fn layout_monocle(
    space: &mut Space<Window>,
    tiled: &[Window],
    spawn_times: &HashMap<Window, Instant>,
    configured_sizes: &mut HashMap<Window, (i32, i32)>,
    origin: Point<i32, Logical>,
    screen_w: i32,
    screen_h: i32,
    border_width: i32,
    focused: Option<&WlSurface>,
) {
    let now = Instant::now();

    debug!(
        count = tiled.len(),
        focused = ?focused.map(|s| s.id()),
        "layout[monocle]: tiling all windows to full area"
    );

    for (i, window) in tiled.iter().enumerate() {
        let (loc, size) = animate_slot(
            window,
            spawn_times,
            now,
            origin,
            (screen_w, screen_h),
        );
        debug!(idx = i, ?loc, ?size, "layout[monocle]: slot");
        place_tile(space, window, loc, size, border_width, configured_sizes);
    }

    let top = focused
        .and_then(|surf| {
            tiled.iter().find(|w| {
                w.toplevel()
                    .map(|t| t.wl_surface() == surf)
                    .unwrap_or(false)
            })
        })
        .or(tiled.last())
        .cloned();

    if let Some(ref w) = top {
        debug!(
            surface = ?w.toplevel().map(|t| t.wl_surface().id()),
            "layout[monocle]: raising focused window"
        );
        space.raise_element(w, true);
    }
}

// -------------------------------------------------------------------------
// Grid layout
// -------------------------------------------------------------------------

fn layout_grid(
    space: &mut Space<Window>,
    tiled: &[Window],
    spawn_times: &HashMap<Window, Instant>,
    configured_sizes: &mut HashMap<Window, (i32, i32)>,
    origin: Point<i32, Logical>,
    screen_w: i32,
    screen_h: i32,
    inner_gap: i32,
    border_width: i32,
) {
    let n = tiled.len();
    let now = Instant::now();

    let cols = (n as f64).sqrt().ceil() as i32;
    let rows = ((n as f64) / (cols as f64)).ceil() as i32;

    debug!(n, cols, rows, "layout[grid]: grid dimensions");

    let total_gap_x = inner_gap * (cols - 1).max(0);
    let total_gap_y = inner_gap * (rows - 1).max(0);

    let cell_w = ((screen_w - total_gap_x) / cols).max(1);
    let cell_h = ((screen_h - total_gap_y) / rows).max(1);

    for (i, window) in tiled.iter().enumerate() {
        let col = (i as i32) % cols;
        let row = (i as i32) / cols;

        let is_last_col = col == cols - 1;
        let is_last_row = row == rows - 1;

        let x = origin.x + col * (cell_w + inner_gap);
        let y = origin.y + row * (cell_h + inner_gap);

        let w = if is_last_col {
            (origin.x + screen_w - x).max(1)
        } else {
            cell_w
        };

        let h = if is_last_row {
            (origin.y + screen_h - y).max(1)
        } else {
            cell_h
        };

        let (loc, size) = animate_slot(
            window,
            spawn_times,
            now,
            (x, y).into(),
            (w, h),
        );
        debug!(idx = i, col, row, ?loc, ?size, "layout[grid]: slot");
        place_tile(space, window, loc, size, border_width, configured_sizes);
    }
}

// -------------------------------------------------------------------------
// Animation helpers
// -------------------------------------------------------------------------

fn animate_slot(
    window: &Window,
    spawn_times: &HashMap<Window, Instant>,
    now: Instant,
    loc: Point<i32, Logical>,
    size: (i32, i32),
) -> (Point<i32, Logical>, (i32, i32)) {
    let Some(spawn) = spawn_times.get(window) else {
        return (loc, size);
    };
    let Some(progress) = animation_progress(now, *spawn) else {
        return (loc, size);
    };
    let scale = animation_scale(progress);
    let visual_w = ((size.0 as f32) * scale).round() as i32;
    let visual_h = ((size.1 as f32) * scale).round() as i32;
    let dx = (size.0 - visual_w) / 2;
    let dy = (size.1 - visual_h) / 2;
    let new_loc = Point::<i32, Logical>::from((loc.x + dx, loc.y + dy));
    (new_loc, size)
}

fn place_tile(
    space: &mut Space<Window>,
    window: &Window,
    location: Point<i32, Logical>,
    size: (i32, i32),
    border_width: i32,
    configured_sizes: &mut HashMap<Window, (i32, i32)>,
) {
    let bw = border_width.max(0);
    let inner_w = size.0 - 2 * bw;
    let inner_h = size.1 - 2 * bw;

    let (final_loc, final_size) = if inner_w > 0 && inner_h > 0 {
        (
            Point::<i32, Logical>::from((location.x + bw, location.y + bw)),
            (inner_w, inner_h),
        )
    } else {
        (location, size)
    };

    if let Some(toplevel) = window.toplevel() {
        let initial_sent = with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().unwrap().initial_configure_sent)
                .unwrap_or(false)
        });

        toplevel.with_pending_state(|s| {
            s.size = Some(final_size.into());
        });

        let last = configured_sizes.get(window).copied();
        let size_changed = last != Some(final_size);

        if initial_sent && size_changed {
            debug!(
                surface = ?toplevel.wl_surface().id(),
                ?final_loc,
                ?final_size,
                initial_sent,
                "place_tile: configure (size changed)"
            );
            toplevel.send_configure();
            configured_sizes.insert(window.clone(), final_size);
        } else {
            trace!(
                surface = ?toplevel.wl_surface().id(),
                ?final_loc,
                ?final_size,
                "place_tile: position only (size unchanged)"
            );
        }
    }
    space.map_element(window.clone(), final_loc, false);
}