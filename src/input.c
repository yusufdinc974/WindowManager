#include <stdlib.h>
#include <xkbcommon/xkbcommon.h>
#include <wlr/types/wlr_cursor.h>
#include <wlr/types/wlr_input_device.h>
#include <wlr/types/wlr_keyboard.h>
#include <wlr/types/wlr_pointer.h>
#include <wlr/types/wlr_scene.h>
#include <wlr/util/log.h>

#include "input.h"
#include "server.h"
#include "window.h"
#include "bsp.h"

// Find the first visible window
static struct window *find_first_window(struct server *server) {
    struct window *window;
    wl_list_for_each(window, &server->windows, link) {
        if (window->xdg_toplevel->base->surface->mapped) {
            return window;
        }
    }
    return NULL;
}

// Find the next window to focus
static struct window *find_next_window(struct server *server, struct window *current) {
    if (wl_list_empty(&server->windows)) {
        return NULL;
    }
    
    // If no current window, focus the first one
    if (!current) {
        return find_first_window(server);
    }
    
    // Find the next mapped window after current
    struct window *window;
    bool found = false;
    
    wl_list_for_each(window, &server->windows, link) {
        if (found && window->xdg_toplevel->base->surface->mapped) {
            return window;
        }
        if (window == current) {
            found = true;
        }
    }
    
    // Wrap around to the first window
    return find_first_window(server);
}

static void handle_keyboard_key(struct wl_listener *listener, void *data) {
    struct keyboard *keyboard = wl_container_of(listener, keyboard, key);
    struct server *server = keyboard->server;
    struct wlr_keyboard_key_event *event = data;
    
    // Get the keyboard from the device
    struct wlr_keyboard *wlr_keyboard = wlr_keyboard_from_input_device(keyboard->device);
    
    // Translate keycode to keysym
    uint32_t keycode = event->keycode + 8; // Offset by 8 for Xkb
    const xkb_keysym_t *syms;
    int nsyms = xkb_state_key_get_syms(wlr_keyboard->xkb_state, keycode, &syms);
    
    bool handled = false;
    bool is_mod_pressed = xkb_state_mod_name_is_active(
        wlr_keyboard->xkb_state, XKB_MOD_NAME_LOGO, XKB_STATE_MODS_DEPRESSED);
    
    // Handle just key down events
    if (event->state == WL_KEYBOARD_KEY_STATE_PRESSED) {
        for (int i = 0; i < nsyms; i++) {
            xkb_keysym_t sym = syms[i];
            
            // Handle special keys
            switch (sym) {
                case XKB_KEY_Escape:
                    wlr_log(WLR_INFO, "Escape pressed, terminating compositor");
                    wl_display_terminate(server->display);
                    handled = true;
                    break;
                    
                case XKB_KEY_q:
                    // Check for Ctrl+q
                    if (xkb_state_mod_name_is_active(wlr_keyboard->xkb_state,
                            XKB_MOD_NAME_CTRL, XKB_STATE_MODS_DEPRESSED)) {
                        wlr_log(WLR_INFO, "Ctrl+q pressed, terminating compositor");
                        wl_display_terminate(server->display);
                        handled = true;
                    }
                    break;
                    
                // Window management keybindings (using Super/Logo key)
                case XKB_KEY_Tab:
                    if (is_mod_pressed) {
                        // Super+Tab: Focus next window
                        struct window *next = find_next_window(server, server->focused_window);
                        if (next) {
                            server_focus_window(server, next);
                            handled = true;
                        }
                    }
                    break;
                    
                case XKB_KEY_f:
                    if (is_mod_pressed && server->focused_window) {
                        // Super+f: Toggle floating
                        if (server->focused_window->floating) {
                            // Make it tiled
                            struct bsp_node *node = bsp_find_node_at(
                                server->active_workspace->root,
                                server->focused_window->x + 5,
                                server->focused_window->y + 5);
                            
                            if (node && !node->window) {
                                window_set_tiled(server->focused_window, node);
                                server_update_layout(server);
                                handled = true;
                            }
                        } else {
                            // Make it floating
                            window_set_floating(server->focused_window);
                            handled = true;
                        }
                    }
                    break;
                    
                case XKB_KEY_c:
                    if (is_mod_pressed && server->focused_window) {
                        // Super+c: Close window
                        wlr_xdg_toplevel_send_close(server->focused_window->xdg_toplevel);
                        handled = true;
                    }
                    break;
                    
                case XKB_KEY_h:
                    if (is_mod_pressed && server->focused_window && !server->focused_window->floating) {
                        // Super+h: Split horizontally
                        struct bsp_node *node = server->focused_window->node;
                        if (node && !node->left_child && !node->right_child) {
                            bsp_split_node(node, SPLIT_HORIZONTAL, 0.5);
                            server_update_layout(server);
                            handled = true;
                        }
                    }
                    break;
                    
                case XKB_KEY_v:
                    if (is_mod_pressed && server->focused_window && !server->focused_window->floating) {
                        // Super+v: Split vertically
                        struct bsp_node *node = server->focused_window->node;
                        if (node && !node->left_child && !node->right_child) {
                            bsp_split_node(node, SPLIT_VERTICAL, 0.5);
                            server_update_layout(server);
                            handled = true;
                        }
                    }
                    break;
            }
        }
    }
    
    // If we didn't handle the key, pass it along to the client
    if (!handled) {
        wlr_seat_set_keyboard(server->seat, wlr_keyboard);
        wlr_seat_keyboard_notify_key(
            server->seat, event->time_msec, event->keycode, event->state);
    }
}

