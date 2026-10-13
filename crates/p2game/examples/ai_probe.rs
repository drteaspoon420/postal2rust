//! NPC AI end-to-end (headless): boots a populated day map, runs 60 s and measures how far the
//! script-controlled pawns moved along paths, then makes the player's gun noise next to a group
//! of bystanders (the GunfireMarker notification P2Weapon sends on every shot) and reports the
//! controllers' states before/after (`GetStateName`).
//! `ai_probe [Map] [--secs N] [--radius R]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::collections::BTreeMap;
use std::sync::Arc;
use uscript::vm::{ObjId, Value, Vm};

fn ctrl_state(vm: &Vm, p: ObjId) -> String {
    vm.get_prop(p, "Controller").and_then(|c| c.as_obj()).map(|c| vm.state_name(c)).unwrap_or_else(|| "-".into())
}

/// `--summon` (FIDELITY #17): summons a bystander 72uu in front of the player the way the
/// `Summon` cheat does, shoots it in the back once (TakeDamage + PawnShotMarker, as P2Weapon),
/// then reports every 0.5 s its distance to the player, state and velocity. The original's pawn
/// never gets closer than the two collision radii.
fn summon_probe(game: &mut Game) {
    let Some(pc) = game.player_controller else { return };
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
    }
    let vm = &mut game.world.vm;
    let Some(pp) = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()) else { return };
    let pl = vm.actor_loc(pp);
    let yaw = vm.actor_rot(pc)[1] as f32 * std::f32::consts::TAU / 65536.0;
    let at = [pl[0] + 72.0 * yaw.cos(), pl[1] + 72.0 * yaw.sin(), pl[2] + 15.0];
    let cls = vm.find_class_by_name("Bystanders").expect("Bystanders");
    // The Summon cheat itself (CheatManager spawns at Pawn.Location + 72 * Vector(Rotation)).
    let cm = vm.get_prop(pc, "CheatManager").and_then(|v| v.as_obj()).or_else(|| {
        vm.call_event(pc, "AddCheatManager", vec![]).ok();
        vm.get_prop(pc, "CheatManager").and_then(|v| v.as_obj())
    });
    let before: std::collections::HashSet<ObjId> = vm.actors.iter().copied().collect();
    if let Some(cm) = cm {
        let _ = vm.call_event(cm, "Summon", vec![Value::Str("BasePeople.Bystanders".into())]);
    }
    let Some(b) = vm.actors.iter().copied().find(|a| !before.contains(a) && vm.class_is_a(vm.objects[*a].class, "Bystanders")) else {
        println!("summon failed at {at:?} (cheat manager {cm:?})");
        return;
    };
    let _ = cls;
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
    }
    let vm = &mut game.world.vm;
    // Shot in the back: the momentum along the bystander's facing.
    let br = vm.actor_rot(b);
    let by = br[1] as f32 * std::f32::consts::TAU / 65536.0;
    let bl = vm.actor_loc(b);
    let dmg = vm.find_class_by_name("BulletDamage").ok();
    vm.fire_event(b, "TakeDamage", vec![Value::Int(5), Value::Object(Some(pp)), Value::Vector(bl), Value::Vector([3000.0 * by.cos(), 3000.0 * by.sin(), 0.0]), Value::Class(dmg)]);
    if let Ok(hit) = vm.find_class_by_name("PawnShotMarker") {
        let hr = vm.default_value(hit, "CollisionRadius").map(|v| v.as_float()).unwrap_or(1024.0);
        let level = vm.level;
        let _ = vm.call(hit, None, "NotifyControllersStatic", vec![Value::Object(level), Value::Class(Some(hit)), Value::Object(Some(pp)), Value::Object(Some(b)), Value::Float(hr), Value::Vector(bl)]);
    }
    let r = |vm: &Vm, a: ObjId| vm.get_prop(a, "CollisionRadius").map(|v| v.as_float()).unwrap_or(0.0);
    println!("summoned {} health {:?}; radii {} + {}", vm.object_name(b), vm.get_prop(b, "Health"), r(vm, b), r(vm, pp));
    let mut min_d = f32::MAX;
    for f in 0..600 {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
        let vm = &game.world.vm;
        if vm.objects[b].deleted {
            println!("bystander gone at {f}");
            break;
        }
        let (l, p) = (vm.actor_loc(b), vm.actor_loc(pp));
        let d = ((l[0] - p[0]).powi(2) + (l[1] - p[1]).powi(2)).sqrt();
        min_d = min_d.min(d);
        if f % 15 == 0 {
            let c = vm.get_prop(b, "Controller").and_then(|c| c.as_obj());
            let goal = c.and_then(|c| vm.get_prop(c, "MoveTarget")).and_then(|v| v.as_obj()).map(|o| vm.object_name(o));
            let ac = c.and_then(|c| vm.get_prop(c, "Destination")).map(|v| v.as_vec());
            println!("t={:.1} d={d:.0} state {} vel {:?} movetarget {goal:?} dest {ac:?}", f as f32 / 30.0, ctrl_state(vm, b), vm.get_prop(b, "Velocity"));
        }
    }
    println!("SUMMON min 2D distance {min_d:.0} (two radii {})", r(&game.world.vm, b) + r(&game.world.vm, pp));
}

