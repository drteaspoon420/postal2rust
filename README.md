# Postal 2 Rust/Bevy

> **Work in progress.** This is an unfinished, unofficial engine port. The whole week (Monday to Friday plus Apocalypse Weekend) can be played start to finish, but expect visual differences, AI oddities, missing effects and bugs compared with the original game. Bug reports are welcome.

A clean-room Rust / [Bevy](https://bevyengine.org) engine that runs **POSTAL 2** (Monday to Friday plus Apocalypse Weekend) using the assets of **your own installed copy** of the game.

## No game files here

- This repository contains **no game assets** (maps, textures, sounds, meshes, scripts) and **no original or decompiled game code**.
- Everything is read at runtime from a legitimately owned install (for example the Steam "POSTAL 2" / POSTAL2Complete edition).
- POSTAL 2 is © Running With Scissors. This project is unaffiliated with them.

## Playing (release build)

1. Download the latest release zip from the [Releases](../../releases) page and unzip it anywhere.
2. Run **`postal2rust.exe`**.
3. On first launch it asks for your **POSTAL 2 install folder** (the one containing `System`, `Maps`, `Textures`; on Steam usually `...\steamapps\common\POSTAL2Complete`). If it finds one in your Steam library it opens the picker there. The choice is saved in `postal2dir.txt` next to the exe.

Launcher options:

| Option | What it does |
|---|---|
| `--change-folder` | Ask for the POSTAL 2 folder again |
| `--windowed` | Play in a window instead of borderless fullscreen |
| `--map suburbs-1` | Start directly on a map |

The game log is written to `p2game.log` next to the exe.

Requirements: Windows 10/11 64-bit, a GPU with Vulkan or DirectX 12, and a copy of POSTAL 2.

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
- Apocalypse Weekend Sunday: zombies taunt but rarely attack.
- Thursday: some errand items (Krotchy, steak) do not reveal their errands when given by cheat.
- Paycheck errand: the Vince / RWS scene does not play when the map is loaded directly.
- NPC pathfinding is improved but crowds can still get stuck in tight spots (mall interiors especially).
- Some pawns use poses or idle animations that differ from the original.

**Rendering**
- Lighting, sky and fog colours differ: outdoor maps look brighter and cooler than the original's warm haze; some night and interior maps (MilitaryBase cells, VincesHouse sky) are off.
- First-person weapons are lit flatter than the original.
- Pistol muzzle flash is missing; explosions and the grenade blast look different; tracers are unchecked.
- A few missing details (e.g. the US flag poster in the police station).

**Interface**
- Map screen: fade timing, the "you are here" arrow and the "Press Enter/Space to continue" prompt.
- Map paper colour, a missing "Purge Workshop" entry in Options, the selected item at the start of Apocalypse Weekend.

**Not started / out of scope**
- Multiplayer.
- POSTAL 2: Paradise Lost (out of scope).
- Linux / macOS builds (the code is mostly portable but only Windows is built and tested).

## Building from source

Windows, Rust stable with the `x86_64-pc-windows-gnullvm` target and [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) (no Visual Studio needed). `tools/cargo.bat` wraps cargo with that toolchain; edit the paths at its top for your machine.

```
tools\cargo.bat build --release -p p2game -p postal2rust
```

This gives `target\release\postal2rust.exe` (launcher) and `p2game.exe` (the game). The game needs `libunwind.dll` from llvm-mingw next to it. You can also run the game directly:

```
target\release\p2game.exe --game "C:\path\to\POSTAL2Complete" --map suburbs-1
```

Regression modes used during development: `--smoke` (boots a set of maps) and `--progress 7` (plays every day's errands headlessly).

## What is in it

| Crate | What |
|---|---|
| `launcher` | `postal2rust.exe`: finds or asks for the POSTAL 2 install, starts the game |
| `upkg` | Unreal Engine 2 package reader: tables, tagged properties, textures, static/skeletal meshes, animations, BSP, lightmaps, terrain, sounds |
| `uscript` | UnrealScript bytecode interpreter: object graph, states, latent functions, iterators, navigation natives |
| `p2game` | The game: boot, map actors, the VM host, AI movement, weapons, HUD, menus, save/load, windowed play |
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
