//! Research probe: moves a map's pawn of a class to a spot and asks the path network how to get
//! to a point (PointReachable + FindPathTo), printing the route.
//! Usage: route_probe <Map> <PawnClass> <x,y,z from> <x,y,z to>
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::nav::Goal;
use uscript::vm::Value;

fn v3(s: &str) -> [f32; 3] {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
    [v[0], v[1], v[2]]
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (map, class, from, to) = (&a[0], &a[1], v3(&a[2]), v3(&a[3]));
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(map).cloned().unwrap();
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    p2game::host::register_level_hook(p2game::physics::level_hook);
    let mut game = Game::new(world, lv, None);
    game.begin_play("");
    for _ in 0..5 {
        game.tick(1.0 / 30.0);
    }
    if std::env::var_os("CATS").is_some() {
        for sec in 0..40 {
            for _ in 0..30 {
                game.tick(1.0 / 30.0);
            }
            let vm = &game.world.vm;
            if sec % 5 == 0 {
                for &p in &vm.actors {
                    if !vm.objects[p].deleted && vm.class_is_a(vm.objects[p].class, class) {
                        let c = vm.get_prop(p, "Controller").and_then(|v| v.as_obj());
                        let l = vm.actor_loc(p);
                        println!("t{sec} {} {:.0},{:.0},{:.0} phys {:?} state {}", vm.object_name(p), l[0], l[1], l[2], vm.get_prop(p, "Physics"), c.map(|c| vm.state_name(c)).unwrap_or_default());
                    }
                }
            }
        }
        return;
    }
    let vm = &mut game.world.vm;
    let pawn = vm.actors.iter().copied().find(|&p| !vm.objects[p].deleted && vm.class_is_a(vm.objects[p].class, class)).expect("no pawn of that class");
    let ctrl = vm.get_prop(pawn, "Controller").and_then(|v| v.as_obj()).expect("controller");
    println!("pawn {} ctrl {} radius {:?} height {:?}", vm.object_name(pawn), vm.object_name(ctrl), vm.get_prop(pawn, "CollisionRadius"), vm.get_prop(pawn, "CollisionHeight"));
    vm.set_prop(pawn, "Location", Value::Vector(from));
    vm.set_prop(pawn, "Anchor", Value::Object(None));
    let m = vm.mover(pawn, Some(ctrl));
    println!("mover {:?}", (m.radius, m.height, m.can_jump, m.can_open_doors));
    let hop = vm.find_route(ctrl, Goal::Point(to), true);
    println!("FindPathTo -> {:?}", hop.as_ref().map(|h| h.map(|o| (vm.object_name(o), vm.actor_loc(o)))));
    if let Some(Value::Array(rc)) = vm.get_prop(ctrl, "RouteCache").cloned() {
        for o in rc.iter().filter_map(|v| v.as_obj()) {
            println!("  route {} {:?}", vm.object_name(o), vm.actor_loc(o));
        }
    }
    println!("stats {:?}", vm.nav.stats);
    // Navigation points near `from` / `to` and their outgoing reach specs.
    let near: Vec<_> = vm.actors.iter().copied().filter(|&n| vm.class_is_a(vm.objects[n].class, "NavigationPoint")).filter(|&n| {
        let l = vm.actor_loc(n);
        let d = |p: [f32; 3]| ((l[0] - p[0]).powi(2) + (l[1] - p[1]).powi(2)).sqrt();
        d(from) < 450.0 || d(to) < 60.0
    }).collect();
    for n in near {
        println!("node {} {:?}", vm.object_name(n), vm.actor_loc(n));
        if let Some(Value::Array(pl)) = vm.get_prop(n, "PathList").cloned() {
            for s in pl.iter().filter_map(|v| v.as_obj()) {
                let e = vm.get_prop(s, "End").and_then(|v| v.as_obj());
                println!("    -> {} {:?} dist {:?} flags {:?}", e.map(|e| vm.object_name(e)).unwrap_or_default(), e.map(|e| vm.actor_loc(e)), vm.get_prop(s, "Distance"), vm.get_prop(s, "reachFlags"));
            }
        }
    }
    // Free walking distance from `from` in 16 directions (up to 1000 UU).
    let m = vm.mover(pawn, Some(ctrl));
    if let Some(ph) = p2game::physics::current() {
        let mut w = ph.world.borrow_mut();
        let f = glam::Vec3::from(from);
        for k in 0..16 {
            let a = k as f32 * std::f32::consts::TAU / 16.0;
            let to = f + glam::Vec3::new(a.cos(), a.sin(), 0.0) * 1000.0;
            let r = p2game::navfix::walk_test(&mut *w, f, to, m.radius, m.height, m.max_step);
            println!("  dir {:>5.0} deg: {r:?}", a.to_degrees());
        }
    }
    // Extra args: points to test PointReachable from `from`.
    vm.set_prop(pawn, "Location", Value::Vector(from));
    for p in a.iter().skip(4) {
        let p = v3(p);
        let m = vm.mover(pawn, Some(ctrl));
        let ok = vm.point_reachable(&m, p);
        println!("reachable {:?} -> {:?}: {ok} (level tests {}, fallback {})", from, p, p2game::navfix::REACH_TESTS.with(|c| c.get()), p2game::navfix::REACH_FALLBACK.with(|c| c.get()));
        if let Some(ph) = p2game::physics::current() {
            let mut w = ph.world.borrow_mut();
            let r = p2game::navfix::walk_test(&mut *w, glam::Vec3::from(from), glam::Vec3::from(p), m.radius, m.height, m.max_step);
            println!("  walk_test {r:?}");
        }
    }
}
