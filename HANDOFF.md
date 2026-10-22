# Agent handoff: Magicka wizard in Postal 2

## Latest continuation: player saves and travel (2026-10-22)

Implemented `p2magicka::persistence::SavedWizard`, a versioned JSON player
snapshot. It retains actual robe/equipment, camera, health and damage
accumulators, self-shield data, all nine elemental status slots plus their
cached mask, conjure/prepared spell storage, item enchantments/cooldowns and
gun clip/timer/spread/tracer fields, resistance auras/buffs, Haste TTL and levitation timers. Render
references and old actor handles do not cross the boundary; ward buff IDs are
remapped consistently. Restore validates the entire snapshot and loadout before
applying it to a freshly constructed world.

Postal `Game` now holds opaque save extensions and adapter save errors. The
existing `.p2r` sidecar hex-encodes extension keys/payloads, preserving arbitrary
Unicode/newlines without injecting sidecar statements. `.usa` is unchanged.
Loads replace the extension map (legacy saves clear it); travel carries it.
The wizard publishes at initialized/frame boundaries and restores on map/save
replacement. Fresh native pawns after death get fresh health with the chosen
gear; dead saved snapshots retain the dead flag. Invalid/loading snapshots block
new saves with an explicit error. Legacy native saves initialize the CLI robe
using native saved health instead of inheriting the previous wizard.

Found and fixed a Postal save issue: after loading `Save17`, resaving recorded
`map=save17` and could lose the original geometry on the next load. Sidecars now
use the host's original map identity. Also added the missing `window` feature
guard on `magicka_fx` (headless compilation had tried to build render code) and
Home-key support in the real-input QA driver.

Verification:

- Release build and default check pass; `cargo check --locked --offline
  --no-default-features -p p2game -j 2` passes.
- Full bridge suite: 2 unit + 11 integration tests passed; the subsequently added
  elemental ward test passed separately (12 integration tests total). One item
  bounds diagnostic remains intentionally ignored. New tests cover complete
  serialized-field roundtrip, shield damage after restore, wet status, enchanted
  weapons, cooldowns, Haste expiry, invalid versions/data, dead-state retention,
  and functioning restored fire immunity with remapped buff IDs.
- Postal sidecar unit test passes for Unicode/newlines and corrupt encodings.
- `P2GAME_SMOKE_OFFSCREEN=1 tools/magicka-save-smoke.sh` PASSED. It changes the
  gear/camera after saving a hurt, shielded Arctic wizard; restores the older
  slot; travels to suburbs-2; loads in a fresh process; resaves to another slot;
  then loads that in a third process and verifies original collision geometry.
  The initial test expected a loaded map key of `suburbs-1`; the port names that
  VM package `save17` while retaining original geometry. Corrected the test to
  distinguish package identity from geometry rather than changing the adapter.
- Visually inspected restored, traveled and resaved-load screenshots under
  `out/magicka/persistence/`. Isolated save directory from the passing run:
  `out/magicka/persistence/user.UFZPjV/Save/`.

Still incomplete: this is player continuity, not a full Magicka-world save.
General timed staff special-ability buffs, transformations, active cast/animation
state, projectiles, summons and mirrored NPC statuses need further persistence.
In particular a frozen Postal NPC's native speed may be saved without its
Magicka thaw timer; address NPC status snapshots before claiming world parity.
Broader live melee/staff/magick behavior and camera/first-person fidelity remain
required by the original objective. Do not mark the overall goal complete.

## Previous continuation: in-game loadouts (2026-10-22)

Implemented `p2magicka::catalog::Catalog` and `WizardWorld::equip` on top of
the existing rewrite's loadout API. The original install enumerates 41 robes,
80 staffs and 159 melee weapons. Every listed robe was tested for original
rig/geometry/pose loading, and every listed weapon/staff for model loading.
Selection retains health fraction, combat statuses/wards and unchanged live
items (cooldowns, enchantments, ammo). Invalid selections restore the old gear.
The new robe owns its own base stats and resistances.