static void handle_keyboard_modifiers(struct wl_listener *listener, void *data) {
    struct keyboard *keyboard = wl_container_of(listener, keyboard, modifiers);
    struct server *server = keyboard->server;
    struct wlr_keyboard *wlr_keyboard = wlr_keyboard_from_input_device(keyboard->device);
    
    // Send modifiers to the client
    wlr_seat_set_keyboard(server->seat, wlr_keyboard);
    wlr_seat_keyboard_notify_modifiers(
        server->seat, &wlr_keyboard->modifiers);
}

static void handle_keyboard_destroy(struct wl_listener *listener, void *data) {
    struct keyboard *keyboard = wl_container_of(listener, keyboard, destroy);
    
    wl_list_remove(&keyboard->modifiers.link);
    wl_list_remove(&keyboard->key.link);
    wl_list_remove(&keyboard->destroy.link);
    
    free(keyboard);
}

static void handle_new_keyboard(struct server *server, struct wlr_input_device *device) {
    struct keyboard *keyboard = calloc(1, sizeof(struct keyboard));
    if (!keyboard) {
        wlr_log(WLR_ERROR, "Failed to allocate keyboard");
        return;
    }
    
    keyboard->server = server;
    keyboard->device = device;
    
    // Setup keyboard
    struct wlr_keyboard *wlr_keyboard = wlr_keyboard_from_input_device(device);
    
    // Set up keymap
    struct xkb_context *context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    if (context) {
        struct xkb_keymap *keymap = xkb_keymap_new_from_names(
            context, NULL, XKB_KEYMAP_COMPILE_NO_FLAGS);
            
        if (keymap) {
            wlr_keyboard_set_keymap(wlr_keyboard, keymap);
            xkb_keymap_unref(keymap);
        }
        
        xkb_context_unref(context);
    }
    
    // Setup handlers for keyboard events
    keyboard->modifiers.notify = handle_keyboard_modifiers;
    wl_signal_add(&wlr_keyboard->events.modifiers, &keyboard->modifiers);
    
    keyboard->key.notify = handle_keyboard_key;
    wl_signal_add(&wlr_keyboard->events.key, &keyboard->key);
    
    keyboard->destroy.notify = handle_keyboard_destroy;
    wl_signal_add(&device->events.destroy, &keyboard->destroy);
    
    // Add to the list of keyboards
    wl_list_insert(&server->keyboards, &keyboard->link);
    
    wlr_log(WLR_INFO, "New keyboard connected");
}

static void handle_new_pointer(struct server *server, struct wlr_input_device *device) {
    // Attach the pointer to the cursor
    wlr_cursor_attach_input_device(server->cursor, device);
    
    wlr_log(WLR_INFO, "New pointer connected");
}

// Alternative approach for finding surfaces - checking directly in the scene hierarchy
static struct wlr_surface *get_surface_at(struct server *server, double lx, double ly, double *sx, double *sy) {
    // In wlroots 0.18, we need to use a different approach
    struct wlr_scene_node *node = wlr_scene_node_at(&server->scene->tree.node, lx, ly, sx, sy);
    if (!node) {
        return NULL;
    }
    
    // Try to convert the node to a surface
    if (node->type == WLR_SCENE_NODE_BUFFER) {
        struct wlr_scene_buffer *scene_buffer = wlr_scene_buffer_from_node(node);
        if (!scene_buffer || !scene_buffer->primary_output) {
            return NULL;
        }
        
        // Get the surface - but only if it's an XDG surface
        struct wlr_xdg_surface *xdg_surface = NULL;
        struct window *window;
        wl_list_for_each(window, &server->windows, link) {
            if (window->scene_surface == node) {
                return window->xdg_toplevel->base->surface;
            }
        }
    }
    
