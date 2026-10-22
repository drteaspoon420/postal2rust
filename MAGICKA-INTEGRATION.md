# Magicka in Postal 2

Goal: bring the Magicka rewrite's player, robes, casting, staffs, melee weapons
and magicks into Postal 2, with both first-person and classic Magicka cameras.
The intended experience is a Magicka wizard causing mayhem in Postal's world.

The sibling `../rust-bevy-magicka/output/rewrite` workspace is the gameplay source.
`crates/p2magicka` references its runtime, simulation and content crates directly.
Original assets stay in their existing user-owned locations. Postal scripts and
AI continue to run; the adapter must deliver combat through Postal pawn events.

## Implementation and verification in progress

- Direct runtime host: real robe templates, equipment, animation and cast logic.
- Collision conversion from Postal BSP, meshes and terrain to Magicka surfaces.
- Mirrored Postal combat characters and damage delivery back to native scripts.
- `--magicka`, `--magicka-content`, `--robe`, `--staff`, `--melee`, and
  `--magicka-camera classic` options.
- First-person aiming; F6 toggles a classic overhead view with mouse ground aim.
- F7 loadout panel: browse original robes, staffs and melee weapons; Enter
  equips in place. Arrow keys navigate, PgUp/PgDn skip ten choices, F7/Esc closes.
  Native walking/look/casting input is suppressed while choosing. The world
  continues running. Selection keeps health fraction, statuses and wards, and
  unchanged equipment keeps its cooldowns/enchantments.
- F7's Colour category offers 12 robe colours. Colour survives player saves
  and map travel; older saves default to white. Basic skinned materials now
  combine the original detail and tinted fabric maps, including their combined
  alpha coverage, instead of cutting holes through recolourable fabric.
- Projectiles now use the original Earth, IceEarth and Ice3 models at authored
  size with velocity orientation. Model-less projectiles retain native effects.
  This corrects the sphere stand-ins; it does not establish projectile or spell
  combination parity. Launch/muzzle alignment and broader effects need work.
- Versioned wizard snapshots now accompany Postal's normal save/load and map
  travel. They retain loadout, camera, health, self-shield, elemental statuses,
  spell/enchantment queues, equipment cooldowns, resistance wards and Haste.
  This is player continuity; it does not yet save the entire Magicka world.
- PASSED: 3 unit tests and 13 local-content integration tests, including original
  rig/model loading for 41 robes, 80 staffs and 159 melee weapons. Coverage also
  includes queue rules, healing, Haste expiry, beam hits, particle assets,
  incoming damage/shields, and transactional loadout selection.
- The release build passes. Existing windowed screenshots show both cameras;
  `tools/magicka-loadout-smoke.sh` exercises selection, casts and camera switching
  through real input. Offscreen screenshot target sharing now works across
  Bevy's startup worker and the VM's main thread.

## Requirement-by-requirement evidence

| Requirement | Current evidence | Still unproven |
| --- | --- | --- |
| Magicka player | Real PlayerAvatar; healing/incoming damage/ward tests; versioned player save snapshots; live save/load, fresh-process restore, resaving and map travel pass | Broader death/status checks, complete movement, full timed-buff and world persistence |
| Spell casting | Queue/combinations, beam hits, particle/beam requests tested; render/audio adapter implemented | Every cast form/family in Postal, audio, damage events/geometry in live maps |
| Staffs | 80 original staff definitions/models load; F7 selection; cooldowns survive gear swaps and player snapshot restoration | Passives and every special working in Postal; general timed special-ability buffs |
| Melee weapons | 159 original weapon definitions/models load; F7 selection and native avatar attack path implemented | Actual melee strikes, enchantments and all weapon behaviors in Postal |
| Magicks | Native recognition/dispatch reused; Haste activation and expiry test passes | World/AI/physics/render/audio effects for the full native magick catalog |
| Robes | 41 original robes load rigs/geometry/poses; F7 swaps template/rig; Arctic robe and selected items survive live save/load and map travel | Every robe's properties and appearance in live gameplay; transformations and full temporary state |
| First person | Release runtime and screenshots show original held staff/weapon; cast/melee pose and bob implemented | Windowed feel, sleeves, muzzle/hit alignment and full controls |
| Classic camera | F6/CLI view, cursor aim and original controls; screenshots show original robed wizard in Postal | Click movement, camera collision and indoor visibility coverage |

