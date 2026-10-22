#!/usr/bin/env bash
# Run after building p2game. All screenshots/logs remain local in out/.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_dir"
mkdir -p out/magicka
# First person: arcane beam, fire spray, lightning, charged earth rock and a
# shield wall; then F6 to the classic camera and a cursor-aimed fire spray.
export P2GAME_REAL='50:key 6;55:mdown right;85:mup right;95:key 8;100:key 8;105:mdown right;130:mup right;140:key 5;145:mdown right;165:mup right;175:key 7;180:key 7;185:mdown right;205:mup right;220:key 3;225:mdown right;229:mup right;242:mdown left;244:mup left;250:key F6;255:cursor 800,300;270:key F;275:mdown right;300:mup right;305:key E;306:key F;308:down ShiftLeft;309:mdown right;312:mup right;313:up ShiftLeft'
export P2GAME_SHOTS='40:out/magicka/first-person.png;75:out/magicka/arcane-beam.png;120:out/magicka/fire-spray.png;155:out/magicka/lightning.png;210:out/magicka/earth-rock.png;238:out/magicka/shield-wall.png;247:out/magicka/melee.png;265:out/magicka/classic.png;290:out/magicka/classic-fire.png;325:out/magicka/classic-area.png'
export P2GAME_EXIT=340
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
