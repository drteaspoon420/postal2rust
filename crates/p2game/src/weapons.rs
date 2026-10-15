//! First-person weapon view and weapon/firing diagnostics.
//!
//! `Weapon.RenderOverlays` (called from the HUD's PostRender) places the weapon actor at the
//! player's eye (`CalcDrawOffset`, `PlayerViewOffset`) and calls `Canvas.DrawActor(self, false,
//! ClearZ, DisplayFOV)`. The Canvas native records each call (`p2canvas::natives::ActorDraw`). This
//! module renders those actors the way the engine does: a skinned rig per drawn actor, posed by the
//! actor's own `AnimInstance` (so the script's PlayAnim/LoopAnim fire, reload and idle animations
//! show), drawn by an overlay camera with its own depth buffer (the ClearZ) at the canvas camera's
//! position and `DisplayFOV`, under the 2D canvas.
//!
//! The rigs live on render layer [`FP_LAYER`], so the world camera never sees them and the overlay
//! camera sees nothing else.

use crate::window::Sim;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use p2anim::rig::{RigCache, c_mat};
use p2canvas::natives::{self, ActorDraw};
use p2game::host::{actor_world, resolve_path};
use std::collections::HashMap;
use uscript::vm::{ObjId, Value};

/// Render layer of first-person overlay meshes.
pub(crate) const FP_LAYER: usize = 7;

pub(crate) struct WeaponsPlugin;

thread_local! {
    /// Window size in pixels for the resolution console queries.
    static WINDOW_SIZE: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((1600, 900)) };
}

/// Extension host answering the viewport queries weapon/HUD script makes (`GetCurrentRes`,
/// `GetFullScreen`, the ini viewport sizes) so `PlayerController.GetResolution` (and the
/// aspect-derived weapon DisplayFOV) are real numbers.
struct ViewportExt;

impl uscript::host::Host for ViewportExt {
    fn console_command(&self, _vm: &mut uscript::vm::Vm, _from: Option<ObjId>, command: &str) -> String {
        let (w, h) = WINDOW_SIZE.with(|s| s.get());
        let c = command.trim().to_ascii_lowercase();
        let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
        match c.as_str() {
            "getcurrentres" => format!("{w}x{h}"),
            "getfullscreen" => "False".into(),
            "getcurrentcolordepth" => "32".into(),
            _ if c.starts_with("get ini:engine.engine.viewportmanager ") => {
                let k = &c["get ini:engine.engine.viewportmanager ".len()..];
                match k {
                    "windowedviewportx" | "fullscreenviewportx" => w.to_string(),
                    "windowedviewporty" | "fullscreenviewporty" => h.to_string(),
                    "startupfullscreen" => "False".into(),
                    _ => String::new(),
                }
            }
            _ => String::new(),
        }
    }

    fn actor_spawned(&self, vm: &mut uscript::vm::Vm, actor: ObjId) {
        if std::env::var("P2GAME_FP_TRACE").is_ok() {
            let c = &vm.classes[vm.objects[actor].class].name;
            if !c.starts_with("ScriptedTrigger") {
                println!("SPAWN {} at {:?}", c, vm.get_prop(actor, "Location").map(|v| v.as_vec()));
            }
        }
    }

    fn actor_destroyed(&self, vm: &mut uscript::vm::Vm, actor: ObjId) {
        // Diagnostics: who destroys player pawns (`P2GAME_FP_TRACE`).
        if std::env::var("P2GAME_FP_TRACE").is_err() {
            return;
        }
        let c = vm.objects[actor].class;
        if vm.class_is_a(c, "Pawn") {
            let ctrl = vm.get_prop(actor, "Controller").and_then(|v| v.as_obj());
            let player = ctrl.is_some_and(|k| vm.class_is_a(vm.objects[k].class, "PlayerController")) || vm.classes[c].name.contains("Dude");
            if player {
                let lhb = vm.get_prop(actor, "LastHitBy").and_then(|v| v.as_obj());
                let lhb_pawn = lhb.and_then(|k| vm.get_prop(k, "Pawn")).and_then(|v| v.as_obj());
                println!(
                    "DESTROY {} health {:?} lastdamage {:?} lasthitby {:?} ({:?} at {:?}); stack:",
                    vm.object_name(actor),
                    vm.get_prop(actor, "Health"),
                    vm.get_prop(actor, "LastDamageType"),
                    lhb.map(|k| vm.object_name(k)),
                    lhb_pawn.map(|p| vm.object_name(p)),
                    lhb_pawn.and_then(|p| vm.get_prop(p, "Location")).map(|v| v.as_vec())
                );
                for f in vm.backtrace() {
                    println!("    {f}");
                }
            }
        }
    }
}

fn level_hook(host: &p2game::host::GameHost, _vm: &mut uscript::vm::Vm) {
    host.ext.borrow_mut().push(std::rc::Rc::new(ViewportExt));
}

/// Registers the weapons extension host for every level (and the running one).
pub(crate) fn install(sim: &mut Sim) {
    p2game::host::register_level_hook(level_hook);
    level_hook(&sim.game.host, &mut sim.game.world.vm);
}

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FpState>()
            .init_resource::<CanvasMeshes>()
            .add_systems(Update, exec_hook.before(crate::ui::ui_frame))
            .add_systems(Update, (fp_weapon, canvas_meshes).chain().after(crate::ui::ui_frame));
    }
}

/// Runs a console/exec command for the local player the way the console does (controller,
/// CheatManager, pawn, HUD, weapon, GameInfo, else a host console command).
pub(crate) fn exec(vm: &mut uscript::vm::Vm, pc: ObjId, command: &str) -> bool {
    crate::frontend::exec_command(vm, pc, command)
}

