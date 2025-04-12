#ifndef INPUT_H
#define INPUT_H

#include <wayland-server-core.h>
#include <wlr/types/wlr_input_device.h>
#include <wlr/types/wlr_keyboard.h>
#include <wlr/types/wlr_pointer.h>

#include "server.h"

// Keyboard structure
struct keyboard {
    struct wl_list link; // server::keyboards
    struct server *server;
    struct wlr_input_device *device;
    
    struct wl_listener modifiers;
    struct wl_listener key;
    struct wl_listener destroy;
};

// Function declarations
void handle_new_input_device(struct server *server, struct wlr_input_device *device);
void handle_cursor_motion(struct wl_listener *listener, void *data);
void handle_cursor_motion_absolute(struct wl_listener *listener, void *data);
void handle_cursor_button(struct wl_listener *listener, void *data);
void handle_cursor_axis(struct wl_listener *listener, void *data);
void handle_cursor_frame(struct wl_listener *listener, void *data);

#endif // INPUT_H