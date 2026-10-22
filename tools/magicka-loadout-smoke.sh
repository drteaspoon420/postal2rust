#!/usr/bin/env bash
# Exercise the actual keyboard loadout UI and render selected gear in both views.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_dir"
mkdir -p out/magicka/loadout
export P2GAME_REAL='25:key F7;32:key Right;36:key Down;40:key Right;44:key Down;48:key Right;50:key Down;51:key Right;54:key Enter;58:key 6;60:mdown left;63:mup left;65:key F7;78:key 8;82:mdown left;96:mup left;100:key F6;110:key F7;125:key Escape'
export P2GAME_SHOTS='30:out/magicka/loadout/open.png;56:out/magicka/loadout/equipped.png;75:out/magicka/loadout/first-person.png;90:out/magicka/loadout/cast.png;105:out/magicka/loadout/classic.png;120:out/magicka/loadout/classic-panel.png;135:out/magicka/loadout/closed.png'
export P2GAME_EXIT=145
export P2GAME_EXIT_SECS=180
export P2GAME_NO_PAUSE=1
export P2GAME_WINDOWED=1
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
    --game "$project_dir/postal2" --magicka --windowed --map suburbs-1 --no-debug "${smoke_args[@]}" "$@" \
    > out/magicka/loadout/log.txt 2>&1
# A fresh launch must enumerate the real content and apply our three selections.
rg -q 'MAGICKA CATALOG [1-9][0-9]* robes [1-9][0-9]* staffs [1-9][0-9]* melee weapons' out/magicka/loadout/log.txt
rg -q 'MAGICKA EQUIPPED' out/magicka/loadout/log.txt
rg -q 'MAGICKA COLOUR Red' out/magicka/loadout/log.txt
if rg -q 'panicked at|Could not equip|MAGICKA visual:' out/magicka/loadout/log.txt; then
    echo 'Loadout smoke failed; inspect out/magicka/loadout/log.txt' >&2
    exit 1
fi
for shot in open equipped first-person cast classic classic-panel closed; do
    test -s "out/magicka/loadout/$shot.png"
    rg -Fq "Screenshot saved to out/magicka/loadout/$shot.png" out/magicka/loadout/log.txt
done
python3 - <<'PY'
from pathlib import Path
import re
frame = 0
casts = 0
for line in Path('out/magicka/loadout/log.txt').read_text().splitlines():
    if match := re.search(r'REAL@(\d+)', line):
        frame = int(match[1])
    if 'MAGICKA CAST' in line:
        assert not 25 <= frame <= 65, 'Casting input leaked through the loadout panel'
        casts += 1
assert casts, 'Casting did not resume after closing the panel'
PY
echo 'Loadout input/render smoke passed. Screenshots still require visual inspection.'