thread_local! {
    /// Pawns spawned by P2GAME_EXEC commands, reported every 10 frames (TRACK lines).
    static TRACKED: std::cell::RefCell<Vec<ObjId>> = const { std::cell::RefCell::new(Vec::new()) };
    /// First actor index after the tracked spawn: every 60 frames the visible actors spawned since
    /// are listed (TRACKNEW lines: dropped weapons, gibs, effects).
    static TRACK_FROM: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// Test hook: `P2GAME_EXEC="60:PackNHeat;80:SwitchWeapon 2"` runs commands at script frames,
/// `P2GAME_PROBE=frame` prints the weapon/inventory state at that frame (and every 60 after),
/// `P2GAME_SHOTS="200:out/a.png;300:out/b.png"` saves screenshots and `P2GAME_EXIT=frame` quits.
#[allow(clippy::too_many_arguments)]
fn exec_hook(mut sim: NonSendMut<Sim>, mut frame: Local<u32>, mut last: Local<String>, mut seen_log: Local<usize>, mut commands: Commands, mut exit: MessageWriter<AppExit>) {
    *frame += 1;
    let f = *frame;
    if let Ok(spec) = std::env::var("P2GAME_SHOTS") {
        for item in spec.split(';') {
            if let Some((n, path)) = item.split_once(':') {
                if n.trim().parse::<u32>().ok() == Some(f) {
                    let vm = &sim.game.world.vm;
                    let t = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|t| t.as_float()).unwrap_or(0.0);
                    println!("SHOT {path} at frame {f} t={t:.2}");
                    commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(bevy::render::view::screenshot::save_to_disk(std::path::PathBuf::from(path.trim())));
                }
            }
        }
    }
    if std::env::var("P2GAME_EXIT").ok().and_then(|s| s.parse::<u32>().ok()) == Some(f) {
        exit.write(AppExit::Success);
    }
    let Some(pc) = sim.game.player_controller else { return };
    let anim_host = sim.game.host.clone();
    let vm = &mut sim.game.world.vm;
    // Pawns spawned by EXEC commands: position, state and physics every 10 frames.
    TRACKED.with(|t| {
        if f % 10 == 0 {
            for &a in t.borrow().iter() {
                let c = vm.get_prop(a, "Controller").and_then(|c| c.as_obj());
                println!("TRACK@{f} t={:.2} {} deleted {} at {:?} phys {:?} health {:?} ctrl {:?}", vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|t| t.as_float()).unwrap_or(0.0), vm.object_name(a), vm.objects[a].deleted, vm.actor_loc(a), vm.get_prop(a, "Physics"), vm.get_prop(a, "Health"), c.map(|c| vm.state_name(c)));
            }
        }
        let from = TRACK_FROM.with(|t| t.get());
        if f % 60 == 0 && from != usize::MAX {
            for &a in vm.actors.iter().skip(from) {
                let hidden = vm.get_prop(a, "bHidden").is_some_and(|v| v.as_bool());
                if vm.objects[a].deleted || (hidden && std::env::var_os("P2GAME_TRACK_HIDDEN").is_none()) {
                    continue;
                }
                let dt = vm.get_prop(a, "DrawType").map(|v| v.as_int()).unwrap_or(0);
                if dt == 0 {
                    continue;
                }
                let mesh = vm.get_prop(a, "StaticMesh").or_else(|| vm.get_prop(a, "Mesh")).and_then(|v| v.as_resource().map(|s| s.to_string()));
                println!("TRACKNEW@{f} {} DrawType {dt} hidden {hidden} mesh {mesh:?} at {:?} rot {:?} phys {:?} base {:?} owner {:?}", vm.object_name(a), vm.actor_loc(a), vm.actor_rot(a), vm.get_prop(a, "Physics"), vm.get_prop(a, "Base").and_then(|v| v.as_obj()).map(|o| vm.object_name(o)), vm.get_prop(a, "Owner").and_then(|v| v.as_obj()).map(|o| vm.object_name(o)));
            }
        }
    });
    if std::env::var("P2GAME_FP_TRACE").is_ok() {
        // Report pawn health changes (and the controller state), with recent log lines.
        let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
        let h = pawn.and_then(|p| vm.get_prop(p, "Health")).map(|v| v.as_int());
        let st = vm.state_name(pc);
        let weap = pawn.and_then(|p| vm.get_prop(p, "Weapon")).and_then(|v| v.as_obj());
        let pend = pawn.and_then(|p| vm.get_prop(p, "PendingWeapon")).and_then(|v| v.as_obj());
        let wkey = weap.map(|w| format!("{} {} latent {:?}", vm.object_name(w), vm.state_name(w), vm.objects[w].exec.latent.as_ref().map(|l| l.name.clone())));
        let key = format!("{h:?} {st} | {wkey:?} pending {:?}", pend.map(|p| vm.object_name(p)));
        if *last != key {
            let lt = pawn.and_then(|p| vm.get_prop(p, "LastDamageType")).cloned();
            let loc = pawn.and_then(|p| vm.get_prop(p, "Location")).map(|v| v.as_vec());
            println!("HEALTH@{f} {key} lastdamage {lt:?} loc {loc:?} pawn {:?}", pawn.map(|p| vm.object_name(p)));
            *last = key;
        }
        // Every new log line (minus Accessed None noise) with its frame.
        let n = vm.log.len();
        let skip = if *seen_log <= n { *seen_log } else { 0 };
        for l in vm.log.iter().skip(skip).filter(|l| !l.contains("Accessed None")) {
            println!("   .{f} {l}");
        }
        *seen_log = n;
    }
    if std::env::var_os("P2GAME_ANIM_TRACE").is_some() {
        let host = &anim_host;
        // Player weapon / pawn channel-0 anim: logs each sequence start and end with wall time,
        // so durations can be compared with frames / rate from the ukx.
        let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
        let weap = pawn.and_then(|p| vm.get_prop(p, "Weapon")).and_then(|v| v.as_obj());
        let now = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|t| t.as_float()).unwrap_or(0.0);
        thread_local! { static ANIM_LAST: std::cell::RefCell<std::collections::HashMap<&'static str, (String, f32)>> = Default::default(); }
        for (tag, a) in [("weap", weap), ("pawn", pawn)] {
            let Some(a) = a else { continue };
            let ch = uscript::host::Host::anim_channel(&**host, vm, a, 0);
            let key = ch.as_ref().map(|c| format!("{} {}", c.sequence, c.playing)).unwrap_or_default();
            ANIM_LAST.with(|m| {
                let mut m = m.borrow_mut();
                let e = m.entry(tag).or_insert((String::new(), 0.0));
                if e.0 != key {
                    if let Some(c) = &ch {
                        println!("ANIM {tag} {} {:.3} {} -> {} rate {:.3}/s frame {:.2} (prev lasted {:.3}s)", vm.object_name(a), now, e.0, key, c.rate, c.frame, now - e.1);
                    }
                    *e = (key, now);
                }
            });
        }
    }
    if let Ok(spec) = std::env::var("P2GAME_EXEC") {
        for item in spec.split(';') {
            let Some((n, cmd)) = item.split_once(':') else { continue };
            if n.trim().parse::<u32>().ok() == Some(f) {
                // `@menu Choice`: print the on-screen centre of the open shell menu's control
                // `Choice` (for P2GAME_REAL cursor/click steps).
                if let Some(choice) = cmd.trim().strip_prefix("@menu ") {
                    let root = vm.get_prop(pc, "Player").and_then(|v| v.as_obj()).and_then(|p| vm.get_prop(p, "InteractionMaster")).and_then(|v| v.as_obj()).and_then(|m| vm.get_prop(m, "BaseMenu")).and_then(|v| v.as_obj());
                    let menu = root.and_then(|r| vm.get_prop(r, "MyMenu")).and_then(|v| v.as_obj());
                    let c = {
                        let ch = choice.trim();
                        let (name, idx) = match ch.split_once('[') {
                            Some((n, i)) => (n, i.trim_end_matches(']').parse::<usize>().ok()),
                            None => (ch, None),
                        };
                        let v = menu.and_then(|m| vm.get_prop(m, name)).cloned();
                        match (v, idx) {
                            (Some(Value::Array(a)), Some(i)) => a.get(i).and_then(|v| v.as_obj()),
                            (Some(v), None) => v.as_obj(),
                            _ => None,
                        }
                    };
                    if let (Some(root), Some(c)) = (root, c) {
                        let g = |o: ObjId, n: &str| vm.get_prop(o, n).map(|v| v.as_float()).unwrap_or(0.0);
                        let (mut x, mut y) = (g(c, "WinWidth") / 2.0, g(c, "WinHeight") / 2.0);
                        let mut p = Some(c);
                        while let Some(o) = p.filter(|&o| o != root) {
                            x += g(o, "WinLeft");
                            y += g(o, "WinTop");
                            p = vm.get_prop(o, "ParentWindow").and_then(|v| v.as_obj());
                        }
                        let s = Some(g(root, "GUIScale")).filter(|s| *s > 0.0).unwrap_or(1.0);
                        println!("MENU@{f} {} {choice} at {:.0},{:.0}", menu.map(|m| vm.object_name(m)).unwrap_or_default(), x * s, y * s);
                    } else {
                        println!("MENU@{f} no {choice} (menu {:?})", menu.map(|m| vm.object_name(m)));
                    }
                    continue;
                }
                // `?Actor Prop,Prop..`: print props of the actor (name/tag/class; `pc` = player
                // controller, `pawn` = its pawn), following object values one level by name.
                if let Some(rest) = cmd.trim().strip_prefix('?') {
                    // `?inv`: the player pawn's inventory chain (class group/offset).
                    if rest.trim() == "inv" {
                        let mut it = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).and_then(|p| vm.get_prop(p, "Inventory")).and_then(|v| v.as_obj());
                        let mut out = Vec::new();
                        while let Some(i) = it.filter(|_| out.len() < 200) {
                            let g = |n: &str| vm.get_prop(i, n).map(|v| v.as_int()).unwrap_or(-1);
                            out.push(format!("{}({},{})", vm.classes[vm.objects[i].class].name, g("InventoryGroup"), g("GroupOffset")));
                            it = vm.get_prop(i, "Inventory").and_then(|v| v.as_obj());
                        }
                        println!("PROBE@{f} inv: {}", out.join(" "));
                        continue;
                    }
                    // `?near X,Y,Z R`: actors (non-hidden or with collision) within R of a point.
                    if let Some(a) = rest.strip_prefix("near ") {
                        let (p, r) = a.split_once(' ').unwrap_or((a, "200"));
                        let v: Vec<f32> = p.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                        let r: f32 = r.trim().parse().unwrap_or(200.0);
                        if v.len() == 3 {
                            for &x in vm.actors.iter().filter(|&&x| !vm.objects[x].deleted) {
                                let l = vm.actor_loc(x);
                                let d = ((l[0] - v[0]).powi(2) + (l[1] - v[1]).powi(2) + (l[2] - v[2]).powi(2)).sqrt();
                                if d <= r {
                                    let n = |p: &str| vm.get_prop(x, p).map(|v| format!("{v:?}")).unwrap_or_default();
                                    println!("PROBE@{f} near: {} ({}) [{}] at {l:?} d {d:.0} tag {} attach {} base {} collide {}", vm.object_name(x), vm.classes[vm.objects[x].class].name, vm.state_name(x), n("Tag"), n("AttachTag"), n("Base"), n("bCollideActors"));
                                }
                            }
                        }
                        continue;
                    }
                    let (who, props) = rest.split_once(' ').unwrap_or((rest, ""));
                    let target = match who {
                        "pc" => Some(pc),
                        "pawn" => vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()),
                        k => p2game::demo::find_actor(vm, k).or_else(|| (0..vm.objects.len()).find(|&o| !vm.objects[o].deleted && vm.object_name(o).eq_ignore_ascii_case(k))),
                    };
                    if let Some(o) = target {
                        let vals: Vec<String> = props
                            .split(',')
                            .filter(|p| !p.is_empty())
                            .map(|p| {
                                let v = vm.get_prop(o, p.trim()).cloned();
                                let s = match v.as_ref().and_then(|v| v.as_obj()) {
                                    Some(x) => format!("{} [{}] at {:?}", vm.object_name(x), vm.state_name(x), vm.actor_loc(x)),
                                    None => format!("{v:?}"),
                                };
                                format!("{p}={s}")
                            })
                            .collect();
                        println!("PROBE@{f} {} [{}] at {:?}: {}", vm.object_name(o), vm.state_name(o), vm.actor_loc(o), vals.join("  "));
                    } else {
                        println!("PROBE@{f} {who}: not found");
                    }
                    continue;
                }
                // `@Prop Func`: call Func on the object in the controller's property Prop.
                if let Some(rest) = cmd.trim().strip_prefix('@') {
                    let (prop, func) = rest.split_once(' ').unwrap_or((rest, ""));
                    if let Some(o) = vm.get_prop(pc, prop).and_then(|v| v.as_obj()) {
                        let n0 = vm.log.len();
                        let r = vm.call_event(o, func.trim(), vec![]);
                        println!("EXEC@{f} {}.{func} -> {r:?}", vm.object_name(o));
                        for l in vm.log.iter().skip(n0).take(10) {
                            println!("   | {l}");
                        }
                    }
                    continue;
                }
                let n0 = vm.log.len();
                let e0: u64 = vm.script_errors.values().sum();
                let n_actors = vm.actors.len();
                let ok = exec(vm, pc, cmd.trim());
                let e1: u64 = vm.script_errors.values().sum();
                println!("EXEC@{f} {cmd} -> {ok} (script errors +{})", e1 - e0);
                // Actors the command spawned (Summon etc.), with where they ended up.
                for &a in vm.actors.iter().skip(n_actors).take(8) {
                    if !vm.objects[a].deleted {
                        println!("   spawned {} at {:?}", vm.object_name(a), vm.actor_loc(a));
                        if vm.class_is_a(vm.objects[a].class, "Pawn") || vm.class_is_a(vm.objects[a].class, "KActor") {
                            TRACKED.with(|t| t.borrow_mut().push(a));
                            TRACK_FROM.with(|t| t.set(n_actors));
                        }
                    }
                }
                if std::env::var("P2GAME_FP_TRACE").is_ok() {
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
                    let pend = pawn.and_then(|p| vm.get_prop(p, "PendingWeapon")).and_then(|v| v.as_obj());
                    println!("   pending after exec: {:?} pc state {} errors {:?}", pend.map(|o| vm.object_name(o)), vm.state_name(pc), vm.script_errors.iter().filter(|(_, n)| **n > 0).take(8).collect::<Vec<_>>());
                    let mut cur = pawn.and_then(|p| vm.get_prop(p, "Inventory")).and_then(|v| v.as_obj());
                    while let Some(i) = cur {
                        let cn = vm.classes[vm.objects[i].class].name.clone();
                        if cn == "PistolWeapon" {
                            let r = vm.call_event(i, "HasAmmo", vec![]);
                            let rank = vm.call_event(i, "GetRank", vec![]);
                            println!("   PistolWeapon HasAmmo {r:?} GetRank {rank:?} state {} group {:?}", vm.state_name(i), vm.get_prop(i, "InventoryGroup"));
                        }
                        cur = vm.get_prop(i, "Inventory").and_then(|v| v.as_obj());
                    }
                }
                for l in vm.log.iter().skip(n0).take(20) {
                    println!("   | {l}");
                }
            }
        }
    }
    if std::env::var("P2GAME_PROBE").ok().and_then(|s| s.parse::<u32>().ok()).is_some_and(|p| p == f || (f > p && (f - p) % 60 == 0 && f < p + 600)) {
        probe(&sim.game.world.vm, Some(&sim.game.host), pc, f);
    }
    // `P2GAME_POS_EVERY=N`: the player's position, yaw/pitch, physics and map every N frames
    // (for scripting real-input walks).
    if let Some(n) = std::env::var("P2GAME_POS_EVERY").ok().and_then(|s| s.parse::<u32>().ok()).filter(|n| *n > 0) {
        if f % n == 0 {
            let vm = &sim.game.world.vm;
            let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).filter(|&p| !vm.objects[p].deleted);
            let r = vm.get_prop(pc, "Rotation").map(|v| v.as_rot()).unwrap_or_default();
            let (l, v) = pawn.map(|p| (vm.actor_loc(p), vm.get_prop(p, "Velocity").map(|v| v.as_vec()).unwrap_or_default())).unwrap_or_default();
            println!(
                "POS@{f} map {} loc {:.0} {:.0} {:.0} yaw {} pitch {} speed {:.0} pc {} pawn {:?} interp {:?}/{:?} aTurn {:?} vt {:?}",
                sim.game.host.map_name.borrow(),
                l[0],
                l[1],
                l[2],
                (r[1] & 0xffff),
                r[0] as i16,
                (v[0] * v[0] + v[1] * v[1]).sqrt(),
                vm.state_name(pc),
                pawn.map(|p| vm.state_name(p)),
                vm.get_prop(pc, "bInterpolating").map(|v| v.as_bool()),
                pawn.and_then(|p| vm.get_prop(p, "bInterpolating")).map(|v| v.as_bool()),
                vm.get_prop(pc, "PlayerInput").and_then(|v| v.as_obj()).and_then(|_| vm.get_prop(pc, "aTurn")).map(|v| v.as_float()),
                vm.get_prop(pc, "ViewTarget").and_then(|v| v.as_obj()).map(|o| vm.object_name(o))
            );
            if std::env::var_os("P2GAME_POS_TURN").is_some() {
                let g = |n: &str| vm.get_prop(pc, n).cloned();
                println!("   turn: bTurnToNearest {:?} bTurn180 {:?} bFreeCamera {:?} bBehindView {:?} DesiredRotation {:?} TurnTarget {:?} pawn rot {:?}", g("bTurnToNearest"), g("bTurn180"), g("bFreeCamera"), g("bBehindView"), g("DesiredRotation"), g("TurnTarget"), pawn.map(|p| vm.actor_rot(p)));
            }
        }
    }
}

