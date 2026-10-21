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
- `--magicka`, `--magicka-content`, `--robe`, `--staff`, `--melee` options.
- First-person aiming; F6 toggles a classic overhead view with mouse ground aim.
- Integration tests for equipment, element combinations, healing, haste expiry
  and an arcane beam hitting a hosted pawn; these require local Magicka content.

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
