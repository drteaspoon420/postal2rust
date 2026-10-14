//! `P2GAME_PILOT`: plays the game through the same synthetic OS input as `P2GAME_REAL` (Bevy
//! keyboard / mouse messages, so bindings, the console, menus and the HUD see a player), but
//! closed-loop: it reads where the player is and steers along the map's navigation network to
//! named actors. No cheats or script calls; only keys, mouse motion and clicks.
//!
//! Spec: `;`-separated steps run in order:
//! - `wait N`: N frames
//! - `key K` / `down K` / `up K`: a key press (Bevy KeyCode names as in P2GAME_REAL)
//! - `click X,Y`: move the cursor to X,Y and left-click
//! - `go KEY [R]`: walk to the actor KEY (Tag, object name or class, `+Class` = newest) until within
//!   R units (default 70), or until the level changes (portals). `go X,Y,Z [R]` walks to a point.
//! - `face KEY`: turn toward KEY
//! - `skip N`: press Space every 30 frames for N frames (cutscenes, screens)
//! - `map NAME`: wait until the level is NAME
//! - `look P`: set the view pitch (Unreal units, + is up) with mouse motion
//!
//! Progress goes to stdout as `PILOT@frame ...` lines.

use crate::window::Sim;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::input::mouse::{MouseButtonInput, MouseMotion};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use uscript::nav::reach;
use uscript::vm::Vm;

/// Yaw units per pixel of mouse motion at the default sensitivity (measured: 100 px = 4325).
const YAW_PER_PX: f32 = 43.25;

pub(crate) struct PilotPlugin;

impl Plugin for PilotPlugin {
    fn build(&self, app: &mut App) {
        if let Ok(spec) = std::env::var("P2GAME_PILOT") {
            let steps: Vec<String> = spec.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            app.insert_non_send(Pilot { steps, best_dist: f32::MAX, weapon_cycle: std::env::var_os("P2GAME_PILOT_CYCLE").is_some(), ..Default::default() });
            app.add_systems(PreUpdate, pilot.before(bevy::input::InputSystems));
        }
    }
}

#[derive(Default)]
struct Pilot {
    steps: Vec<String>,
    step: usize,
    /// Frames spent in the current step.
    t: u32,
    frame: u32,
    held: Vec<KeyCode>,
    path: Vec<[f32; 3]>,
    repath_at: u32,
    last_pos: [f32; 3],
    stuck_check_at: u32,
    stuck: u32,
    /// Closest distance to the current `go` target so far.
    best_dist: f32,
    map: String,
    unstick_until: u32,
    every: u32,
    every_keys: Vec<KeyCode>,
    shots: Vec<String>,
    /// `weapon` also cycles the number keys (P2GAME_PILOT_CYCLE=1); else it only verifies.
    weapon_cycle: bool,
    done: bool,
}

