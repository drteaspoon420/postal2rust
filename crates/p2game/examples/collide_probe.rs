//! Collision survey probe: boots a map headless and lists the dynamic physics actors that collide
//! as cylinders although they are drawn as static meshes (UE2 collides those on their mesh unless
//! bUseCylinderCollision), plus how many level static mesh actors are baked into the static
//! level triangles while also being dynamic actors.
//! `collide_probe [Map] [--all]` (`--all` lists every such actor, not just the class summary).
//! `--walk X,Y,Z,YAW` additionally walks a standing pawn-sized cylinder forward from the point and
//! reports where it stops and what blocks it.
use glam::Vec3;
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::collections::BTreeMap;
use std::sync::Arc;
use uscript::vm::Value;

fn main() {
    std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let all = a.iter().any(|x| x == "--all");
    let walk: Option<Vec<f32>> = a.iter().position(|x| x == "--walk").and_then(|i| a.get(i + 1)).map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect());
    let root = p2game::game_root(None);
    // `--mesh Pkg.Group.Name[,..]`: static mesh collision data only.
    if let Some(list) = a.iter().position(|x| x == "--mesh").and_then(|i| a.get(i + 1)) {
        let lib = upkg::library::Library::new(&root).unwrap();
        for m in list.split(',') {
            let Some(o) = p2game::host::resolve_path(&lib, m, Some("StaticMesh")) else {
                println!("{m}: not found");
                continue;
            };
            let sm = upkg::staticmesh::read_static_mesh(&o.pkg, o.export).unwrap();
            if a.iter().any(|x| x == "--mat") {
                fn dump(lib: &upkg::library::Library, o: &upkg::library::Obj, depth: usize) {
                    let mat = upkg::material::read_material(&o.pkg, o.export);
                    println!("{}{} {}: {:?}", "  ".repeat(depth), o.class(), o.pkg.full_path(upkg::ObjRef::Export(o.export)), mat.as_ref().map(|m| format!("{m:?}")).unwrap_or_else(|e| format!("err {e}")).chars().take(600).collect::<String>());
                    if depth < 6 {
                        if let Ok(m) = mat {
                            for r in m.inputs() {
                                if let Some(c) = lib.resolve(&o.pkg, r) {
                                    dump(lib, &c, depth + 1);
                                }
                            }
                        }
                    }
                }
                for (k, r) in sm.materials.iter().enumerate() {
                    println!("{m} section {k}:");
                    if let Some(mo) = lib.resolve(&o.pkg, *r) {
                        dump(&lib, &mo, 1);
                    }
                }
            }
            let p = |k: &str| sm.props.get(k).map(|v| format!("{v:?}")).unwrap_or("-".into());
            let cm = if sm.collision_model == upkg::ObjRef::Null { "none".to_string() } else { o.pkg.full_path(sm.collision_model) };
            println!(
                "{m}: bounds {:?}..{:?} render tris {} collision tris {} model {cm} mat_coll {:?} UseSimpleBox {} UseSimpleLine {} UseSimpleKarma {}",
                sm.bounds_min,
                sm.bounds_max,
                sm.indices.len() / 3,
                sm.collision_tris.len(),
                sm.material_collision,
                p("UseSimpleBoxCollision"),
                p("UseSimpleLineCollision"),
                p("UseSimpleKarmaCollision")
            );
            if let upkg::ObjRef::Export(e) = sm.collision_model {
                if let Ok(model) = upkg::model::read_model(&o.pkg, e) {
                    let pts: Vec<[f32; 3]> = model.points.clone();
                    let (lo, hi) = pts.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), q| ([lo[0].min(q[0]), lo[1].min(q[1]), lo[2].min(q[2])], [hi[0].max(q[0]), hi[1].max(q[1]), hi[2].max(q[2])]));
                    println!("   model: {} nodes {} points bounds {lo:?}..{hi:?}", model.nodes.len(), pts.len());
                }
            }
        }
        return;
    }
    p2game::host::register_level_hook(p2game::physics::level_hook);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, None);
    game.begin_play("");
    for _ in 0..3 {
        game.tick(1.0 / 30.0);
    }
    let Some(ph) = p2game::physics::current() else {
        println!("no physics");
        return;
    };
    let vm = &game.world.vm;
    let w = ph.world.borrow();
    let br = ph.bridge.borrow();
    let mut by_class: BTreeMap<String, (usize, f32, f32)> = BTreeMap::new();
    let mut mesh_dyn = 0;
    for id in w.ids() {
        let Some(act) = w.get(id) else { continue };
        let Some(o) = br.obj(id) else { continue };
        let sm = matches!(vm.get_prop(o, "DrawType"), Some(Value::Byte(8)));
        let has_mesh = vm.get_prop(o, "StaticMesh").and_then(|v| v.as_resource().map(|s| s.to_string()));
        if !sm || has_mesh.is_none() || !act.collide_actors || !(act.block_actors || act.block_players) {
            continue;
        }
        let use_cyl = vm.get_prop(o, "bUseCylinderCollision").is_some_and(|v| v.as_bool());
        if act.collider.is_cylinder() {
            let e = by_class.entry(format!("{}{}", act.class, if use_cyl { " (bUseCylinderCollision)" } else { "" })).or_insert((0, 0.0, 0.0));
            e.0 += 1;
            e.1 = e.1.max(act.collision_radius);
            e.2 = e.2.max(act.collision_height);
            if all {
                println!("cyl {} {} r {:.0} h {:.0} at {:?} mesh {:?}", vm.object_name(o), act.class, act.collision_radius, act.collision_height, act.location, has_mesh);
            }
        } else {
            mesh_dyn += 1;
        }
    }
    println!("{map}: dynamic blocking static-mesh actors colliding as cylinders (class: count, max r, max h):");
    for (k, (n, r, h)) in &by_class {
        println!("  {k}: {n} r<={r:.0} h<={h:.0}");
    }
    println!("  with mesh colliders: {mesh_dyn}");
    let st = &w.level.stats;
    println!("level: bsp tris {} actor tris {} blocking actors {} (collision tris {}, render fallback {})", st.bsp_tris, st.actor_tris, st.blocking_actors, st.meshes_with_collision_tris, st.meshes_render_fallback);
    drop(w);
    drop(br);
    if a.iter().any(|x| x == "--windows") {
        // Sweep a Dude-sized cylinder (crouched, r 25 h 40, and standing h 69) through each
        // window's centre along its facing, intact and with collision off (Broken state).
        let mut w = ph.world.borrow_mut();
        let br = ph.bridge.borrow();
        let probe = w.add(p2physics::Actor::pawn("ProbePawn", Vec3::new(0.0, 0.0, -1.0e5), 25.0, 40.0, p2physics::PawnInfo { is_player: true, ..Default::default() }));
        let (mut n, mut blocked_intact, mut open_broken) = (0, 0, 0);
        for id in w.ids() {
            let Some(o) = br.obj(id) else { continue };
            if !vm.class_is_a(vm.objects[o].class, "Window") {
                continue;
            }
            let (loc, rot, r) = { let x = w.get(id).unwrap(); (x.location, x.rotation, x.collision_radius) };
            let k = std::f32::consts::TAU / 65536.0;
            let dir = Vec3::new((rot[1] as f32 * k).cos(), (rot[1] as f32 * k).sin(), 0.0);
            let start = loc - dir * 80.0;
            let res = |w: &mut p2physics::PhysWorld, h: f32| {
                w.get_mut(probe).unwrap().collision_height = h;
                w.get_mut(probe).unwrap().location = start;
                w.sweep(probe, start, dir * 160.0, &[]).map(|h| (format!("{:?}", h.actor), h.t * 160.0, h.start_solid))
            };
            let intact = res(&mut w, 40.0);
            // Opening at the glass plane: sill (first hit below the centre) and lintel (above),
            // and the floor 80 units in front (start side), all with the window's collision off.
            let sill = w.level.line(loc, loc - Vec3::Z * 400.0).map(|h| h.location.z);
            let lintel = w.level.line(loc, loc + Vec3::Z * 400.0).map(|h| h.location.z);
            let floor = w.level.line(start, start - Vec3::Z * 600.0).map(|h| h.location.z);
            println!("opening {}: floor {floor:?} sill {sill:?} lintel {lintel:?} -> sill above floor {:?} opening {:?}", vm.object_name(o), sill.zip(floor).map(|(s, f)| (s - f).round()), sill.zip(lintel).map(|(s, l)| (l - s).round()));
            let flags = { let x = w.get(id).unwrap(); (x.collide_actors, x.block_actors, x.block_players) };
            w.set_collision(id, false, false, false);
            let broken = res(&mut w, 40.0);
            let broken_stand = res(&mut w, 69.0);
            w.set_collision(id, flags.0, flags.1, flags.2);
            n += 1;
            if intact.as_ref().is_some_and(|h| h.0.contains(&format!("Actor({id})"))) {
                blocked_intact += 1;
            }
            if broken.is_none() {
                open_broken += 1;
            }
            println!("window {} r {r:.0} at {loc:.0}: intact {intact:?} | broken crouched {broken:?} standing {broken_stand:?}", vm.object_name(o));
        }
        println!("windows {n}: blocked by the glass when intact {blocked_intact}, crouch-passable when broken {open_broken}");
        w.remove(probe);
    }
    if let Some(name) = a.iter().position(|x| x == "--through").and_then(|i| a.get(i + 1)) {
        // Walk a standing Dude (r 25 h 69, crouched h 40 with --crouch) from 120 units in front of
        // the named actor through its centre, with its collision on, then off.
        let crouch = a.iter().any(|x| x == "--crouch");
        let mut w = ph.world.borrow_mut();
        let br = ph.bridge.borrow();
        let Some(target) = w.ids().into_iter().find(|&i| br.obj(i).is_some_and(|o| vm.object_name(o).eq_ignore_ascii_case(name))) else {
            println!("{name}: not found");
            return;
        };
        let (loc, rot) = { let x = w.get(target).unwrap(); (x.location, x.rotation) };
        let k = std::f32::consts::TAU / 65536.0;
        let dir = Vec3::new((rot[1] as f32 * k).cos(), (rot[1] as f32 * k).sin(), 0.0);
        for side in [1.0f32, -1.0] {
            for on in [true, false] {
                let flags = { let x = w.get(target).unwrap(); (x.collide_actors, x.block_actors, x.block_players) };
                if !on {
                    w.set_collision(target, false, false, false);
                }
                let d = dir * side;
                let start = loc - d * 120.0;
                let h = if crouch { 40.0 } else { 69.0 };
                let mut pawn = p2physics::Actor::pawn("ProbePawn", start, 25.0, h, p2physics::PawnInfo { is_player: true, ..Default::default() });
                pawn.physics = p2physics::Physics::Walking;
                let id = w.add(pawn);
                let placed = w.place_on_floor(id, start, 300.0);
                let p0 = w.get(id).unwrap().location;
                let mut last_hit = String::new();
                if a.iter().any(|x| x == "--jump") {
                    // Run up (walking) and jump at 60 units from the plane, holding forward:
                    // the real PHYS_Walking / PHYS_Falling tick with the Dude's JumpZ.
                    let pp = w.level.pawn;
                    if let Some(p) = w.get_mut(id).and_then(|x| x.pawn.as_mut()) {
                        p.jump_z = pp.jump_z;
                        p.ground_speed = pp.ground_speed;
                        p.air_control = pp.air_control;
                    }
                    let mut jumped = false;
                    for _ in 0..90 {
                        let x = w.get(id).unwrap();
                        let dist = (loc - x.location).dot(d);
                        if !jumped && dist < 75.0 && x.physics == p2physics::Physics::Walking {
                            let v = x.velocity + Vec3::Z * pp.jump_z;
                            w.get_mut(id).unwrap().velocity = v;
                            w.set_physics(id, p2physics::Physics::Falling);
                            jumped = true;
                        }
                        w.get_mut(id).unwrap().acceleration = d * 2048.0;
                        w.tick_actor(id, 1.0 / 30.0);
                    }
                    last_hit = format!("jumped {jumped} (JumpZ {})", pp.jump_z);
                } else {
                for _ in 0..40 {
                    let r = w.move_smooth(id, d * 8.0);
                    if let Some(hh) = r.hit {
                        last_hit = match hh.actor {
                            p2physics::HitActor::Actor(b) => format!("{} n {:.2}", br.obj(b).map(|o| vm.object_name(o)).unwrap_or_default(), hh.normal),
                            other => format!("{other:?} n {:.2}", hh.normal),
                        };
                    }
                }
                }
                let p1 = w.get(id).unwrap().location;
                let past = (p1 - loc).dot(d);
                println!("{name} side {side} collision {on}: start {p0:.0} (floor {placed}, window z {:.0}) end {p1:.0} -> {} ({past:.0} past the plane) last hit {last_hit}", loc.z, if past > 30.0 { "THROUGH" } else { "blocked" });
                w.remove(id);
                if !on {
                    w.set_collision(target, flags.0, flags.1, flags.2);
                }
            }
        }
    }
    if let Some(v) = walk.filter(|v| v.len() >= 4) {
        let mut w = ph.world.borrow_mut();
        let start = Vec3::new(v[0], v[1], v[2]);
        let yaw = v[3].to_radians();
        let dir = Vec3::new(yaw.cos(), yaw.sin(), 0.0);
        let mut pawn = p2physics::Actor::pawn("ProbePawn", start, 25.0, 69.0, p2physics::PawnInfo { is_player: true, ..Default::default() });
        pawn.physics = p2physics::Physics::Walking;
        let id = w.add(pawn);
        w.place_on_floor(id, start, 400.0);
        for step in 0..60 {
            let before = w.get(id).unwrap().location;
            let r = w.move_smooth(id, dir * 10.0);
            let after = w.get(id).unwrap().location;
            if let Some(h) = r.hit {
                let what = match h.actor {
                    p2physics::HitActor::Actor(b) => w.get(b).map(|x| format!("{} r{:.0}", x.class, x.collision_radius)).unwrap_or_default(),
                    other => format!("{other:?}"),
                };
                println!("walk step {step} {before:?} -> {after:?} hit {what} n {:?}", h.normal);
                if (after - before).length() < 1.0 {
                    break;
                }
            }
        }
        println!("walk end {:?}", w.get(id).unwrap().location);
    }
}
