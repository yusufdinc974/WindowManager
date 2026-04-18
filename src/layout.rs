//! Master/stack tiling layout with outer + inner gaps.
//!
//! Extracted from `main.rs` during Phase 10. The math is untouched — only
//! its home has moved. We keep `recalculate_layout_for` / `recalculate_layout`
//! as inherent `impl State` methods so every existing
//! `Self::recalculate_layout_for(...)` call site in `main.rs` keeps working.

use smithay::{
    desktop::{layer_map_for_output, Space, Window},
    output::Output,
    utils::{Logical, Point},
};

use crate::state::{State, Workspace};

impl State {
    /// Static helper — operates on a single workspace so we can call it
    /// without needing `&mut self` (avoids borrow-checker gymnastics).
    pub fn recalculate_layout_for(ws: &mut Workspace, output: &Output) {
        let outer_gap: i32 = 15;
        let inner_gap: i32 = 10;

        let Some(geo) = ws.space.output_geometry(output) else {
            return;
        };

        // Layer-shell surfaces that request an exclusive zone (e.g. a
        // 30px top bar from waybar) reserve space on the edges of the
        // output. Tile only inside whatever's left.
        let non_exclusive = layer_map_for_output(output).non_exclusive_zone();

        // Shrink the usable rectangle by `outer_gap` on all four sides so
        // the outermost tiles don't hug the screen edges (or the bar).
        let origin = Point::<i32, Logical>::from((
            geo.loc.x + non_exclusive.loc.x + outer_gap,
            geo.loc.y + non_exclusive.loc.y + outer_gap,
        ));
        let screen_w = (non_exclusive.size.w - 2 * outer_gap).max(0);
        let screen_h = (non_exclusive.size.h - 2 * outer_gap).max(0);

        if screen_w <= 0 || screen_h <= 0 {
            return;
        }

        match ws.windows.len() {
            0 => {}
            1 => {
                // Single window: fills the whole inner rectangle (only the
                // outer gap applies — there's nothing to put inner gaps between).
                place_tile(
                    &mut ws.space,
                    &ws.windows[0],
                    origin,
                    (screen_w, screen_h),
                );
            }
            n => {
                // Column split: master on the left, stack on the right,
                // with `inner_gap` between the two columns.
                let half = screen_w / 2;
                let master_w = (half - inner_gap / 2).max(1);
                let stack_w = (screen_w - master_w - inner_gap).max(1);
                let stack_x = origin.x + master_w + inner_gap;

                let stack_count = (n - 1) as i32;

                // Master tile spans the full inner height.
                place_tile(
                    &mut ws.space,
                    &ws.windows[0],
                    origin,
                    (master_w, screen_h),
                );

                // Stack: divide the column's height into `stack_count`
                // slots, each separated by `inner_gap`.
                let total_inner = inner_gap * (stack_count - 1).max(0);
                let usable_h = (screen_h - total_inner).max(0);
                let slice_h = usable_h / stack_count.max(1);

                for (i, window) in ws.windows.iter().skip(1).enumerate() {
                    let i = i as i32;
                    let y = origin.y + i * (slice_h + inner_gap);
                    // Last tile absorbs integer-division remainder so it
                    // reaches exactly the bottom of the usable region.
                    let h = if i == stack_count - 1 {
                        (origin.y + screen_h - y).max(1)
                    } else {
                        slice_h.max(1)
                    };
                    place_tile(
                        &mut ws.space,
                        window,
                        (stack_x, y).into(),
                        (stack_w, h),
                    );
                }
            }
        }
    }

    /// Convenience: recalculate the active workspace.
    pub fn recalculate_layout(&mut self) {
        let output = self.output.clone();
        let ws = &mut self.workspaces[self.active_workspace];
        Self::recalculate_layout_for(ws, &output);
    }
}

fn place_tile(
    space: &mut Space<Window>,
    window: &Window,
    location: Point<i32, Logical>,
    size: (i32, i32),
) {
    if let Some(toplevel) = window.toplevel() {
        toplevel.with_pending_state(|s| {
            s.size = Some(size.into());
        });
        toplevel.send_configure();
    }
    space.map_element(window.clone(), location, false);
}
