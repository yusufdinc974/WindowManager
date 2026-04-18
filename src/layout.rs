//! Dynamic tiling layouts: Master/Stack, Monocle, and Grid.

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
    /// Recalculate the tiling layout for a single workspace.
    ///
    /// `focused` is the `WlSurface` that currently holds keyboard focus.
    /// The Monocle layout uses it to decide which window to raise on top;
    /// other layouts ignore it.
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
            tiles = ws.windows.len(),
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

        if ws.windows.is_empty() {
            return;
        }

        match ws.layout {
            LayoutType::MasterStack => {
                layout_master_stack(ws, origin, screen_w, screen_h, inner_gap, border_width);
            }
            LayoutType::Monocle => {
                layout_monocle(ws, origin, screen_w, screen_h, border_width, focused);
            }
            LayoutType::Grid => {
                layout_grid(ws, origin, screen_w, screen_h, inner_gap, border_width);
            }
        }
    }

    /// Convenience wrapper: retile the active workspace, automatically
    /// pulling the current keyboard focus.
    pub fn recalculate_layout(&mut self) {
        let output = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let focused = self.keyboard.current_focus();
        let ws = &mut self.workspaces[self.active_workspace];
        Self::recalculate_layout_for(ws, &output, outer, inner, border, focused.as_ref());
    }

    /// Cycle the active workspace's layout and immediately retile.
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
// Master/Stack layout
// -------------------------------------------------------------------------

fn layout_master_stack(
    ws: &mut Workspace,
    origin: Point<i32, Logical>,
    screen_w: i32,
    screen_h: i32,
    inner_gap: i32,
    border_width: i32,
) {
    let now = Instant::now();

    match ws.windows.len() {
        0 => unreachable!(),
        1 => {
            let (loc, size) = animate_slot(
                &ws.windows[0],
                &ws.spawn_times,
                now,
                origin,
                (screen_w, screen_h),
            );
            debug!(idx = 0, ?loc, ?size, "layout[master/stack]: single window");
            place_tile(
                &mut ws.space,
                &ws.windows[0],
                loc,
                size,
                border_width,
                &mut ws.configured_sizes,
            );
        }
        n => {
            let half = screen_w / 2;
            let master_w = (half - inner_gap / 2).max(1);
            let stack_w = (screen_w - master_w - inner_gap).max(1);
            let stack_x = origin.x + master_w + inner_gap;
            let stack_count = (n - 1) as i32;

            debug!(
                master_w,
                stack_w,
                stack_count,
                screen_w,
                screen_h,
                "layout[master/stack]: params"
            );

            let (m_loc, m_size) = animate_slot(
                &ws.windows[0],
                &ws.spawn_times,
                now,
                origin,
                (master_w, screen_h),
            );
            debug!(idx = 0, ?m_loc, ?m_size, "layout[master/stack]: master slot");
            place_tile(
                &mut ws.space,
                &ws.windows[0],
                m_loc,
                m_size,
                border_width,
                &mut ws.configured_sizes,
            );

            let total_inner = inner_gap * (stack_count - 1).max(0);
            let usable_h = (screen_h - total_inner).max(0);
            let slice_h = usable_h / stack_count.max(1);

            for (i, window) in ws.windows.iter().skip(1).enumerate() {
                let i = i as i32;
                let y = origin.y + i * (slice_h + inner_gap);
                let h = if i == stack_count - 1 {
                    (origin.y + screen_h - y).max(1)
                } else {
                    slice_h.max(1)
                };
                let (loc, size) = animate_slot(
                    window,
                    &ws.spawn_times,
                    now,
                    (stack_x, y).into(),
                    (stack_w, h),
                );
                debug!(idx = i + 1, ?loc, ?size, "layout[master/stack]: stack slot");
                place_tile(
                    &mut ws.space,
                    window,
                    loc,
                    size,
                    border_width,
                    &mut ws.configured_sizes,
                );
            }
        }
    }
}

// -------------------------------------------------------------------------
// Monocle layout — every window occupies the full area; the *focused*
// window is raised to the top of the Z-stack.
// -------------------------------------------------------------------------

fn layout_monocle(
    ws: &mut Workspace,
    origin: Point<i32, Logical>,
    screen_w: i32,
    screen_h: i32,
    border_width: i32,
    focused: Option<&WlSurface>,
) {
    let now = Instant::now();

    debug!(
        count = ws.windows.len(),
        focused = ?focused.map(|s| s.id()),
        "layout[monocle]: tiling all windows to full area"
    );

    for (i, window) in ws.windows.iter().enumerate() {
        let (loc, size) = animate_slot(
            window,
            &ws.spawn_times,
            now,
            origin,
            (screen_w, screen_h),
        );
        debug!(idx = i, ?loc, ?size, "layout[monocle]: slot");
        place_tile(
            &mut ws.space,
            window,
            loc,
            size,
            border_width,
            &mut ws.configured_sizes,
        );
    }

    // Determine which window to raise.
    //
    //  1. Prefer the window whose toplevel surface matches `focused`.
    //  2. Fall back to the last window in the list (i.e. the most
    //     recently focused or spawned window).
    //
    // This guarantees that the visible window in Monocle mode is always
    // the one the user has actually focused, not just the most recently
    // spawned one.
    let top = focused
        .and_then(|surf| {
            ws.windows.iter().find(|w| {
                w.toplevel()
                    .map(|t| t.wl_surface() == surf)
                    .unwrap_or(false)
            })
        })
        .or(ws.windows.last())
        .cloned();

    if let Some(ref w) = top {
        debug!(
            surface = ?w.toplevel().map(|t| t.wl_surface().id()),
            "layout[monocle]: raising focused window"
        );
        ws.space.raise_element(w, true);
    }
}

// -------------------------------------------------------------------------
// Grid layout
// -------------------------------------------------------------------------

fn layout_grid(
    ws: &mut Workspace,
    origin: Point<i32, Logical>,
    screen_w: i32,
    screen_h: i32,
    inner_gap: i32,
    border_width: i32,
) {
    let n = ws.windows.len();
    let now = Instant::now();

    let cols = (n as f64).sqrt().ceil() as i32;
    let rows = ((n as f64) / (cols as f64)).ceil() as i32;

    debug!(n, cols, rows, "layout[grid]: grid dimensions");

    let total_gap_x = inner_gap * (cols - 1).max(0);
    let total_gap_y = inner_gap * (rows - 1).max(0);

    let cell_w = ((screen_w - total_gap_x) / cols).max(1);
    let cell_h = ((screen_h - total_gap_y) / rows).max(1);

    for (i, window) in ws.windows.iter().enumerate() {
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
            &ws.spawn_times,
            now,
            (x, y).into(),
            (w, h),
        );
        debug!(idx = i, col, row, ?loc, ?size, "layout[grid]: slot");
        place_tile(
            &mut ws.space,
            window,
            loc,
            size,
            border_width,
            &mut ws.configured_sizes,
        );
    }
}

// -------------------------------------------------------------------------
// Animation helpers (unchanged)
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