#!/usr/bin/env bash
# Save a hurt, shielded wizard, change the loadout/camera, load the older save,
# travel, then load the same slot in a fresh process. Uses an isolated user dir.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_dir"
mkdir -p out/magicka/persistence
export P2_USER_DIR
P2_USER_DIR="$(mktemp -d "$project_dir/out/magicka/persistence/user.XXXXXX")"
export P2GAME_NO_PAUSE=1 P2GAME_WINDOWED=1 P2GAME_EXIT_SECS=220
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
run_game() {
    timeout 250s "${smoke_runner[@]}" \
        "${CARGO_TARGET_DIR:-$project_dir/target}/release/p2game" \
        --game "$project_dir/postal2" --magicka --windowed --map suburbs-1 --no-debug "${smoke_args[@]}" "$@"
}
export P2GAME_REAL='25:key F7;32:key Right;36:key Right;40:key Down;44:key Right;48:key Down;52:key Right;56:key Enter;60:key F7;66:key F6;80:key E;85:mdown middle;100:mup middle;108:key Q;140:key F7;144:key Home;148:key Up;152:key Home;156:key Up;160:key End;164:key Enter;168:key F7;175:key F6'
export P2GAME_EXEC='64:set AWPostalDude Health 37;120:SaveGame 17;210:LoadGame 17;280:open suburbs-2'
export P2GAME_SHOTS='125:out/magicka/persistence/saved.png;185:out/magicka/persistence/changed.png;260:out/magicka/persistence/restored.png;350:out/magicka/persistence/traveled.png'
export P2GAME_EXIT=365
run_game "$@" > out/magicka/persistence/roundtrip.log 2>&1
unset P2GAME_REAL
export P2GAME_EXEC='25:LoadGame 17;65:SaveGame 18'
export P2GAME_SHOTS='80:out/magicka/persistence/fresh-load.png'
export P2GAME_EXIT=95
run_game "$@" > out/magicka/persistence/fresh-load.log 2>&1
export P2GAME_EXEC='25:LoadGame 18'
export P2GAME_SHOTS='80:out/magicka/persistence/resaved-load.png'
run_game "$@" > out/magicka/persistence/resaved-load.log 2>&1
python3 - <<'PY'
from pathlib import Path
import json, os, re
out = Path('out/magicka/persistence')
sidecar = Path(os.environ['P2_USER_DIR']) / 'Save/Save17.p2r'
rows = [line.split(' ', 2) for line in sidecar.read_text().splitlines() if line.startswith('extension ')]
saved = next(json.loads(bytes.fromhex(payload)) for _, key, payload in rows if bytes.fromhex(key) == b'magicka')
assert saved['loadout'] == dict(robe='Wizard_Arctic', staff='staff_bo', weapon='weapon_brass_knuckle'), saved['loadout']
assert saved['classic'] is True
assert 0 < saved['health']['hit_points'] < saved['health']['max_hit_points'] * .8
assert saved['shield']['kind'] == 1 and saved['shield']['health'] > 0, saved['shield']
assert len(saved['queue']) == 1, saved['queue']
roundtrip = (out / 'roundtrip.log').read_text()
fresh = (out / 'fresh-load.log').read_text()
resaved = (out / 'resaved-load.log').read_text()
assert 'SaveGame 17:' in roundtrip and 'LoadGame 17:' in roundtrip
assert 'LoadGame 17:' in fresh
assert 'SaveGame 18:' in fresh and 'LoadGame 18:' in resaved
assert (sidecar.parent / 'Save18.p2r').read_text().splitlines()[0] == 'map=suburbs-1', 'Resaving lost the source map identity'
triangles = re.search(r'level (\d+) tris', roundtrip)[1]
loaded_geometry_log = resaved.split('travel: ?load=18#', 1)[1]
assert f'level {triangles} tris' in loaded_geometry_log, 'Resaved game did not restore its original collision geometry'
pattern = re.compile(r'MAGICKA RESTORED (\S+) camera=(\S+) hp=(\S+) shield=(\S+) status=(\S+) queue=(\d+) (.*)')
def restores(log):
    rows = list(pattern.finditer(log))
    assert rows, 'No wizard restore reached the live adapter'
    for row in rows:
        assert row[2] == 'classic', row[0]
        assert abs(float(row[3]) - saved['health']['hit_points']) < .01, row[0]
        assert float(row[4]) > 0, row[0]
        assert int(row[6]) == 1, row[0]
        assert all(s in row[7] for s in saved['loadout'].values()), row[0]
    return rows
same_process = restores(roundtrip)
assert any(row[1].startswith('save17:') for row in same_process)
assert any(row[1].startswith('suburbs-2:') for row in same_process), 'Map travel did not restore the wizard'
restores(fresh)
restores(resaved)
equipped = [line for line in roundtrip.splitlines() if line.startswith('MAGICKA EQUIPPED')]
assert len(equipped) == 2 and equipped[0] != equipped[1], 'Test did not change loadouts before restoring'
for name, log in [('saved', roundtrip), ('changed', roundtrip), ('restored', roundtrip), ('traveled', roundtrip), ('fresh-load', fresh), ('resaved-load', resaved)]:
    path = out / f'{name}.png'
    assert path.is_file() and path.stat().st_size
    assert f'Screenshot saved to {path}' in log
for log in [roundtrip, fresh, resaved]:
    assert 'panicked at' not in log
    assert 'Wizard state could not be saved' not in log
print('Wizard save/load, fresh-process restoration, resaving and map travel passed.')
print('Private save evidence:', sidecar)
PY
