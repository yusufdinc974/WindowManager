#define _POSIX_C_SOURCE 200112L  // For setenv
#include <stdlib.h>
#include <wayland-server-core.h>
#include <wlr/backend.h>
#include <wlr/render/allocator.h>
#include <wlr/render/wlr_renderer.h>
#include <wlr/types/wlr_compositor.h>
#include <wlr/types/wlr_data_device.h>
#include <wlr/types/wlr_output_layout.h>
#include <wlr/types/wlr_xdg_shell.h>
#include <wlr/util/log.h>
#include <wlr/types/wlr_scene.h>
#include <wlr/types/wlr_input_device.h>
#include <wlr/types/wlr_xcursor_manager.h>

#include "server.h"
#include "output.h"
#include "input.h"
#include "window.h"
#include "bsp.h"

static void handle_new_xdg_surface(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, new_xdg_surface);
    struct wlr_xdg_surface *xdg_surface = data;
    
    // We only want to handle toplevel windows
    if (xdg_surface->role != WLR_XDG_SURFACE_ROLE_TOPLEVEL) {
        return;
    }
    
    wlr_log(WLR_DEBUG, "New XDG toplevel surface: %s",
        xdg_surface->toplevel->title ? xdg_surface->toplevel->title : "(unnamed)");
    
    // Create a window for this surface
    struct window *window = window_create_xdg(server, xdg_surface->toplevel);
    if (!window) {
        wlr_log(WLR_ERROR, "Failed to create window for surface");
        return;
    }
    
    // The window will be added to the BSP tree when it's mapped
    wlr_log(WLR_DEBUG, "Window created successfully, waiting for map event");
}

static void handle_request_cursor(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, request_cursor);
    struct wlr_seat_pointer_request_set_cursor_event *event = data;
    struct wlr_seat_client *focused_client = server->seat->pointer_state.focused_client;
    
    // Only set the cursor if it's coming from the focused client
    if (focused_client == event->seat_client) {
        wlr_cursor_set_surface(server->cursor, event->surface,
            event->hotspot_x, event->hotspot_y);
    }
}

static void handle_request_set_selection(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, request_set_selection);
    struct wlr_seat_request_set_selection_event *event = data;
    
    wlr_seat_set_selection(server->seat, event->source, event->serial);
}

static void handle_new_input(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, new_input);
    struct wlr_input_device *device = data;
    
    wlr_log(WLR_INFO, "New input device: %s", device->name);
    
    handle_new_input_device(server, device);
    
    // Update seat capabilities
    uint32_t capabilities = 0;
    if (!wl_list_empty(&server->keyboards)) {
        capabilities |= WL_SEAT_CAPABILITY_KEYBOARD;
    }
    
    // Check if we have a pointer device (simpler approach)
    if (device->type == WLR_INPUT_DEVICE_POINTER) {
        capabilities |= WL_SEAT_CAPABILITY_POINTER;
    }
    
    wlr_seat_set_capabilities(server->seat, capabilities);
}

