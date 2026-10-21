#!/usr/bin/env bash
# Run after building p2game. All screenshots/logs remain local in out/.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_dir"
mkdir -p out/magicka
export P2GAME_REAL='50:key Digit6;60:mdown left;85:mup left;100:key F6;105:cursor 400,225'
export P2GAME_SHOTS='40:out/magicka/first-person.png;75:out/magicka/arcane-cast.png;130:out/magicka/classic.png'
export P2GAME_EXIT=150
export P2GAME_EXIT_SECS=180
export P2GAME_NO_PAUSE=1
export P2GAME_WINDOWED=1
export P2GAME_UI_TRACE=1
# With P2GAME_SMOKE_OFFSCREEN=1 no display server is needed. This uses the
# normal renderer with an image target, including its UI and input systems.
if [[ "${P2GAME_SMOKE_OFFSCREEN:-0}" == 1 ]]; then
    smoke_runner=(env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET)
    smoke_args=(--offscreen)
    if [[ -f /usr/share/vulkan/icd.d/lvp_icd.x86_64.json ]]; then
        export VK_DRIVER_FILES="${VK_DRIVER_FILES:-/usr/share/vulkan/icd.d/lvp_icd.x86_64.json}"
    fi
    export XDG_CACHE_HOME="$project_dir/out/magicka/cache"
else
    smoke_runner=(env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET xvfb-run -a)
    smoke_args=()
fi
timeout 210s "${smoke_runner[@]}" \
    "${CARGO_TARGET_DIR:-$project_dir/target}/release/p2game" \
    --game "$project_dir/postal2" --magicka --windowed --map suburbs-1 "${smoke_args[@]}" "$@" \
    > out/magicka/windowed-smoke.log 2>&1
