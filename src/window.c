// window.c
#include <stdlib.h>
#include <string.h>
#include <wayland-server-core.h>
#include <wlr/types/wlr_scene.h>
#include <wlr/types/wlr_xdg_shell.h>
#include <wlr/util/log.h>

#include "window.h"
#include "server.h"
#include "bsp.h"
#include "output.h" // Include output.h for struct output

// Forward declarations for event handlers
static void handle_xdg_toplevel_destroy(struct wl_listener *listener, void *data);
static void handle_xdg_toplevel_map(struct wl_listener *listener, void *data);
static void handle_xdg_toplevel_unmap(struct wl_listener *listener, void *data);
static void handle_xdg_toplevel_commit(struct wl_listener *listener, void *data);
static void handle_xdg_toplevel_request_move(struct wl_listener *listener, void *data);
static void handle_xdg_toplevel_request_resize(struct wl_listener *listener, void *data);
static void handle_xdg_toplevel_request_maximize(struct wl_listener *listener, void *data);
static void handle_xdg_toplevel_request_fullscreen(struct wl_listener *listener, void *data);

struct window *window_create_xdg(struct server *server, struct wlr_xdg_toplevel *xdg_toplevel) {
    struct window *window = calloc(1, sizeof(struct window));
    if (!window) {
        wlr_log(WLR_ERROR, "Failed to allocate window");
        return NULL;
    }
    
    window->type = WINDOW_XDG_TOPLEVEL;
    window->server = server;
    window->xdg_toplevel = xdg_toplevel;
    window->floating = false;
    window->decorated = true;
    
    // Initialize position and size to 0
    window->x = window->y = 0;
    window->width = window->height = 0;
    
    window->scene_tree = wlr_scene_tree_create(&server->scene->tree);
    // In wlroots 0.18, this returns a wlr_scene_tree, not a node directly
    struct wlr_scene_tree *xdg_tree = wlr_scene_xdg_surface_create(
        window->scene_tree, xdg_toplevel->base);
    // Get the scene node from the tree
    window->scene_surface = &xdg_tree->node;
    
    // Set up listeners for xdg_toplevel events
    window->destroy.notify = handle_xdg_toplevel_destroy;
    wl_signal_add(&xdg_toplevel->base->events.destroy, &window->destroy);
    
    window->map.notify = handle_xdg_toplevel_map;
    wl_signal_add(&xdg_toplevel->base->surface->events.map, &window->map);
    
    window->unmap.notify = handle_xdg_toplevel_unmap;
    wl_signal_add(&xdg_toplevel->base->surface->events.unmap, &window->unmap);
    
    window->commit.notify = handle_xdg_toplevel_commit;
    wl_signal_add(&xdg_toplevel->base->surface->events.commit, &window->commit);
    
    window->request_move.notify = handle_xdg_toplevel_request_move;
    wl_signal_add(&xdg_toplevel->events.request_move, &window->request_move);
    
    window->request_resize.notify = handle_xdg_toplevel_request_resize;
    wl_signal_add(&xdg_toplevel->events.request_resize, &window->request_resize);
    
    window->request_maximize.notify = handle_xdg_toplevel_request_maximize;
    wl_signal_add(&xdg_toplevel->events.request_maximize, &window->request_maximize);
    
    window->request_fullscreen.notify = handle_xdg_toplevel_request_fullscreen;
    wl_signal_add(&xdg_toplevel->events.request_fullscreen, &window->request_fullscreen);
    
    // Store window data in the surface
    xdg_toplevel->base->data = window;
    
    // Add to the server's list of windows (positioned at the end of the list)
    wl_list_insert(&server->windows, &window->link);
    
    return window;
}

void window_destroy(struct window *window) {
    // Remove from window list
    wl_list_remove(&window->link);
    
    // Remove from BSP tree if tiled
    if (window->node) {
        bsp_remove_node(window->node);
    }
    
    // Remove listeners
    wl_list_remove(&window->destroy.link);
    wl_list_remove(&window->map.link);
    wl_list_remove(&window->unmap.link);
    wl_list_remove(&window->commit.link);
    wl_list_remove(&window->request_move.link);
    wl_list_remove(&window->request_resize.link);
    wl_list_remove(&window->request_maximize.link);
    wl_list_remove(&window->request_fullscreen.link);
    
    // The scene_tree will be destroyed by wlroots
    
    free(window);
}

