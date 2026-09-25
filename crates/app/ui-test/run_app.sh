#!/bin/bash
# Starts Xvfb and SnapRSS, detached, and waits for the window to appear.
export PATH="$HOME/.cargo/bin:$PATH"
export DISPLAY=:99
pkill -f snaprss-app 2>/dev/null
pkill Xvfb 2>/dev/null
sleep 1
setsid Xvfb :99 -screen 0 1440x900x24 -ac > /tmp/xvfb.log 2>&1 < /dev/null &
sleep 3
cd /home/claude/snaprss
WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11 \
  setsid ./target/debug/snaprss-app > /tmp/app.log 2>&1 < /dev/null &
for i in $(seq 1 40); do
  if xdotool search --name "^SnapRSS$" >/dev/null 2>&1; then echo "window up after ${i}s"; exit 0; fi
  sleep 1
done
echo "window never appeared"; tail -5 /tmp/app.log; exit 1
