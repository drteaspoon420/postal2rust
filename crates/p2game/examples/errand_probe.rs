//! Day errand probe: boots a map as single player (day 1 unless `--day N`), prints the current
//! day's errands and goals, and the actors involved (errand pickups, store/bank cashiers, police).
//! With `--watch S` it then runs S seconds and reports errand state changes as script sets them.
//! `--steps "T:goto Tag;T:exec Cmd;T:use;T:near Class"` drives the player at T seconds: `goto`
//! teleports the pawn next to the first actor with that Tag (or class / object name), `exec` runs
//! a console command for the player, `use` presses Use (InventoryActivate/Use exec), `face` turns
//! the player toward the last goto target.
//! `errand_probe [Map] [--day N] [--watch S] [--steps ...]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::host::Host;
use uscript::vm::{ObjId, Value, Vm};

fn props(vm: &Vm, o: ObjId, names: &[&str]) -> String {
    names
        .iter()
        .filter_map(|n| {
            let v = vm.get_prop(o, n)?;
            let s = match v {
                Value::Object(Some(x)) => vm.object_name(*x),
                Value::Object(None) => return None,
                Value::Str(s) if s.is_empty() => return None,
                Value::Name(s) if s.is_empty() || s == "None" => return None,
                Value::Str(s) | Value::Name(s) => s.clone(),
                Value::Class(Some(c)) => format!("class {}", vm.classes[*c].name),
                other => format!("{other:?}"),
            };
            Some(format!("{n}={s}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn objs(v: Option<&Value>) -> Vec<ObjId> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_obj()).collect(),
        _ => Vec::new(),
    }
}

pub fn errand_state(vm: &Vm, game: ObjId) -> Vec<(String, bool, bool)> {
    let Some(day) = current_day(vm, game) else { return Vec::new() };
    objs(vm.get_prop(day, "Errands")).into_iter().map(|e| (vm.get_prop(e, "UniqueName").map(|v| v.as_str()).unwrap_or_default(), vm.get_prop(e, "bActivated").is_some_and(|v| v.as_bool()), vm.get_prop(e, "bCompleted").is_some_and(|v| v.as_bool()))).collect()
}

fn current_day(vm: &Vm, game: ObjId) -> Option<ObjId> {
    let gs = vm.get_prop(game, "TheGameState").and_then(|v| v.as_obj());
    let d = gs.and_then(|g| vm.get_prop(g, "CurrentDay")).map(|v| v.as_int()).unwrap_or(0);
    objs(vm.get_prop(game, "Days")).get(d.max(0) as usize).copied()
}

/// First live actor whose Tag, object name or class name matches `key` (case-insensitive).
fn find_actor(vm: &Vm, key: &str) -> Option<ObjId> {
    // `Key.Prop`: an object property of the found actor (e.g. BankTellerController.CustomerStand).
    if let Some((base, prop)) = key.split_once('.') {
        let a = find_actor(vm, base)?;
        return vm.get_prop(a, prop).and_then(|v| v.as_obj());
    }
    let k = key.to_ascii_lowercase();
    let tag = |a: ObjId| vm.get_prop(a, "Tag").map(|v| v.as_str().to_ascii_lowercase()).unwrap_or_default();
    let live = || vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted);
    // `last:Class`: the most recently spawned live actor of that class (e.g. a summoned pawn).
    // `pc` / `pawn`: the local player's pawn (bPlayer) and its Controller.
    if k == "pc" || k == "pawn" {
        let p = live().find(|&a| matches!(vm.get_prop(a, "bPlayer"), Some(Value::Bool(true))))?;
        return if k == "pawn" { Some(p) } else { vm.get_prop(p, "Controller").and_then(|v| v.as_obj()) };
    }
    if let Some(c) = k.strip_prefix("last:") {
        return live().filter(|&a| vm.classes[vm.objects[a].class].name.to_ascii_lowercase() == c).max();
    }
    live()
        .find(|&a| tag(a) == k)
        .or_else(|| live().find(|&a| vm.object_name(a).to_ascii_lowercase() == k))
        .or_else(|| live().find(|&a| vm.classes[vm.objects[a].class].name.to_ascii_lowercase() == k))
}

/// Puts the player pawn `dist` units from `target` (trying 8 directions and a few heights) facing it.
fn goto(game: &mut Game, target: ObjId, dist: f32) -> bool {
    let Some(pc) = game.player_controller else { return false };
    let Some(pawn) = game.world.vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()) else { return false };
    let t = game.world.vm.actor_loc(target);
    let host = game.host.clone();
    // At `dist` first, then farther rings (pickups embedded in props or against walls).
    for d in [dist, dist + 40.0, dist + 80.0, dist + 130.0] {
    for k in 0..8 {
        let ang = k as f32 * std::f32::consts::FRAC_PI_4;
        for dz in [0.0, 20.0, 40.0, 80.0] {
            let at = [t[0] + ang.cos() * d, t[1] + ang.sin() * d, t[2] + dz];
            if host.move_actor(&mut game.world.vm, pawn, at, false) {
                let yaw = ((t[1] - at[1]).atan2(t[0] - at[0]) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
                let vm = &mut game.world.vm;
                vm.set_prop(pc, "Rotation", Value::Rotator([0, yaw, 0]));
                vm.set_prop(pawn, "Rotation", Value::Rotator([0, yaw, 0]));
                vm.set_prop(pawn, "Velocity", Value::Vector([0.0; 3]));
                return true;
            }
        }
    }
    }
    false
}

fn inventory(vm: &Vm, pawn: ObjId) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = vm.get_prop(pawn, "Inventory").and_then(|v| v.as_obj());
    for _ in 0..64 {
        let Some(i) = cur else { break };
        let amt = vm.get_prop(i, "Amount").map(|v| format!("x{:?}", v)).unwrap_or_default();
        out.push(format!("{}{amt}", vm.classes[vm.objects[i].class].name));
        cur = vm.get_prop(i, "Inventory").and_then(|v| v.as_obj());
    }
    out
}

/// Exec command on the first of controller / pawn / weapon / GameInfo that defines it.
fn exec(vm: &mut Vm, pc: ObjId, command: &str) -> bool {
    let mut it = command.splitn(2, char::is_whitespace);
    let Some(f) = it.next().filter(|f| !f.is_empty()) else { return false };
    let rest = it.next().unwrap_or("");
    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
    let weapon = pawn.and_then(|p| vm.get_prop(p, "Weapon").and_then(|v| v.as_obj()));
    let game = vm.level.and_then(|l| vm.get_prop(l, "Game").and_then(|v| v.as_obj()));
    let cheat = vm.get_prop(pc, "CheatManager").and_then(|v| v.as_obj());
    for t in [Some(pc), pawn, weapon, game, cheat].into_iter().flatten() {
        if vm.find_function(vm.objects[t].class, f).is_some() {
            let args = vm.exec_args(t, f, rest).unwrap_or_default();
            if let Err(e) = vm.call_event(t, f, args) {
                vm.record_error(&format!("exec {f}"), &e);
            }
            return true;
        }
    }
    false
}

type Drive = Option<(f32, bool, bool, bool, u32, u32, f32)>;

/// Headless stand-in for the front end's screen tick: advance Level.TimeSecondsAlways (50 ms
/// cap like frontend.rs) and Tick every active P2Screen in the player's LocalInteractions.
fn screens_tick(game: &mut Game, dt: f32, t: f32, last: &mut String) {
    let Some(pc) = game.player_controller else { return };
    let vm = &mut game.world.vm;
    if let Some(l) = vm.level {
        let now = vm.get_prop(l, "TimeSecondsAlways").map(|v| v.as_float()).unwrap_or(0.0) + dt.min(0.05);
        vm.set_prop(l, "TimeSecondsAlways", Value::Float(now));
    }
    let Some(Value::Array(locals)) = vm.get_prop(pc, "LocalInteractions").cloned() else { return };
    let mut line = String::new();
    for s in locals.iter().filter_map(|v| v.as_obj()) {
        if !vm.class_is_a(vm.objects[s].class, "P2Screen") || !vm.get_prop(s, "bActive").is_some_and(|v| v.as_bool()) {
            continue;
        }
        vm.fire_event(s, "Tick", vec![Value::Float(dt)]);
        line += &format!("{} [{}] {} ", vm.object_name(s), vm.state_name(s), props(vm, s, &["RevealErrand", "RevealCount", "bGoFast"]));
    }
    if line != *last {
        println!("t={t:.2}s screens {line}");
        *last = line;
    }
}

/// `wtrace N`: for N frames print every change of the pawn's weapon, its state and channel-0 anim.
fn wtrace_tick(game: &mut Game, left: &mut u32, last: &mut String, frame: usize) {
    if *left == 0 {
        return;
    }
    *left -= 1;
    let Some(pc) = game.player_controller else { return };
    let host = game.host.clone();
    let vm = &mut game.world.vm;
    let Some(pawn) = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()) else { return };
    let w = vm.get_prop(pawn, "Weapon").and_then(|v| v.as_obj());
    let s = match w {
        Some(w) => {
            let anim = uscript::host::Host::anim_channel(&*host, vm, w, 0).map(|c| c.sequence).unwrap_or_default();
            let ammo = vm.get_prop(w, "AmmoType").and_then(|v| v.as_obj()).and_then(|a| vm.get_prop(a, "AmmoAmount")).map(|v| v.as_int()).unwrap_or(-1);
            let ch = uscript::host::Host::anim_channel(&*host, vm, w, 0);
            let rate = ch.as_ref().map(|c| c.rate).unwrap_or(0.0);
            format!("{} [{}] anim {anim} rate {rate:.2} ammo {ammo}", vm.classes[vm.objects[w].class].name, vm.state_name(w))
        }
        None => "none".into(),
    };
    if s != *last {
        println!("WT f={frame} {s}");
        *last = s;
    }
}

