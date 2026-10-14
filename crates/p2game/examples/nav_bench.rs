//! Perf probe: times route searches (FindRandomDest) for every AI pawn of a map and checks that
//! the results are unchanged between builds (checksum of the chosen destinations).
//! Usage: nav_bench <Map> [rounds]
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use std::time::Instant;
use uscript::nav::Goal;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct LevelOnly(Arc<p2collide::World>);

/// CPU time used by this thread, in seconds (machine load does not inflate it).
#[cfg(windows)]
fn thread_cpu() -> f64 {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        lo: u32,
        hi: u32,
    }
    unsafe extern "system" {
        fn GetCurrentThread() -> isize;
        fn GetThreadTimes(h: isize, c: *mut FileTime, e: *mut FileTime, k: *mut FileTime, u: *mut FileTime) -> i32;
    }
    let (mut c, mut e, mut k, mut u) = (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
    unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) };
    let t = |f: FileTime| ((f.hi as u64) << 32 | f.lo as u64) as f64 * 1e-7;
    t(k) + t(u)
}

#[cfg(not(windows))]
fn thread_cpu() -> f64 {
    0.0
}

impl p2game::navfix::Sweep for LevelOnly {
    fn sweep(&mut self, start: glam::Vec3, end: glam::Vec3, radius: f32, half_height: f32) -> Option<(f32, glam::Vec3, bool)> {
        self.0.trace(start, end, &p2collide::Shape::Cylinder { radius, half_height }, p2collide::TraceFlags::ALL).map(|h| (h.t, h.normal, h.start_solid))
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().cloned().unwrap_or_else(|| "suburbs-3".into());
    let rounds: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().unwrap();
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    p2game::host::register_level_hook(p2game::physics::level_hook);
    let mut game = Game::new(world, lv, None);
    game.begin_play("");
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
    }
    let vm = &mut game.world.vm;
    let ctrls: Vec<usize> = vm
        .actors
        .iter()
        .copied()
        .filter(|&c| !vm.objects[c].deleted && vm.class_is_a(vm.objects[c].class, "AIController"))
        .filter(|&c| vm.get_prop(c, "Pawn").and_then(|v| v.as_obj()).is_some())
        .collect();
    println!("{map}: {} AI controllers with pawns", ctrls.len());
    // Reset the script RNG-independent part: the pick uses frand, so the checksum compares runs
    // of the same binary/seed; route caches (RouteCache) are hashed too.
    let mut checksum = 0u64;
    let (mut n, mut found) = (0u32, 0u32);
    let t = Instant::now();
    let c0 = thread_cpu();
    let r0 = p2game::navfix::REACH_TESTS.with(|c| c.get());
    for _ in 0..rounds {
        for &c in &ctrls {
            let r = vm.find_route(c, Goal::Random, true).ok().flatten();
            n += 1;
            if let Some(o) = r {
                found += 1;
                checksum = checksum.wrapping_mul(31).wrapping_add(o as u64);
            }
            if let Some(uscript::vm::Value::Array(rc)) = vm.get_prop(c, "RouteCache") {
                for v in rc {
                    checksum = checksum.wrapping_mul(31).wrapping_add(v.as_obj().map_or(7, |o| o as u64));
                }
            }
        }
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let cpu_ms = (thread_cpu() - c0) * 1000.0;
    let reach = p2game::navfix::REACH_TESTS.with(|c| c.get()) - r0;
    println!("FindRandomDest: {n} searches, {found} found, {:.3} ms/search wall, {:.3} ms/search CPU, {:.1} reach tests/search, checksum {checksum}", ms / n as f64, cpu_ms / n as f64, reach as f64 / n as f64);
    println!("stats {:?}", vm.nav.stats);
    // Seeding cost split: the same pawn-to-node walk tests against the level only vs the full
    // physics world (level + static blockers).
    let mut pairs = Vec::new();
    if let Some(g) = vm.nav.graph.as_ref() {
        for &c in &ctrls {
            let Some(p) = vm.get_prop(c, "Pawn").and_then(|v| v.as_obj()) else { continue };
            let m = vm.mover(p, Some(c));
            for k in 0..g.node_count() as u32 {
                let l = g.node_location(k);
                let d = ((l[0] - m.location[0]).powi(2) + (l[1] - m.location[1]).powi(2) + (l[2] - m.location[2]).powi(2)).sqrt();
                if d < 1200.0 {
                    pairs.push((m, l));
                }
            }
        }
    }
    if let Some(ph) = p2game::physics::current() {
        let mut w = ph.world.borrow_mut();
        let t = Instant::now();
        let c0 = thread_cpu();
        let mut ok = 0;
        let mut wsum = 0u64;
        for (m, l) in &pairs {
            let r = p2game::navfix::walk_test(&mut *w, glam::Vec3::from(m.location), glam::Vec3::from(*l), m.radius, m.height, m.max_step);
            ok += matches!(r, p2game::navfix::Walk::Reached) as u32;
            let bits = match r {
                p2game::navfix::Walk::Reached => 1,
                p2game::navfix::Walk::Blocked(f) => 2 ^ (f.to_bits() as u64) << 2,
                p2game::navfix::Walk::Drop(f) => 3 ^ (f.to_bits() as u64) << 2,
            };
            wsum = wsum.wrapping_mul(1_000_003).wrapping_add(bits);
        }
        println!("walk checksum {wsum}, {:.1} us/test CPU", (thread_cpu() - c0) * 1e6 / pairs.len().max(1) as f64);
        let full = t.elapsed().as_secs_f64() * 1e6 / pairs.len().max(1) as f64;
        let mut lvl = LevelOnly(w.level.clone());
        let t = Instant::now();
        for (m, l) in &pairs {
            p2game::navfix::walk_test(&mut lvl, glam::Vec3::from(m.location), glam::Vec3::from(*l), m.radius, m.height, m.max_step);
        }
        let level = t.elapsed().as_secs_f64() * 1e6 / pairs.len().max(1) as f64;
        println!("walk tests: {} pairs ({ok} reached), full world {full:.1} us/test, level only {level:.1} us/test", pairs.len());
    }
}