/// Prints the player's weapon state and inventory chain.
pub(crate) fn probe(vm: &uscript::vm::Vm, host: Option<&p2game::host::GameHost>, pc: ObjId, f: u32) {
    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
    let weap = pawn.and_then(|p| vm.get_prop(p, "Weapon")).and_then(|v| v.as_obj());
    let mut inv = Vec::new();
    let mut cur = pawn.and_then(|p| vm.get_prop(p, "Inventory")).and_then(|v| v.as_obj());
    while let Some(i) = cur {
        if inv.len() > 64 {
            break;
        }
        let c = &vm.classes[vm.objects[i].class].name;
        match (vm.get_prop(i, "InventoryGroup"), vm.get_prop(i, "AmmoType").and_then(|v| v.as_obj())) {
            (Some(g), Some(a)) => inv.push(format!("{c}(g{} a{})", g.as_int(), vm.get_prop(a, "AmmoAmount").map(|v| v.as_int()).unwrap_or(-1))),
            (Some(g), None) => inv.push(format!("{c}(g{} noammo)", g.as_int())),
            _ => inv.push(c.clone()),
        }
        cur = vm.get_prop(i, "Inventory").and_then(|v| v.as_obj());
    }
    let w = weap.map(|w| {
        let ammo = vm.get_prop(w, "AmmoType").and_then(|v| v.as_obj());
        let chan = host.and_then(|h| h.anims.borrow().get(&w).map(|x| format!("{:?} animating {} rate {:?} holster {:?}", x.inst.get_anim_params(0), x.inst.is_animating(0), x.inst.channels.first().map(|c| (c.rate, c.frame, c.playing)), vm.get_prop(w, "WeaponSpeedHolster"))));
        format!(
            "{} state {} mesh {:?} ammo {:?} anim {:?} chan0 {:?}",
            vm.object_name(w),
            vm.state_name(w),
            vm.get_prop(w, "Mesh").and_then(|v| v.as_resource()),
            ammo.and_then(|a| vm.get_prop(a, "AmmoAmount")),
            vm.get_prop(w, "bHidden"),
            chan,
        )
    });
    println!("PROBE@{f} pc {} weapon {w:?} pending {:?} pauser {:?} timedil {:?}", vm.state_name(pc), pawn.and_then(|p| vm.get_prop(p, "PendingWeapon")).and_then(|v| v.as_obj()).map(|o| vm.object_name(o)), vm.level.and_then(|l| vm.get_prop(l, "Pauser")), vm.level.and_then(|l| vm.get_prop(l, "TimeDilation")));
    if let Some((day, errands)) = crate::progress::errand_state(vm) {
        println!("  errands day {day}: {}", errands.iter().map(|(n, c)| format!("{n}{}", if *c { "+" } else { "-" })).collect::<Vec<_>>().join(" "));
    }
    if let Some(w) = weap {
        let p = |n: &str| vm.get_prop(w, n).map(|v| format!("{v:?}")).unwrap_or_else(|| "-".into());
        println!(
            "  muzzle bMuzzleFlash {} bDraw {} MFTexture {} MuzzleScale {} FlashOffset {}/{} bSpawnMF {} MFClass {} MFBone {} ShakeRotMag {} ShakeOffsetMag {} glow {}",
            p("bMuzzleFlash"), p("bDrawMuzzleFlash"), p("MFTexture"), p("MuzzleScale"), p("FlashOffsetX"), p("FlashOffsetY"),
            p("bSpawnMuzzleFlash"), p("MFClass"), p("MFBoneName"), p("ShakeRotMag"), p("ShakeOffsetMag"), p("AmbientGlow")
        );
        let att = vm.get_prop(w, "ThirdPersonActor").and_then(|v| v.as_obj());
        if let Some(a) = att {
            println!("  3p attachment {} MuzzleFlashClass {:?} MuzzleFlash3rd {:?}", vm.object_name(a), vm.get_prop(a, "MuzzleFlashClass"), vm.get_prop(a, "MuzzleFlash3rd").and_then(|v| v.as_obj()).map(|o| (vm.object_name(o), vm.get_prop(o, "StaticMesh").cloned(), vm.get_prop(o, "bHidden").cloned(), vm.state_name(o))));
        }
    }
    let game = vm.level.and_then(|l| vm.get_prop(l, "Game")).and_then(|v| v.as_obj());
    if let Some(p) = pawn {
        println!(
            "  pawn {} health {:?} physics {:?} loc {:?} vel {:?} lasthit {:?}",
            vm.object_name(p),
            vm.get_prop(p, "Health"),
            vm.get_prop(p, "Physics"),
            vm.get_prop(p, "Location"),
            vm.get_prop(p, "Velocity"),
            vm.get_prop(p, "LastDamageType"),
        );
    }
    println!(
        "  cheatmanager {:?} bAllowCManager {:?} bWarnedCheater {:?} log tail {:?}",
        vm.get_prop(pc, "CheatManager"),
        game.and_then(|g| vm.get_prop(g, "bAllowCManager")),
        game.and_then(|g| vm.get_prop(g, "bWarnedCheater")),
        vm.log.iter().rev().filter(|l| !l.contains("Accessed None")).take(4).collect::<Vec<_>>()
    );
    println!("  inventory [{}]", inv.join(", "));
    if let Some(p) = pawn {
        let sel = vm.get_prop(p, "SelectedItem").and_then(|v| v.as_obj());
        println!("  selected item {:?} state {:?}", sel.map(|s| vm.object_name(s)), sel.map(|s| vm.state_name(s)));
    }
    if let Some(ws) = vm.get_prop(pc, "WeaponSelector").and_then(|v| v.as_obj()) {
        let groups: Vec<String> = match vm.get_prop(ws, "WeaponGroups") {
            Some(uscript::vm::Value::Array(a)) => a
                .iter()
                .map(|g| match g {
                    uscript::vm::Value::Struct(f) => {
                        let n = |k: &str| f.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v);
                        let wl = match n("WList") {
                            Some(uscript::vm::Value::Array(w)) => w.iter().filter_map(|x| x.as_obj()).map(|o| vm.classes[vm.objects[o].class].name.clone()).collect::<Vec<_>>().join("/"),
                            _ => String::new(),
                        };
                        format!("{}:{wl}", n("InvGroup").map(|v| v.as_int()).unwrap_or(-1))
                    }
                    _ => "?".into(),
                })
                .collect(),
            _ => Vec::new(),
        };
        println!("  selector {} groups [{}]", vm.object_name(ws), groups.join(", "));
    }
    if let Some(pi) = vm.get_prop(pc, "PlayerInput").and_then(|v| v.as_obj()) {
        println!(
            "  input sens {:?} invert {:?} smoothing {:?} rot {:?}",
            vm.get_prop(pi, "MouseSensitivity"),
            vm.get_prop(pi, "bInvertMouse"),
            vm.get_prop(pi, "MouseSmoothingMode"),
            vm.get_prop(pc, "Rotation").map(|v| v.as_rot())
        );
    }
    if let Some(ws) = vm.get_prop(pc, "WeaponSelector").and_then(|v| v.as_obj()) {
        let groups: Vec<String> = match vm.get_prop(ws, "WeaponGroups") {
            Some(Value::Array(g)) => g
                .iter()
                .map(|e| match e {
                    Value::Struct(fs) => {
                        let ig = fs.iter().find(|(k, _)| k.eq_ignore_ascii_case("InvGroup")).map(|(_, v)| v.as_int()).unwrap_or(-9);
                        let n = fs.iter().find(|(k, _)| k.eq_ignore_ascii_case("WList")).map(|(_, v)| if let Value::Array(a) = v { a.len() } else { 0 }).unwrap_or(0);
                        format!("{ig}:{n}")
                    }
                    _ => "?".into(),
                })
                .collect(),
            other => vec![format!("{other:?}")],
        };
        println!(
            "  selector {} group {:?} wlist {:?} active {:?} visible {:?} state {} groups {:?}",
            vm.object_name(ws),
            vm.get_prop(ws, "SelectedGroupIndex"),
            vm.get_prop(ws, "SelectedWListIndex"),
            vm.get_prop(ws, "bActive"),
            vm.get_prop(ws, "bVisible"),
            vm.state_name(ws),
            groups
        );
    }
    if let Some(s) = vm.get_prop(pc, "CurrentScreen").and_then(|v| v.as_obj()) {
        println!(
            "  screen {} state {} bActive {:?} bVisible {:?} oldstate {:?} EndKeys {:?} bRequiresTick {:?} wantsend {:?} showmsg {:?} endnow {:?} waitend {:?} running {:?}",
            vm.object_name(s),
            vm.state_name(s),
            vm.get_prop(s, "bActive"),
            vm.get_prop(s, "bVisible"),
            vm.get_prop(pc, "MyOldState"),
            vm.get_prop(s, "EndKeys"),
            vm.get_prop(s, "bRequiresTick"),
            vm.get_prop(s, "bPlayerWantsToEnd"),
            vm.get_prop(s, "bShowMsg"),
            vm.get_prop(s, "bEndNow"),
            vm.get_prop(s, "bWaitForEnd"),
            vm.get_prop(s, "bIsRunning"),
        );
    }
}