    return NULL;
}

void handle_new_input_device(struct server *server, struct wlr_input_device *device) {
    switch (device->type) {
        case WLR_INPUT_DEVICE_KEYBOARD:
            handle_new_keyboard(server, device);
            break;
        case WLR_INPUT_DEVICE_POINTER:
            handle_new_pointer(server, device);
            break;
        case WLR_INPUT_DEVICE_TOUCH:
            wlr_log(WLR_INFO, "New touch device connected (not implemented)");
            break;
        case WLR_INPUT_DEVICE_TABLET_PAD:
            wlr_log(WLR_INFO, "New tablet pad connected (not implemented)");
            break;
        case WLR_INPUT_DEVICE_SWITCH:
            wlr_log(WLR_INFO, "New switch device connected (not implemented)");
            break;
        default:
            wlr_log(WLR_INFO, "New unknown input device connected");
            break;
    }
}

void handle_cursor_motion(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, cursor_motion);
    struct wlr_pointer_motion_event *event = data;
    
    // Move the cursor
    wlr_cursor_move(server->cursor, &event->pointer->base, event->delta_x, event->delta_y);
    
    // Check if we're over a surface and update pointer focus
    double sx, sy;
    struct wlr_surface *surface = get_surface_at(server, server->cursor->x, server->cursor->y, &sx, &sy);
    
    if (surface) {
        // Update the seat pointer focus
        wlr_seat_pointer_notify_enter(server->seat, surface, sx, sy);
        wlr_seat_pointer_notify_motion(server->seat, event->time_msec, sx, sy);
    } else {
        // Clear pointer focus
        wlr_seat_pointer_clear_focus(server->seat);
    }
}

void handle_cursor_motion_absolute(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, cursor_motion_absolute);
    struct wlr_pointer_motion_absolute_event *event = data;
    
    // Convert to absolute position
    wlr_cursor_warp_absolute(server->cursor, &event->pointer->base, event->x, event->y);
    
    // Update pointer focus (same code as motion, could be refactored)
    double sx, sy;
    struct wlr_surface *surface = get_surface_at(server, server->cursor->x, server->cursor->y, &sx, &sy);
    
    if (surface) {
        // Update the seat pointer focus
        wlr_seat_pointer_notify_enter(server->seat, surface, sx, sy);
        wlr_seat_pointer_notify_motion(server->seat, event->time_msec, sx, sy);
    } else {
        // Clear pointer focus
        wlr_seat_pointer_clear_focus(server->seat);
    }
}

// Alternative approach to find what window the cursor is over
static struct window *find_window_at_cursor(struct server *server, double x, double y) {
    // Find node under cursor
    double sx, sy;
    struct wlr_scene_node *node = wlr_scene_node_at(
        &server->scene->tree.node, x, y, &sx, &sy);
    
    if (!node) {
        return NULL;
    }
    
    // Try to match this node against each window's scene tree
    struct window *window;
    wl_list_for_each(window, &server->windows, link) {
        if (!window->xdg_toplevel->base->surface->mapped) {
            continue;
        }
        
        // Simple approach: just check if the clicked position is within the window's bounds
        if (x >= window->x && x < window->x + window->width &&
            y >= window->y && y < window->y + window->height) {
            return window;
        }
    }
    
    return NULL;
}

void handle_cursor_button(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, cursor_button);
    struct wlr_pointer_button_event *event = data;
    
    // Notify client of button press
    wlr_seat_pointer_notify_button(server->seat, event->time_msec,
        event->button, event->state);
    
    // Handle window focus on button press
    if (event->state == WL_POINTER_BUTTON_STATE_PRESSED) {
        struct window *window = find_window_at_cursor(server, server->cursor->x, server->cursor->y);
        
        if (window) {
            server_focus_window(server, window);
        }
    }
}

void handle_cursor_axis(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, cursor_axis);
    struct wlr_pointer_axis_event *event = data;
    
    // Notify client of axis event (wlroots 0.18 version)
    wlr_seat_pointer_notify_axis(server->seat,
        event->time_msec, event->orientation, event->delta,
        event->delta_discrete, event->source, 0); // 0 for relative direction
}

void handle_cursor_frame(struct wl_listener *listener, void *data) {
    struct server *server = wl_container_of(listener, server, cursor_frame);
    
    // Notify client of frame event
    wlr_seat_pointer_notify_frame(server->seat);
}