# Postal 2 Rust/Bevy

> **Work in progress.** This is an unfinished, unofficial engine port. The whole week (Monday to Friday plus Apocalypse Weekend) can be played start to finish, but expect visual differences, AI oddities, missing effects and bugs compared with the original game. Bug reports are welcome.

A clean-room Rust / [Bevy](https://bevyengine.org) engine that runs **POSTAL 2** (Monday to Friday plus Apocalypse Weekend) using the assets of **your own installed copy** of the game.

## No game files here

- This repository contains **no game assets** (maps, textures, sounds, meshes, scripts) and **no original or decompiled game code**.
- Everything is read at runtime from a legitimately owned install (for example the Steam "POSTAL 2" / POSTAL2Complete edition).
- POSTAL 2 is © Running With Scissors. This project is unaffiliated with them.

## Playing (release build)

1. Download the latest release zip from the [Releases](../../releases) page and unzip it anywhere.
2. Run **`Postal2Rust.exe`** (one exe: the game itself, with a console window for its messages; if it crashes the console stays open with the error).
3. If your **POSTAL 2 install folder** is not found automatically (the one containing `System`, `Maps`, `Textures`; on Steam usually `...\steamapps\common\POSTAL2Complete`), it asks for it. The choice is saved in `postal2dir.txt` next to the exe.

Options:

| Option | What it does |
|---|---|
| `--change-folder` | Ask for the POSTAL 2 folder again |
| `--windowed` | Play in a window instead of fullscreen |
| `--map suburbs-1` | Start directly on a map |

Requirements: Windows 10/11 64-bit, a GPU with Vulkan or DirectX 12, and a copy of POSTAL 2.

## Magicka integration (in development)

Local branch `magicka-in-postal2` hosts gameplay from the sibling Magicka rewrite
in Postal maps. The initial bridge tests pass; the full integration is unfinished.
See [progress and remaining work](MAGICKA-INTEGRATION.md) and
[agent handoff](HANDOFF.md) for current verification and known gaps.

```sh
./play.sh --magicka --map suburbs-1
./play.sh --magicka --map suburbs-1 --magicka-camera classic
```

This requires the sibling `../rust-bevy-magicka/output/rewrite` source workspace and
your own Magicka content. The local content folder is detected at
`../rust-bevy-magicka/target/Magicka/Content`; use `--magicka-content /path/to/Content`
for a different install. F6 switches cameras. `--robe`, `--staff` and `--melee`
select original template/item names. Camera-specific controls appear in the HUD.

## Keybinds

The port reads your bindings from the game's own `User.ini` / `DefUser.ini`, so these are POSTAL 2's defaults and any rebinding in the Controls menu applies.

**Movement**

| Key | Action |
|---|---|
| W / A / S / D | Move forward / strafe left / move back / strafe right |
| Mouse | Look |
| Space | Jump (also skips cutscenes) |
| C or Ctrl | Crouch |
| Shift | Walk |

**Combat**

| Key | Action |
|---|---|
| Left mouse | Fire |
| Right mouse | Alt fire |
| Middle mouse | Reload |
| Mouse wheel | Next / previous weapon (zoom on scoped weapons) |
| 0 to 9 | Weapon slots |
| F | Holster (switch to hands) |
| Q | Kick |
| Z | Throw weapon |

**Items and actions**

| Key | Action |
|---|---|
| E or Enter | Use the selected inventory item / confirm |
| [ / ] | Previous / next inventory item |
| Tab | Inventory menu |
| T | Quick health |
| M | Map |
| X | Throw powerup |
| R | Unzip (piss) |
| B | Talk |
| G | Middle finger |
| H | Dance |
| J | Scream |
| Y | Get down (order people to the ground) |
| K | Suicide |
| L | Toggle inventory hints |

**Game**

| Key | Action |
|---|---|
| Esc | Pause menu |
| F5 / F8 | Quick save / quick load |
| F9 | Screenshot |
| Pause | Pause |
| ~ (tilde) | Console |

**Port extras (not in the original)**

| Key | Action |
|---|---|
| F1 | Print frame timing to the log |
| F3 | Debug overlay (FPS, map, player state, position) |
| F10 | Developer menu (cheats, spawn NPCs, teleport, time of day) |
| F11 | Scene editor (pauses the world; Space resumes, left click selects, right mouse looks) |

## What still needs to be finished or fixed

Known differences from the original, roughly by impact:

**Gameplay and AI**
- NPC pathfinding handles stairs and blocked spots much better, but busy queues (the EastMall food court) can still jostle.
- Some pawns use poses or idle animations that differ from the original.
- Wandering NPCs picking a new random destination now cost about 1 ms instead of 3-14 ms; small hitches can remain right after arriving on a map.

**Rendering**
- Lighting, sky and fog colours still differ in places. Night maps, MilitaryBase cells, VincesHouse glass, EasterArena grass and the suburbs trailer were brought closer to the original in v0.3.0; the MilitaryBase cell mattresses still look like a lattice instead of a solid sheet.
- Explosions and the grenade blast look different (yellow-grey cloud instead of an orange flash with brown smoke); tracers are unchecked.
- A few missing details (e.g. the US flag poster in the police station).

**Interface**
- No known differences left in the map screen; report anything you spot.

**Not started / out of scope**
- Multiplayer.
- POSTAL 2: Paradise Lost (out of scope).
- Linux and macOS builds are produced by CI but have had little testing so far; please report problems.

## Building from source

On Linux, with Rust stable and the ALSA, udev, Wayland and xkbcommon development
libraries installed, place your game files in `postal2/` (containing `System/`,
`Maps/` and `Textures/`) and run:

```sh
./play.sh
```

The launcher builds the release binary and starts it in a window. The first build
requires network access to download Rust dependencies. Extra arguments are passed
to the game, for example `./play.sh --map suburbs-1`. To use another install,
set `POSTAL2_DIR=/path/to/POSTAL2Complete` when running the launcher.

Windows, Rust stable with the `x86_64-pc-windows-gnullvm` target and [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) (no Visual Studio needed). `tools/cargo.bat` wraps cargo with that toolchain; edit the paths at its top for your machine.

```
tools\cargo.bat build --release -p p2game
```

This gives `target\release\p2game.exe`; the release ships it renamed to `Postal2Rust.exe` (under any name other than `p2game` it defaults to fullscreen and asks for the install folder when needed). The app icon (`crates/p2game/res/app.ico`, original art from `tools/release/make_icon.ps1`) is embedded by `build.rs` with llvm-windres. For a shipping build use `--profile dist` instead of `--release` (fat LTO, about 10% faster, slow to build; output in `target\dist`). The game needs `libunwind.dll` from llvm-mingw next to it. You can also run the game directly:

```
target\release\p2game.exe --game "C:\path\to\POSTAL2Complete" --map suburbs-1
```

Regression modes used during development: `--smoke` (boots a set of maps) and `--progress 7` (plays every day's errands headlessly).

## What is in it

| Crate | What |
|---|---|
| `p2game` | The game (shipped as `Postal2Rust.exe`): install detection and folder prompt, boot, map actors, the VM host, AI movement, weapons, HUD, menus, save/load, windowed play |
| `upkg` | Unreal Engine 2 package reader: tables, tagged properties, textures, static/skeletal meshes, animations, BSP, lightmaps, terrain, sounds |
| `uscript` | UnrealScript bytecode interpreter: object graph, states, latent functions, iterators, navigation natives |
| `p2view` | Bevy level renderer: BSP with baked lightmaps, static meshes, terrain, sky, projectors |
| `p2physics` / `p2collide` | UE2 actor physics (walking, falling, swimming, ladders, movers, volumes) and swept collision |
| `p2anim` | Skeletal animation and GPU skinning |
| `p2fx` | Particle emitters and decals |
| `p2sound` / `p2audio` | UE2-style positional audio, music |
| `p2canvas` / `p2input` | UE2 Canvas (HUD, menus, fonts) and input bindings |
| `p2matinee` / `p2movie` / `p2save` | Cutscenes, RoQ movie decoder, save games |
| `p2nav` | Path network loading |

## License

MIT OR Apache-2.0