/// The overlay camera marker.
#[derive(Component)]
pub(crate) struct FpCamera;

/// Render layer and camera order of static meshes drawn by Canvas.DrawActor: above the 2D canvas
/// (order 100), as UE2 draws DrawActor into the frame after the canvas items queued before it.
const CANVAS_MESH_LAYER: usize = 8;

/// Static mesh actors drawn through Canvas.DrawActor this frame (MapScreen HeadMarker /
/// TrailMarker, the "you are here" arrow and its dashes, FIDELITY #49), and their entities.
#[derive(Resource, Default)]
struct CanvasMeshes {
    draws: Vec<ActorDraw>,
    meshes: crate::dynmesh::DynMeshTracker,
    shown: HashMap<ObjId, (String, Vec<Entity>)>,
    camera: Option<Entity>,
    level_key: usize,
}

#[derive(Component)]
struct CanvasMeshCamera;

#[allow(clippy::too_many_arguments)]
fn canvas_meshes(
    mut commands: Commands,
    sim: NonSend<Sim>,
    mut st: ResMut<CanvasMeshes>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut cams: Query<(&mut Transform, &mut Projection, &mut Camera), With<CanvasMeshCamera>>,
    mut tfs: Query<&mut Transform, Without<CanvasMeshCamera>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
) {
    let st = &mut *st;
    let vm = &sim.game.world.vm;
    let level_key = sim.game.level_serial as usize;
    if st.level_key != level_key {
        for (_, (_, es)) in st.shown.drain() {
            for e in es {
                commands.entity(e).despawn();
            }
        }
        st.level_key = level_key;
        st.draws.clear();
    }
    if st.camera.is_none() {
        let e = commands
            .spawn((
                Camera3d::default(),
                Camera { order: 110, clear_color: ClearColorConfig::None, is_active: false, ..default() },
                Projection::Perspective(PerspectiveProjection { fov: 60f32.to_radians(), near: 0.02, far: 100.0, ..default() }),
                Transform::IDENTITY,
                RenderLayers::layer(CANVAS_MESH_LAYER),
                // Canvas props are unlit in UE2's DrawActor of the map screen: full ambient.
                AmbientLight { color: Color::WHITE, brightness: 4000.0, affects_lightmapped_meshes: true },
                bevy::core_pipeline::tonemapping::Tonemapping::None,
                CanvasMeshCamera,
            ))
            .id();
        st.camera = Some(e);
        return;
    }
    let Ok((mut cam_tf, mut proj, mut cam)) = cams.single_mut() else { return };
    let wanted: Vec<(ObjId, f32, String)> = st
        .draws
        .iter()
        .filter_map(|d| {
            let a = d.actor?;
            // No bHidden check: Canvas.DrawActor draws hidden actors (MapScreen's RawActors are
            // bHidden so the world view skips them).
            if vm.objects[a].deleted {
                return None;
            }
            let p = vm.get_prop(a, "StaticMesh").and_then(|v| v.as_resource())?.to_string();
            Some((a, d.fov, p))
        })
        .collect();
    cam.is_active = !wanted.is_empty();
    // Drop entities of actors no longer drawn (or whose mesh changed).
    let keep: Vec<ObjId> = wanted.iter().map(|w| w.0).collect();
    st.shown.retain(|a, (p, es)| {
        let ok = wanted.iter().any(|w| w.0 == *a && w.2 == *p);
        if !ok || !keep.contains(a) {
            for &e in es.iter() {
                commands.entity(e).despawn();
            }
        }
        ok
    });
    if wanted.is_empty() {
        return;
    }
    let c = natives::with_context(|ctx| ctx.camera);
    let fov_h = wanted.iter().map(|w| w.1).find(|f| *f > 1.0).unwrap_or(c.fov).clamp(10.0, 170.0);
    let aspect = windows.single().map(|w| w.width() / w.height().max(1.0)).unwrap_or(16.0 / 9.0);
    if let Projection::Perspective(p) = &mut *proj {
        p.fov = 2.0 * ((fov_h.to_radians() * 0.5).tan() / aspect).atan();
    }
    let r = p2anim::rig::rotator_mat(c.rotation);
    let swap = |v: Vec3| Vec3::new(v.x, v.z, v.y);
    *cam_tf = Transform::from_translation(p2anim::rig::to_bevy(c.location)).looking_to(swap(r.x_axis.truncate()).normalize_or_zero(), swap(r.z_axis.truncate()).normalize_or_zero());
    let cm = c_mat();
    let ci = cm.inverse();
    let lib = &sim.game.host.lib;
    for (a, _, path) in &wanted {
        if !st.shown.contains_key(a) {
            let skins: Vec<Option<upkg::library::Obj>> = match vm.get_prop(*a, "Skins") {
                Some(Value::Array(s)) => s.iter().map(|v| v.as_resource().and_then(|p| resolve_path(lib, p, None))).collect(),
                _ => Vec::new(),
            };
            let darken = canvas_darken(lib, path);
            let es: Vec<Entity> = st
                .meshes
                .sections(lib, path, &skins, &mut meshes, &mut materials, &mut images)
                .unwrap_or_default()
                .into_iter()
                .map(|(m, mat)| {
                    // Unlit copy: the canvas draws these at their texture colour.
                    let mut src = materials.get(&mat).cloned().unwrap_or_default();
                    // Shader OB_Darken (MapScreen's ArrowBox: a cyan arrow on black) multiplies
                    // the frame by (1 - texel): Multiply with an inverted texture copy gives the
                    // red arrow over the map.
                    if darken {
                        let inv = src.base_color_texture.as_ref().and_then(|h| images.get(h)).cloned().map(|mut img| {
                            if let Some(d) = img.data.as_mut() {
                                for p in d.chunks_exact_mut(4) {
                                    for c in &mut p[..3] {
                                        *c = 255 - *c;
                                    }
                                }
                            }
                            img
                        });
                        if let Some(img) = inv {
                            src.base_color_texture = Some(images.add(img));
                            src.alpha_mode = AlphaMode::Multiply;
                        }
                    }
                    let mat = materials.add(StandardMaterial { unlit: true, fog_enabled: false, ..src });
                    commands.spawn((Mesh3d(m), MeshMaterial3d(mat), Transform::from_matrix(cm * actor_world(vm, *a) * ci), RenderLayers::layer(CANVAS_MESH_LAYER))).id()
                })
                .collect();
            if std::env::var_os("P2GAME_FP_TRACE").is_some() {
                println!("canvas mesh {} {path} at {:?}: {} sections", vm.object_name(*a), vm.actor_loc(*a), es.len());
            }
            st.shown.insert(*a, (path.clone(), es));
        }
        let tf = Transform::from_matrix(cm * actor_world(vm, *a) * ci);
        for &e in &st.shown[a].1 {
            if let Ok(mut t) = tfs.get_mut(e) {
                *t = tf;
            }
        }
    }
}

