# Agent handoff: Magicka wizard in Postal 2

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
- **Gameplay checks have not passed yet.**
- A first dependency build is still running. `mlua-sys` initially failed because
  ccache attempted to write outside the sandbox; use `CCACHE_DISABLE=1`.
- Builds run with `-j 2` because the Magicka source documents compiler crashes
  under high load. Do not launch more competing builds.

Current tool session (must poll/revalidate; logs alone are not live evidence):
- Session 40707: `CCACHE_DISABLE=1 cargo test --locked --offline -p p2magicka -j 2`
  log `/tmp/p2magicka-tests.log`. This is the current rerun after lock and source fixes.
- Prior sessions 51741 and 89342 ended with compilation errors already fixed.
  The subsequent integrated check (19287) succeeded.

The lock now includes all direct bridge dependencies. Use --locked on subsequent
checks. Dependency changes are authorized by this integration; avoid an unrelated
cargo update.

## Immediate next steps

1. Poll live sessions; inspect compiler diagnostics and fix them.
2. Finish lock resolution offline, then use --locked on subsequent checks.
3. Pass `p2magicka` tests against actual local content. They intentionally fail
   instead of silently skipping when content is absent. Tests cover queue rules,
   robe/equipment geometry/textures, healing, Haste expiry, beam damage.
4. Build p2game and run a real windowed integration check, capture both cameras.
5. Fix/complete incoming damage, spells/effects, controls, models/animation,
   NPC status/forces, world magicks, selection UI and persistence as listed in
   the integration document. Audit the full objective before completion.

Known adapter gaps include: damage from Postal to the wizard is not connected;
spell visuals/audio are not
bridged; native first-person weapons are suppressed but Magicka equipment pose
and muzzle agreement need runtime review; NPC status and movement don't yet
feed back to Postal; rebuilding on travel resets wizard state. The original
full runtime being available does not prove these host capabilities work.
