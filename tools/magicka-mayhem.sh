#!/usr/bin/env bash
# Summons bystanders in front of the wizard and works through the elements on
# them in first person: fire spray, lightning, a frost-then-earth shatter and
# an arcane beam. Screenshots and the log land in out/magicka/mayhem.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_dir"
mkdir -p out/magicka/mayhem
export P2GAME_EXEC='20:sissy;30:summon BasePeople.Bystanders;32:summon BasePeople.Bystanders;34:summon BasePeople.Bystanders;36:summon BasePeople.Bystanders'
export P2GAME_REAL='60:key 8;64:key 8;66:mdown left;110:mup left;125:key 5;129:key 5;130:mdown left;160:mup left;175:key 4;179:key 4;180:mdown left;215:mup left;220:key 7;222:mdown left;235:mup left;250:key 6;254:key 6;255:mdown left;300:mup left;310:key F6;312:cursor 800,350;320:key D;324:key D;328:mdown right;350:mup right'
export P2GAME_SHOTS='55:out/magicka/mayhem/summoned.png;85:out/magicka/mayhem/fire.png;105:out/magicka/mayhem/burning.png;150:out/magicka/mayhem/lightning.png;205:out/magicka/mayhem/frost.png;232:out/magicka/mayhem/shatter.png;280:out/magicka/mayhem/arcane.png;300:out/magicka/mayhem/aftermath.png;345:out/magicka/mayhem/classic-earth.png;370:out/magicka/mayhem/classic-after.png'
export P2GAME_EXIT=380
export P2GAME_EXIT_SECS=200
export P2GAME_NO_PAUSE=1
export P2GAME_WINDOWED=1
timeout 230s env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET xvfb-run -a \
    "${CARGO_TARGET_DIR:-$project_dir/target}/release/p2game" \
    --game "$project_dir/postal2" --magicka --windowed --map suburbs-1 "$@" \
    > out/magicka/mayhem/log.txt 2>&1
