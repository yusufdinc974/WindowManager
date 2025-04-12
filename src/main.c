#define _POSIX_C_SOURCE 200112L  // For sigemptyset, sigaction
#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <unistd.h>
#include <sys/time.h>
#include <wlr/util/log.h>

#include "server.h"

// Global server struct for signal handling
static struct server server;
static struct itimerval watchdog_timer;

// Watchdog to prevent system hanging
static void watchdog_handler(int signum) {
    static int countdown = 5;
    
    if (--countdown <= 0) {
        fprintf(stderr, "Watchdog timeout - something is frozen. Exiting!\n");
        exit(1);
    }
    
    // Reset the timer
    setitimer(ITIMER_REAL, &watchdog_timer, NULL);
}

static void sig_handler(int signal) {
    wlr_log(WLR_INFO, "Received signal %d, shutting down", signal);
    wl_display_terminate(server.display);
}

int main(int argc, char *argv[]) {
    // Initialize wlroots logging
    wlr_log_init(WLR_DEBUG, NULL);
    wlr_log(WLR_INFO, "Starting my-compositor...");
    
    // Print help message
    printf("=== My Wayland Compositor ===\n");
    printf("Press Ctrl+C to exit\n");
    
    // Setup watchdog timer (extend to 5 minutes for development)
    watchdog_timer.it_value.tv_sec = 300;  // 5 minutes
    watchdog_timer.it_value.tv_usec = 0;
    watchdog_timer.it_interval.tv_sec = 300;
    watchdog_timer.it_interval.tv_usec = 0;
    
    signal(SIGALRM, watchdog_handler);
    setitimer(ITIMER_REAL, &watchdog_timer, NULL);
    
    // Initialize server (set to zeros)
    server = (struct server){0};
    
    // Initialize server
    if (!server_init(&server)) {
        wlr_log(WLR_ERROR, "Failed to initialize server");
        return 1;
    }
    
    // Set up signal handling
    struct sigaction sa;
    sa.sa_flags = 0;
    sigemptyset(&sa.sa_mask);
    sa.sa_handler = sig_handler;
    sigaction(SIGINT, &sa, NULL);
    sigaction(SIGTERM, &sa, NULL);
    
    // Start server
    server_start(&server);
    
    // Run Wayland event loop
    wl_display_run(server.display);
    
    // Clean up
    server_finish(&server);
    
    wlr_log(WLR_INFO, "Exiting my-compositor");
    return 0;
}