/// `headtrack N [all]`: for N frames, after the full frame (script, anims, physics, all hooks:
/// what the renderer sees), every pawn with MyHead: the head actor's Location/Rotation against
/// the body's `MALE01 head` bone placed with the pawn's current actor_world and the head's
/// RelativeLocation/Rotation (what the attachment should be). Prints per-frame max/mean distance
/// (uu) and angle (deg) for moving pawns (2D speed > 50) and all; `all` also prints each pawn
/// over 0.5 uu / 1 deg.
fn headtrack_tick(game: &mut Game, left: &mut u32, verbose: bool, frame: usize, acc: &mut [f64; 6]) {
    if *left == 0 {
        return;
    }
    *left -= 1;
    let host = game.host.clone();
    let vm = &mut game.world.vm;
    let ps: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.get_prop(a, "MyHead").and_then(|v| v.as_obj()).is_some_and(|h| !vm.objects[h].deleted)).collect();
    let (mut n, mut nm, mut dmax, mut amax, mut dsum, mut asum, mut dmaxm, mut amaxm) = (0, 0, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
    for p in ps {
        let h = vm.get_prop(p, "MyHead").and_then(|v| v.as_obj()).unwrap();
        if vm.get_prop(h, "Base").and_then(|v| v.as_obj()) != Some(p) || p2game::ai::render_hidden(vm, h) {
            continue;
        }
        let Some(bm) = host.with_inst(vm, p, |i| i.skeleton.bone("MALE01 head").map(|b| i.bone_actor_matrix(b))).flatten() else { continue };
        let rel_l = glam::Vec3::from(vm.get_prop(h, "RelativeLocation").map(|v| v.as_vec()).unwrap_or_default());
        let rel_r = vm.get_prop(h, "RelativeRotation").map(|v| v.as_rot()).unwrap_or_default();
        let rel = glam::Mat4::from_translation(rel_l) * glam::Mat4::from_mat3(p2anim::runtime::rotator_mat3(rel_r));
        let want = p2game::host::actor_world(vm, p) * bm * rel;
        let wl = want.w_axis.truncate();
        let hl = glam::Vec3::from(vm.actor_loc(h));
        let d = wl.distance(hl);
        let m3 = glam::Mat3::from_mat4(want);
        let x = m3.x_axis.normalize_or_zero();
        let y = (m3.y_axis - x * x.dot(m3.y_axis)).normalize_or_zero();
        let z = (m3.z_axis - x * x.dot(m3.z_axis) - y * y.dot(m3.z_axis)).normalize_or_zero();
        let wq = glam::Quat::from_mat3(&glam::Mat3::from_cols(x, y, z));
        let hq = glam::Quat::from_mat3(&p2anim::runtime::rotator_mat3(vm.actor_rot(h)));
        let ang = wq.angle_between(hq).to_degrees();
        let vel = vm.get_prop(p, "Velocity").map(|v| v.as_vec()).unwrap_or_default();
        let sp = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
        n += 1;
        dmax = dmax.max(d);
        amax = amax.max(ang);
        dsum += d;
        asum += ang;
        if sp > 50.0 {
            nm += 1;
            dmaxm = dmaxm.max(d);
            amaxm = amaxm.max(ang);
        }
        if verbose && (d > 0.5 || ang > 1.0) {
            let anim = uscript::host::Host::anim_channel(&*host, vm, p, 0).map(|c| c.sequence).unwrap_or_default();
            println!("HT f={frame} {} speed {sp:.0} d {d:.2} ang {ang:.2} anim {anim} state {} crouch {:?}", vm.object_name(p), vm.state_name(p), vm.get_prop(p, "bIsCrouched").map(|v| v.as_bool()));
        }
    }
    acc[0] += n as f64;
    acc[1] += dsum as f64;
    acc[2] = acc[2].max(dmax as f64);
    acc[3] += asum as f64;
    acc[4] = acc[4].max(amax as f64);
    acc[5] += nm as f64;
    println!("HEADTRACK f={frame} pawns {n} moving {nm} dmax {dmax:.2} dmean {:.2} amax {amax:.2} amean {:.2} moving dmax {dmaxm:.2} amax {amaxm:.2}", dsum / n.max(1) as f32, asum / n.max(1) as f32);
    if *left == 0 {
        println!("HEADTRACK TOTAL samples {} moving {} dmean {:.3} dmax {:.2} amean {:.3} amax {:.2}", acc[0], acc[5], acc[1] / acc[0].max(1.0), acc[2], acc[3] / acc[0].max(1.0), acc[4]);
    }
}

/// One frame of the `drive` verb: movement input as PlayerInput leaves it, then PlayerTick.
fn drive_tick(game: &mut Game, drive: &mut Drive) {
    let (Some((fwd, jump, duck, walk, n, k, z0)), Some(pc)) = (drive.as_mut(), game.player_controller) else { return };
    let vm = &mut game.world.vm;
    let Some(pawn) = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()) else { return };
    let loc = vm.actor_loc(pawn);
    if *k == 0 {
        *z0 = loc[2];
        // The window's frontend creates PlayerInput (InitInputSystem); headless has none.
        if vm.get_prop(pc, "PlayerInput").and_then(|v| v.as_obj()).is_none() {
            vm.fire_event(pc, "InitInputSystem", vec![]);
        }
    }
    vm.set_prop(pc, "aBaseY", Value::Float(*fwd * 6000.0));
    vm.set_prop(pc, "aForward", Value::Float(0.0));
    vm.set_prop(pc, "aStrafe", Value::Float(0.0));
    vm.set_prop(pc, "bDuck", Value::Byte(*duck as u8));
    vm.set_prop(pc, "bRun", Value::Byte(*walk as u8));
    if *jump && *k == 0 {
        vm.set_prop(pc, "bPressedJump", Value::Bool(true));
    }
    vm.fire_event(pc, "PlayerTick", vec![Value::Float(probe_dt())]);
    let vel = vm.get_prop(pawn, "Velocity").map(|v| v.as_vec()).unwrap_or_default();
    println!(
        "DRIVE f={k} speed2d {:.0} vz {:.0} dz {:.1} phys {:?} crouched {:?} pc {} pawn {} pj {:?} base {:?}",
        (vel[0] * vel[0] + vel[1] * vel[1]).sqrt(),
        vel[2],
        loc[2] - *z0,
        vm.get_prop(pawn, "Physics").map(|v| v.as_int()),
        vm.get_prop(pawn, "bIsCrouched"),
        vm.state_name(pc),
        vm.state_name(pawn),
        vm.get_prop(pc, "bPressedJump"),
        vm.get_prop(pawn, "Base").and_then(|v| v.as_obj()).map(|b| vm.object_name(b)),
    );
    *k += 1;
    if *k >= *n {
        vm.set_prop(pc, "aForward", Value::Float(0.0));
        vm.set_prop(pc, "bDuck", Value::Byte(0));
        vm.set_prop(pc, "bRun", Value::Byte(0));
        *drive = None;
    }
}

/// `P2_PROBE_DT=0.007`: frame delta of the stepped loop (default 1/30; frame-rate sensitivity).
fn probe_dt() -> f32 {
    std::env::var("P2_PROBE_DT").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0 / 30.0)
}

