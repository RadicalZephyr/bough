#!/bin/sh
# Builds the probe and runs every scenario, or only those whose name
# contains $1, headless under Xvfb. It builds into ./target unless
# CARGO_TARGET_DIR says otherwise.
#   ./run.sh                    all scenarios, control and deferred
#   ./run.sh 3b-tick            a subset
#   PROBE_BLOCKING=1 ./run.sh   blocking main-loop iterations instead
#   PROBE_DEBUG=1 ./run.sh X    trace every bind and painted frame
# One scenario by hand: <binary> <scenario> control|deferred; `list` names them.
set -e
cd "$(dirname "$0")"
TARGET="${CARGO_TARGET_DIR:-$(pwd)/target}"
CARGO_TARGET_DIR="$TARGET" cargo build
xvfb-run -a -s "-screen 0 1280x1024x24" "$TARGET/debug/defer-paint-probe" all "$@" 2>&1 \
  | grep -v -e "libEGL" -e "session bus" -e '^$'