bool server_init(struct server *server) {
    // Initialize server state
    wl_list_init(&server->outputs);
    wl_list_init(&server->keyboards);
    wl_list_init(&server->windows);
    
    // Default configuration
    server->inner_gaps = 5;
    server->outer_gaps = 10;
    server->focused_window = NULL;
    
    // Initialize the display
    server->display = wl_display_create();
    if (!server->display) {
        wlr_log(WLR_ERROR, "Failed to create Wayland display");
        return false;
    }
    
    // Get event loop
    server->event_loop = wl_display_get_event_loop(server->display);
    
    // Create the backend
    server->backend = wlr_backend_autocreate(server->event_loop, NULL);
    if (!server->backend) {
        wlr_log(WLR_ERROR, "Failed to create wlr_backend");
        return false;
    }
    
    // Create the renderer
    server->renderer = wlr_renderer_autocreate(server->backend);
    if (!server->renderer) {
        wlr_log(WLR_ERROR, "Failed to create wlr_renderer");
        return false;
    }
    
    wlr_renderer_init_wl_display(server->renderer, server->display);
    
    // Create the allocator
    server->allocator = wlr_allocator_autocreate(server->backend, server->renderer);
    if (!server->allocator) {
        wlr_log(WLR_ERROR, "Failed to create wlr_allocator");
        return false;
    }
    
    // Create output layout with display parameter
    server->output_layout = wlr_output_layout_create(server->display);
    if (!server->output_layout) {
        wlr_log(WLR_ERROR, "Failed to create output layout");
        return false;
    }
    
    // Create scene
    server->scene = wlr_scene_create();
    if (!server->scene) {
        wlr_log(WLR_ERROR, "Failed to create wlr_scene");
        return false;
    }
    
    // Attach scene to output layout
    wlr_scene_attach_output_layout(server->scene, server->output_layout);
    
    // Create compositor and data device manager
    server->compositor = wlr_compositor_create(server->display, 5, server->renderer);
    server->data_device_manager = wlr_data_device_manager_create(server->display);
    
    // Setup XDG shell
    server->xdg_shell = wlr_xdg_shell_create(server->display, 3);
    server->new_xdg_surface.notify = handle_new_xdg_surface;
    wl_signal_add(&server->xdg_shell->events.new_surface, &server->new_xdg_surface);
    
    // Setup cursor
    server->cursor = wlr_cursor_create();
    if (!server->cursor) {
        wlr_log(WLR_ERROR, "Failed to create cursor");
        return false;
    }
    
    // Attach cursor to output layout
    wlr_cursor_attach_output_layout(server->cursor, server->output_layout);
    
    server->cursor_mgr = wlr_xcursor_manager_create(NULL, 24);
    if (!server->cursor_mgr) {
        wlr_log(WLR_ERROR, "Failed to create xcursor manager");
        return false;
    }
    
    // In wlroots 0.18, loading the cursor theme is enough
    // The cursor will be displayed by the system when moving
    if (wlr_xcursor_manager_load(server->cursor_mgr, 1)) {
        wlr_log(WLR_ERROR, "Failed to load xcursor theme");
    }
    
    // Setup cursor event handlers
    server->cursor_motion.notify = handle_cursor_motion;
    wl_signal_add(&server->cursor->events.motion, &server->cursor_motion);
    
    server->cursor_motion_absolute.notify = handle_cursor_motion_absolute;
    wl_signal_add(&server->cursor->events.motion_absolute, &server->cursor_motion_absolute);
    
    server->cursor_button.notify = handle_cursor_button;
    wl_signal_add(&server->cursor->events.button, &server->cursor_button);
    
    server->cursor_axis.notify = handle_cursor_axis;
    wl_signal_add(&server->cursor->events.axis, &server->cursor_axis);
    
    server->cursor_frame.notify = handle_cursor_frame;
    wl_signal_add(&server->cursor->events.frame, &server->cursor_frame);
    
    // Setup seat
    server->seat = wlr_seat_create(server->display, "seat0");
    if (!server->seat) {
        wlr_log(WLR_ERROR, "Failed to create wlr_seat");
        return false;
    }
    
    server->request_cursor.notify = handle_request_cursor;
    wl_signal_add(&server->seat->events.request_set_cursor, &server->request_cursor);
    
    server->request_set_selection.notify = handle_request_set_selection;
    wl_signal_add(&server->seat->events.request_set_selection, &server->request_set_selection);
    
    // Initialize outputs list
    wl_list_init(&server->outputs);
    
    // Listen for new outputs
    server->new_output.notify = server_new_output;
    wl_signal_add(&server->backend->events.new_output, &server->new_output);
    
    // Listen for new input devices
    server->new_input.notify = handle_new_input;
    wl_signal_add(&server->backend->events.new_input, &server->new_input);
    
    // Initialize the workspace
    server->active_workspace = calloc(1, sizeof(struct workspace));
    if (!server->active_workspace) {
        wlr_log(WLR_ERROR, "Failed to allocate workspace");
        return false;
    }
    
    server->active_workspace->number = 1;
    server->active_workspace->root = bsp_create_node();
    if (!server->active_workspace->root) {
        wlr_log(WLR_ERROR, "Failed to create BSP root node");
        free(server->active_workspace);
        return false;
    }
    
    wl_list_init(&server->active_workspace->windows);
    server->active_workspace->assigned_output = NULL;  // Will be set when output is available
    
    wlr_log(WLR_INFO, "Server initialized successfully");
    return true;
}

void server_start(struct server *server) {
    // Add socket to Wayland display
    const char *socket = wl_display_add_socket_auto(server->display);
    if (!socket) {
        wlr_log(WLR_ERROR, "Failed to create Wayland socket");
        return;
    }
    
    // Start the backend
    if (!wlr_backend_start(server->backend)) {
        wlr_log(WLR_ERROR, "Failed to start backend");
        wl_display_destroy(server->display);
        return;
    }
    
    wlr_log(WLR_INFO, "Running compositor on Wayland display '%s'", socket);
    setenv("WAYLAND_DISPLAY", socket, 1);
}