void window_focus(struct window *window) {
    struct server *server = window->server;
    struct wlr_seat *seat = server->seat;
    struct wlr_surface *prev_surface = seat->keyboard_state.focused_surface;
    
    if (prev_surface == window->xdg_toplevel->base->surface) {
        // Already focused
        return;
    }
    
    // Deactivate previously focused window
    if (prev_surface) {
        struct wlr_xdg_surface *previous = wlr_xdg_surface_try_from_wlr_surface(prev_surface);
        if (previous && previous->role == WLR_XDG_SURFACE_ROLE_TOPLEVEL) {
            wlr_xdg_toplevel_set_activated(previous->toplevel, false);
        }
    }
    
    // Move window to the top of the scene graph for proper stacking
    wlr_scene_node_raise_to_top(window->scene_surface);
    
    // Activate new window
    wlr_xdg_toplevel_set_activated(window->xdg_toplevel, true);
    
    // Focus the window's surface
    struct wlr_keyboard *keyboard = wlr_seat_get_keyboard(seat);
    if (keyboard) {
        wlr_seat_keyboard_notify_enter(seat, window->xdg_toplevel->base->surface,
            keyboard->keycodes, keyboard->num_keycodes, &keyboard->modifiers);
    }
    
    // Update server's focused window pointer
    server->focused_window = window;
}

void window_move(struct window *window, int x, int y) {
    window->x = x;
    window->y = y;
    
    // Update the position of the window in the scene graph
    wlr_scene_node_set_position(window->scene_surface, x, y);
}

void window_resize(struct window *window, int width, int height) {
    window->width = width;
    window->height = height;
    
    // Only actually resize if the window is mapped
    if (window->xdg_toplevel->base->surface->mapped) {
        wlr_xdg_toplevel_set_size(window->xdg_toplevel, width, height);
    }
}

void window_set_tiled(struct window *window, struct bsp_node *node) {
    // Remove from previous node if any
    if (window->node) {
        bsp_remove_node(window->node);
    }
    
    // Set new node
    window->node = node;
    node->window = window;
    window->floating = false;
    
    // Apply the node's position and size to the window
    window_move(window, node->x, node->y);
    window_resize(window, node->width, node->height);
    
    // Send tiled state to the client
    wlr_xdg_toplevel_set_tiled(window->xdg_toplevel, 
        WLR_EDGE_TOP | WLR_EDGE_BOTTOM | WLR_EDGE_LEFT | WLR_EDGE_RIGHT);
}

void window_set_floating(struct window *window) {
    // Remove from BSP tree if tiled
    if (window->node) {
        bsp_remove_node(window->node);
        window->node = NULL;
    }
    
    window->floating = true;
    
    // Send un-tiled state to the client
    wlr_xdg_toplevel_set_tiled(window->xdg_toplevel, 0);
    
    // Get window's desired size
    struct wlr_box box;
    wlr_xdg_surface_get_geometry(window->xdg_toplevel->base, &box);
    
    // Center the window on the current output
    // This is a simplified version - you may want to improve this
    struct wlr_output *output = NULL;
    if (!wl_list_empty(&window->server->outputs)) {
        // Get the first output
        struct output *first_output = NULL;
        first_output = wl_container_of(window->server->outputs.next, first_output, link);
        if (first_output) {
            output = first_output->wlr_output;
        }
    }
    
    if (output) {
        int output_width = output->width;
        int output_height = output->height;
        int x = (output_width - box.width) / 2;
        int y = (output_height - box.height) / 2;
        window_move(window, x, y);
    }
    
    window_resize(window, box.width, box.height);
}

/* Event handlers */

static void handle_xdg_toplevel_destroy(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, destroy);
    wlr_log(WLR_DEBUG, "Window destroyed");
    window_destroy(window);
}

