# Magicka in Postal 2

Goal: bring the Magicka rewrite's player, robes, casting, staffs, melee weapons
and magicks into Postal 2, with both first-person and classic Magicka cameras.
The intended experience is a Magicka wizard causing mayhem in Postal's world.

The sibling `../re-lab-starter/output/rewrite` workspace is the gameplay source.
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
- PASSED: one coordinate unit test and three local-content integration tests
  covering robe/equipment geometry and textures, element combinations, healing,
  Haste expiry, and an arcane beam damaging a hosted pawn.
- The release build and actual windowed smoke checks are still pending.

## Requirement-by-requirement evidence

| Requirement | Current evidence | Still unproven |
| --- | --- | --- |
| Magicka player | Real PlayerAvatar, character definitions and SceneWorld; healing integration test passes | Incoming Postal damage, death, statuses, complete movement, persistence |
| Spell casting | Native queue/combinations and beam-on-hosted-pawn tests pass | Every cast form/family in Postal, visuals/audio, damage events/geometry in live maps |
| Staffs | Original staff data, model and texture loading tested on Wizard | Catalog selection, passives and every special working in Postal |
| Melee weapons | Original melee model/data and native avatar attack path wired | Actual melee strikes, enchantments and all weapon behaviors in Postal |
| Magicks | Native recognition/dispatch reused; Haste activation and expiry test passes | World/AI/physics/render/audio effects for the full native magick catalog |
| Robes | Original Wizard rig, textured geometry and animation sampler loaded | Visual inspection, every robe's appearance and properties, selection/persistence |
| First person | Compile-checked adapter with mouse aiming and held staff/weapon placement | Windowed feel, animation, sleeves, muzzle/hit alignment and full controls |
| Classic camera | F6/CLI camera mode, mouse-plane aiming, original element/control layout | Actual framing, click movement, camera collision, indoor visibility and visual inspection |

Passing the inherited simulation suite would not substitute for the missing host
checks above. Source ownership remains with each repository; this branch currently
requires the sibling Magicka rewrite workspace to build.

## Still required before claiming completion

- Compilation passed (`cargo check --locked --offline -p p2game -j 2`); run the
  integration and fix runtime/adapter discrepancies.
- Original textured robe and equipment meshes are loaded and animated by the
  native rewrite pose sampler; verify their rendering in both camera modes.
- First-person presentation and muzzle placement; classic camera framing,
  cursor, movement, indoor visibility and player visibility verification.
- Render/audio bridge for spells, shields, projectiles, effects and magicks.
- Verify full spell families, weapon enchantments and equipment passives/specials.
- Synchronize incoming Postal damage, Magicka health/status/resistance/shields,
  NPC elemental reactions, forces and all magick world changes.
- Robe/staff/melee selection UI, persistence across maps, death and save/load.
- Real windowed gameplay checks and screenshots for both camera modes, plus
  broad spell/equipment/magick coverage. Existing rewrite tests alone do not
  establish that the Postal adapter works.

## Coordinate convention

The host uses 52.5 Postal Unreal units per Magicka metre and maps (X,Y,Z) to
(X,Z,Y). This follows the existing Postal renderer convention; it is not a claim about engine parity.
The reflection reverses winding, so imported collision triangles are reversed.

## Commands

```sh
CCACHE_DISABLE=1 cargo check --locked --offline -p p2game -j 2
CCACHE_DISABLE=1 cargo test --locked --offline -p p2magicka -j 2
./play.sh --magicka --map suburbs-1
```

The first-person element keys are 1..8 (Water, Life, Shield, Cold, Lightning,
Arcane, Earth, Fire), preserving WASD movement. LMB casts, Shift+LMB casts area,
MMB self-casts or uses the staff special with an empty queue, RMB attacks with
the melee weapon, and Q boosts/casts a prepared magick. Classic mode uses the
original QWER/ASDF elements, LMB movement, RMB casting and Space boost/magick.