void server_new_output(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, new_output);
    struct wlr_output *wlr_output = data;
    
    wlr_log(WLR_INFO, "New output %s", wlr_output->name);
    
    // Initialize the output
    if (!wlr_output_init_render(wlr_output, server->allocator, server->renderer)) {
        wlr_log(WLR_ERROR, "Failed to initialize output rendering");
        return;
    }
    
    // Create output structure
    struct output *output = calloc(1, sizeof(struct output));
    if (!output) {
        wlr_log(WLR_ERROR, "Failed to allocate output");
        return;
    }
    
    output->wlr_output = wlr_output;
    output->server = server;
    output->scene_output = NULL;  // Will be initialized in handle_output_frame
    
    // Setup listeners
    output->frame.notify = handle_output_frame;
    wl_signal_add(&wlr_output->events.frame, &output->frame);
    
    output->destroy.notify = handle_output_destroy;
    wl_signal_add(&wlr_output->events.destroy, &output->destroy);
    
    // Add to outputs list
    wl_list_insert(&server->outputs, &output->link);
    
    // Configure output
    struct wlr_output_state state;
    wlr_output_state_init(&state);
    
    // Find preferred mode
    struct wlr_output_mode *mode = wlr_output_preferred_mode(wlr_output);
    if (mode) {
        wlr_log(WLR_INFO, "Setting preferred mode: %dx%d@%.2fHz",
            mode->width, mode->height, mode->refresh / 1000.0);
        wlr_output_state_set_mode(&state, mode);
    } else {
        wlr_log(WLR_INFO, "No preferred mode found for %s", wlr_output->name);
    }
    
    // Enable the output
    wlr_output_state_set_enabled(&state, true);
    
    // Attempt to commit the state
    if (!wlr_output_commit_state(wlr_output, &state)) {
        wlr_log(WLR_ERROR, "Failed to commit output state");
    }
    
    wlr_output_state_finish(&state);
    
    // Add to output layout
    wlr_output_layout_add_auto(server->output_layout, wlr_output);
    
    // If this is the first output, make it the active workspace's output
    if (server->active_workspace && !server->active_workspace->assigned_output) {
        server->active_workspace->assigned_output = output;
        wlr_log(WLR_INFO, "Set %s as the active workspace's output", wlr_output->name);
    }
    
    // Update the layout to use the new output's dimensions
    server_update_layout(server);
    
    wlr_log(WLR_INFO, "Output %s initialized: %dx%d", 
        wlr_output->name, wlr_output->width, wlr_output->height);
}

void server_update_layout(struct server *server) {
    // Find the active output
    struct output *output = NULL;
    if (server->active_workspace && server->active_workspace->assigned_output) {
        output = server->active_workspace->assigned_output;
    } else if (!wl_list_empty(&server->outputs)) {
        output = wl_container_of(server->outputs.next, output, link);
    }
    
    if (!output) {
        wlr_log(WLR_DEBUG, "No output to update layout for");
        return;
    }
    
    // Get output dimensions
    int width = output->wlr_output->width;
    int height = output->wlr_output->height;
    
    // Apply outer gaps
    int outer_gap = server->outer_gaps;
    int x = outer_gap;
    int y = outer_gap;
    width -= 2 * outer_gap;
    height -= 2 * outer_gap;
    
    // Apply the layout to the BSP tree
    struct bsp_node *root = server->active_workspace->root;
    bsp_apply_layout(root, x, y, width, height);
    
    // Update all tiled windows positions
    struct window *window;
    wl_list_for_each(window, &server->windows, link) {
        if (!window->floating && window->xdg_toplevel->base->surface->mapped) {
            struct bsp_node *node = bsp_find_node_at(root, window->x + 5, window->y + 5);
            if (node) {
                window_move(window, node->x, node->y);
                window_resize(window, node->width, node->height);
            }
        }
    }
}

void server_focus_window(struct server *server, struct window *window) {
    if (!window || !window->xdg_toplevel->base->surface->mapped) {
        return;
    }
    
    // Focus the window
    window_focus(window);
    
    // Move cursor to center of window
    if (server->cursor) {
        wlr_cursor_warp(server->cursor, NULL, 
            window->x + window->width / 2,
            window->y + window->height / 2);
    }
}

void server_finish(struct server *server) {
    wlr_log(WLR_INFO, "Shutting down compositor");
    
    // Free the workspace
    if (server->active_workspace) {
        bsp_destroy_node(server->active_workspace->root);
        free(server->active_workspace);
    }
    
    // Destroy everything in reverse order
    wl_display_destroy_clients(server->display);
    wl_display_destroy(server->display);
    
    // Note: wlroots cleans up most other resources automatically
    // when the display is destroyed
}