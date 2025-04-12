#ifndef BSP_H
#define BSP_H

#include <stdbool.h>

// Forward declaration
struct window;

// Split type enum
enum split_type {
    SPLIT_HORIZONTAL,  // Split along X axis (child windows are stacked vertically)
    SPLIT_VERTICAL     // Split along Y axis (child windows are side by side)
};

// BSP node structure
struct bsp_node {
    struct bsp_node *parent;
    struct bsp_node *left_child;
    struct bsp_node *right_child;
    
    struct window *window;  // NULL for internal nodes
    
    int x, y;              // Position
    int width, height;     // Size
    enum split_type split; // How this node is split
    float split_ratio;     // Ratio between children (0.0 - 1.0)
};

// Function declarations
struct bsp_node *bsp_create_node(void);
void bsp_destroy_node(struct bsp_node *node);

struct bsp_node *bsp_split_node(struct bsp_node *node, enum split_type split, float ratio);
void bsp_remove_node(struct bsp_node *node);

void bsp_apply_layout(struct bsp_node *root, int x, int y, int width, int height);
struct bsp_node *bsp_find_node_at(struct bsp_node *root, double x, double y);

#endif // BSP_H