static void handle_xdg_toplevel_map(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, map);
    struct server *server = window->server;
    
    wlr_log(WLR_DEBUG, "Window mapped: %s", 
        window->xdg_toplevel->title ? window->xdg_toplevel->title : "(unnamed)");
    
    // Get initial size
    struct wlr_box box;
    wlr_xdg_surface_get_geometry(window->xdg_toplevel->base, &box);
    window->width = box.width;
    window->height = box.height;
    
    // Add window to the BSP tree (for tiled windows)
    if (!window->floating) {
        // Find an empty node in the BSP tree
        struct bsp_node *root = server->active_workspace->root;
        struct bsp_node *target = bsp_find_node_at(root, 
            root->x + root->width / 2, 
            root->y + root->height / 2);
            
        if (target && !target->window) {
            // Found an empty node, assign the window directly
            window->node = target;
            target->window = window;
        } else {
            // No empty node found, create one by splitting
            if (!root->window && !root->left_child && !root->right_child) {
                // Root is empty, use it directly
                root->window = window;
                window->node = root;
            } else if (target) {
                // Split the target node
                enum split_type split = (target->width > target->height) ? 
                    SPLIT_VERTICAL : SPLIT_HORIZONTAL;
                
                struct bsp_node *new_node = bsp_split_node(target, split, 0.5);
                if (new_node) {
                    new_node->window = window;
                    window->node = new_node;
                } else {
                    // If splitting failed, make it floating
                    window->floating = true;
                }
            } else {
                // Fallback to floating if no suitable node found
                window->floating = true;
            }
        }
    }
    
    // Apply layout position and size for tiled windows
    if (!window->floating && window->node) {
        window_move(window, window->node->x, window->node->y);
        window_resize(window, window->node->width, window->node->height);
    } else {
        // Apply default positioning for floating windows
        window_set_floating(window);
    }
    
    // Focus the new window
    window_focus(window);
    
    // Update the layout
    server_update_layout(server);
}

static void handle_xdg_toplevel_unmap(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, unmap);
    struct server *server = window->server;
    
    wlr_log(WLR_DEBUG, "Window unmapped");
    
    // If this was the focused window, focus another window
    if (server->focused_window == window) {
        server->focused_window = NULL;
        
        // Find another mapped window to focus
        struct window *next_focus = NULL;
        struct window *w;
        wl_list_for_each(w, &server->windows, link) {
            if (w->xdg_toplevel->base->surface->mapped) {
                next_focus = w;
                break;
            }
        }
        
        if (next_focus) {
            window_focus(next_focus);
        }
    }
    
    // Update the layout
    server_update_layout(server);
}

static void handle_xdg_toplevel_commit(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, commit);
    // This is where you would handle window resizing, if needed
}

static void handle_xdg_toplevel_request_move(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, request_move);
    
    // If the window is tiled, make it floating first
    if (!window->floating) {
        window_set_floating(window);
    }
    
    // Begin interactive move
    // TODO: Implement interactive move functionality with the cursor
    wlr_log(WLR_INFO, "Window requested move (not implemented yet)");
}

static void handle_xdg_toplevel_request_resize(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, request_resize);
    
    // If the window is tiled, make it floating first
    if (!window->floating) {
        window_set_floating(window);
    }
    
    // Begin interactive resize
    // TODO: Implement interactive resize functionality with the cursor
    wlr_log(WLR_INFO, "Window requested resize (not implemented yet)");
}

static void handle_xdg_toplevel_request_maximize(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, request_maximize);
    
    // In a tiling WM, we typically don't honor maximize requests directly
    // But you could implement custom behavior if desired
    wlr_log(WLR_INFO, "Window requested maximize (ignored in tiling mode)");
    
    // We can acknowledge the request but not actually maximize
    wlr_xdg_toplevel_set_maximized(window->xdg_toplevel, false);
}

static void handle_xdg_toplevel_request_fullscreen(struct wl_listener *listener, void *data) {
    struct window *window = wl_container_of(listener, window, request_fullscreen);
    
    // For fullscreen, you might want to honor this request
    // TODO: Implement proper fullscreen support
    wlr_log(WLR_INFO, "Window requested fullscreen (not implemented yet)");
    
    // For now, just tell the client we won't fullscreen it
    wlr_xdg_toplevel_set_fullscreen(window->xdg_toplevel, false);
}