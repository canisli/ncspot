#!/usr/bin/env bash

SOCK="/tmp/ncspot-$(id -u)/ncspot.sock"

if pgrep -x ncspot >/dev/null && [ -S "$SOCK" ]; then
    # ncspot is running → send command
    echo "previous" | nc -U "$SOCK"
else
    # fallback: trigger system previous media key
    osascript -e 'tell application "System Events" to key code 15'
fi

