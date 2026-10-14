//! Crowd movement probe (headless): finds NPCs that try to walk but do not get anywhere.
//! Every pawn within `--radius` of the player is kept awake (LastRenderTime = now, as if seen) so
//! stasis does not hide the problem. Each second, a pawn whose Acceleration is set (a MoveTo /
//! MoveToward is steering it) but that moved less than 40 uu in the last 3 s counts as stalled.
//! Prints the stall seconds per pawn, hotspots (200 uu cells) and the steering counters.
//! `crowd_probe [Map] [--secs N] [--at X,Y,Z] [--radius R] [--game Class] [--day N]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use uscript::vm::{ObjId, Value, Vm};

fn ctrl_state(vm: &Vm, p: ObjId) -> String {
    vm.get_prop(p, "Controller").and_then(|c| c.as_obj()).map(|c| vm.state_name(c)).unwrap_or_else(|| "-".into())
}

fn parse3(s: &str) -> Option<[f32; 3]> {
    let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    (v.len() == 3).then(|| [v[0], v[1], v[2]])
}

fn main() {
    std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let secs: f32 = get("--secs").and_then(|s| s.parse().ok()).unwrap_or(90.0);
    let radius: f32 = get("--radius").and_then(|s| s.parse().ok()).unwrap_or(6000.0);
    let day: i32 = get("--day").and_then(|s| s.parse().ok()).unwrap_or(1);
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::viewport::install();
    p2game::clock::install();
    let _ai = p2game::ai::install();
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let game_class = get("--game").unwrap_or_else(|| "GameTypes.GameSinglePlayer".into());
    let mut game = Game::new(world, lv, Some(&game_class));
    game.begin_play(&format!("?SetDay={day}"));
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    game.tick(1.0 / 30.0);
    let pc = game.player_controller.expect("player");
    let pp = game.world.vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).expect("pawn");
    if let Some(at) = get("--at").and_then(|s| parse3(&s)) {
        let host = game.host.clone();
        let ok = uscript::host::Host::move_actor(&*host, &mut game.world.vm, pp, at, false);
        println!("player to {at:?}: {ok}");
    }
    let centre = game.world.vm.actor_loc(pp);
    // `--edges X,Y,Z R`: walk-test every ReachSpec starting within R of the point with the AI's
    // own reachability sweep (navfix::walk_test on the physics world) and print the blocked ones.
    if let Some(spot) = get("--edges").and_then(|s| parse3(&s)) {
        let r: f32 = a.iter().position(|x| x == "--edges").and_then(|i| a.get(i + 2)).and_then(|s| s.parse().ok()).unwrap_or(600.0);
        let vm = &game.world.vm;
        let g = vm.nav.graph.as_ref().expect("nav graph");
        let ph = p2game::physics::current().expect("physics");
        let mut w = ph.world.borrow_mut();
        for k in 0..g.node_count() as u32 {
            let l = g.node_location(k);
            if ((l[0] - spot[0]).powi(2) + (l[1] - spot[1]).powi(2)).sqrt() > r || (l[2] - spot[2]).abs() > 400.0 {
                continue;
            }
            let name = g.node_obj(k).map(|o| vm.object_name(o)).unwrap_or_default();
            println!("node {name} at {:.0},{:.0},{:.0}", l[0], l[1], l[2]);
            for e in g.edges(k) {
                let to = g.node_location(e.end);
                let res = p2game::navfix::walk_test(&mut *w, glam::Vec3::from(l), glam::Vec3::from(to), 30.0, 70.0, 35.0);
                let other = g.node_obj(e.end).map(|o| vm.object_name(o)).unwrap_or_default();
                println!("   -> {other} {:.0},{:.0},{:.0} dist {} flags {:#x} r {} h {}: {res:?}", to[0], to[1], to[2], e.distance, e.reach_flags, e.collision_radius, e.collision_height);
            }
        }
        return;
    }
    // `--blockers X,Y,Z R`: physics actors that block pawns with collision bounds within R.
    if let Some(spot) = get("--blockers").and_then(|s| parse3(&s)) {
        let r: f32 = a.iter().position(|x| x == "--blockers").and_then(|i| a.get(i + 2)).and_then(|s| s.parse().ok()).unwrap_or(200.0);
        let vm = &game.world.vm;
        let ph = p2game::physics::current().expect("physics");
        let w = ph.world.borrow();
        let p = glam::Vec3::from(spot);
        for id in w.ids() {
            let o = w.get(id).unwrap();
            if !o.collide_actors || !o.block_actors || o.is_pawn() {
                continue;
            }
            let b = o.bounds();
            let d = (p.clamp(b.min, b.max) - p).length();
            if d > r {
                continue;
            }
            let name = if (o.user as usize) < vm.objects.len() { vm.object_name(o.user as ObjId) } else { String::new() };
            println!("blocker {name} {} d {d:.0} bounds {:.0?}..{:.0?} static {} worldgeo {} players {} nonzero {}", o.class, b.min, b.max, o.is_static, o.world_geometry, o.block_players, o.block_nonzero_extent);
        }
        return;
    }
    // `--walktest X,Y,Z X,Y,Z`: the AI reachability walk between two points against the level only
    // and against the physics world (level + static blockers), and the steering probe there.
    if let (Some(from), Some(to)) = (get("--walktest").and_then(|s| parse3(&s)), a.iter().position(|x| x == "--walktest").and_then(|i| a.get(i + 2)).and_then(|s| parse3(s))) {
        let ph = p2game::physics::current().expect("physics");
        let lvl = p2game::physics::level_collision().expect("level");
        struct L(std::sync::Arc<p2collide::World>);
        impl p2game::navfix::Sweep for L {
            fn sweep(&mut self, s: glam::Vec3, e: glam::Vec3, r: f32, h: f32) -> Option<(f32, glam::Vec3, bool)> {
                self.0.trace(s, e, &p2collide::Shape::Cylinder { radius: r, half_height: h }, p2collide::TraceFlags::ALL).map(|h| (h.t, h.normal, h.start_solid))
            }
        }
        let mut lw = L(lvl);
        let mut w = ph.world.borrow_mut();
        let (f, t) = (glam::Vec3::from(from), glam::Vec3::from(to));
        println!("level only: {:?}", p2game::navfix::walk_test(&mut lw, f, t, 28.0, 72.0, 35.0));
        println!("physics:    {:?}", p2game::navfix::walk_test(&mut *w, f, t, 28.0, 72.0, 35.0));
        let dir = glam::Vec3::new(t.x - f.x, t.y - f.y, 0.0).normalize_or_zero();
        println!("steer level {:?} physics {:?}", p2game::navfix::steer_dir(&mut lw, f, dir, 124.0, 28.0, 72.0, 35.0, 1.0), p2game::navfix::steer_dir(&mut *w, f, dir, 124.0, 28.0, 72.0, 35.0, 1.0));
        return;
    }
    let dt: f32 = get("--dt").and_then(|s| s.parse().ok()).unwrap_or(1.0 / 30.0);
    let per_sec = (1.0 / dt).round() as usize;
    let frames = (secs / dt) as usize;
    let mut wants_hist: HashMap<ObjId, Vec<bool>> = HashMap::new();
    let mut jam_secs: HashMap<ObjId, u32> = HashMap::new();
    let mut hist: HashMap<ObjId, Vec<[f32; 3]>> = HashMap::new();
    let mut stall_secs: HashMap<ObjId, u32> = HashMap::new();
    let mut moving_secs: HashMap<ObjId, u32> = HashMap::new();
    let mut path_len: HashMap<ObjId, f32> = HashMap::new();
    let mut last: HashMap<ObjId, [f32; 3]> = HashMap::new();
    let mut cells: BTreeMap<(i32, i32), u32> = BTreeMap::new();
    let mut stall_states: BTreeMap<String, u32> = BTreeMap::new();
    // `--zombies`: state entries of every zombie controller (attacks vs walking / taunting) and
    // the player's health over time (God mode off; the Dude gets healed back to 100 at 30).
    let zombies = a.iter().any(|x| x == "--zombies");
    let mut zstate: HashMap<ObjId, String> = HashMap::new();
    let mut zentries: BTreeMap<String, u32> = BTreeMap::new();
    let mut hp_lost = 0i64;
    let mut last_hp: Option<i64> = None;
    let t0 = std::time::Instant::now();
    // `--circle R SPEED`: the player walks a circle of radius R around his start at SPEED uu/s
    // (swept moves, so he stays out of walls), like a player moving through a fight.
    let circle: Option<(f32, f32)> = a.iter().position(|x| x == "--circle").and_then(|i| Some((a.get(i + 1)?.parse().ok()?, a.get(i + 2)?.parse().ok()?)));
    let mut angle = 0.0f32;
    for f in 0..frames {
        if let Some((r, sp)) = circle {
            angle += sp * dt / r.max(1.0);
            let host = game.host.clone();
            let vm = &mut game.world.vm;
            let cur = vm.actor_loc(pp);
            let want = [centre[0] + r * angle.cos(), centre[1] + r * angle.sin(), cur[2]];
            let delta = [want[0] - cur[0], want[1] - cur[1], 0.0];
            let _ = uscript::host::Host::move_smooth(&*host, vm, pp, delta);
        }
        // Keep the crowd awake: everything near the player counts as seen.
        {
            let vm = &mut game.world.vm;
            let now = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|v| v.as_float()).unwrap_or(0.0);
            let near: Vec<ObjId> = vm
                .actors
                .iter()
                .copied()
                .filter(|&x| !vm.objects[x].deleted && x != pp && vm.class_is_a(vm.objects[x].class, "Pawn"))
                .filter(|&x| {
                    let l = vm.actor_loc(x);
                    ((l[0] - centre[0]).powi(2) + (l[1] - centre[1]).powi(2)).sqrt() < radius
                })
                .collect();
            for x in near {
                vm.set_prop(x, "LastRenderTime", Value::Float(now));
            }
        }
        game.tick(dt);
        game.drain_log(false);
        if zombies {
            let vm = &mut game.world.vm;
            for x in vm.actors.clone() {
                if vm.objects[x].deleted || !vm.classes[vm.objects[x].class].name.to_ascii_lowercase().contains("zombie") {
                    continue;
                }
                let Some(c) = vm.get_prop(x, "Controller").and_then(|v| v.as_obj()) else { continue };
                let s = vm.state_name(c);
                if zstate.get(&x) != Some(&s) {
                    *zentries.entry(s.clone()).or_default() += 1;
                    zstate.insert(x, s);
                }
            }
            let hp = vm.get_prop(pp, "Health").map(|v| v.as_int() as i64).unwrap_or(0);
            if let Some(l) = last_hp {
                if hp < l {
                    hp_lost += l - hp;
                }
            }
            if hp < 30 {
                vm.set_prop(pp, "Health", Value::Int(100));
            }
            last_hp = Some(vm.get_prop(pp, "Health").map(|v| v.as_int() as i64).unwrap_or(0));
            if f % (30 * 20) == 30 * 20 - 1 {
                println!("t={:.0}s ZOMBIE entries {zentries:?} player hp lost {hp_lost}", (f + 1) as f32 * dt);
            }
        }
        let vm = &game.world.vm;
        let pawns: Vec<ObjId> = vm.actors.iter().copied().filter(|&x| !vm.objects[x].deleted && x != pp && vm.class_is_a(vm.objects[x].class, "Pawn") && vm.get_prop(x, "Controller").and_then(|c| c.as_obj()).is_some()).collect();
        for &p in &pawns {
            let l = vm.actor_loc(p);
            if let Some(o) = last.get(&p) {
                *path_len.entry(p).or_default() += ((l[0] - o[0]).powi(2) + (l[1] - o[1]).powi(2)).sqrt();
            }
            last.insert(p, l);
        }
        if f % per_sec != per_sec - 1 {
            continue;
        }
        for &p in &pawns {
            let l = vm.actor_loc(p);
            let h = hist.entry(p).or_default();
            h.push(l);
            let acc = vm.get_prop(p, "Acceleration").map(|v| v.as_vec()).unwrap_or_default();
            let wants = (acc[0] * acc[0] + acc[1] * acc[1]).sqrt() > 1.0;
            let phys = vm.get_prop(p, "Physics").map(|v| v.as_int()).unwrap_or(0);
            let wh = wants_hist.entry(p).or_default();
            wh.push(wants && phys == 1);
            // Jam: steering for at least 5 of the last 6 s but net displacement under 120 uu.
            if h.len() >= 7 && wh[wh.len() - 6..].iter().filter(|&&w| w).count() >= 5 {
                let o = h[h.len() - 7];
                if ((l[0] - o[0]).powi(2) + (l[1] - o[1]).powi(2)).sqrt() < 120.0 {
                    *jam_secs.entry(p).or_default() += 1;
                }
            }
            if !wants || phys != 1 {
                continue;
            }
            *moving_secs.entry(p).or_default() += 1;
            if h.len() >= 4 {
                let o = h[h.len() - 4];
                if ((l[0] - o[0]).powi(2) + (l[1] - o[1]).powi(2)).sqrt() < 40.0 {
                    *stall_secs.entry(p).or_default() += 1;
                    *cells.entry(((l[0] / 200.0).floor() as i32, (l[1] / 200.0).floor() as i32)).or_default() += 1;
                    *stall_states.entry(ctrl_state(vm, p)).or_default() += 1;
                }
            }
        }
        if f % (per_sec * 30) == per_sec * 30 - 1 {
            let stalled_now = stall_secs.values().sum::<u32>();
            println!("t={:.0}s stall-seconds so far {stalled_now} ({:.1}s wall)", (f + 1) as f32 * dt, t0.elapsed().as_secs_f32());
        }
    }
    let vm = &game.world.vm;
    let total_moving: u32 = moving_secs.values().sum();
    let total_stall: u32 = stall_secs.values().sum();
    let walked = path_len.values().filter(|&&d| d > 50.0).count();
    let dist: f32 = path_len.values().sum();
    println!(
        "CROWD {map} {secs}s: {} pawns, {walked} walked >50uu, path {dist:.0}uu, moving-seconds {total_moving}, STALL-seconds {total_stall} ({:.1}%), stuck {} detour {} deflect {}",
        last.len(),
        100.0 * total_stall as f32 / total_moving.max(1) as f32,
        p2game::physics::NAV_STUCK.with(|c| c.get()),
        p2game::physics::NAV_DETOUR.with(|c| c.get()),
        p2game::physics::NAV_DEFLECT.with(|c| c.get()),
    );
    println!("stall by state: {stall_states:?}");
    if zombies {
        let attacks: u32 = ["BigSmash", "SwipeLeft", "SwipeRight", "VomitAttack"].iter().map(|k| zentries.get(*k).copied().unwrap_or(0)).sum();
        println!("ZOMBIES {secs}s: attack-state entries {attacks}, player hp lost {hp_lost}, entries {zentries:?}");
    }
    let total_jam: u32 = jam_secs.values().sum();
    println!("JAM-seconds {total_jam} ({:.1}% of moving) over {} pawns", 100.0 * total_jam as f32 / total_moving.max(1) as f32, jam_secs.len());
    let mut jw: Vec<(ObjId, u32)> = jam_secs.iter().map(|(p, s)| (*p, *s)).collect();
    jw.sort_by(|a, b| b.1.cmp(&a.1));
    for &(p, s) in jw.iter().take(10) {
        let l = vm.actor_loc(p);
        println!("  jammed {s:>3}s {} at {:.0},{:.0},{:.0} state {}", vm.object_name(p), l[0], l[1], l[2], ctrl_state(vm, p));
    }
    println!("reach tests {} fallback(no collision) {}", p2game::navfix::REACH_TESTS.with(|c| c.get()), p2game::navfix::REACH_FALLBACK.with(|c| c.get()));
    let mut worst: Vec<(ObjId, u32)> = stall_secs.iter().map(|(p, s)| (*p, *s)).filter(|x| x.1 >= 5).collect();
    worst.sort_by(|a, b| b.1.cmp(&a.1));
    for &(p, s) in worst.iter().take(15) {
        let l = vm.actor_loc(p);
        let dest = vm.get_prop(p, "Controller").and_then(|c| c.as_obj()).map(|c| {
            let d = vm.get_prop(c, "Destination").map(|v| v.as_vec()).unwrap_or_default();
            let mt = vm.get_prop(c, "MoveTarget").and_then(|v| v.as_obj()).map(|m| vm.object_name(m)).unwrap_or_default();
            format!("dest {:.0},{:.0},{:.0} movetarget {mt}", d[0], d[1], d[2])
        });
        println!("  stalled {s:>3}s {} at {:.0},{:.0},{:.0} state {} {}", vm.object_name(p), l[0], l[1], l[2], ctrl_state(vm, p), dest.unwrap_or_default());
    }
    let mut hot: Vec<_> = cells.into_iter().collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1));
    for ((x, y), n) in hot.iter().take(10) {
        println!("  hotspot {},{} .. +200: {n} stall-seconds", x * 200, y * 200);
    }
    println!("script errors {}", vm.script_errors.values().sum::<u64>());
}
