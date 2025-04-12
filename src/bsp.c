#include <stdlib.h>
#include <stdbool.h>
#include <wlr/util/log.h>

#include "bsp.h"

struct bsp_node *bsp_create_node(void) {
    struct bsp_node *node = calloc(1, sizeof(struct bsp_node));
    if (!node) {
        wlr_log(WLR_ERROR, "Failed to allocate BSP node");
        return NULL;
    }
    
    // Initialize with default values
    node->parent = NULL;
    node->left_child = NULL;
    node->right_child = NULL;
    node->window = NULL;
    node->x = 0;
    node->y = 0;
    node->width = 0;
    node->height = 0;
    node->split = SPLIT_VERTICAL;  // Default to vertical split
    node->split_ratio = 0.5;       // Default to 50/50 split
    
    return node;
}

void bsp_destroy_node(struct bsp_node *node) {
    if (!node) {
        return;
    }
    
    // Recursively destroy children
    if (node->left_child) {
        bsp_destroy_node(node->left_child);
    }
    
    if (node->right_child) {
        bsp_destroy_node(node->right_child);
    }
    
    // Free this node
    free(node);
}

struct bsp_node *bsp_split_node(struct bsp_node *node, enum split_type split, float ratio) {
    if (!node || ratio <= 0.0 || ratio >= 1.0) {
        return NULL;
    }
    
    // Can't split a node that already has children
    if (node->left_child || node->right_child) {
        return NULL;
    }
    
    // Create children
    node->left_child = bsp_create_node();
    node->right_child = bsp_create_node();
    
    if (!node->left_child || !node->right_child) {
        // Clean up on error
        if (node->left_child) {
            free(node->left_child);
            node->left_child = NULL;
        }
        
        if (node->right_child) {
            free(node->right_child);
            node->right_child = NULL;
        }
        
        return NULL;
    }
    
    // Set up parent references
    node->left_child->parent = node;
    node->right_child->parent = node;
    
    // Set split properties
    node->split = split;
    node->split_ratio = ratio;
    
    // Move window to left child (if any)
    node->left_child->window = node->window;
    node->window = NULL;
    
    return node->right_child;
}

void bsp_remove_node(struct bsp_node *node) {
    if (!node || !node->parent) {
        return;  // Can't remove the root or NULL node
    }
    
    struct bsp_node *parent = node->parent;
    struct bsp_node *sibling = parent->left_child == node ? parent->right_child : parent->left_child;
    
    // Move sibling properties to parent
    parent->window = sibling->window;
    parent->left_child = sibling->left_child;
    parent->right_child = sibling->right_child;
    
    // Update parent refs of children
    if (parent->left_child) {
        parent->left_child->parent = parent;
    }
    
    if (parent->right_child) {
        parent->right_child->parent = parent;
    }
    
    // Free the nodes
    sibling->left_child = NULL;
    sibling->right_child = NULL;
    free(sibling);
    
    node->parent = NULL;
    free(node);
}

void bsp_apply_layout(struct bsp_node *root, int x, int y, int width, int height) {
    if (!root) {
        return;
    }
    
    // Set the dimensions of this node
    root->x = x;
    root->y = y;
    root->width = width;
    root->height = height;
    
    // If this is a leaf node (has a window), we're done
    if (!root->left_child && !root->right_child) {
        return;
    }
    
    // Calculate dimensions for children based on split type and ratio
    if (root->split == SPLIT_VERTICAL) {
        // Split along Y axis (side by side windows)
        int left_width = (int)(width * root->split_ratio);
        
        bsp_apply_layout(root->left_child, x, y, left_width, height);
        bsp_apply_layout(root->right_child, x + left_width, y, width - left_width, height);
    } else {
        // Split along X axis (stacked windows)
        int top_height = (int)(height * root->split_ratio);
        
        bsp_apply_layout(root->left_child, x, y, width, top_height);
        bsp_apply_layout(root->right_child, x, y + top_height, width, height - top_height);
    }
}

struct bsp_node *bsp_find_node_at(struct bsp_node *root, double x, double y) {
    if (!root) {
        return NULL;
    }
    
    // Check if point is within this node
    if (x < root->x || y < root->y || x >= root->x + root->width || y >= root->y + root->height) {
        return NULL;
    }
    
    // If leaf node, return this node
    if (!root->left_child && !root->right_child) {
        return root;
    }
    
    // Check children
    struct bsp_node *found;
    
    found = bsp_find_node_at(root->left_child, x, y);
    if (found) {
        return found;
    }
    
    found = bsp_find_node_at(root->right_child, x, y);
    if (found) {
        return found;
    }
    
    // Should not reach here - if point is in this node, it should be in one of the children
    return NULL;
}

// Add this function to your bsp.c file

void bsp_remove_window(struct bsp_node *node) {
    if (!node) {
        return;
    }
    
    // Clear the window reference
    node->window = NULL;
    
    // If this is a leaf node with no parent, just clear it
    if (!node->parent) {
        return;
    }
    
    // Get sibling node
    struct bsp_node *parent = node->parent;
    struct bsp_node *sibling = (parent->left_child == node) ? 
                              parent->right_child : parent->left_child;
    
    // Move sibling's properties to parent
    if (sibling->window) {
        // Sibling is a leaf node with a window
        parent->window = sibling->window;
        parent->left_child = NULL;
        parent->right_child = NULL;
    } else if (sibling->left_child && sibling->right_child) {
        // Sibling is an internal node
        parent->left_child = sibling->left_child;
        parent->right_child = sibling->right_child;
        parent->split = sibling->split;
        parent->split_ratio = sibling->split_ratio;
        
        // Update parent references
        parent->left_child->parent = parent;
        parent->right_child->parent = parent;
    } else {
        // This is an unusual case (sibling is empty leaf)
        parent->window = NULL;
        parent->left_child = NULL;
        parent->right_child = NULL;
    }
    
    // Free the nodes
    free(node);
    free(sibling);
}