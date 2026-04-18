//! Master/stack tiling layout with outer + inner gaps.

use smithay::reexports::wayland_server::Resource;

use std::collections::HashMap;
use std::time::Instant;

use smithay::{
    desktop::{layer_map_for_output, Space, Window},
    output::Output,
    utils::{Logical, Point},
    wayland::shell::xdg::XdgToplevelSurfaceData,
    wayland::compositor::with_states,
};
use tracing::{debug, trace};

use crate::state::{animation_progress, animation_scale, State, Workspace};

impl State {
    pub fn recalculate_layout_for(
        ws: &mut Workspace,
        output: &Output,
        outer_gap: i32,
        inner_gap: i32,
        border_width: i32,
    ) {
        trace!(
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

        let now = Instant::now();

        match ws.windows.len() {
            0 => {}
            1 => {
                let (loc, size) = animate_slot(
                    &ws.windows[0],
                    &ws.spawn_times,
                    now,
                    origin,
                    (screen_w, screen_h),
                );
                debug!(idx = 0, ?loc, ?size, "layout: single window slot");
                place_tile(&mut ws.space, &ws.windows[0], loc, size, border_width, &mut ws.configured_sizes);
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
                    "layout: master/stack params"
                );

                let (m_loc, m_size) = animate_slot(
                    &ws.windows[0],
                    &ws.spawn_times,
                    now,
                    origin,
                    (master_w, screen_h),
                );
                debug!(idx = 0, ?m_loc, ?m_size, "layout: master slot");
                place_tile(&mut ws.space, &ws.windows[0], m_loc, m_size, border_width, &mut ws.configured_sizes);

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
                    debug!(idx = i + 1, ?loc, ?size, "layout: stack slot");
                    place_tile(&mut ws.space, window, loc, size, border_width, &mut ws.configured_sizes);
                }
            }
        }
    }

    pub fn recalculate_layout(&mut self) {
        let output = self.output.clone();
        let outer = self.config.outer_gaps;
        let inner = self.config.inner_gaps;
        let border = self.config.border_width;
        let ws = &mut self.workspaces[self.active_workspace];
        Self::recalculate_layout_for(ws, &output, outer, inner, border);
    }
}

/// Compute the animated position for a window. The animation only
/// affects the **position** (a centering offset that converges to zero),
/// NOT the size. This avoids flooding clients with configure events
/// during the 200 ms spawn-in animation.
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
    // Compute a visual shrink factor for the position offset only.
    // The window is configured at its full target size; the offset
    // slides it toward its final position as the animation completes.
    let visual_w = ((size.0 as f32) * scale).round() as i32;
    let visual_h = ((size.1 as f32) * scale).round() as i32;
    let dx = (size.0 - visual_w) / 2;
    let dy = (size.1 - visual_h) / 2;
    let new_loc = Point::<i32, Logical>::from((loc.x + dx, loc.y + dy));
    // Return the TARGET size — no size animation.
    (new_loc, size)
}

/// Map a window into the space and send a configure **only** if the
/// size actually changed since the last configure we sent.
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

        // Only send configure if the size actually changed from what
        // we last configured. This is critical: without this guard the
        // animation loop sends 12+ configures in 200 ms, overwhelming
        // newly-connected clients and killing their socket.
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