fn keycode(n: &str) -> Option<KeyCode> {
    let n = n.trim();
    Some(match n {
        "Space" => KeyCode::Space,
        "Enter" => KeyCode::Enter,
        "Escape" | "Esc" => KeyCode::Escape,
        "Tab" => KeyCode::Tab,
        "ShiftLeft" | "Shift" => KeyCode::ShiftLeft,
        "ControlLeft" | "Ctrl" => KeyCode::ControlLeft,
        "BracketLeft" | "[" => KeyCode::BracketLeft,
        "BracketRight" | "]" => KeyCode::BracketRight,
        "F3" => KeyCode::F3,
        "F10" => KeyCode::F10,
        "F11" => KeyCode::F11,
        "Delete" | "Del" => KeyCode::Delete,
        "End" => KeyCode::End,
        "ArrowUp" | "Up" => KeyCode::ArrowUp,
        _ if n.len() == 1 => {
            let c = n.chars().next()?.to_ascii_uppercase();
            match c {
                'A'..='Z' => {
                    const K: [KeyCode; 26] = [
                        KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE, KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI,
                        KeyCode::KeyJ, KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO, KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR,
                        KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX, KeyCode::KeyY, KeyCode::KeyZ,
                    ];
                    K[(c as u8 - b'A') as usize]
                }
                '0'..='9' => {
                    const D: [KeyCode; 10] = [
                        KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9,
                    ];
                    D[(c as u8 - b'0') as usize]
                }
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn d2(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Walkable route (node locations) from `from` to `to` over the level's navigation network:
/// Dijkstra between the nodes nearest to each end (same floor preferred).
fn route(vm: &Vm, from: [f32; 3], to: [f32; 3]) -> Vec<[f32; 3]> {
    let Some(g) = vm.nav.graph.as_ref() else { return vec![to] };
    let n = g.node_count();
    if n == 0 {
        return vec![to];
    }
    // Candidate end nodes near each end (same floor preferred): the nearest node can sit across
    // a fence from the player, so several are tried.
    let near = |p: [f32; 3], k: usize| {
        let mut v: Vec<(u32, f32)> = (0..n as u32)
            .map(|i| {
                let l = g.node_location(i);
                let dz = (l[2] - p[2]).abs();
                (i, d2(l, p) + if dz > 120.0 { dz * 4.0 } else { 0.0 })
            })
            .collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
        v.into_iter().take(k).collect::<Vec<_>>()
    };
    let starts = near(from, 8);
    let ends = near(to, 8);
    // Prefer end nodes the pawn can walk to / from (the AI reachability walk against the level and
    // static blockers): the nearest node can sit behind glass (suburbs-3 laundromat storefront,
    // BlockingVolume106 at -4030,-11188 with PathNode257 115 UU inside), and `go` walked into the
    // window. Falls back to the plain nearest nodes.
    let walkable = |a: [f32; 3], b: [f32; 3]| p2game::physics::current().and_then(|ph| ph.world.try_borrow_mut().ok().map(|mut w| matches!(p2game::navfix::walk_test(&mut *w, Vec3::from(a), Vec3::from(b), 25.0, 69.0, 35.0), p2game::navfix::Walk::Reached))).unwrap_or(true);
    let keep = |v: Vec<(u32, f32)>, f: &dyn Fn(u32) -> bool| -> Vec<(u32, f32)> { let ok: Vec<(u32, f32)> = v.iter().copied().filter(|&(k, _)| f(k)).collect(); if ok.is_empty() { v } else { ok } };
    let starts = keep(starts, &|k| walkable(from, g.node_location(k)));
    let ends = keep(ends, &|k| walkable(g.node_location(k), to));
    if starts.is_empty() || ends.is_empty() {
        return vec![to];
    }
    let mut dist = vec![i64::MAX; n];
    let mut prev = vec![u32::MAX; n];
    let mut heap = std::collections::BinaryHeap::new();
    for &(s, d) in &starts {
        dist[s as usize] = d as i64;
        heap.push(std::cmp::Reverse((d as i64, s)));
    }
    let is_end = |k: u32| ends.iter().find(|e| e.0 == k).map(|e| e.1 as i64);
    let mut best: Option<(i64, u32)> = None;
    while let Some(std::cmp::Reverse((w, k))) = heap.pop() {
        if w > dist[k as usize] {
            continue;
        }
        if best.is_some_and(|b| w > b.0) {
            break;
        }
        if let Some(extra) = is_end(k) {
            if best.is_none_or(|b| w + extra < b.0) {
                best = Some((w + extra, k));
            }
        }
        for ed in g.edges(k) {
            let f = ed.reach_flags;
            if f & (reach::PROSCRIBED | reach::FLY | reach::SWIM | reach::LADDER) != 0 || ed.collision_radius < 25 || ed.collision_height < 40 {
                continue;
            }
            let nw = w + ed.distance.max(1) as i64;
            if nw < dist[ed.end as usize] {
                dist[ed.end as usize] = nw;
                prev[ed.end as usize] = k;
                heap.push(std::cmp::Reverse((nw, ed.end)));
            }
        }
    }
    let Some((_, e)) = best else { return vec![to] };
    let mut out = vec![to];
    let mut k = e;
    while k != u32::MAX {
        out.push(g.node_location(k));
        k = prev[k as usize];
    }
    out.reverse();
    // Drop leading nodes we are already past (closer to the second node than the first is).
    while out.len() > 2 && d2(from, out[1]) < d2(out[0], out[1]) {
        out.remove(0);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn pilot(
    mut p: NonSendMut<Pilot>,
    mut sim: NonSendMut<Sim>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    mut keys: ResMut<Messages<KeyboardInput>>,
    mut buttons: ResMut<Messages<MouseButtonInput>>,
    mut motion: ResMut<Messages<MouseMotion>>,
    mut commands: Commands,
) {
    for path in std::mem::take(&mut p.shots) {
        if let Some(dir) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(bevy::render::view::screenshot::save_to_disk(std::path::PathBuf::from(path)));
    }
    if std::env::var_os("P2GAME_REAL").is_none() {
        keys.drain().for_each(drop);
        buttons.drain().for_each(drop);
        motion.drain().for_each(drop);
    }
    let Ok((window, mut win)) = windows.single_mut() else { return };
    p.frame += 1;
    let f = p.frame;
    if p.done {
        return;
    }
    let key = |k: KeyCode, down: bool| KeyboardInput {
        key_code: k,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state: if down { ButtonState::Pressed } else { ButtonState::Released },
        text: None,
        repeat: false,
        window,
    };
    // `exec CMD`: test setup through the console exec chain (summon, cheats), not player input.
    if let Some(cmd) = p.steps.get(p.step).and_then(|s| s.strip_prefix("exec ")).map(str::to_string) {
        if let Some(pc) = sim.game.player_controller {
            let ok = crate::frontend::exec_command(&mut sim.game.world.vm, pc, &cmd);
            println!("PILOT@{f} exec {cmd} -> {ok}");
        }
        p.step += 1;
        p.t = 0;
        return;
    }
    let vm = &sim.game.world.vm;
    let map = sim.game.host.map_name.borrow().to_ascii_lowercase();
    let level_changed = p.map != map;
    if level_changed {
        println!("PILOT@{f} level {map}");
        p.map = map.clone();
        p.path.clear();
    }
    // Taps pressed last frame are released this frame (unless still wanted as a hold).
    let tap_release: Vec<KeyCode> = std::mem::take(&mut p.held);
    let mut want_held: Vec<KeyCode> = Vec::new();

    let Some(step) = p.steps.get(p.step).cloned() else {
        println!("PILOT@{f} done");
        for k in tap_release {
            keys.write(key(k, false));
        }
        p.done = true;
        return;
    };
    p.t += 1;
    let t = p.t;
    let (verb, arg) = step.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((step.as_str(), ""));
    let mut next = false;
    let pc = sim.game.player_controller;
    let pawn = pc.and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj()).filter(|&a| !vm.objects[a].deleted);
    let loc = pawn.map(|a| vm.actor_loc(a)).unwrap_or_default();
    let yaw = pc.and_then(|c| vm.get_prop(c, "Rotation")).map(|v| v.as_rot()[1]).unwrap_or(0) & 0xffff;
    let pitch = pc.and_then(|c| vm.get_prop(c, "Rotation")).map(|v| v.as_rot()[0] as i16 as i32).unwrap_or(0);
    let turn_to = |target: [f32; 3], motion: &mut Messages<MouseMotion>| -> i32 {
        let want = ((target[1] - loc[1]).atan2(target[0] - loc[0]) / std::f32::consts::TAU * 65536.0) as i32 & 0xffff;
        let mut d = want - yaw;
        if d > 32768 {
            d -= 65536;
        }
        if d < -32768 {
            d += 65536;
        }
        let px = (d as f32 / YAW_PER_PX).clamp(-60.0, 60.0);
        if px.abs() >= 0.5 {
            motion.write(MouseMotion { delta: Vec2::new(px, 0.0) });
        }
        d
    };
    match verb {
        "list" => {
            // `list CLASS`: print every live actor of that class, nearest first.
            let mut v: Vec<(f32, String, [f32; 3])> = vm
                .actors
                .iter()
                .copied()
                .filter(|&a| !vm.objects[a].deleted && vm.classes[vm.objects[a].class].name.eq_ignore_ascii_case(arg))
                .map(|a| (d2(loc, vm.actor_loc(a)), vm.object_name(a), vm.actor_loc(a)))
                .collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (d, n, l) in v {
                println!("PILOT@{f} list {n} at {:.0},{:.0},{:.0} d {d:.0}", l[0], l[1], l[2]);
            }
            next = true;
        }
        "wait" => next = t >= arg.parse().unwrap_or(30),
        "every" => {
            // `every N K1,K2,..`: from now on tap the keys in turn every N frames during the
            // following steps (a player jumping/kicking while walking). `every 0` stops.
            let (n, ks) = arg.split_once(' ').unwrap_or((arg, ""));
            p.every = n.parse().unwrap_or(0);
            p.every_keys = ks.split(',').filter_map(keycode).collect();
            next = true;
        }
        "key" | "down" | "up" => {
            if let Some(k) = keycode(arg) {
                match verb {
                    "key" => {
                        keys.write(key(k, true));
                        p.held.push(k);
                    }
                    "down" => {
                        keys.write(key(k, true));
                    }
                    _ => {
                        keys.write(key(k, false));
                    }
                }
            }
            next = true;
        }
        "weapon" => {
            // `weapon CLASS`: press number keys (1..9, 0, then each again) until the pawn's
            // Weapon is of class CLASS, as a player would.
            let cur = pawn.and_then(|a| vm.get_prop(a, "Weapon")).and_then(|v| v.as_obj()).map(|o| vm.classes[vm.objects[o].class].name.clone());
            if cur.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(arg)) {
                println!("PILOT@{f} weapon {arg}: selected");
                next = true;
            } else if t > 25 * 30 || (t > 120 && !p.weapon_cycle) {
                println!("PILOT@{f} weapon {arg}: NOT HELD (have {cur:?})");
                next = true;
            } else if p.weapon_cycle && t % 25 == 1 {
                const DIG: [KeyCode; 10] = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9, KeyCode::Digit0];
                let k = DIG[((t / 25) as usize) % 10];
                keys.write(key(k, true));
                p.held.push(k);
            }
        }
        "shot" => {
            // `shot PATH`: screenshot plus a SHOTSTATE line (time, states) for contact sheets.
            let pawn_s = pawn.map(|a| vm.state_name(a)).unwrap_or_default();
            let wep = pawn.and_then(|a| vm.get_prop(a, "Weapon")).and_then(|v| v.as_obj());
            let foot = pawn.and_then(|a| vm.get_prop(a, "MyFoot")).and_then(|v| v.as_obj());
            let phys = pawn.and_then(|a| vm.get_prop(a, "Physics")).map(|v| v.as_int()).unwrap_or(-1);
            let tsec = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|v| v.as_float()).unwrap_or(0.0);
            let pcs = pc.map(|c| vm.state_name(c)).unwrap_or_default();
            println!(
                "SHOTSTATE {arg} t={tsec:.2} pc {pcs} pawn {pawn_s} phys {phys} weapon {} {} foot {}",
                wep.map(|w| vm.object_name(w)).unwrap_or_default(),
                wep.map(|w| vm.state_name(w)).unwrap_or_default(),
                foot.map(|w| vm.state_name(w)).unwrap_or_default()
            );
            p.shots.push(arg.to_string());
            next = true;
        }
        "mdown" | "mup" => {
            // Left mouse button down/up in place (Fire) without moving the cursor.
            let state = if verb == "mdown" { ButtonState::Pressed } else { ButtonState::Released };
            buttons.write(MouseButtonInput { button: MouseButton::Left, state, window });
            next = true;
        }
        "click" => {
            let (x, y) = arg.split_once(',').map(|(a, b)| (a.trim().parse().unwrap_or(0.0), b.trim().parse().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
            if t == 1 {
                win.set_cursor_position(Some(Vec2::new(x, y)));
            } else if t == 3 {
                buttons.write(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Pressed, window });
            } else if t == 5 {
                buttons.write(MouseButtonInput { button: MouseButton::Left, state: ButtonState::Released, window });
                next = true;
            }
        }
        "skip" => {
            if t % 30 == 1 {
                keys.write(key(KeyCode::Space, true));
                p.held.push(KeyCode::Space);
            }
            next = t >= arg.parse().unwrap_or(300);
        }
        "select" => {
            // Cycle the inventory (InventoryNext, `]`) until the HUD's selected item is of class
            // ARG, as a player watching the HUD would.
            let sel = pawn.and_then(|a| vm.get_prop(a, "SelectedItem")).and_then(|v| v.as_obj()).map(|o| vm.classes[vm.objects[o].class].name.clone());
            if sel.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(arg)) {
                println!("PILOT@{f} select {arg}: selected");
                next = true;
            } else if t > 20 * 12 {
                println!("PILOT@{f} select {arg}: not found (selected {sel:?})");
                next = true;
            } else if t % 20 == 1 {
                keys.write(key(KeyCode::BracketRight, true));
                p.held.push(KeyCode::BracketRight);
            }
        }
        "map" => next = map.eq_ignore_ascii_case(arg),
        "queue" => {
            // `queue QStartPointN`: wait in that line like a player: stand ~90 uu behind the
            // pawn ahead (nearest pawn closer to the line start), step up as it moves, done when
            // a cashier hooks the player (PlayerController.InterestPawn set) or after 10 min.
            let qp = p2game::demo::find_actor(vm, arg);
            let hooked = pc.and_then(|c| vm.get_prop(c, "InterestPawn")).and_then(|v| v.as_obj()).is_some();
            if hooked || qp.is_none() || t > 30 * 600 {
                println!("PILOT@{f} queue {arg}: {}", if hooked { "hooked by cashier" } else if qp.is_none() { "no such QStartPoint" } else { "timed out" });
                next = true;
            } else if let (Some(q), Some(me)) = (qp, pawn) {
                let front = vm.actor_loc(q);
                let my_d = d2(loc, front);
                let ahead = vm
                    .actors
                    .iter()
                    .copied()
                    .filter(|&a| a != me && !vm.objects[a].deleted && vm.get_prop(a, "Health").is_some_and(|h| h.as_int() > 0) && vm.get_prop(a, "Controller").and_then(|c| c.as_obj()).is_some())
                    .filter(|&a| vm.get_prop(a, "bIsPlayer").is_some() || vm.get_prop(a, "MyBodyFire").is_some())
                    .map(|a| (a, vm.actor_loc(a)))
                    .filter(|(_, l)| d2(*l, front) < my_d - 20.0 && d2(*l, loc) < 400.0 && (l[2] - loc[2]).abs() < 100.0)
                    .min_by(|a, b| d2(a.1, loc).total_cmp(&d2(b.1, loc)));
                let (goal, gap) = match ahead {
                    Some((_, l)) => (l, 90.0),
                    None => (front, 25.0),
                };
                if d2(loc, goal) > gap {
                    turn_to(goal, &mut motion);
                    want_held.push(KeyCode::KeyW);
                } else if let Some(q2) = Some(front) {
                    turn_to(q2, &mut motion);
                }
                if t % 150 == 1 {
                    println!("PILOT@{f} queue {arg}: {:.0} from front, ahead {:?}", my_d, ahead.map(|(a, _)| vm.object_name(a)));
                }
            }
        }
        "look" => {
            let want: i32 = arg.parse().unwrap_or(0);
            let d = want - pitch;
            if d.abs() < 200 || t > 60 {
                next = true;
            } else {
                motion.write(MouseMotion { delta: Vec2::new(0.0, -(d as f32 / YAW_PER_PX).clamp(-40.0, 40.0)) });
            }
        }
        "face" => {
            // `face KEY` or `face X,Y,Z`.
            let fv: Vec<f32> = arg.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            let at = if fv.len() == 3 { Some([fv[0], fv[1], fv[2]]) } else { p2game::demo::find_actor(vm, arg).map(|x| vm.actor_loc(x)) };
            if let Some(at) = at {
                let d = turn_to(at, &mut motion);
                next = d.abs() < 300 || t > 120;
            } else {
                println!("PILOT@{f} face {arg}: not found");
                next = true;
            }
        }
        "go" => {
            let (k, r) = arg.split_once(' ').map(|(a, b)| (a, b.trim().parse().unwrap_or(70.0))).unwrap_or((arg, 70.0));
            let point: Option<[f32; 3]> = {
                let v: Vec<f32> = k.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                (v.len() == 3).then(|| [v[0], v[1], v[2]])
            };
            // `Class~N`: the Nth live actor of that class (doors, pickups of one kind).
            let nth = |vm: &Vm| {
                let (c, i) = k.split_once('~')?;
                let i: usize = i.parse().ok()?;
                vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.classes[vm.objects[a].class].name.eq_ignore_ascii_case(c)).nth(i)
            };
            let target_of = |vm: &Vm| point.or_else(|| nth(vm).or_else(|| p2game::demo::find_actor(vm, k)).map(|x| vm.actor_loc(x)));
            if level_changed && t > 1 {
                println!("PILOT@{f} go {k}: level changed to {map}");
                next = true;
            } else if let (Some(_), Some(target)) = (pawn, target_of(vm)) {
                if p.best_dist == f32::MAX {
                    p.best_dist = d2(loc, target) + 1000.0;
                }
                if d2(loc, target) <= r && (loc[2] - target[2]).abs() < 150.0 {
                    println!("PILOT@{f} go {k}: arrived at {:.0} {:.0} {:.0}", loc[0], loc[1], loc[2]);
                    next = true;
                } else {
                    if p.path.is_empty() || f >= p.repath_at {
                        p.path = route(vm, loc, target);
                        p.repath_at = f + 90;
                        if t == 1 {
                            println!("PILOT@{f} go {k} ({:.0} away): {} waypoints", d2(loc, target), p.path.len());
                        }
                    }
                    while p.path.len() > 1 && d2(loc, p.path[0]) < 60.0 {
                        p.path.remove(0);
                    }
                    let mut wp = p.path.first().copied().unwrap_or(target);
                    // Near the goal but blocked (desk, chair): try approach points around it.
                    if p.path.len() <= 1 && p.stuck > 0 {
                        let a = p.stuck as f32 * 1.1;
                        wp = [target[0] + 70.0 * a.cos(), target[1] + 70.0 * a.sin(), target[2]];
                    }
                    let d = turn_to(wp, &mut motion);
                    // Stuck: no progress toward the target for 3 s (moving in circles counts).
                    if f >= p.stuck_check_at {
                        let dt = d2(loc, target);
                        if dt > p.best_dist - 40.0 && t > 90 {
                            p.stuck += 1;
                            p.unstick_until = f + 25;
                            p.repath_at = f;
                            println!("PILOT@{f} go {k}: stuck at {:.0} {:.0} {:.0}, {dt:.0} away (x{})", loc[0], loc[1], loc[2], p.stuck);
                        }
                        p.best_dist = p.best_dist.min(dt);
                        p.last_pos = loc;
                        p.stuck_check_at = f + 90;
                    }
                    if f < p.unstick_until {
                        // Jump and strafe out of whatever we are stuck on.
                        want_held.push(if p.stuck % 2 == 0 { KeyCode::KeyA } else { KeyCode::KeyD });
                        if f + 24 == p.unstick_until {
                            keys.write(key(KeyCode::Space, true));
                            p.held.push(KeyCode::Space);
                        }
                    }
                    if d.abs() < 6000 {
                        want_held.push(KeyCode::KeyW);
                    }
                    if t > 30 * 60 * 4 {
                        println!("PILOT@{f} go {k}: giving up");
                        next = true;
                    }
                }
            } else if t > 1 && p.best_dist < f32::MAX && pawn.is_some() {
                // The target existed and is gone (a pickup was picked up).
                println!("PILOT@{f} go {k}: target gone");
                next = true;
            } else if t % 120 == 1 {
                println!("PILOT@{f} go {k}: no target or pawn yet");
                if t > 30 * 60 {
                    next = true;
                }
            }
        }
        _ => {
            println!("PILOT@{f} bad step {step:?}");
            next = true;
        }
    }
    if p.every > 0 && !p.every_keys.is_empty() && f % p.every == 0 && !(verb == "key" || verb == "skip" || verb == "select") {
        let k = p.every_keys[(f / p.every) as usize % p.every_keys.len()];
        if !p.held.contains(&k) && !want_held.contains(&k) {
            keys.write(key(k, true));
            p.held.push(k);
        }
    }
    // Key holds: press keys newly wanted, release the rest (taps from last frame included).
    for k in tap_release {
        if !want_held.contains(&k) {
            keys.write(key(k, false));
        }
    }
    for &k in &want_held {
        if !WALK_HELD.with(|h| h.borrow().contains(&k)) {
            keys.write(key(k, true));
        }
    }
    WALK_HELD.with(|h| {
        let mut h = h.borrow_mut();
        for k in h.iter() {
            if !want_held.contains(k) {
                keys.write(key(*k, false));
            }
        }
        *h = want_held.clone();
    });
    if next {
        println!("PILOT@{f} step {} done: {step}", p.step);
        p.step += 1;
        p.t = 0;
        p.path.clear();
        p.stuck = 0;
        p.best_dist = f32::MAX;
    }
}

thread_local! {
    static WALK_HELD: std::cell::RefCell<Vec<KeyCode>> = const { std::cell::RefCell::new(Vec::new()) };
}