Passing the inherited simulation suite would not substitute for the missing host
checks above. Source ownership remains with each repository; this branch currently
requires the sibling Magicka rewrite workspace to build.

## Still required before claiming completion

- Continue live gameplay checks and fix runtime/adapter discrepancies.
- Verify robe/equipment behavior in both camera modes, beyond model loading.
- First-person presentation and muzzle placement; classic camera framing,
  cursor, movement, indoor visibility and player visibility verification.
- Complete and verify render/audio coverage for spells, shields, projectiles,
  effects and magicks.
- Verify full spell families, weapon enchantments and equipment passives/specials.
- Broaden checks for incoming Postal damage, Magicka health/status/resistance/
  shields, NPC elemental reactions and forces. Implement missing magick changes.
- Complete persistence for general timed staff buffs, transformed characters,
  active casts/projectiles, summons, and mirrored NPC statuses. Existing player
  snapshots cover health/status/shields/gear/camera, not full world parity.
- Real windowed gameplay checks and screenshots for both camera modes, plus
  broad spell/equipment/magick coverage. Existing rewrite tests alone do not
  establish that the Postal adapter works.

## Coordinate convention

The host uses 52.5 Postal Unreal units per Magicka metre and maps (X,Y,Z) to
(X,Z,Y). This follows the existing Postal renderer convention; it is not a claim about engine parity.
The reflection reverses winding, so imported collision triangles are reversed.

## Commands

To test the existing build from this directory:

```sh
./target/release/p2game --game ./postal2 --magicka --windowed --map suburbs-1
```

F6 switches cameras. Builds now discover Magicka content beside the running
executable or working directory, including the sibling
`rust-bevy-magicka/target/Magicka/Content` checkout. For an older build or a separate
content installation, supply the location explicitly:

```sh
./target/release/p2game --game ./postal2 --magicka --windowed --map suburbs-1 \
  --magicka-content ../rust-bevy-magicka/target/Magicka/Content
```

`--magicka-content` takes priority over `MAGICKA_CONTENT`, which takes priority
over automatic discovery. Content remains in the user's original installation.

```sh
CCACHE_DISABLE=1 cargo check --locked --offline -p p2game -j 2
CCACHE_DISABLE=1 cargo test --locked --offline -p p2magicka -j 2
./play.sh --magicka --map suburbs-1
P2GAME_SMOKE_OFFSCREEN=1 tools/magicka-loadout-smoke.sh
P2GAME_SMOKE_OFFSCREEN=1 tools/magicka-save-smoke.sh
```

The save smoke uses a fresh private `P2_USER_DIR` under
`out/magicka/persistence/`. It saves a hurt, shielded Arctic wizard, changes
robes/equipment/camera, restores the older slot, travels to suburbs-2, then
loads and resaves the slot in separate game processes. It checks the sidecar,
restored health/gear/camera/queue, original collision geometry, and screenshot
completion. Screenshots were visually inspected for restored and traveled
robes, health and camera. Logs and saves stay private in that output directory.

Wizard payloads use the existing `.p2r` sidecar, hex encoded under an extension
namespace; the `.usa` package format remains compatible. Saves without a wizard
payload start the requested CLI robe using the native saved health. Invalid
wizard payloads produce an explicit load error, and missing/invalid snapshots
block new saves rather than writing a silently reset wizard. The sidecar now
records the original map identity when resaving a loaded game.

The first-person element keys are 1..8 (Water, Life, Shield, Cold, Lightning,
Arcane, Earth, Fire), preserving WASD movement. LMB casts, Shift+LMB casts area,
MMB self-casts or uses the staff special with an empty queue, RMB attacks with
the melee weapon, and Q boosts/casts a prepared magick. Classic mode uses the
original QWER/ASDF elements, LMB movement, RMB casting and Space boost/magick.