F7 opens the keyboard loadout panel in either camera. Up/Down changes category,
Left/Right cycles items, PgUp/PgDn skips ten, Home/End jumps, Enter equips and
F7/Esc closes. The world keeps running. UI routing releases held native keys and
suppresses typed/cast/look input while choosing. Original models rebuild when
equipped names change. Map rebuilds now take the actual equipped loadout from
the previous world instead of resetting to CLI defaults. Full wizard health,
statuses, item state and save/load persistence remain unfinished.

Verification: release build passed; 2 unit and 9 content integration tests
passed (one bounds diagnostic ignored). `tools/magicka-loadout-smoke.sh` checks
real input, gear replacement, resumed casting, camera switching and fresh
screenshot saves; it rejects casting while the panel is open. Run with
`P2GAME_SMOKE_OFFSCREEN=1` for software rendering without a display.
The final run passed, and the equipped panel, changed first-person/classic
models, and Escape returning to gameplay were visually inspected. The panel
fits the 800x450 capture viewport. Artifacts: `out/magicka/loadout/`.
The offscreen capture helper had used a
thread-local image handle written by a Bevy startup worker and read on the VM
thread; sharing it through a mutex fixed skipped screenshots.

Next substantive work: wizard state in Postal save/load and travel; broad
live spell/melee/staff behavior; magick world changes, summons and presentation
in Postal. Current `magicka_fx.rs` bridges particles/beams/bolts/shields/missiles,
but that alone does not establish full magick rendering or behavior.

## Previous continuation: launch path (2026-10-22)

The user reported a startup failure referencing the previous build machine's
`/run/media/system/Cold_Storage/...` content directory. The adapter now discovers
local Magicka content from the executable and CWD ancestors instead of using
`CARGO_MANIFEST_DIR`. Explicit `--magicka-content` and `MAGICKA_CONTENT` still
override discovery. See the launch examples in `MAGICKA-INTEGRATION.md`.

Verified this continuation:

- Release build passed (`CCACHE_DISABLE=1 cargo build --locked --offline --release -p p2game --bin p2game -j 2`).
- Bridge tests passed: 2 unit tests (including checkout relocation/CWD discovery)
  and 7 local-content integration tests. One diagnostic item-bounds test remains
  intentionally ignored.
- Started the rebuilt release from `/tmp`, with `MAGICKA_CONTENT` unset and no
  `--magicka-content`. Offscreen runtime reports the hosted wizard, Postal
  collision, six original robe/equipment mesh parts and an interactive frame.
  Log: `/tmp/p2magicka-relocation-startup.log`.
- Inspected existing first-person and classic screenshots under `out/magicka`.
  The existing mayhem log records native bystander burning/fleeing/death. These
  artifacts predate this continuation and do not prove full gameplay coverage.

Preserved the pre-existing uncommitted damage/status and first-person equipment
changes. In-game gear selection and map/save persistence remain unfinished;
this continuation addressed the user's launch error before starting those.
Older build/session and verification notes below are historical.

## Objective (keep full scope)

Implement the Magicka rewrite's player, spell casting, staffs, melee weapons,
magicks **and robes** in Postal 2. Both first-person and classic Magicka cameras
must work. First-person can use creative presentation, but the result should
feel like a wizard dropped from Magicka into Postal's world to cause mayhem.

User explicitly requests progress on a **local git branch in postal2rust**.
Current branch: `magicka-in-postal2`. Do not push. Commit verified milestones.
A different AI agent may take over: keep this file current and describe actual
verification, not intended behavior as if it already worked.

## Workspace

- Postal repository: this directory (`postal2rust`). User confirmed the
  unmodified port built and ran successfully using `./play.sh`.
