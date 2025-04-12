// window.h
#ifndef WINDOW_H
#define WINDOW_H

#include <wayland-server-core.h>
#include <wlr/types/wlr_xdg_shell.h>
#include <wlr/types/wlr_scene.h>

struct server;
struct bsp_node;

enum window_type {
    WINDOW_XDG_TOPLEVEL,
    // Add other types later (X11, layer shell, etc.)
};

struct window {
    enum window_type type;
    struct server *server;
    struct wl_list link; // Link in server.windows
    
    // XDG toplevel state
    struct wlr_xdg_toplevel *xdg_toplevel;
    struct wlr_scene_tree *scene_tree; // Scene node that contains the window
    struct wlr_scene_node *scene_surface; // Scene node for the surface
    
    // Window position and dimensions
    int x, y;
    int width, height;
    
    // BSP node that contains this window (if tiled)
    struct bsp_node *node;
    
    // Whether this window is floating
    bool floating;
    
    // Window decorations
    bool decorated;
    
    // Listeners
    struct wl_listener destroy;
    struct wl_listener map;
    struct wl_listener unmap;
    struct wl_listener commit;
    struct wl_listener request_move;
    struct wl_listener request_resize;
    struct wl_listener request_maximize;
    struct wl_listener request_fullscreen;
};

struct window *window_create_xdg(struct server *server, struct wlr_xdg_toplevel *xdg_toplevel);
void window_destroy(struct window *window);
void window_focus(struct window *window);
void window_move(struct window *window, int x, int y);
void window_resize(struct window *window, int width, int height);
void window_set_tiled(struct window *window, struct bsp_node *node);
void window_set_floating(struct window *window);

#endif // WINDOW_H