fn main() {
    // Deep script call chains with P2_CALL_TRACE overflow the default 1 MB main stack.
    std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let day: i32 = a.iter().position(|x| x == "--day").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(1);
    let watch: f32 = a.iter().position(|x| x == "--watch").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::viewport::install();
    p2game::clock::install();
    p2game::ai::install();
    // Silent audio engine: script waits on Say() durations (SayTime), e.g. people asked to sign
    // the petition listen for the Dude's whole line before deciding.
    p2game::audio::install(root.clone(), None);
    // Matinee scenes (errand cutscenes: the grave attack, the Brewery gimp scene) drive story
    // triggers and travel; `--no-cinema` leaves them frozen as before.
    if !a.iter().any(|x| x == "--no-cinema") {
        p2game::cinema::install(root.clone());
    }
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    // `--game Class` (e.g. GameTypes.AWPGameInfo: the 7-day game with Apocalypse Weekend).
    let game_class = a.iter().position(|x| x == "--game").and_then(|i| a.get(i + 1)).cloned().unwrap_or_else(|| "GameTypes.GameSinglePlayer".into());
    let mut game = Game::new(world, lv, Some(&game_class));
    // The original's day option (P2GameInfoSingle.InitGame: SetDay -> ForceDayOnStartup).
    game.begin_play(&format!("?SetDay={day}"));
    // The front end sends TravelPostAccept after login: P2GameInfoSingle.PostTravel creates the
    // GameState ("testing maps via command line") and starts the day.
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    for _ in 0..10 {
        game.tick(1.0 / 30.0);
    }
    let g = game.game_info.unwrap();
    {
        let vm = &game.world.vm;
        let gs = vm.get_prop(g, "TheGameState").and_then(|v| v.as_obj());
        println!("GameInfo {} GameState {:?} CurrentDay {:?}", game.game_class, gs.map(|o| vm.object_name(o)), gs.and_then(|o| vm.get_prop(o, "CurrentDay")));
        if let Some(d) = current_day(vm, g) {
            println!("Day {} : {}", vm.object_name(d), props(vm, d, &["UniqueName", "Description", "StartDayURL", "FinishedDayURL"]));
            for e in objs(vm.get_prop(d, "Errands")) {
                println!("  errand {}: {}", vm.object_name(e), props(vm, e, &["UniqueName", "bInitiallyActive", "bActivated", "bCompleted", "NameTex", "LocationTex", "LocationCrossTex", "IgnoreTag"]));
                for goal in objs(vm.get_prop(e, "Goals")) {
                    let cls = vm.classes[vm.objects[goal].class].name.clone();
                    println!("    goal {cls}: {}", props(vm, goal, &["InvClassName", "PickupTag", "TagToCheck", "PersonTag", "UniqueTag", "TriggerOnCompletionTag", "ActivateErrandName", "SendPlayerURL", "HateClass"]));
                }
            }
        }
        let interesting = ["Pickup", "Cashier", "Clerk", "Teller", "Police", "Cop", "Errand"];
        let mut seen = std::collections::BTreeMap::<String, usize>::new();
        for &x in &vm.actors {
            if vm.objects[x].deleted {
                continue;
            }
            let c = vm.classes[vm.objects[x].class].name.clone();
            let ctrl = vm.get_prop(x, "Controller").and_then(|v| v.as_obj()).map(|c| vm.classes[vm.objects[c].class].name.clone()).unwrap_or_default();
            if interesting.iter().any(|k| c.contains(k) || ctrl.contains(k)) && !c.contains("Weapon") && !c.contains("Ammo") && !c.contains("Health") {
                *seen.entry(format!("{c} {ctrl}")).or_default() += 1;
            }
        }
        for (k, n) in seen {
            println!("  actors: {n:>3} x {k}");
        }
    }
    let steps_text: Option<String> = a
        .iter()
        .position(|x| x == "--steps")
        .and_then(|i| a.get(i + 1))
        .cloned()
        // `--steps-file F`: the same syntax from a file (long scripted runs exceed cmd's line limit).
        .or_else(|| a.iter().position(|x| x == "--steps-file").and_then(|i| a.get(i + 1)).and_then(|p| std::fs::read_to_string(p).ok()).map(|s| s.replace(['\r', '\n'], "")));
    let steps: Vec<(f32, String)> = steps_text
        .map(|s| s.split(';').filter_map(|p| p.split_once(':')).filter_map(|(t, c)| Some((t.trim().parse().ok()?, c.trim().to_string()))).collect())
        .unwrap_or_default();
    let mut next = 0;
    let mut last = errand_state(&game.world.vm, g);
    let mut last_target = None;
    // `await Key Prop=Value` (or `await Key State`) holds the remaining steps until the condition
    // holds; their times then count from that moment (`shift`).
    let mut shift = 0.0f32;
    let mut drive: Drive = None;
    let (mut wt_left, mut wt_last) = (0u32, String::new());
    let (mut ht_left, mut ht_all, mut ht_acc) = (0u32, false, [0f64; 6]);
    // `--screens`: tick the player's P2Screens (map reveal, cross-out) the way the front end does,
    // printing each screen state change and the Dude's map lines.
    let screens = a.iter().any(|x| x == "--screens");
    let mut screen_last = String::new();
    for f in 0..(watch * 30.0) as usize {
        let t = f as f32 / 30.0;
        while next < steps.len() && steps[next].0 + shift <= t {
            let cmd = steps[next].1.clone();
            let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd.as_str(), ""));
            if verb == "await" {
                let (key, cond) = rest.split_once(' ').unwrap_or((rest, ""));
                let vm = &game.world.vm;
                let ok = find_actor(vm, key).is_some_and(|x| match cond.split_once('=') {
                    Some((p, want)) => props(vm, x, &[p]).eq_ignore_ascii_case(&format!("{p}={want}")),
                    None => vm.state_name(x).eq_ignore_ascii_case(cond),
                });
                if !ok {
                    break;
                }
                println!("t={t:.1}s await {rest}: met");
                shift = t - steps[next].0;
                next += 1;
                continue;
            }
            // Cutscenes unpossess the player (bCinematicView scenes, map-start cinematics):
            // hold player steps until a pawn is back instead of panicking on it.
            let needs_pawn = !matches!(verb, "dump" | "event" | "screen" | "activate" | "setf" | "around" | "process" | "travel" | "crossout" | "state" | "bones" | "exits" | "actions" | "cheats");
            if needs_pawn && game.player_controller.and_then(|pc| game.world.vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj())).is_none() {
                // Keep the remaining steps' spacing: they resume relative to the pawn's return.
                shift = t - steps[next].0;
                break;
            }
            next += 1;
            match verb {
                "goto" | "near" => {
                    let (key, dist) = rest.split_once(' ').map(|(k, d)| (k, d.parse().unwrap_or(60.0))).unwrap_or((rest, if verb == "goto" { 0.0 } else { 80.0 }));
                    match find_actor(&game.world.vm, key) {
                        Some(x) => {
                            let ok = goto(&mut game, x, dist);
                            last_target = Some(x);
                            println!("t={t:.1}s {verb} {key}: {} {} at {:?} -> {ok}", game.world.vm.object_name(x), game.world.vm.classes[game.world.vm.objects[x].class].name, game.world.vm.actor_loc(x));
                        }
                        None => println!("t={t:.1}s {verb} {key}: not found"),
                    }
                }
                "exec" => {
                    let pc = game.player_controller.unwrap();
                    let ok = exec(&mut game.world.vm, pc, rest);
                    println!("t={t:.1}s exec {rest:?} -> {ok}");
                }
                "inv" => {
                    let pc = game.player_controller.unwrap();
                    let vm = &game.world.vm;
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
                    println!("t={t:.1}s inventory {:?}", inventory(vm, pawn));
                }
                "place" => {
                    // `place Key Target [dz]`: move actor Key onto actor Target (offset dz), no sweep.
                    let w: Vec<&str> = rest.split_whitespace().collect();
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    let (a, b) = (w.first().and_then(|k| find_actor(vm, k)), w.get(1).and_then(|k| find_actor(vm, k)));
                    let dz: f32 = w.get(2).and_then(|d| d.parse().ok()).unwrap_or(0.0);
                    if let (Some(a), Some(b)) = (a, b) {
                        let l = vm.actor_loc(b);
                        let ok = uscript::host::Host::move_actor(&*host, vm, a, [l[0], l[1], l[2] + dz], false);
                        println!("t={t:.1}s place {} at {} -> {ok}", vm.object_name(a), vm.object_name(b));
                    } else {
                        println!("t={t:.1}s place {rest}: not found");
                    }
                }
                "walkers" => {
                    // `walkers [N]`: moving pawns: speed, channel-0 anim, normalized rate, cycle
                    // period (1/rate) and the distance covered per cycle (speed/rate).
                    let n: usize = rest.trim().parse().unwrap_or(8);
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    let pawns: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn")).collect();
                    let mut k = 0;
                    for p in pawns {
                        let sp = match vm.get_prop(p, "Velocity") { Some(Value::Vector(v)) => (v[0] * v[0] + v[1] * v[1]).sqrt(), _ => 0.0 };
                        if sp < 20.0 || k >= n {
                            continue;
                        }
                        k += 1;
                        if let Some(c) = uscript::host::Host::anim_channel(&*host, vm, p, 0) {
                            let per = if c.rate > 0.0 { 1.0 / c.rate } else { 0.0 };
                            println!("t={t:.1}s walker {} speed {sp:.0} anim {} rate {:.3}/s period {per:.2}s stride/cycle {:.0}uu", vm.object_name(p), c.sequence, c.rate, sp * per);
                        }
                    }
                }
                "foot" => {
                    // `foot Key`: world position of the lower foot bone (planted foot) and pawn speed;
                    // consecutive samples give the planted foot's slide speed.
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    if let Some(x) = find_actor(vm, rest.trim()) {
                        let world = p2game::host::actor_world(vm, x);
                        let mut best: Option<(String, [f32; 3])> = None;
                        for b in ["MALE01 L Foot", "MALE01 r foot"] {
                            if let Some(c) = host.with_inst(vm, x, |i| i.get_bone_coords(b)).flatten() {
                                let w = world.transform_point3(c.origin);
                                if best.as_ref().map_or(true, |(_, p)| w.z < p[2]) {
                                    best = Some((b.to_string(), [w.x, w.y, w.z]));
                                }
                            }
                        }
                        let sp = match vm.get_prop(x, "Velocity") { Some(Value::Vector(v)) => (v[0] * v[0] + v[1] * v[1]).sqrt(), _ => 0.0 };
                        let ch = uscript::host::Host::anim_channel(&*host, vm, x, 0);
                        println!("FOOT {t:.3} {:?} speed {sp:.0} anim {:?}", best, ch.map(|c| (c.sequence, c.rate, c.frame)));
                    }
                }
                "meshes" => {
                    // `meshes Sub`: pawns whose Mesh contains Sub: name, mesh, loc, channel-0 anim.
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    let sub = rest.trim().to_ascii_lowercase();
                    let ps: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn")).collect();
                    for p in ps {
                        let m = vm.get_prop(p, "Mesh").and_then(|v| v.as_resource().map(|s| s.to_string())).unwrap_or_default();
                        if m.to_ascii_lowercase().contains(&sub) {
                            let ch = uscript::host::Host::anim_channel(&*host, vm, p, 0).map(|c| c.sequence);
                            println!("t={t:.1}s mesh {} {m} at {:?} anim {ch:?}", vm.object_name(p), vm.actor_loc(p));
                        }
                    }
                }
                "navstat" => {
                    // `navstat`: AI pawns that want to move (Acceleration set) but barely move
                    // (pressing into a wall/prop), out of all accelerating pawns; plus the
                    // stuck/deflect counters of the steering code.
                    let vm = &mut game.world.vm;
                    let ps: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn")).collect();
                    let (mut want, mut blocked) = (0, 0);
                    for p in ps {
                        let Some(c) = vm.get_prop(p, "Controller").and_then(|v| v.as_obj()) else { continue };
                        if !vm.class_is_a(vm.objects[c].class, "AIController") {
                            continue;
                        }
                        let a = vm.get_prop(p, "Acceleration").map(|v| v.as_vec()).unwrap_or_default();
                        if (a[0] * a[0] + a[1] * a[1]).sqrt() < 10.0 {
                            continue;
                        }
                        want += 1;
                        let v = vm.get_prop(p, "Velocity").map(|v| v.as_vec()).unwrap_or_default();
                        if (v[0] * v[0] + v[1] * v[1]).sqrt() < 30.0 {
                            blocked += 1;
                            if rest.trim() == "v" {
                                println!("t={t:.1}s   blocked {} at {:?} state {}", vm.object_name(p), vm.actor_loc(p), vm.state_name(c));
                            }
                        }
                    }
                    println!("t={t:.1}s navstat accelerating {want} blocked {blocked} stuck {} deflect {} detour {} reach {} reach_fallback {}", p2game::physics::NAV_STUCK.with(|c| c.get()), p2game::physics::NAV_DEFLECT.with(|c| c.get()), p2game::physics::NAV_DETOUR.with(|c| c.get()), p2game::navfix::REACH_TESTS.with(|c| c.get()), p2game::navfix::REACH_FALLBACK.with(|c| c.get()));
                }
                "stuckchan" => {
                    // `stuckchan`: pawns with a finished (not playing) blend channel still at alpha>0.5.
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    let ps: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn")).collect();
                    let (mut n, mut tot) = (0, 0);
                    for p in ps {
                        let s: Vec<String> = host.with_inst(vm, p, |i| i.channels.iter().enumerate().skip(1).filter(|(_, c)| !c.playing && c.alpha > 0.5 && c.seq.is_some()).map(|(k, c)| format!("{k}:{}", c.sequence_name().unwrap_or(""))).collect()).unwrap_or_default();
                        tot += 1;
                        if !s.is_empty() {
                            n += 1;
                        }
                    }
                    println!("t={t:.1}s stuckchan {n}/{tot} pawns");
                }
                "headcheck" => {
                    // `headcheck`: every pawn with MyHead: distance from the head actor to the
                    // body's head bone (should be a few uu), its Base and AttachmentBone.
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    let ps: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.get_prop(a, "MyHead").and_then(|v| v.as_obj()).is_some()).collect();
                    let (mut bad, mut n) = (0, 0);
                    for p in ps {
                        let h = vm.get_prop(p, "MyHead").and_then(|v| v.as_obj()).unwrap();
                        let world = p2game::host::actor_world(vm, p);
                        let hb = host.with_inst(vm, p, |i| i.get_bone_coords("MALE01 head").or_else(|| i.get_bone_coords("MALE01 neck"))).flatten().map(|c| world.transform_point3(c.origin));
                        let hl = glam::Vec3::from(vm.actor_loc(h));
                        let d = hb.map(|b| b.distance(hl)).unwrap_or(-1.0);
                        n += 1;
                        if !(0.0..=40.0).contains(&d) {
                            bad += 1;
                            let base = vm.get_prop(h, "Base").and_then(|v| v.as_obj()).map(|b| vm.object_name(b));
                            println!("HEADBAD {t:.2} {} head {} d {d:.0} base {base:?} bone {:?} phys {:?} state {}", vm.object_name(p), vm.object_name(h), vm.get_prop(h, "AttachmentBone"), vm.get_prop(p, "Physics").map(|v| v.as_int()), vm.state_name(p));
                        }
                    }
                    println!("HEADCHECK {t:.2} pawns {n} detached {bad}");
                }
                "headprop" => {
                    // `headprop Pawn`: for each actor attached to the pawn's MyHead (hats, glasses):
                    // its Location in the head bone's frame (should stay constant).
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    if let Some(p) = find_actor(vm, rest.trim()) {
                        let head = vm.get_prop(p, "MyHead").and_then(|v| v.as_obj());
                        if let Some(h) = head {
                            let hw = p2game::host::actor_world(vm, h);
                            let kids: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.get_prop(a, "Base").and_then(|v| v.as_obj()) == Some(h)).collect();
                            let _ = &host;
                            for k in kids {
                                let l = glam::Vec3::from(vm.actor_loc(k));
                                let local = hw.inverse().transform_point3(l);
                                println!("HP {t:.3} {} in {} frame: {:.2} {:.2} {:.2}", vm.object_name(k), vm.object_name(h), local.x, local.y, local.z);
                            }
                        }
                    }
                }
                "posecheck" => {
                    // `posecheck`: every visible human pawn; flags poses where the head is far from
                    // above the pelvis or the feet are far apart (broken skeleton pose), with all
                    // channel anims and blend alphas.
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    let ps: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn")).collect();
                    for p in ps {
                        let world = p2game::host::actor_world(vm, p);
                        let bone = |b: &str| host.with_inst(vm, p, |i| i.get_bone_coords(b)).flatten().map(|c| world.transform_point3(c.origin));
                        let (Some(pel), Some(head), Some(lf), Some(rf)) = (bone("MALE01 pelvis"), bone("MALE01 head"), bone("MALE01 L Foot"), bone("MALE01 r foot")) else { continue };
                        // Bend: angle between pelvis->spine2 and spine2->head (a straight spine ~< 30 deg),
                        // knee: thigh->calf vs calf->foot.
                        let sp2 = bone("MALE01 spine2").unwrap_or(head);
                        let ang = |a: glam::Vec3, b: glam::Vec3, c: glam::Vec3| (b - a).normalize_or_zero().angle_between((c - b).normalize_or_zero()).to_degrees();
                        let bend = ang(pel, sp2, head);
                        let knee = match (bone("MALE01 L Thigh"), bone("MALE01 L Calf")) { (Some(th), Some(ca)) => ang(th, ca, lf), _ => 0.0 };
                        let tilt = (sp2 - pel).normalize_or_zero().angle_between(glam::Vec3::Z).to_degrees();
                        let lean = ((head.x - pel.x).powi(2) + (head.y - pel.y).powi(2)).sqrt();
                        let feet = ((lf.x - rf.x).powi(2) + (lf.y - rf.y).powi(2)).sqrt();
                        let height = head.z - (lf.z.min(rf.z));
                        let moving_slow = match vm.get_prop(p, "Velocity") { Some(Value::Vector(v)) => (v[0] * v[0] + v[1] * v[1]).sqrt() < 10.0, _ => true };
                        if (moving_slow && (lean > 25.0 || feet > 45.0 || height < 50.0)) || bend > 40.0 || tilt > 35.0 || knee > 50.0 {
                            let m = vm.get_prop(p, "Mesh").and_then(|v| v.as_resource().map(|s| s.to_string())).unwrap_or_default();
                            let chans: Vec<String> = host.with_inst(vm, p, |i| i.channels.iter().enumerate().filter_map(|(k, c)| c.sequence_name().map(|s| format!("{k}:{s}@{:.2}a{:.2}", c.frame, c.alpha))).collect()).unwrap_or_default();
                            let ctl = vm.get_prop(p, "Controller").and_then(|v| v.as_obj()).map(|c| vm.state_name(c));
                            println!("t={t:.1}s POSE {} {m} lean {lean:.0} feet {feet:.0} height {height:.0} bend {bend:.0} tilt {tilt:.0} knee {knee:.0} state {ctl:?} chans {chans:?} phys {:?}", vm.object_name(p), vm.get_prop(p, "Physics"));
                        }
                    }
                }
                "jail" => {
                    // `jail`: P2GameInfoSingle.SendPlayerToJail (what an arrest ends in), then the travel.
                    let pc = game.player_controller.unwrap();
                    let gi = game.game_info.unwrap();
                    let r = game.world.vm.call_event(gi, "SendPlayerToJail", vec![Value::Object(Some(pc))]);
                    let req = game.take_travel();
                    println!("t={t:.1}s SendPlayerToJail -> {:?}, travel {req:?}", r.err());
                    if let Some((m, o)) = req {
                        let ok = game.travel(&m, &o);
                        if let Some(pc) = game.player_controller {
                            game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
                        }
                        println!("t={t:.1}s arrived {} ok={ok}", game.level.map.name);
                    }
                }
                "travel" => {
                    // What a level portal does: P2GameInfoSingle.SendPlayerTo (GameState into the
                    // pawn's inventory, then ClientTravel), then the host loads the map.
                    let pc = game.player_controller.unwrap();
                    let gi = game.game_info.unwrap();
                    let r = game.world.vm.call_event(gi, "SendPlayerTo", vec![Value::Object(Some(pc)), Value::Str(rest.into())]);
                    let req = game.take_travel();
                    println!("t={t:.1}s SendPlayerTo {rest:?} -> {r:?}, travel request {req:?}");
                    let (m, o) = req.unwrap_or_else(|| p2game::boot::split_url(rest));
                    let ok = game.travel(&m, &o);
                    if let Some(pc) = game.player_controller {
                        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
                    }
                    let st = p2game::travel::STATS.with(|s| s.borrow().clone());
                    println!("t={t:.1}s arrived {} ok={ok} carried {st:?}", game.level.map.name);
                }
                "gotov" => {
                    // `gotov Key Prop`: teleport to a vector property of an actor (QStartPoint EndLoc).
                    let (key, prop) = rest.split_once(' ').unwrap_or((rest, "Location"));
                    let at = find_actor(&game.world.vm, key).and_then(|x| match game.world.vm.get_prop(x, prop) {
                        Some(Value::Vector(v)) => Some(*v),
                        _ => None,
                    });
                    let pc = game.player_controller.unwrap();
                    let pawn = game.world.vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
                    let host = game.host.clone();
                    // On the point if there is room, else on rings around it (and a bit higher).
                    let ok = at.is_some_and(|v| {
                        std::iter::once((0.0f32, 0.0f32))
                            .chain((1..=4).flat_map(|r| (0..8).map(move |k| (r as f32 * 30.0, k as f32 * std::f32::consts::FRAC_PI_4))))
                            .flat_map(|(r, a)| (0..4).map(move |k| [v[0] + r * a.cos(), v[1] + r * a.sin(), v[2] + 20.0 * k as f32]))
                            .any(|p| host.move_actor(&mut game.world.vm, pawn, p, false))
                    });
                    println!("t={t:.1}s gotov {key}.{prop} {at:?} -> {ok}");
                }
                "hurtby" => {
                    // `hurtby Victim Attacker [Damage]`: Attacker damages Victim (bullet TakeDamage).
                    let w: Vec<&str> = rest.split_whitespace().collect();
                    let vm = &mut game.world.vm;
                    let (v, a) = (w.first().and_then(|k| find_actor(vm, k)), w.get(1).and_then(|k| find_actor(vm, k)));
                    let dmg = w.get(2).and_then(|d| d.parse().ok()).unwrap_or(10);
                    if let (Some(v), Some(a)) = (v, a) {
                        let l = vm.actor_loc(v);
                        let cls = vm.find_class_by_name("BulletDamage").ok();
                        let r = vm.call_event(v, "TakeDamage", vec![Value::Int(dmg), Value::Object(Some(a)), Value::Vector([l[0], l[1], l[2] + 20.0]), Value::Vector([0.0; 3]), Value::Class(cls)]);
                        println!("t={t:.1}s hurtby {} <- {}: {:?} health {:?}", vm.object_name(v), vm.object_name(a), r.err(), vm.get_prop(v, "Health"));
                    } else {
                        println!("t={t:.1}s hurtby {rest}: not found");
                    }
                }
                "kick" => {
                    // `kick Key X,Y,Z`: TakeDamage with that momentum at the actor (what the foot does
                    // to a KActor), then watch it with P2_KARMA_TRACE.
                    let (key, m) = rest.split_once(' ').unwrap_or((rest, "30000,0,15000"));
                    let mv: Vec<f32> = m.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                    let mom = [mv.first().copied().unwrap_or(0.0), mv.get(1).copied().unwrap_or(0.0), mv.get(2).copied().unwrap_or(0.0)];
                    let pc = game.player_controller.unwrap();
                    let vm = &mut game.world.vm;
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
                    match find_actor(vm, key) {
                        Some(x) => {
                            let l = vm.actor_loc(x);
                            let cls = vm.find_class_by_name("KickingDamage").or_else(|_| vm.find_class_by_name("BludgeonDamage")).ok();
                            let r = vm.call_event(x, "TakeDamage", vec![Value::Int(10), Value::Object(pawn), Value::Vector(l), Value::Vector(mom), Value::Class(cls)]);
                            println!("t={t:.1}s kick {} momentum {mom:?}: {:?} vel {:?} phys {:?} at {:?}", vm.object_name(x), r.err(), vm.get_prop(x, "Velocity"), vm.get_prop(x, "Physics"), l);
                            last_target = Some(x);
                        }
                        None => println!("t={t:.1}s kick {key}: not found"),
                    }
                }
                "hurt" => {
                    // `hurt Key Damage`: the player damages an actor (TakeDamage with a bullet, as a
                    // pistol hit does), then the script AI reacts as to any attack.
                    let (key, dmg) = rest.split_once(' ').map(|(k, d)| (k, d.parse().unwrap_or(10))).unwrap_or((rest, 10));
                    let pc = game.player_controller.unwrap();
                    let vm = &mut game.world.vm;
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
                    match find_actor(vm, key) {
                        Some(x) => {
                            let l = vm.actor_loc(x);
                            let cls = vm.find_class_by_name("BulletDamage").ok();
                            let r = vm.call_event(x, "TakeDamage", vec![Value::Int(dmg), Value::Object(pawn), Value::Vector([l[0], l[1], l[2] + 20.0]), Value::Vector([0.0; 3]), Value::Class(cls)]);
                            println!("t={t:.1}s hurt {} by {dmg}: {:?} health now {:?}", vm.object_name(x), r.err(), vm.get_prop(x, "Health"));
                            last_target = Some(x);
                        }
                        None => println!("t={t:.1}s hurt {key}: not found"),
                    }
                }
                "cops" => {
                    // Every police pawn: controller state and who it is after.
                    let vm = &game.world.vm;
                    let pp = game.player_controller.and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj());
                    let ploc = pp.map(|p| vm.actor_loc(p)).unwrap_or_default();
                    for &p in &vm.actors {
                        if vm.objects[p].deleted {
                            continue;
                        }
                        let Some(c) = vm.get_prop(p, "Controller").and_then(|v| v.as_obj()) else { continue };
                        if !vm.class_is_a(vm.objects[c].class, "PoliceController") {
                            continue;
                        }
                        let l = vm.actor_loc(p);
                        let d = ((l[0] - ploc[0]).powi(2) + (l[1] - ploc[1]).powi(2)).sqrt();
                        let atk = vm.get_prop(c, "Attacker").and_then(|v| v.as_obj()).map(|a| vm.object_name(a));
                        println!("t={t:.1}s cop {} [{}] dist {d:.0} Attacker {atk:?} health {:?}", vm.object_name(p), vm.state_name(c), vm.get_prop(p, "Health"));
                    }
                }
                "front" | "behind" => {
                    // `front Key Dist`: stand in front of an actor (along its facing), facing it.
                    // `behind Key Dist`: the same from behind it.
                    let (key, dist) = rest.split_once(' ').map(|(k, d)| (k, d.parse().unwrap_or(150.0))).unwrap_or((rest, 150.0));
                    let dist = if verb == "behind" { -dist } else { dist };
                    match find_actor(&game.world.vm, key) {
                        Some(x) => {
                            let l = game.world.vm.actor_loc(x);
                            let yaw = game.world.vm.actor_rot(x)[1] as f32 * std::f32::consts::PI / 32768.0;
                            let at = [l[0] + yaw.cos() * dist, l[1] + yaw.sin() * dist, l[2] + 10.0];
                            let pc = game.player_controller.unwrap();
                            let pawn = game.world.vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
                            let host = game.host.clone();
                            let ok = host.move_actor(&mut game.world.vm, pawn, at, false);
                            let face = ((l[1] - at[1]).atan2(l[0] - at[0]) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
                            game.world.vm.set_prop(pc, "Rotation", Value::Rotator([0, face, 0]));
                            game.world.vm.set_prop(pawn, "Rotation", Value::Rotator([0, face, 0]));
                            last_target = Some(x);
                            println!("t={t:.1}s front {key}: {} -> {ok}", game.world.vm.object_name(x));
                        }
                        None => println!("t={t:.1}s front {key}: not found"),
                    }
                }
                "shoot" => {
                    // `shoot Key DZ [Damage]`: a pistol bullet from the player pawn hits the actor at
                    // its MyHead location + DZ (or Location + DZ with no head), with the momentum
                    // along the shot, as the pistol's ProcessTraceHit does.
                    let mut it = rest.split_whitespace();
                    let key = it.next().unwrap_or("");
                    let dz: f32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                    let dmg: i32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(20);
                    let pc = game.player_controller.unwrap();
                    let vm = &mut game.world.vm;
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
                    match find_actor(vm, key) {
                        Some(x) => {
                            let base = vm.get_prop(x, "MyHead").and_then(|v| v.as_obj()).map(|h| vm.actor_loc(h)).unwrap_or_else(|| vm.actor_loc(x));
                            let hit = [base[0], base[1], base[2] + dz];
                            let from = pawn.map(|p| vm.actor_loc(p)).unwrap_or(hit);
                            let d = [hit[0] - from[0], hit[1] - from[1], hit[2] - from[2]];
                            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-3);
                            let mom = [d[0] / len * 25000.0, d[1] / len * 25000.0, d[2] / len * 25000.0];
                            let cls = vm.find_class_by_name("BulletDamage").ok();
                            let h0 = vm.get_prop(x, "Health").cloned();
                            let r = vm.call_event(x, "TakeDamage", vec![Value::Int(dmg), Value::Object(pawn), Value::Vector(hit), Value::Vector(mom), Value::Class(cls)]);
                            println!("t={t:.1}s shoot {} at {hit:?} (head+{dz}) dist {len:.0}: {:?} health {h0:?} -> {:?} state {}", vm.object_name(x), r.err(), vm.get_prop(x, "Health"), vm.state_name(x));
                        }
                        None => println!("t={t:.1}s shoot {key}: not found"),
                    }
                }
                "aim" => {
                    // `aim Key Bone|Z`: point the player's view at a bone of an actor (e.g. `aim X head`)
                    // or at its location plus a Z offset.
                    let (key, what) = rest.split_once(' ').unwrap_or((rest, "0"));
                    let vm = &mut game.world.vm;
                    let pc = game.player_controller.unwrap();
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
                    match find_actor(vm, key) {
                        Some(x) => {
                            let l = vm.actor_loc(x);
                            let target = match what.parse::<f32>() {
                                Ok(z) => [l[0], l[1], l[2] + z],
                                // The MyHead actor's location is the neck base; aim a little above it.
                                Err(_) => vm.get_prop(x, "MyHead").and_then(|v| v.as_obj()).map(|h| {
                                    let hl = vm.actor_loc(h);
                                    [hl[0], hl[1], hl[2] + 8.0]
                                }).unwrap_or([l[0], l[1], l[2] + 60.0]),
                            };
                            let eye = vm.actor_loc(pawn);
                            let eh = vm.get_prop(pawn, "EyeHeight").map(|v| v.as_float()).unwrap_or(0.0);
                            let d = [target[0] - eye[0], target[1] - eye[1], target[2] - (eye[2] + eh)];
                            let yaw = (d[1].atan2(d[0]) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
                            let pitch = (d[2].atan2((d[0] * d[0] + d[1] * d[1]).sqrt()) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
                            vm.set_prop(pc, "Rotation", Value::Rotator([pitch, yaw, 0]));
                            vm.set_prop(pawn, "Rotation", Value::Rotator([0, yaw, 0]));
                            println!("t={t:.1}s aim {key} {what}: target {target:?} dist {:.0}", (d[0] * d[0] + d[1] * d[1]).sqrt());
                        }
                        None => println!("t={t:.1}s aim {key}: not found"),
                    }
                }
                "process" => {
                    // Run queued SaveGame/LoadGame requests (the front end does this after a frame).
                    for l in p2game::save::process_requests(&mut game) {
                        println!("t={t:.1}s {l}");
                    }
                }
                "screen" => {
                    // The player's state and current screen (map cross-out etc.).
                    let vm = &game.world.vm;
                    let pc = game.player_controller.unwrap();
                    let cur = vm.get_prop(pc, "CurrentScreen").and_then(|v| v.as_obj());
                    let map = vm.get_prop(pc, "MyMapScreen").and_then(|v| v.as_obj());
                    println!(
                        "t={t:.1}s player [{}] bErrandCompleted={:?} CompletionTrigger={:?} CurrentScreen={:?} map={:?}",
                        vm.state_name(pc),
                        vm.get_prop(pc, "bErrandCompleted"),
                        vm.get_prop(pc, "CompletionTrigger"),
                        cur.map(|c| format!("{} [{}]", vm.object_name(c), vm.state_name(c))),
                        map.map(|m| format!("{} [{}] {}", vm.object_name(m), vm.state_name(m), props(vm, m, &["CrossOutErrand", "Mode", "RevealErrand", "DayIndex"])))
                    );
                }
                "los" => {
                    // `los Viewer Other`: Controller.LineOfSightTo from Viewer's controller.
                    let (a, b) = rest.split_once(' ').unwrap_or((rest, ""));
                    let vm = &mut game.world.vm;
                    if let (Some(x), Some(y)) = (find_actor(vm, a), find_actor(vm, b)) {
                        let c = vm.get_prop(x, "Controller").and_then(|v| v.as_obj()).unwrap_or(x);
                        let h = vm.host();
                        let r = uscript::host::Host::line_of_sight(&*h, vm, c, y);
                        println!("t={t:.1}s los {} -> {}: {r}", vm.object_name(c), vm.object_name(y));
                    }
                }
                "around" => {
                    // `around X Y Z R`: every drawn actor within R of the point (class, mesh, owner).
                    let v: Vec<f32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                    let (p, r) = ([v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0)], v.get(3).copied().unwrap_or(150.0));
                    let vm = &game.world.vm;
                    for &x in &vm.actors {
                        if vm.objects[x].deleted {
                            continue;
                        }
                        let l = vm.actor_loc(x);
                        let d = ((l[0] - p[0]).powi(2) + (l[1] - p[1]).powi(2) + (l[2] - p[2]).powi(2)).sqrt();
                        let dt = vm.get_prop(x, "DrawType").map(|v| v.as_int()).unwrap_or(0);
                        if d > r || dt == 0 {
                            continue;
                        }
                        let mesh = vm.get_prop(x, "StaticMesh").or_else(|| vm.get_prop(x, "Mesh")).and_then(|v| v.as_resource().map(|s| s.to_string()));
                        println!("t={t:.1}s around {} [{}] d {d:.0} DrawType {dt} hidden {:?} mesh {mesh:?} owner {:?} base {:?} rot {:?}", vm.object_name(x), vm.state_name(x), vm.get_prop(x, "bHidden"), vm.get_prop(x, "Owner").and_then(|v| v.as_obj()).map(|o| vm.object_name(o)), vm.get_prop(x, "Base").and_then(|v| v.as_obj()).map(|o| vm.object_name(o)), vm.actor_rot(x));
                    }
                }
                "dump" => {
                    // `dump Key Prop,Prop,...`: state and properties of an actor.
                    let (key, list) = rest.split_once(' ').unwrap_or((rest, ""));
                    let vm = &game.world.vm;
                    match find_actor(vm, key) {
                        Some(x) => {
                            let names: Vec<&str> = list.split(',').filter(|s| !s.is_empty()).collect();
                            println!("t={t:.1}s {} [{}] {}", vm.object_name(x), vm.state_name(x), props(vm, x, &names));
                        }
                        None => println!("t={t:.1}s dump {key}: not found"),
                    }
                }
                "gotostate" => {
                    // `gotostate Key State`: GotoState on the actor's Controller (or the actor),
                    // e.g. force a bystander into PerformBegForLifeOnKnees.
                    let (key, st) = rest.split_once(' ').unwrap_or((rest, ""));
                    let pc = game.player_controller;
                    let vm = &mut game.world.vm;
                    if let Some(x) = find_actor(vm, key) {
                        let c = vm.get_prop(x, "Controller").and_then(|v| v.as_obj()).unwrap_or(x);
                        // Begging needs an Attacker to look at: the player.
                        if let Some(p) = pc.and_then(|pc| vm.get_prop(pc, "Pawn")).and_then(|v| v.as_obj()) {
                            vm.set_prop(c, "Attacker", Value::Object(Some(p)));
                        }
                        let r = vm.goto_state(c, st.trim(), "");
                        println!("t={t:.1}s gotostate {} -> {} ({:?})", vm.object_name(c), vm.state_name(c), r.err());
                        last_target = Some(x);
                    }
                }
                "bones" => {
                    // `bones Key Bone,Bone,...`: world Z of bones relative to the actor's Location
                    // (GetBoneCoords of the current pose), plus the anim on channel 0.
                    let (key, list) = rest.split_once(' ').unwrap_or((rest, "MALE01 head"));
                    let host = game.host.clone();
                    let vm = &mut game.world.vm;
                    match find_actor(vm, key) {
                        Some(x) => {
                            let world = p2game::host::actor_world(vm, x);
                            let l = vm.actor_loc(x);
                            let mut out = Vec::new();
                            for b in list.split(',').filter(|s| !s.is_empty()) {
                                let c = host.with_inst(vm, x, |i| i.get_bone_coords(b)).flatten();
                                if let Some(c) = c {
                                    let w = world.transform_point3(c.origin);
                                    out.push(format!("{b}: dz {:.1} dxy {:.1}", w.z - l[2], ((w.x - l[0]).powi(2) + (w.y - l[1]).powi(2)).sqrt()));
                                } else {
                                    out.push(format!("{b}: none"));
                                }
                            }
                            let seq = uscript::host::Host::anim_channel(&*host, vm, x, 0).map(|c| format!("{} f{:.2} r{:.2} p{}", c.sequence, c.frame, c.rate, c.playing));
                            println!("t={t:.1}s bones {} seq {:?} PrePivot {:?}: {}", vm.object_name(x), seq, vm.get_prop(x, "PrePivot"), out.join("; "));
                        }
                        None => println!("t={t:.1}s bones {key}: not found"),
                    }
                }
                "event" => {
                    // `event Name`: actors whose Event (or Tag) is Name, with class, state, location.
                    let k = rest.trim().to_ascii_lowercase();
                    let vm = &game.world.vm;
                    for &x in &vm.actors {
                        if vm.objects[x].deleted {
                            continue;
                        }
                        let ev = vm.get_prop(x, "Event").map(|v| v.as_name().to_ascii_lowercase()).unwrap_or_default();
                        let tag = vm.get_prop(x, "Tag").map(|v| v.as_name().to_ascii_lowercase()).unwrap_or_default();
                        if ev == k || tag == k {
                            println!("t={t:.1}s event {}: {} ({}) [{}] at {:?} tag {tag} event {ev}", rest.trim(), vm.object_name(x), vm.classes[vm.objects[x].class].name, vm.state_name(x), vm.actor_loc(x));
                        }
                    }
                }
                "hold" | "release" => {
                    // `hold`: press and keep the fire button (bFire=1 then Fire), for streams
                    // (urethra, gas can) that stop when bFire drops. `release`: bFire=0, StopFire.
                    let pc = game.player_controller.unwrap();
                    let vm = &mut game.world.vm;
                    let on = verb == "hold";
                    vm.set_prop(pc, "bFire", Value::Byte(on as u8));
                    let r = exec(vm, pc, if on { "Fire" } else { "StopFire" });
                    println!("t={t:.1}s {verb} -> {r}");
                }
                "killtag" => {
                    // `killtag Event [Damage]`: the player shoots every live pawn whose Event is
                    // the given name (an AWTrigger's kill set: Event=KillZombies) with a bullet.
                    let (ev, dmg) = rest.split_once(' ').map(|(e, d)| (e, d.trim().parse().unwrap_or(5000))).unwrap_or((rest, 5000));
                    let pc = game.player_controller.unwrap();
                    let vm = &mut game.world.vm;
                    let pawn = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj());
                    let cls = vm.find_class_by_name("BulletDamage").ok();
                    // AWZombie.TakeDamage only subtracts Health for HeadKillDamage (or shotgun/rifle/
                    // sledge head shots), so zombies get that.
                    let hk = vm.find_class_by_name("HeadKillDamage").ok();
                    let targets: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn") && Some(a) != pawn && vm.get_prop(a, "Event").is_some_and(|v| v.as_name().eq_ignore_ascii_case(ev)) && vm.get_prop(a, "Health").is_some_and(|h| h.as_int() > 0)).collect();
                    let n = targets.len();
                    for x in targets {
                        let l = vm.actor_loc(x);
                        // Hit the body surface facing the shooter: P2Pawn.ModifyDamageByBodyLocation
                        // scales by |dot(shot dir, hit-to-center)|, which is 0 at the exact center.
                        let pl = pawn.map(|p| vm.actor_loc(p)).unwrap_or([0.0; 3]);
                        let (dx, dy) = (pl[0] - l[0], pl[1] - l[1]);
                        let d = (dx * dx + dy * dy).sqrt().max(1.0);
                        let hit = [l[0] + dx / d * 10.0, l[1] + dy / d * 10.0, l[2]];
                        if let Err(e) = vm.call_event(x, "TakeDamage", vec![Value::Int(dmg), Value::Object(pawn), Value::Vector(hit), Value::Vector([0.0; 3]), Value::Class(if vm.class_is_a(vm.objects[x].class, "AWZombie") { hk } else { cls })]) {
                            println!("t={t:.1}s killtag {}: {e:?}", vm.object_name(x));
                        }
                    }
                    println!("t={t:.1}s killtag {ev}: {n} pawns");
                }
                "actions" => {
                    // `actions Filter`: every ScriptedSequence/Trigger whose action list mentions
                    // Filter (action class or a property value), with its tag and full action list.
                    let f = rest.trim().to_ascii_lowercase();
                    let vm = &game.world.vm;
                    for &x in &vm.actors {
                        if vm.objects[x].deleted || !vm.class_is_a(vm.objects[x].class, "ScriptedSequence") {
                            continue;
                        }
                        let Some(Value::Array(acts)) = vm.get_prop(x, "Actions") else { continue };
                        let desc: Vec<String> = acts
                            .iter()
                            .filter_map(|v| v.as_obj())
                            .map(|o| {
                                let cls = vm.classes[vm.objects[o].class].name.clone();
                                let extra = props(vm, o, &["URL", "Event", "EventName", "Tag", "UniqueName", "PawnTag", "DestinationTag", "ScriptTag", "PauseTime", "screen"]);
                                format!("{cls}({extra})")
                            })
                            .collect();
                        if f.is_empty() || desc.iter().any(|d| d.to_ascii_lowercase().contains(&f)) {
                            println!("t={t:.1}s script {} ({}) tag {} [{}]: {}", vm.object_name(x), vm.classes[vm.objects[x].class].name, vm.get_prop(x, "Tag").map(|v| v.as_name()).unwrap_or_default(), vm.state_name(x), desc.join(" > "));
                        }
                    }
                }
                "exits" => {
                    // Level exits: actors with a travel URL (Telepad URL, SendPlayerURL, ...).
                    let vm = &game.world.vm;
                    for &x in &vm.actors {
                        if vm.objects[x].deleted {
                            continue;
                        }
                        for p in ["URL", "SendPlayerURL", "DestURL", "TravelURL", "NextLevel"] {
                            if let Some(Value::Str(s)) = vm.get_prop(x, p) {
                                if !s.is_empty() {
                                    println!("t={t:.1}s exit {} ({}) {p}={s} tag {} at {:?} [{}]", vm.object_name(x), vm.classes[vm.objects[x].class].name, vm.get_prop(x, "Tag").map(|v| v.as_name()).unwrap_or_default(), vm.actor_loc(x), vm.state_name(x));
                                }
                            }
                        }
                    }
                }
                "setf" => {
                    // `setf Key Prop Value`: set a float property (e.g. MoneyInv Amount 200).
                    let mut it = rest.split_whitespace();
                    let (key, prop, val) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0));
                    let vm = &mut game.world.vm;
                    match find_actor(vm, key) {
                        Some(x) => {
                            vm.set_prop(x, prop, Value::Float(val));
                            println!("t={t:.1}s setf {} {prop}={val}", vm.object_name(x));
                        }
                        None => println!("t={t:.1}s setf {key}: not found"),
                    }
                }
                "showmap" => {
                    // `showmap`: P2Player.DisplayMapErrands (reveals unrevealed errands; use --screens).
                    let pc = game.player_controller.unwrap();
                    let r = game.world.vm.call_event(pc, "DisplayMapErrands", vec![]);
                    println!("t={t:.1}s showmap: {:?}", r.err());
                }
                "cheats" => {
                    // Sissy toggles bAllowCManager and persists it in the user ini, so runs
                    // alternate; this sets it on (P2GAME's P2FX_CHEATS does the same).
                    let gi = game.game_info.unwrap();
                    game.world.vm.set_prop(gi, "bAllowCManager", Value::Bool(true));
                    game.world.vm.set_prop(gi, "bWarnedCheater", Value::Bool(true));
                    println!("t={t:.1}s cheats on");
                }
                "activate" => {
                    // `activate UniqueName`: P2GameInfoSingle.ActivateErrand (as ACTION_Errand does).
                    let gi = game.game_info.unwrap();
                    let r = game.world.vm.call_event(gi, "ActivateErrand", vec![Value::Str(rest.trim().into())]);
                    println!("t={t:.1}s activate {}: {:?}", rest.trim(), r.err());
                }
                "crossout" => {
                    // Headless has no map screen, so emulate P2Player WaitMapScreen.EndState after an
                    // errand cross-out: Trigger(None, Pawn) every actor tagged CompletionTrigger.
                    let pc = game.player_controller.unwrap();
                    let vm = &mut game.world.vm;
                    let tag = vm.get_prop(pc, "CompletionTrigger").map(|v| v.as_name()).unwrap_or_default();
                    let pawn = vm.get_prop(pc, "Pawn").cloned().unwrap_or(Value::Object(None));
                    let mut n = 0;
                    if vm.get_prop(pc, "bErrandCompleted").is_some_and(|v| v.as_bool()) && !tag.eq_ignore_ascii_case("None") {
                        let hits: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.get_prop(a, "Tag").is_some_and(|v| v.as_name().eq_ignore_ascii_case(&tag))).collect();
                        for a in hits {
                            let _ = vm.call_event(a, "Trigger", vec![Value::Object(None), pawn.clone()]);
                            n += 1;
                        }
                    }
                    vm.set_prop(pc, "bErrandCompleted", Value::Bool(false));
                    println!("t={t:.1}s crossout {tag}: triggered {n}");
                }
                "state" => {
                    let vm = &game.world.vm;
                    if let Some(x) = last_target {
                        let c = vm.get_prop(x, "Controller").and_then(|v| v.as_obj());
                        println!("t={t:.1}s target {} state {} ctrl {:?}", vm.object_name(x), vm.state_name(x), c.map(|c| (vm.classes[vm.objects[c].class].name.clone(), vm.state_name(c))));
                    }
                }
                "zombies" => {
                    // `zombies [Sub]`: pawns whose class contains Sub (default "zombie"): health,
                    // distance to the player, pawn/controller state, controller Enemy/Attacker; plus the player's health.
                    let pc = game.player_controller;
                    let vm = &mut game.world.vm;
                    let mut w = rest.split_whitespace();
                    let sl = w.next().unwrap_or("zombie").to_ascii_lowercase();
                    let maxd: f32 = w.next().and_then(|s| s.parse().ok()).unwrap_or(f32::MAX);
                    let pp = pc.and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj());
                    let pl = pp.map(|p| glam::Vec3::from(vm.actor_loc(p))).unwrap_or_default();
                    println!("t={t:.1}s player {:?} health {:?}", pp.map(|p| vm.object_name(p)), pp.and_then(|p| vm.get_prop(p, "Health")));
                    let host = vm.host();
                    for x in vm.actors.clone() {
                        if vm.objects[x].deleted || !vm.classes[vm.objects[x].class].name.to_ascii_lowercase().contains(&sl) || vm.get_prop(x, "Health").is_none() {
                            continue;
                        }
                        let c = vm.get_prop(x, "Controller").and_then(|v| v.as_obj());
                        let d = (glam::Vec3::from(vm.actor_loc(x)) - pl).length();
                        if d > maxd {
                            continue;
                        }
                        let ch = uscript::host::Host::anim_channel(&*host, vm, x, 0);
                        let anim = ch.map(|c| format!("{}@{:.2} r{:.2}", c.sequence, c.frame, c.rate)).unwrap_or_default();
                        let cs = c.map(|c| format!("{}[{}] {}", vm.classes[vm.objects[c].class].name, vm.state_name(c), props(vm, c, &["Enemy", "Attacker", "MoveTarget"]))).unwrap_or_default();
                        println!("  {} hp {:?} dist {d:.0} pawn[{}] phys {:?} anim {anim} ctrl {cs}", vm.object_name(x), vm.get_prop(x, "Health").map(|v| v.as_int()), vm.state_name(x), vm.get_prop(x, "Physics").map(|v| v.as_int()));
                    }
                }
                "spread" => {
                    // `spread [Sub]`: live pawns whose class contains Sub (default bystander):
                    // mean / median / max distance from the player and how many moved more than
                    // 200 uu since the previous `spread` (fleeing outcome, not just intent).
                    thread_local! { static LAST: std::cell::RefCell<std::collections::HashMap<ObjId, glam::Vec3>> = Default::default(); }
                    let pc = game.player_controller;
                    let vm = &game.world.vm;
                    let sl = rest.trim().to_ascii_lowercase();
                    let sl = if sl.is_empty() { "bystander".to_string() } else { sl };
                    // Only pawns first seen within 1500 uu of the player (the crowd around him),
                    // tracked from then on: map-wide averages hid the fleeing group.
                    thread_local! { static TRACKED: std::cell::RefCell<std::collections::HashSet<ObjId>> = Default::default(); }
                    let pl = pc.and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj()).map(|p| glam::Vec3::from(vm.actor_loc(p))).unwrap_or_default();
                    let mut ds = Vec::new();
                    let mut moved = 0;
                    LAST.with(|l| {
                        let mut l = l.borrow_mut();
                        for &x in &vm.actors {
                            if vm.objects[x].deleted || !vm.classes[vm.objects[x].class].name.to_ascii_lowercase().contains(&sl) || vm.get_prop(x, "Health").is_none_or(|h| h.as_float() <= 0.0) {
                                continue;
                            }
                            let p = glam::Vec3::from(vm.actor_loc(x));
                            let near = (p - pl).truncate().length() < 1500.0;
                            if !TRACKED.with(|t| if near { t.borrow_mut().insert(x); true } else { t.borrow().contains(&x) }) {
                                continue;
                            }
                            ds.push((p - pl).truncate().length());
                            if l.insert(x, p).is_some_and(|o| (o - p).truncate().length() > 200.0) {
                                moved += 1;
                            }
                        }
                    });
                    ds.sort_by(|a, b| a.total_cmp(b));
                    let n = ds.len().max(1) as f32;
                    println!("t={t:.1}s spread {} n {} mean {:.0} median {:.0} max {:.0} moved>200 {moved}", sl, ds.len(), ds.iter().sum::<f32>() / n, ds.get(ds.len() / 2).copied().unwrap_or(0.0), ds.last().copied().unwrap_or(0.0));
                }
                "count" => {
                    // `count Sub,Sub`: live actors per class whose name contains Sub (with states).
                    let vm = &game.world.vm;
                    for sub in rest.split(',').filter(|s| !s.is_empty()) {
                        let sl = sub.to_ascii_lowercase();
                        let mut by: std::collections::BTreeMap<String, usize> = Default::default();
                        for &x in &vm.actors {
                            if vm.objects[x].deleted {
                                continue;
                            }
                            let cn = &vm.classes[vm.objects[x].class].name;
                            if cn.to_ascii_lowercase().contains(&sl) {
                                *by.entry(format!("{cn}[{}]", vm.state_name(x))).or_default() += 1;
                            }
                        }
                        println!("t={t:.1}s count {sub}: {by:?}");
                    }
                }
                "headtrack" => {
                    let mut it = rest.split_whitespace();
                    ht_left = it.next().and_then(|s| s.parse().ok()).unwrap_or(150);
                    ht_all = it.next() == Some("all");
                    ht_acc = [0.0; 6];
                }
                "wtrace" => {
                    wt_left = rest.trim().parse().unwrap_or(150);
                    wt_last.clear();
                }
                "drive" => {
                    // `drive FWD JUMP DUCK WALK FRAMES`: hold movement input for FRAMES (PlayerTick
                    // with aForward/bPressedJump/bDuck/bRun like the key bindings), logging speed.
                    let v: Vec<f32> = rest.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                    drive = Some((v.first().copied().unwrap_or(1.0), v.get(1).copied().unwrap_or(0.0) > 0.0, v.get(2).copied().unwrap_or(0.0) > 0.0, v.get(3).copied().unwrap_or(0.0) > 0.0, v.get(4).copied().unwrap_or(60.0) as u32, 0u32, 0.0f32));
                }
                _ => println!("t={t:.1}s unknown step {cmd:?}"),
            }
        }
        drive_tick(&mut game, &mut drive);
        wtrace_tick(&mut game, &mut wt_left, &mut wt_last, f);
        game.tick(probe_dt());
        headtrack_tick(&mut game, &mut ht_left, ht_all, f, &mut ht_acc);
        if screens {
            screens_tick(&mut game, probe_dt(), t, &mut screen_last);
        }
        // Script-driven travel (ClientTravel from a portal, the jail, the load screen): follow it
        // like the window does.
        if let Some((m, o)) = game.take_travel() {
            println!("t={t:.1}s script travel to {m:?} {o:?}");
            let ok = game.travel(&m, &o);
            if let Some(pc) = game.player_controller {
                game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
            }
            let st = p2game::travel::STATS.with(|s| s.borrow().clone());
            println!("t={t:.1}s arrived {} ok={ok} carried {st:?}", game.level.map.name);
        }
        for l in std::mem::take(&mut game.world.vm.log) {
            let ll = l.to_ascii_lowercase();
            if ll.contains("errand") || ll.contains("paycheck") || ll.contains("crossout") || ll.contains("teller") || ll.contains("cashier") {
                println!("t={t:.1}s log: {l}");
            }
        }
        let now = errand_state(&game.world.vm, game.game_info.unwrap_or(g));
        if now != last {
            println!("t={t:.1}s errands {now:?}");
            last = now;
        }
    }
    let vm = &game.world.vm;
    println!("script errors {}", vm.script_errors.values().sum::<u64>());
    for (k, v) in vm.script_errors.iter().take(15) {
        println!("  {v:>5} {k}");
    }
}
