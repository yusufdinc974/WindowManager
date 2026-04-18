#!/usr/bin/env bash
# mywm-workspaces — Waybar custom module script
# Reads workspace state from the compositor's IPC file and outputs
# Waybar-compatible JSON.

IPC_FILE="${XDG_RUNTIME_DIR:-/tmp}/mywm-workspaces.json"

# Kanji labels for workspace indices
KANJI=("一" "二" "三" "四" "五" "六" "七" "八" "九")

format_output() {
    if [[ ! -f "$IPC_FILE" ]]; then
        echo '{"text": "  一 ", "tooltip": "no workspace data", "class": "disconnected"}'
        return
    fi

    local json
    json=$(cat "$IPC_FILE" 2>/dev/null)
    if [[ -z "$json" ]]; then
        echo '{"text": "  一 ", "tooltip": "empty", "class": "disconnected"}'
        return
    fi

    local text=""
    local tooltip=""
    local active_name=""
    local active_class="default"

    local count
    count=$(echo "$json" | jq '.workspaces | length' 2>/dev/null)
    if [[ -z "$count" || "$count" == "null" ]]; then
        echo '{"text": "  一 ", "tooltip": "parse error", "class": "error"}'
        return
    fi

    for ((i=0; i<count; i++)); do
        local index active occupied wcount layout name
        index=$(echo "$json" | jq -r ".workspaces[$i].index")
        active=$(echo "$json" | jq -r ".workspaces[$i].active")
        occupied=$(echo "$json" | jq -r ".workspaces[$i].occupied")
        wcount=$(echo "$json" | jq -r ".workspaces[$i].window_count")
        layout=$(echo "$json" | jq -r ".workspaces[$i].layout")
        name=$(echo "$json" | jq -r ".workspaces[$i].name")

        # Get kanji label (fallback to number)
        local label="${KANJI[$((index-1))]:-$index}"

        if [[ "$active" == "true" ]]; then
            text+=" <span color='#7aa2f7' font_weight='bold'>$label</span> "
            active_name="$name"
            active_class="active"
            tooltip+="→ [$index] $name ($layout, $wcount windows)\n"
        elif [[ "$occupied" == "true" ]]; then
            text+=" <span color='#a9b1d6'>$label</span> "
            tooltip+="  [$index] $name ($wcount windows)\n"
        else
            text+=" <span color='#414868'>$label</span> "
            tooltip+="  [$index] $name (empty)\n"
        fi
    done

    # Remove trailing newline from tooltip
    tooltip="${tooltip%\\n}"

    echo "{\"text\": \"$text\", \"tooltip\": \"$tooltip\", \"class\": \"$active_class\"}"
}

# Initial output
format_output

# Watch for changes using inotifywait (from inotify-tools)
if command -v inotifywait &>/dev/null; then
    while inotifywait -qq -e modify -e move_self -e create "$IPC_FILE" 2>/dev/null; do
        format_output
    done
else
    # Fallback: poll every 250ms
    while true; do
        sleep 0.25
        format_output
    done
fi