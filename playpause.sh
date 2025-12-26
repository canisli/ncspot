#!/usr/bin/env bash

SOCK="/tmp/ncspot-$(id -u)/ncspot.sock"

if pgrep -x ncspot >/dev/null && [ -S "$SOCK" ]; then
    # ncspot is running → send command
    echo "playpause" | nc -U "$SOCK"
else
    # fallback: trigger system Play/Pause media key
	osascript -e 'tell application "Shortcuts Events" to run shortcut "PlayPause"'
fi

