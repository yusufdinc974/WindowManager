#!/bin/bash

# Colors for better readability
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}===== Wayland Compositor Test Script (Debug Version) =====${NC}"

# Path to your compositor - adjust if needed
COMPOSITOR="./build/my-compositor"
COMPOSITOR_LOG="compositor.log"

# Check if compositor binary exists
if [ ! -f "$COMPOSITOR" ]; then
    echo -e "${RED}Error: Compositor binary not found at $COMPOSITOR${NC}"
    echo "Make sure you've built the compositor and specified the correct path."
    exit 1
fi

# Start the compositor with debug output
echo -e "${YELLOW}Starting compositor with debug output...${NC}"
WAYLAND_DEBUG=1 WLR_DEBUG=debug $COMPOSITOR > $COMPOSITOR_LOG 2>&1 &
COMPOSITOR_PID=$!

# Wait for the compositor to initialize
echo -e "${YELLOW}Waiting for compositor to initialize (5 seconds)...${NC}"
sleep 5

# Check if compositor is still running
if ! kill -0 $COMPOSITOR_PID 2>/dev/null; then
    echo -e "${RED}Compositor failed to start. Check $COMPOSITOR_LOG for details.${NC}"
    exit 1
fi

# Get the Wayland display name from the log file
WAYLAND_DISPLAY=$(grep -o "Running compositor on Wayland display '[^']*'" $COMPOSITOR_LOG | cut -d "'" -f 2)

if [ -z "$WAYLAND_DISPLAY" ]; then
    WAYLAND_DISPLAY="wayland-0"  # Default fallback
    echo -e "${YELLOW}Could not detect Wayland display name, using default: $WAYLAND_DISPLAY${NC}"
else
    echo -e "${GREEN}Using Wayland display: $WAYLAND_DISPLAY${NC}"
fi

export WAYLAND_DISPLAY=$WAYLAND_DISPLAY

# First try a very simple Wayland client - weston-info
echo -e "${YELLOW}Testing with weston-info (a simple Wayland client)...${NC}"
if command -v weston-info &> /dev/null; then
    echo -e "${BLUE}Running: weston-info${NC}"
    weston-info > weston-info.log 2>&1
    WESTON_INFO_STATUS=$?
    
    if [ $WESTON_INFO_STATUS -eq 0 ]; then
        echo -e "${GREEN}weston-info ran successfully! Basic Wayland protocol works.${NC}"
        echo -e "Output saved to weston-info.log"
    else
        echo -e "${RED}weston-info failed with status $WESTON_INFO_STATUS${NC}"
        echo -e "Check weston-info.log for details"
    fi
else
    echo -e "${YELLOW}weston-info not found. Install it with 'sudo apt install weston' or equivalent${NC}"
fi

# Try a simple GUI application
echo -e "\n${YELLOW}Now trying with a simple GUI application...${NC}"

# Try multiple simple applications
for app in "weston-terminal" "foot" "alacritty" "wl-paste -l"; do
    if command -v $(echo $app | cut -d' ' -f1) &> /dev/null; then
        echo -e "${BLUE}Attempting to run: $app${NC}"
        $app > ${app// /_}.log 2>&1 &
        APP_PID=$!
        
        # Give it a moment to crash if it's going to
        sleep 2
        
        # Check if it's still running
        if kill -0 $APP_PID 2>/dev/null; then
            echo -e "${GREEN}$app is running! (PID: $APP_PID)${NC}"
            APP_RUNNING=true
            APP_NAME=$app
            break
        else
            echo -e "${RED}$app failed to start or crashed${NC}"
        fi
    fi
done

if [ -z "$APP_RUNNING" ]; then
    echo -e "${RED}All applications failed to run with your compositor.${NC}"
    echo -e "This suggests there may be critical protocol implementation issues."
    echo -e "Try running your compositor with: WAYLAND_DEBUG=1 WLR_DEBUG=debug ./build/my-compositor"
else
    # Display test instructions
    echo -e "\n${BLUE}===== Test Instructions =====${NC}"
    echo -e "${GREEN}$APP_NAME is running and connected to your compositor!${NC}"
    echo -e "${YELLOW}Try the following keybindings:${NC}"
    echo "- Super+Tab: Focus next window"
    echo "- Super+f: Toggle floating mode"
    echo "- Super+c: Close focused window"
    echo "- Super+h: Split horizontally"
    echo "- Super+v: Split vertically"
    echo "- Escape: Exit compositor"
    
    # Wait for user to finish testing
    echo -e "\n${BLUE}Press Enter when you're done testing to clean up...${NC}"
    read
fi

# Clean up
echo -e "${YELLOW}Cleaning up...${NC}"
if [ ! -z "$APP_PID" ]; then
    kill $APP_PID 2>/dev/null
fi
kill $COMPOSITOR_PID 2>/dev/null

# Check if cleanup was successful
sleep 1
if kill -0 $COMPOSITOR_PID 2>/dev/null; then
    echo -e "${RED}Compositor is still running. Force killing...${NC}"
    kill -9 $COMPOSITOR_PID 2>/dev/null
fi

echo -e "${GREEN}Test completed!${NC}"
echo -e "Compositor log is available at: ${BLUE}$COMPOSITOR_LOG${NC}"
echo -e "\n${YELLOW}Debugging tips:${NC}"
echo "1. Check the compositor log (compositor.log) for errors"
echo "2. Look for protocol errors or missing protocol implementations"
echo "3. Common issues include:"
echo "   - XDG shell protocol implementation errors"
echo "   - Surface management problems"
echo "   - Input handling issues"
echo "   - Memory corruption (segfaults)"
echo -e "4. Try running with GDB: ${BLUE}gdb --args ./build/my-compositor${NC}"