- Postal original files: `postal2/` (gitignored). Read only.
- Magicka source: `../re-lab-starter/output/rewrite`.
- Magicka original content: `../re-lab-starter/target/Magicka/Content`. Read only.
- Magicka source commit at integration start: `0122b23`.
- The Magicka repository has unrelated user changes to `mods/load_order.toml`
  and untracked evidence. Preserve them. Read its AGENTS.md before editing it.
- No delegation: local source AGENTS.md permits one agent at a time.

## Architecture and current changes

See `MAGICKA-INTEGRATION.md` for design, controls and completion checklist.
`p2magicka` references the real sibling runtime/content/simulation crates by
path. It creates a fresh Magicka SceneWorld from Postal collision triangles,
loads a real Wizard template/rig/equipment, and feeds actual Avatar input and
SceneWorld stepping. It does not load a Magicka campaign scene.

`p2game/src/magicka.rs` is the Bevy adapter: `--magicka` activation, input
filtering, camera toggle, HUD, hosted Postal pawn characters, damage back through
TakeDamage, and original robe/equipment mesh rendering from local XNB content.
This is work in progress. Do not call it complete or playable without testing.

## Verification at the last update

- Linux launcher syntax and git diff whitespace checks passed.
- Initial integration check found and fixed collision index type (`i32`).
- `CCACHE_DISABLE=1 cargo check --locked --offline -p p2game -j 2` PASSED.
- `cargo test --locked --offline -p p2magicka -j 2` PASSED: 1 unit test and
  3 content-backed integration tests; no skips (0.27 s for integration execution).
- **Windowed gameplay checks have not passed yet.**
- A first dependency build is still running. `mlua-sys` initially failed because
  ccache attempted to write outside the sandbox; use `CCACHE_DISABLE=1`.
- Builds run with `-j 2` because the Magicka source documents compiler crashes
  under high load. Do not launch more competing builds.

Current tool session (must poll/revalidate; logs alone are not live evidence):
- Session 66645: `CCACHE_DISABLE=1 cargo build --locked --offline --release -p p2game -j 2`
  log `/tmp/postal-magicka-build.log`. Compiling the expanded Bevy dependency graph.
- Session 40707 completed successfully (the four bridge tests above).
- Prior sessions 51741 and 89342 ended with compilation errors already fixed.
  The subsequent integrated check (19287) succeeded.

The lock now includes all direct bridge dependencies. Use --locked on subsequent
checks. Dependency changes are authorized by this integration; avoid an unrelated
cargo update.

## Immediate next steps

1. Poll release build 66645; inspect compiler diagnostics and fix them.
   Latest source edits since the passing check add `--magicka-camera classic`,
   mode-specific control help, FP movement speed from robe/statuses/Haste,
   concrete native DamageType, and size-aware pose sampling.
2. Finish lock resolution offline, then use --locked on subsequent checks.
3. The `p2magicka` tests pass against actual local content. They intentionally fail
   instead of silently skipping when content is absent. Tests cover queue rules,
   robe/equipment geometry/textures, healing, Haste expiry, beam damage.
4. Once release builds, run `tools/magicka-smoke.sh`. It injects an arcane
   cast and F6 camera switch, captures first person/cast/classic screenshots,
   writes `out/magicka/windowed-smoke.log`, and exits automatically. Inspect
   screenshots as well as logs; the script itself does not assert gameplay.
5. Fix/complete incoming damage, spells/effects, controls, models/animation,
   NPC status/forces, world magicks, selection UI and persistence as listed in
   the integration document. Audit the full objective before completion.

Known adapter gaps include: damage from Postal to the wizard is not connected;
spell visuals/audio are not
bridged; native first-person weapons are suppressed but Magicka equipment pose
and muzzle agreement need runtime review; NPC status and movement don't yet
feed back to Postal; rebuilding on travel resets wizard state. First-person GroundSpeed now follows the robe and body speed multiplier;
this still needs a windowed check. The original
full runtime being available does not prove these host capabilities work.
