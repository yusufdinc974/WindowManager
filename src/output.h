#ifndef OUTPUT_H
#define OUTPUT_H

#include <wayland-server-core.h>
#include <wlr/types/wlr_output.h>
#include <wlr/types/wlr_scene.h>

// Forward declaration instead of including server.h
struct server;

// Output structure
struct output {
    struct wl_list link; // server::outputs
    struct server *server;
    struct wlr_output *wlr_output;
    struct wlr_scene_output *scene_output;
    
    struct wl_listener frame;
    struct wl_listener destroy;
};

// Function declarations
void handle_output_frame(struct wl_listener *listener, void *data);
void handle_output_destroy(struct wl_listener *listener, void *data);

#endif // OUTPUT_H