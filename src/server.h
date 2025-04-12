#ifndef SERVER_H
#define SERVER_H

#include <wayland-server-core.h>
#include <wlr/backend.h>
#include <wlr/render/allocator.h>
#include <wlr/render/wlr_renderer.h>
#include <wlr/types/wlr_compositor.h>
#include <wlr/types/wlr_output.h>
#include <wlr/types/wlr_output_layout.h>  // Add this include
#include <wlr/types/wlr_xdg_shell.h>
#include <wlr/types/wlr_cursor.h>
#include <wlr/types/wlr_seat.h>
#include <wlr/types/wlr_data_device.h>
#include <wlr/types/wlr_scene.h>
#include <wlr/types/wlr_xcursor_manager.h>

// Forward declarations
struct output;
struct keyboard;
struct window;
struct workspace;

// Main server structure
struct server {
    struct wl_display *display;
    struct wl_event_loop *event_loop;
    
    struct wlr_backend *backend;
    struct wlr_renderer *renderer;
    struct wlr_allocator *allocator;
    struct wlr_scene *scene;
    struct wlr_output_layout *output_layout;  // Add this field
    
    struct wlr_compositor *compositor;
    struct wlr_xdg_shell *xdg_shell;
    struct wlr_cursor *cursor;
    struct wlr_xcursor_manager *cursor_mgr;
    struct wlr_seat *seat;
    struct wlr_data_device_manager *data_device_manager;
    
    struct wl_list outputs;  // struct output::link
    struct wl_list keyboards; // struct keyboard::link
    struct wl_list windows;  // struct window::link
    
    struct window *focused_window;  // Currently focused window
    struct workspace *active_workspace;  // Currently active workspace
    
    // Listeners
    struct wl_listener new_output;
    struct wl_listener new_input;
    struct wl_listener new_xdg_surface;
    struct wl_listener cursor_motion;
    struct wl_listener cursor_motion_absolute;
    struct wl_listener cursor_button;
    struct wl_listener cursor_axis;
    struct wl_listener cursor_frame;
    struct wl_listener request_cursor;
    struct wl_listener request_set_selection;
    
    // Configuration
    int inner_gaps;
    int outer_gaps;
};

// Workspace structure
struct workspace {
    int number;
    struct bsp_node *root;
    struct wl_list windows;  // Windows in this workspace
    struct output *assigned_output;
};

// Function declarations
bool server_init(struct server *server);
void server_start(struct server *server);
void server_finish(struct server *server);
void server_new_output(struct wl_listener *listener, void *data);
void server_update_layout(struct server *server);
void server_focus_window(struct server *server, struct window *window);
void server_request_cursor(struct wl_listener *listener, void *data);

#endif // SERVER_H