/// True if the static mesh's first material is a Shader with OutputBlending OB_Darken (6).
fn canvas_darken(lib: &upkg::library::Library, path: &str) -> bool {
    (|| {
        let o = resolve_path(lib, path, Some("StaticMesh"))?;
        let sm = upkg::staticmesh::read_static_mesh(&o.pkg, o.export).ok()?;
        let m = sm.materials.first().and_then(|r| lib.resolve(&o.pkg, *r))?;
        Some(m.class().eq_ignore_ascii_case("Shader") && upkg::begin_object(&m.pkg, m.export).ok()?.1.byte("OutputBlending") == Some(6))
    })()
    .unwrap_or(false)
}

/// The view-model key light (FP_LAYER only).
#[derive(Component)]
struct FpKeyLight;

/// View-model lighting, tunable with `P2GAME_FP_AMBIENT` / `P2GAME_FP_KEY`.
fn fp_ambient() -> f32 {
    std::env::var("P2GAME_FP_AMBIENT").ok().and_then(|s| s.parse().ok()).unwrap_or(1000.0)
}

fn fp_key() -> f32 {
    std::env::var("P2GAME_FP_KEY").ok().and_then(|s| s.parse().ok()).unwrap_or(9500.0)
}

/// A first-person rig for one VM actor.
struct FpRig {
    root: Entity,
    joints: Vec<Entity>,
    sections: Vec<Entity>,
    mesh: String,
}