/// `--headshots N` (FIDELITY #31): N times, summon a bystander in front of the player (Summon
/// cheat), aim the view at its head (MyHead location + 8, as a player aims), fire the real pistol
/// once, and report the head's location, the shot outcome (PuncturedHeadEffects = head shot) and
/// the bystander's health. Each victim is destroyed before the next.
/// `--walk`: instead of summoned (idle) victims, each shot goes at a map bystander that is walking:
/// the player is put 90 uu behind it on its walk line, aims at its drawn head bone and fires.
fn headshot_probe(game: &mut Game, n: usize, walk: bool) {
    let Some(pc) = game.player_controller else { return };
    // Cheats (Summon) need bAllowCManager, which comes from the user ini (the "Sissy" toggle).
    if let Some(g) = game.game_info {
        game.world.vm.set_prop(g, "bAllowCManager", Value::Bool(true));
        game.world.vm.set_prop(g, "bWarnedCheater", Value::Bool(true));
    }
    // As the front end does after login: creates the GameState so PlayerPrepSave can finish.
    game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
    }
    {
        let vm = &mut game.world.vm;
        let pp = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
        let inv = vm.fire_event(pp, "CreateInventory", vec![Value::Str("Inventory.PistolWeapon".into())]);
        if let Some(Value::Object(Some(w))) = inv {
            let wc = vm.objects[w].class;
            vm.fire_event(pc, "GetWeapon", vec![Value::Class(Some(wc))]);
        }
    }
    for _ in 0..45 {
        game.tick(1.0 / 30.0);
    }
    // The level start autosave (PlayerPrepSave) ignores Fire; wait it out as a player would.
    for _ in 0..600 {
        if !game.world.vm.state_name(pc).contains("PrepSave") {
            break;
        }
        game.tick(1.0 / 30.0);
    }
    {
        let vm = &game.world.vm;
        let pp = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
        let w = vm.get_prop(pp, "Weapon").and_then(|v| v.as_obj());
        println!("weapon {:?} state {:?}, pc state {}", w.map(|w| vm.object_name(w)), w.map(|w| vm.state_name(w)), vm.state_name(pc));
    }
    let mut heads = 0;
    let mut kills = 0;
    if walk {
        let mut used = std::collections::HashSet::new();
        let mut shots = 0;
        for _ in 0..(n * 60) {
            if shots >= n {
                break;
            }
            game.tick(1.0 / 30.0);
            let host = game.host.clone();
            let vm = &mut game.world.vm;
            let pp = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
            // A live, unhurt bystander walking in a straight line (speed 40..250).
            let walker = vm.actors.iter().copied().find(|&a| {
                !vm.objects[a].deleted && !used.contains(&a) && vm.class_is_a(vm.objects[a].class, "Bystanders") && vm.get_prop(a, "Health").map(|v| v.as_int()).unwrap_or(0) > 0
                    && vm.get_prop(a, "MyHead").and_then(|v| v.as_obj()).is_some()
                    && match vm.get_prop(a, "Velocity") { Some(Value::Vector(v)) => (40.0..250.0).contains(&(v[0] * v[0] + v[1] * v[1]).sqrt()), _ => false }
            });
            let Some(b) = walker else { continue };
            used.insert(b);
            let v = vm.get_prop(b, "Velocity").map(|v| v.as_vec()).unwrap_or_default();
            let sp = (v[0] * v[0] + v[1] * v[1]).sqrt();
            let bl = vm.actor_loc(b);
            let at = [bl[0] - v[0] / sp * 90.0, bl[1] - v[1] / sp * 90.0, bl[2] + 5.0];
            if !uscript::host::Host::move_actor(&*host, vm, pp, at, false) {
                continue;
            }
            // Aim at the head as drawn: the body's head bone (the head mesh sits on it).
            let world = p2game::host::actor_world(vm, b);
            let Some(hb) = host.with_inst(vm, b, |i| i.get_bone_coords("MALE01 head")).flatten().map(|c| world.transform_point3(c.origin)) else { continue };
            let eye = vm.actor_loc(pp);
            let eh = vm.get_prop(pp, "EyeHeight").map(|v| v.as_float()).unwrap_or(0.0);
            let d = [hb.x - eye[0], hb.y - eye[1], hb.z + 3.0 - (eye[2] + eh)];
            let yaw = (d[1].atan2(d[0]) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
            let pitch = (d[2].atan2((d[0] * d[0] + d[1] * d[1]).sqrt()) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
            vm.set_prop(pc, "Rotation", Value::Rotator([pitch, yaw, 0]));
            vm.set_prop(pp, "Rotation", Value::Rotator([0, yaw, 0]));
            let hl = vm.get_prop(b, "MyHead").and_then(|v| v.as_obj()).map(|h| vm.actor_loc(h)).unwrap_or_default();
            let hp0 = vm.get_prop(b, "Health").map(|v| v.as_int()).unwrap_or(0);
            let st0 = ctrl_state(vm, b);
            let before_shot: std::collections::HashSet<ObjId> = vm.actors.iter().copied().collect();
            let _ = vm.call_event(pc, "Fire", vec![Value::Float(0.0)]);
            for _ in 0..3 {
                game.tick(1.0 / 30.0);
            }
            let vm = &mut game.world.vm;
            vm.fire_event(pc, "StopFire", vec![]);
            let punctured = vm.actors.iter().filter(|x| !before_shot.contains(x)).any(|&x| vm.classes[vm.objects[x].class].name.contains("PuncturedHead"));
            let hp = vm.get_prop(b, "Health").map(|v| v.as_int()).unwrap_or(0);
            heads += punctured as usize;
            kills += (hp <= 0) as usize;
            println!("#{shots} walker {} speed {sp:.0} [{st0}] headbone {:?} MyHead {hl:?} bone-head z {:.1}; health {hp0} -> {hp} headshot {punctured}", vm.object_name(b), [hb.x, hb.y, hb.z], hb.z - hl[2]);
            shots += 1;
            // Let the panic from the shot settle a little before the next victim.
            for _ in 0..20 {
                game.tick(1.0 / 30.0);
            }
        }
        println!("WALK HEADSHOTS {heads}/{shots} (kills {kills})");
        return;
    }
    for i in 0..n {
        let vm = &mut game.world.vm;
        let pp = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).unwrap();
        let cm = vm.get_prop(pc, "CheatManager").and_then(|v| v.as_obj()).or_else(|| {
            vm.call_event(pc, "AddCheatManager", vec![]).ok();
            vm.get_prop(pc, "CheatManager").and_then(|v| v.as_obj())
        });
        let before: std::collections::HashSet<ObjId> = vm.actors.iter().copied().collect();
        if let Some(cm) = cm {
            let _ = vm.call_event(cm, "Summon", vec![Value::Str("BasePeople.Bystanders".into())]);
        }
        let Some(b) = vm.actors.iter().copied().find(|a| !before.contains(a) && vm.class_is_a(vm.objects[*a].class, "Bystanders")) else {
            println!("#{i} summon failed");
            continue;
        };
        // Let it settle and start idling, as in swan's repro (5 frames).
        for f in 0..5 {
            game.tick(1.0 / 30.0);
            let host = game.host.clone();
            let vm = &mut game.world.vm;
            if let Some(h) = vm.get_prop(b, "MyHead").and_then(|v| v.as_obj()) {
                let (hl, bl) = (vm.actor_loc(h), vm.actor_loc(b));
                let anim: Vec<String> = (0..2).filter_map(|c| uscript::host::Host::anim_channel(&*host, vm, b, c).map(|x| format!("{c}:{} f{:.2} r{:.2}", x.sequence, x.frame, x.rate))).collect();
                let bone = vm.call_event(b, "GetBoneCoords", vec![Value::Name("MALE01 head".into())]).ok().flatten();
                println!("   f{f} pawn z {:.1} yaw {} phys {:?} head-pawn z {:.1} bone {:?} anim {anim:?}", bl[2], vm.actor_rot(b)[1] & 0xffff, vm.get_prop(b, "Physics"), hl[2] - bl[2], bone.map(|v| format!("{v:?}").chars().take(60).collect::<String>()));
            }
        }
        let host = game.host.clone();
        let vm = &mut game.world.vm;
        // P2Pawn.HandleSpecialShots only punctures a head shot from behind: stand 90 uu behind it.
        {
            let (bl, by) = (vm.actor_loc(b), vm.actor_rot(b)[1] as f32 * std::f32::consts::TAU / 65536.0);
            let at = [bl[0] - 90.0 * by.cos(), bl[1] - 90.0 * by.sin(), bl[2] + 5.0];
            let ok = uscript::host::Host::move_actor(&*host, vm, pp, at, false);
            if !ok {
                println!("#{i} could not stand behind");
            }
        }
        let head = vm.get_prop(b, "MyHead").and_then(|v| v.as_obj());
        let hl = head.map(|h| vm.actor_loc(h)).unwrap_or_default();
        let target = [hl[0], hl[1], hl[2] + 8.0];
        let eye = vm.actor_loc(pp);
        let eh = vm.get_prop(pp, "EyeHeight").map(|v| v.as_float()).unwrap_or(0.0);
        let d = [target[0] - eye[0], target[1] - eye[1], target[2] - (eye[2] + eh)];
        let yaw = (d[1].atan2(d[0]) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
        let pitch = (d[2].atan2((d[0] * d[0] + d[1] * d[1]).sqrt()) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
        vm.set_prop(pc, "Rotation", Value::Rotator([pitch, yaw, 0]));
        let hp0 = vm.get_prop(b, "Health").map(|v| v.as_int()).unwrap_or(0);
        let before_shot: std::collections::HashSet<ObjId> = vm.actors.iter().copied().collect();
        let r = vm.call_event(pc, "Fire", vec![Value::Float(0.0)]);
        let w = vm.get_prop(pp, "Weapon").and_then(|v| v.as_obj());
        if i == 0 {
            println!("Fire -> {r:?}, weapon state {:?}, pc state {}", w.map(|w| vm.state_name(w)), vm.state_name(pc));
        }
        for _ in 0..3 {
            game.tick(1.0 / 30.0);
        }
        let vm = &mut game.world.vm;
        vm.fire_event(pc, "StopFire", vec![]);
        let punctured = vm.actors.iter().filter(|x| !before_shot.contains(x)).any(|&x| {
            let n = &vm.classes[vm.objects[x].class].name;
            n.contains("PuncturedHead") || n.contains("ExplodedHead") || n.contains("BloodSpoutFeeder")
        });
        let hp = vm.get_prop(b, "Health").map(|v| v.as_int()).unwrap_or(0);
        heads += punctured as usize;
        kills += (hp <= 0) as usize;
        let bl = vm.actor_loc(b);
        let eye_now = vm.actor_loc(pp);
        println!("#{i} eye {eye_now:?} pawn {bl:?} head(at aim) {hl:?} head-pawn z {:.1} xy {:.1}; health {hp0} -> {hp} headshot {punctured}", hl[2] - bl[2], ((hl[0] - bl[0]).powi(2) + (hl[1] - bl[1]).powi(2)).sqrt());
        for _ in 0..40 {
            game.tick(1.0 / 30.0);
        }
        let vm = &mut game.world.vm;
        if let Some(h) = vm.get_prop(b, "MyHead").and_then(|v| v.as_obj()) {
            vm.destroy_actor(h);
        }
        let ok = vm.destroy_actor(b);
        let live = vm.actors.iter().filter(|&&x| !vm.objects[x].deleted && vm.class_is_a(vm.objects[x].class, "Bystanders")).count();
        println!("   destroyed {ok}, live bystanders {live}");
        for _ in 0..30 {
            game.tick(1.0 / 30.0);
        }
    }
    println!("HEADSHOTS {heads}/{n} (kills {kills})");
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let secs: f32 = get("--secs").and_then(|s| s.parse().ok()).unwrap_or(60.0);
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::ragdoll::install(root.clone());
    let ai = p2game::ai::install();
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let game_class = get("--game").unwrap_or_else(|| "GameTypes.GameSinglePlayer".into());
    let mut game = Game::new(world, lv, Some(&game_class));
    game.begin_play("");
    game.tick(1.0 / 30.0);
    if a.iter().any(|x| x == "--summon") {
        summon_probe(&mut game);
        return;
    }
    if let Some(n) = get("--headshots").and_then(|s| s.parse::<usize>().ok()) {
        headshot_probe(&mut game, n, a.iter().any(|x| x == "--walk"));
        return;
    }
    let player_pawn = game.player_controller.and_then(|pc| game.world.vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()));
    let vm = &game.world.vm;
    let npcs: Vec<ObjId> = vm.actors.iter().copied().filter(|&x| !vm.objects[x].deleted && Some(x) != player_pawn && vm.class_is_a(vm.objects[x].class, "P2Pawn") && vm.get_prop(x, "Controller").and_then(|c| c.as_obj()).is_some()).collect();
    let start: Vec<[f32; 3]> = npcs.iter().map(|&p| vm.actor_loc(p)).collect();
    let mut states0: BTreeMap<String, usize> = BTreeMap::new();
    for &p in &npcs {
        *states0.entry(ctrl_state(vm, p)).or_default() += 1;
    }
    println!("{map}: {} AI pawns with controllers; states at start {states0:?}", npcs.len());
    let t0 = std::time::Instant::now();
    let frames = (secs * 30.0) as usize;
    let mut path_len = vec![0.0f32; npcs.len()];
    let mut last = start.clone();
    for f in 0..frames {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
        let vm = &game.world.vm;
        for (i, &p) in npcs.iter().enumerate() {
            if vm.objects[p].deleted {
                continue;
            }
            let l = vm.actor_loc(p);
            path_len[i] += ((l[0] - last[i][0]).powi(2) + (l[1] - last[i][1]).powi(2)).sqrt();
            last[i] = l;
        }
        if f % 300 == 299 {
            let moving = path_len.iter().filter(|&&d| d > 50.0).count();
            println!("t={:.0}s moved>50uu: {moving}/{}  ({:.1}s wall)", (f + 1) as f32 / 30.0, npcs.len(), t0.elapsed().as_secs_f32());
        }
    }
    let vm = &game.world.vm;
    let alive: Vec<usize> = (0..npcs.len()).filter(|&i| !vm.objects[npcs[i]].deleted).collect();
    let moved = alive.iter().filter(|&&i| path_len[i] > 50.0).count();
    let displaced = alive.iter().filter(|&&i| {
        let l = vm.actor_loc(npcs[i]);
        ((l[0] - start[i][0]).powi(2) + (l[1] - start[i][1]).powi(2)).sqrt() > 100.0
    }).count();
    let total: f32 = alive.iter().map(|&i| path_len[i]).sum();
    let mut states1: BTreeMap<String, usize> = BTreeMap::new();
    for &i in &alive {
        *states1.entry(ctrl_state(vm, npcs[i])).or_default() += 1;
    }
    println!("AFTER {secs}s: {} alive, {moved} walked >50uu, {displaced} ended >100uu from start, total path {:.0}uu; states {states1:?}", alive.len(), total);
    if let Some(ai) = &ai {
        println!("perception: {:?}", ai.stats.borrow());
        let vm = &game.world.vm;
        if let Some(pc) = p2game::ai::local_player_controller(vm) {
            let pl = vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).map(|p| vm.actor_loc(p)).unwrap_or_default();
            let near = npcs.iter().filter(|&&p| {
                let l = vm.actor_loc(p);
                ((l[0] - pl[0]).powi(2) + (l[1] - pl[1]).powi(2) + (l[2] - pl[2]).powi(2)).sqrt() < 6000.0
            }).count();
            println!("view: pc {} rot {:?} FOV {:?} EyeHeight {:?} pawn at {pl:?}; {near} AI pawns within 6000uu; LevelTime {:?}", vm.object_name(pc), vm.actor_rot(pc), vm.get_prop(pc, "FOVAngle"), vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).and_then(|p| vm.get_prop(p, "EyeHeight")), vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")));
        }
    }

    // Gunfire next to the nearest group of bystanders: teleport the player there and send the
    // shot notification exactly as P2Weapon does on a shot (static NotifyControllersStatic).
    let vm = &mut game.world.vm;
    let Some(pp) = player_pawn else { return };
    let bys: Vec<ObjId> = alive.iter().map(|&i| npcs[i]).filter(|&p| vm.class_is_a(vm.objects[p].class, "Bystanders") || vm.get_prop(p, "Controller").and_then(|c| c.as_obj()).is_some_and(|c| vm.class_is_a(vm.objects[c].class, "BystanderController"))).collect();
    // Pick a shooting spot (next to a bystander, 4 offsets each) with the most awake bystanders
    // in clear line of sight within 1500uu: a street, not a wall-partitioned interior.
    let world = p2game::physics::level_collision();
    let visible = |vm: &uscript::vm::Vm, spot: [f32; 3]| -> usize {
        bys.iter().filter(|&&o| {
            let lo = vm.actor_loc(o);
            let d = ((spot[0] - lo[0]).powi(2) + (spot[1] - lo[1]).powi(2)).sqrt();
            d < 1500.0 && d > 60.0 && world.as_ref().is_none_or(|w| w.line(glam::Vec3::from(spot) + glam::Vec3::Z * 40.0, glam::Vec3::from(lo) + glam::Vec3::Z * 40.0).is_none_or(|h| h.t > 0.97))
        }).count()
    };
    let mut best: Option<([f32; 3], usize)> = None;
    for &b in &bys {
        let lb = vm.actor_loc(b);
        for (dx, dy) in [(250.0, 0.0), (-250.0, 0.0), (0.0, 250.0), (0.0, -250.0)] {
            let s = [lb[0] + dx, lb[1] + dy, lb[2]];
            if world.as_ref().is_some_and(|w| w.line(glam::Vec3::from(lb) + glam::Vec3::Z * 40.0, glam::Vec3::from(s) + glam::Vec3::Z * 40.0).is_some()) {
                continue;
            }
            let n = visible(vm, s);
            if best.is_none_or(|(_, m)| n > m) {
                best = Some((s, n));
            }
        }
    }
    let Some((spot, nvis)) = best else {
        println!("no bystanders");
        return;
    };
    println!("shooting spot {spot:?}: {nvis} bystanders in clear sight");
    vm.set_prop(pp, "Location", Value::Vector(spot));
    let near: Vec<ObjId> = bys.iter().copied().filter(|&o| {
        let lo = vm.actor_loc(o);
        ((spot[0] - lo[0]).powi(2) + (spot[1] - lo[1]).powi(2)).sqrt() < 1500.0
    }).collect();
    let before: Vec<String> = near.iter().map(|&p| ctrl_state(vm, p)).collect();
    // Line-of-sight diagnostics between the shooter's spot and each bystander.
    if let Some(w) = p2game::physics::level_collision() {
        let mut clear = 0;
        for &p in &near {
            let l = vm.actor_loc(p);
            let h = w.line(glam::Vec3::from(spot), glam::Vec3::from(l));
            match h {
                None => clear += 1,
                Some(h) => println!("  LOS {} blocked at t={:.2} by {:?} start_solid={} n={:?}", vm.object_name(p), h.t, h.source, h.start_solid, h.normal),
            }
        }
        println!("LOS spot->bystanders: {clear}/{} clear", near.len());
    }
    let vm = &mut game.world.vm;
    let marker = vm.find_class_by_name("GunfireMarker").expect("GunfireMarker");
    // Arm the dude with a pistol the way a pickup would (CreateInventory + switch to it).
    let inv = vm.fire_event(pp, "CreateInventory", vec![Value::Str("Inventory.PistolWeapon".into())]);
    if let (Some(pc), Some(Value::Object(Some(w)))) = (game.player_controller, inv.clone()) {
        let wc = vm.objects[w].class;
        vm.fire_event(pc, "GetWeapon", vec![Value::Class(Some(wc))]);
    }
    for _ in 0..45 {
        game.tick(1.0 / 30.0);
    }
    let vm = &mut game.world.vm;
    let radius = vm.default_value(marker, "CollisionRadius").map(|v| v.as_float()).unwrap_or(1500.0);
    let level = vm.level;
    println!("gunfire at {spot:?} (marker radius {radius}), {} bystanders within 1500uu; player bPlayer={:?} weapon={:?}", near.len(), vm.get_prop(pp, "bPlayer"), vm.get_prop(pp, "Weapon").and_then(|w| w.as_obj()).map(|w| vm.object_name(w)));
    let mut visited: Vec<Vec<String>> = before.iter().map(|s| vec![s.clone()]).collect();
    let mut track = |game: &Game, visited: &mut Vec<Vec<String>>| {
        for (k, &p) in near.iter().enumerate() {
            let s = ctrl_state(&game.world.vm, p);
            if visited[k].last() != Some(&s) {
                visited[k].push(s);
            }
        }
    };
    for shot in 0..6 {
        // A real shot: the player aims at the nearest bystander and fires the equipped pistol
        // (P2Weapon makes the GunfireMarker / PawnShotMarker notifications itself); the static
        // marker notification is also sent in case the weapon could not be equipped headless.
        let vm = &mut game.world.vm;
        if let (Some(pc), Some(&target)) = (game.player_controller, near.iter().filter(|&&p| !vm.objects[p].deleted).min_by_key(|&&p| {
            let l = vm.actor_loc(p);
            (((spot[0] - l[0]).powi(2) + (spot[1] - l[1]).powi(2)).sqrt()) as i64
        })) {
            let tl = vm.actor_loc(target);
            let d = [tl[0] - spot[0], tl[1] - spot[1], tl[2] - spot[2]];
            let yaw = (d[1].atan2(d[0]) * 32768.0 / std::f32::consts::PI) as i32;
            vm.set_prop(pc, "Rotation", Value::Rotator([0, yaw, 0]));
            vm.set_prop(pp, "Rotation", Value::Rotator([0, yaw, 0]));
            vm.fire_event(pc, "Fire", vec![Value::Float(0.0)]);
        }
        let r = vm.call(marker, None, "NotifyControllersStatic", vec![Value::Object(level), Value::Class(Some(marker)), Value::Object(Some(pp)), Value::Object(Some(pp)), Value::Float(radius), Value::Vector(spot)]);
        // Second shot hits the nearest bystander: P2Weapon's hit path (TakeDamage on the victim,
        // then the PawnHitMarkerMade = PawnShotMarker notification around the victim).
        if shot == 1 {
            let victim = near.iter().copied().filter(|&p| !vm.objects[p].deleted).min_by_key(|&p| {
                let l = vm.actor_loc(p);
                (((spot[0] - l[0]).powi(2) + (spot[1] - l[1]).powi(2)).sqrt()) as i64
            });
            if let (Some(v), Ok(dmg), Ok(hit)) = (victim, vm.find_class_by_name("BulletDamage"), vm.find_class_by_name("PawnShotMarker")) {
                let vl = vm.actor_loc(v);
                vm.fire_event(v, "TakeDamage", vec![Value::Int(15), Value::Object(Some(pp)), Value::Vector(vl), Value::Vector([3000.0, 0.0, 0.0]), Value::Class(Some(dmg))]);
                let hr = vm.default_value(hit, "CollisionRadius").map(|v| v.as_float()).unwrap_or(1024.0);
                let _ = vm.call(hit, None, "NotifyControllersStatic", vec![Value::Object(level), Value::Class(Some(hit)), Value::Object(Some(pp)), Value::Object(Some(v)), Value::Float(hr), Value::Vector(vl)]);
                println!("shot hit {} (health now {:?})", vm.object_name(v), vm.get_prop(v, "Health"));
            }
        }
        if let Err(e) = r {
            println!("shot {shot}: {e}");
        }
        for _ in 0..36 {
            game.tick(1.0 / 30.0);
            track(&game, &mut visited);
        }
        game.drain_log(false);
    }
    for _ in 0..150 {
        game.tick(1.0 / 30.0);
        track(&game, &mut visited);
    }
    game.drain_log(false);
    let vm = &game.world.vm;
    let mut changed = 0;
    let mut fleeing = 0;
    for (k, &p) in near.iter().enumerate() {
        if vm.objects[p].deleted {
            continue;
        }
        let now = ctrl_state(vm, p);
        let d = ((spot[0] - vm.actor_loc(p)[0]).powi(2) + (spot[1] - vm.actor_loc(p)[1]).powi(2)).sqrt();
        if visited[k].len() > 1 {
            changed += 1;
        }
        let fled = visited[k].iter().any(|s| {
            let l = s.to_ascii_lowercase();
            l.contains("flee") || l.contains("run") || l.contains("panic") || l.contains("scream") || l.contains("cower") || l.contains("getdown") || l.contains("beg")
        });
        if fled {
            fleeing += 1;
        }
        println!("  {}: {} (now {:.0}uu from shot, final {now})", vm.object_name(p), visited[k].join(" > "), d);
    }
    println!("GUNFIRE {} bystanders near: {changed} changed state, {fleeing} fleeing/panicking; script errors {}", near.len(), vm.script_errors.values().sum::<u64>());
    if let Some(ai) = &ai {
        println!("perception after gunfire: {:?}", ai.stats.borrow());
    }
    if let Some(w) = vm.get_prop(pp, "Weapon").and_then(|w| w.as_obj()) {
        let ammo = vm.get_prop(w, "AmmoType").and_then(|a| a.as_obj());
        println!("weapon {} state {} ammo {:?}", vm.object_name(w), vm.state_name(w), ammo.and_then(|a| vm.get_prop(a, "AmmoAmount")));
    }
    let mut errs: Vec<_> = vm.script_errors.iter().collect();
    errs.sort_by(|a, b| b.1.cmp(a.1));
    for (e, n) in errs.iter().take(10) {
        println!("  err x{n} {e}");
    }
    let mut miss: Vec<_> = vm.missing_natives.iter().collect();
    miss.sort_by(|a, b| b.1.cmp(a.1));
    for (e, n) in miss.iter().take(25) {
        println!("  missing native x{n} {e}");
    }
}
