#!/usr/bin/env bash
# Run after building p2game. All screenshots/logs remain local in out/.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_dir"
mkdir -p out/magicka
export P2GAME_REAL='50:key Digit6;60:mdown left;85:mup left;100:key F6;105:cursor 800,450'
export P2GAME_SHOTS='40:out/magicka/first-person.png;75:out/magicka/arcane-cast.png;130:out/magicka/classic.png'
export P2GAME_EXIT=150
export P2GAME_EXIT_SECS=100
export P2GAME_NO_PAUSE=1
export P2GAME_WINDOWED=1
export P2GAME_UI_TRACE=1
# Keep this smoke run off the user's desktop and force X11 under Xvfb.
timeout 120s env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET xvfb-run -a \
    "${CARGO_TARGET_DIR:-$project_dir/target}/release/p2game" \
    --game "$project_dir/postal2" --magicka --windowed --map suburbs-1 "$@" \
    > out/magicka/windowed-smoke.log 2>&1