#[derive(Resource, Default)]
struct FpState {
    rigs: HashMap<ObjId, FpRig>,
    /// Actors whose mesh could not be rigged (skip retrying every frame).
    failed: HashMap<ObjId, String>,
    level_key: usize,
    logged: u32,
}

/// True if the level sun reaches the eye at `eye` (Unreal units): nothing solid on a line toward
/// the sun. `dir` is the Bevy light's forward (the direction it shines).
fn sun_reaches(eye: [f32; 3], dir: Vec3) -> bool {
    let Some(w) = p2game::physics::level_collision() else { return true };
    // Bevy (x, y up, z) -> Unreal (x, z, y).
    let to_sun = Vec3::new(-dir.x, -dir.z, -dir.y).normalize_or_zero();
    if to_sun == Vec3::ZERO {
        return true;
    }
    let from = Vec3::from(eye);
    match w.line(from, from + to_sun * 20000.0) {
        None => true,
        Some(h) => !h.start_solid && h.t >= 0.98,
    }
}

#[allow(clippy::too_many_arguments)]
fn fp_weapon(
    mut commands: Commands,
    sim: NonSend<Sim>,
    mut st: ResMut<FpState>,
    mut cache: ResMut<RigCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>,
    mut cams: Query<(&mut Transform, &mut Projection, &mut Camera), With<FpCamera>>,
    mut keys: Query<(&mut Transform, &mut DirectionalLight), (With<FpKeyLight>, Without<FpCamera>)>,
    suns: Query<(&DirectionalLight, &GlobalTransform), Without<FpKeyLight>>,
    mut tfs: Query<&mut Transform, (Without<FpCamera>, Without<FpKeyLight>)>,
    mut vis: Query<&mut Visibility>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut canvas: ResMut<CanvasMeshes>,
) {
    let draws: Vec<ActorDraw> = natives::with_context(|ctx| std::mem::take(&mut ctx.actor_draws));
    // Static mesh DrawActors (MapScreen arrow and trail, NewsScreen paper) go to canvas_meshes,
    // which draws them above the canvas.
    let (statics, draws): (Vec<ActorDraw>, Vec<ActorDraw>) = draws.into_iter().partition(|d| {
        d.actor.is_some_and(|a| {
            let vm = &sim.game.world.vm;
            !vm.objects[a].deleted && matches!(vm.get_prop(a, "DrawType"), Some(Value::Byte(crate::dynmesh::DT_STATIC_MESH)))
        })
    });
    canvas.draws = statics;
    if let Ok(w) = windows.single() {
        WINDOW_SIZE.with(|s| s.set((w.physical_width().max(1), w.physical_height().max(1))));
        p2game::viewport::set_size(w.physical_width(), w.physical_height());
    }
    let vm = &sim.game.world.vm;
    let host = &sim.game.host;

    // New level: old object ids are meaningless (the object table was truncated and refilled), and
    // that includes this frame's DrawActor requests, which the HUD queued before travel ran.
    let level_key = sim.game.level_serial as usize;
    let draws = if st.level_key != level_key {
        for (_, r) in st.rigs.drain() {
            commands.entity(r.root).despawn();
        }
        st.failed.clear();
        st.level_key = level_key;
        Vec::new()
    } else {
        draws
    };

    // Overlay camera (spawned once).
    if cams.is_empty() {
        commands.spawn((
            Camera3d::default(),
            // Above the world (0) and sky (-1), below the 2D canvas (100); no colour clear, fresh depth.
            Camera { order: 50, clear_color: ClearColorConfig::None, ..default() },
            Projection::Perspective(PerspectiveProjection { fov: 60f32.to_radians(), near: 0.02, far: 100.0, ..default() }),
            Transform::IDENTITY,
            RenderLayers::layer(FP_LAYER),
            // UE2 lights the view model with its AmbientGlow (128 = half bright, shown at about the
            // texture's own colour), not the zone's tinted ambient: neutral ambient plus a soft
            // key light that only this layer sees.
            AmbientLight { color: Color::WHITE, brightness: fp_ambient(), affects_lightmapped_meshes: true },
            FpCamera,
        ));
        commands.spawn((
            DirectionalLight { illuminance: fp_key(), shadow_maps_enabled: false, ..default() },
            Transform::from_xyz(0.0, 0.0, 0.0).looking_to(Vec3::new(-0.3, -0.6, -1.0), Vec3::Y),
            RenderLayers::layer(FP_LAYER),
            FpKeyLight,
        ));
        return;
    }
    let Ok((mut cam_tf, mut proj, mut cam)) = cams.single_mut() else { return };

    let wanted: Vec<(ObjId, f32)> = draws.iter().filter_map(|d| d.actor.map(|a| (a, d.fov))).filter(|(a, _)| !vm.objects[*a].deleted).collect();
    if std::env::var_os("P2GAME_FP_TRACE").is_some() && st.logged < 3 && !draws.is_empty() {
        st.logged += 1;
        for d in &draws {
            let a = d.actor.map(|a| (vm.object_name(a), vm.get_prop(a, "Mesh").cloned(), vm.actor_loc(a)));
            println!("FP DrawActor {a:?} fov {} clearz {}", d.fov, d.clear_z);
        }
    }
    cam.is_active = !wanted.is_empty() && std::env::var_os("P2GAME_NO_FP").is_none();

    // Camera: the canvas camera (the player's eye and view rotation) with the weapon's DisplayFOV
    // (horizontal degrees, as UE2), converted to Bevy's vertical FOV.
    let c = natives::with_context(|ctx| ctx.camera);
    let fov_h = wanted.iter().map(|w| w.1).find(|f| *f > 1.0).unwrap_or(c.fov).clamp(10.0, 170.0);
    let aspect = windows.single().map(|w| w.width() / w.height().max(1.0)).unwrap_or(16.0 / 9.0);
    let fov_v = 2.0 * ((fov_h.to_radians() * 0.5).tan() / aspect).atan();
    if let Projection::Perspective(p) = &mut *proj {
        p.fov = fov_v;
    }
    // Unreal view: X forward, Z up (the rotator's axes), mapped to Bevy (swap Y/Z). The c_mat swap is
    // a reflection, so the axes are mapped directly rather than decomposing a matrix.
    let r = p2anim::rig::rotator_mat(c.rotation);
    let swap = |v: Vec3| Vec3::new(v.x, v.z, v.y);
    let pos = p2anim::rig::to_bevy(c.location);
    let fwd = swap(r.x_axis.truncate()).normalize_or_zero();
    let up = swap(r.z_axis.truncate()).normalize_or_zero();
    *cam_tf = Transform::from_translation(pos).looking_to(fwd, up);
    // The key light: the level sun's direction when it reaches the eye, else from above, slightly
    // left of and behind the eye.
    if let Ok((mut ltf, mut light)) = keys.single_mut() {
        let right = fwd.cross(up).normalize_or_zero();
        let sun = suns.iter().max_by(|a, b| a.0.illuminance.total_cmp(&b.0.illuminance));
        // UE2 lights the view model like any actor, by the level's lights in world space: the sun
        // shines from its own direction, so facing the sun (suburbs-3 start, orig_pfe_00: palm
        // 136,86,61, slide dark steel) the visible side of the gun and hands is in their own
        // shade, and with the sun behind (squid's fo.png, palm 219,129,87) it is lit. A light
        // fixed above-behind the eye lit the visible side always (FIDELITY #15, flat and bright).
        // Where the sun does not reach the eye (indoors, no Sunlight) the earlier view-fixed key
        // stays (stand-in for the room's point lights; no original indoor reference yet).
        let sun_dir = sun.map(|s| s.1.forward().as_vec3()).filter(|d| std::env::var_os("P2GAME_FP_VIEW_KEY").is_none() && sun_reaches(c.location, *d));
        let key_dir = sun_dir.unwrap_or(fwd * 0.8 - up * 0.6 + right * 0.3);
        *ltf = Transform::from_translation(pos).looking_to(key_dir, if key_dir.normalize_or_zero().y.abs() > 0.99 { fwd } else { Vec3::Y });
        // UE2 lights the view model with the level's lights like any actor: outdoors the warm
        // sun colours the hands (FIDELITY #15). Take the colour of the level's brightest sun.
        let color = sun.map(|s| s.0.color).unwrap_or(Color::WHITE);
        if light.color != color {
            light.color = color;
        }
    }
    if std::env::var_os("P2GAME_FP_TRACE").is_some() && st.logged < 6 && !wanted.is_empty() {
        st.logged += 1;
        let a = wanted[0].0;
        println!(
            "FP cam ue {:?} rot {:?} -> bevy {pos:?} fwd {fwd:?}; weapon ue {:?} rot {:?} scale {:?}; fov_h {fov_h} fov_v {:.1}",
            c.location,
            c.rotation,
            vm.actor_loc(a),
            vm.actor_rot(a),
            vm.get_prop(a, "DrawScale"),
            fov_v.to_degrees()
        );
    }

    // Rigs for the drawn actors.
    let mut assets = crate::rigs::rig_assets(&mut meshes, &mut materials, &mut images, &mut bindposes);
    for &(a, _) in &wanted {
        let Some(mesh_path) = vm.get_prop(a, "Mesh").and_then(|v| v.as_resource()).map(str::to_string) else {
            if !st.failed.contains_key(&a) {
                println!("FP rig FAILED for {}: no Mesh ({:?})", vm.object_name(a), vm.get_prop(a, "Mesh"));
                st.failed.insert(a, String::new());
            }
            continue;
        };
        if st.rigs.get(&a).is_some_and(|r| r.mesh.eq_ignore_ascii_case(&mesh_path)) || st.failed.get(&a).is_some_and(|m| m.eq_ignore_ascii_case(&mesh_path)) {
            continue;
        }
        if let Some(old) = st.rigs.remove(&a) {
            commands.entity(old.root).despawn();
        }
        if !host.ensure(vm, a) {
            println!("FP rig FAILED for {} ({mesh_path}): skeletal mesh not resolved", vm.object_name(a));
            st.failed.insert(a, mesh_path);
            continue;
        }
        let Some(mesh) = host.anims.borrow().get(&a).map(|x| x.mesh.clone()) else { continue };
        let skins: Vec<Option<upkg::library::Obj>> = match vm.get_prop(a, "Skins") {
            Some(Value::Array(s)) => s.iter().map(|v| v.as_resource().and_then(|p| resolve_path(&host.lib, p, None))).collect(),
            _ => Vec::new(),
        };
        let Some(rig) = cache.rig(&host.lib, &mut assets, &mesh.obj, &skins) else {
            println!("FP rig FAILED for {} ({mesh_path}): rig build failed", vm.object_name(a));
            st.failed.insert(a, mesh_path);
            continue;
        };
        let part = cache.spawn_part(&mut commands, &host.lib, &rig, &[]);
        let root = commands.spawn((Transform::IDENTITY, Visibility::default())).id();
        for &e in part.joints.iter().chain(&part.sections) {
            commands.entity(e).insert(ChildOf(root));
        }
        for &s in &part.sections {
            commands.entity(s).insert(RenderLayers::layer(FP_LAYER));
        }
        println!("FP rig for {} ({mesh_path})", vm.object_name(a));
        st.rigs.insert(a, FpRig { root, joints: part.joints, sections: part.sections, mesh: mesh_path });
    }

    // Pose and show the drawn ones; hide the rest.
    let cm = c_mat();
    let ci = cm.inverse();
    let mut anims = host.anims.borrow_mut();
    for (a, r) in st.rigs.iter() {
        let drawn = wanted.iter().any(|w| w.0 == *a);
        if let Ok(mut v) = vis.get_mut(r.root) {
            let want = if drawn { Visibility::Inherited } else { Visibility::Hidden };
            if *v != want {
                *v = want;
            }
        }
        if !drawn {
            continue;
        }
        let Some(x) = anims.get_mut(a) else { continue };
        let to_world = p2game::host::skel_actor_world(vm, *a, x.inst.skeleton.mesh_xform) * x.inst.skeleton.mesh_xform;
        for (e, m) in r.joints.iter().zip(x.inst.bone_matrices()) {
            if let Ok(mut tf) = tfs.get_mut(*e) {
                *tf = Transform::from_matrix(cm * to_world * *m * ci);
            }
        }
        let _ = &r.sections;
    }
}
