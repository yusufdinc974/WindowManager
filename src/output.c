#define _POSIX_C_SOURCE 200112L  // For clock_gettime
#include <stdlib.h>
#include <math.h>
#include <time.h>
#include <wlr/types/wlr_output.h>
#include <wlr/types/wlr_scene.h>
#include <wlr/render/wlr_renderer.h>
#include <wlr/util/log.h>

#include "output.h"
#include "server.h"

// Create a demo scene with a color pattern
static void create_demo_scene(struct output *output) {
    if (!output->server->scene) {
        return;
    }
    
    struct wlr_scene_tree *tree = wlr_scene_tree_create(&output->server->scene->tree);
    if (!tree) {
        return;
    }
    
    // Create a scene buffer for some visual output
    struct wlr_scene_rect *rect = wlr_scene_rect_create(
        tree, output->wlr_output->width, output->wlr_output->height, 
        (float[4]){0.3, 0.4, 0.5, 1.0});
    
    if (rect) {
        wlr_scene_node_set_position(&rect->node, 0, 0);
    }
    
    // Add some smaller colored rectangles
    for (int i = 0; i < 5; i++) {
        float r = 0.2 + (i * 0.15);
        float g = 0.3 + (i * 0.1);
        float b = 0.7 - (i * 0.1);
        
        int size = 100;
        int x = 50 + (i * 120);
        int y = output->wlr_output->height / 2 - size / 2;
        
        struct wlr_scene_rect *rect = wlr_scene_rect_create(
            tree, size, size, (float[4]){r, g, b, 1.0});
        
        if (rect) {
            wlr_scene_node_set_position(&rect->node, x, y);
        }
    }
    
    // Add text explaining how to exit (as a rectangle pattern for now)
    // We'll add actual text rendering in a future update
    int x = 20;
    int y = output->wlr_output->height - 50;
    
    struct wlr_scene_rect *exit_hint = wlr_scene_rect_create(
        tree, 400, 30, (float[4]){0.9, 0.9, 0.1, 1.0});
    
    if (exit_hint) {
        wlr_scene_node_set_position(&exit_hint->node, x, y);
    }
    
    wlr_log(WLR_INFO, "Created demo scene for output");
}

static void render_output(struct output *output) {
    // We'll use scene rendering in wlroots 0.18
    if (!output->scene_output) {
        output->scene_output = wlr_scene_output_create(output->server->scene, output->wlr_output);
        
        // Add some visual elements to the scene
        create_demo_scene(output);
    }
    
    // Get current time for animation (if we add it later)
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    
    // Commit the scene to the output
    wlr_scene_output_commit(output->scene_output, NULL);
    
    // Signal successful frame
    wlr_output_schedule_frame(output->wlr_output);
}

void handle_output_frame(struct wl_listener *listener, void *data) {
    struct output *output = wl_container_of(listener, output, frame);
    render_output(output);
}

void handle_output_destroy(struct wl_listener *listener, void *data) {
    struct output *output = wl_container_of(listener, output, destroy);
    
    // Remove from outputs list
    wl_list_remove(&output->link);
    
    // Remove listeners
    wl_list_remove(&output->frame.link);
    wl_list_remove(&output->destroy.link);
    
    // Free the output structure
    free(output);
    
    wlr_log(WLR_INFO, "Output destroyed");
}