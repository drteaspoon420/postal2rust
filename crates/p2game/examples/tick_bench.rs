//! Benchmark: boot a map with every placed actor and time `tick_world`.
//!
//! `cargo run --release -p p2game --no-default-features --example tick_bench -- [map] [frames] [--profile] [--sizes]`
//! (default suburbs-3, 300 frames). Reports load and begin-play time, per-frame times (mean, p50,
//! p95, max; the first frames include one-off work such as class loading), and VM throughput.
//! Times are thread CPU time, so other processes on the machine do not inflate them; wall time is
//! printed too. A calibration loop (a native call on an empty VM) shows the machine's state.

use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::game_root;
use p2game::level::World;
use std::sync::Arc;
use std::time::Instant;

/// The Windows system heap slows down sharply after the interpreter's many short-lived mixed-size
/// allocations (10x and more on the calibration loop); mimalloc does not.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// CPU time used by this thread, in seconds.
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
    Instant::now().elapsed().as_secs_f64()
}

/// Nanoseconds per native call on an empty VM (CPU time).
fn calibrate(system: &std::path::Path) -> f64 {
    let mut cv = uscript::vm::Vm::new(system).expect("vm");
    let object = cv.find_class("Core", "Object").unwrap();
    let t = thread_cpu();
    for i in 0..200_000 {
        cv.call(object, None, "Less_IntInt", vec![uscript::vm::Value::Int(i), uscript::vm::Value::Int(5)]).unwrap();
    }
    (thread_cpu() - t) * 1e9 / 200_000.0
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|s| !s.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let frames: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    let root = game_root(None);
    let calib0 = calibrate(&root.join("System"));
    let cfg = BootConfig::load(&root.join("System")).expect("ini");
    let c0 = thread_cpu();
    let mut world = World::new(cfg).expect("vm");
    let path = world.files.path(&map).cloned().expect("map not found");
    let pkg = Arc::new(upkg::Package::open(&path).expect("open map"));
    let level = world.load_level(pkg);
    let actors = level.actors.len();
    let t_load = thread_cpu() - c0;
    let mut game = Game::new(world, level, None);
    let c1 = thread_cpu();
    game.begin_play("");
    game.drain_log(false);
    let t_begin = thread_cpu() - c1;
    let vm = &mut game.world.vm;
    if a.iter().any(|x| x == "--sizes") {
        let cells: usize = vm.objects.iter().map(|o| o.slots.len()).sum();
        println!("vm: {} objects ({cells} slot cells), {} classes", vm.objects.len(), vm.classes.len());
    }
    vm.profile = a.iter().any(|x| x == "--profile");
    let (s0, k0, n0) = (vm.steps_total, vm.calls_total, vm.native_calls_total);
    let mut ms = Vec::with_capacity(frames);
    let wall = Instant::now();
    let cpu = thread_cpu();
    for _ in 0..frames {
        // Thread CPU time has ~15.6 ms resolution on Windows, so single frames use wall time;
        // the CPU total below is the load-independent figure.
        let f = Instant::now();
        game.tick(1.0 / 30.0);
        ms.push(f.elapsed().as_secs_f64() * 1000.0);
        game.world.vm.log.clear();
    }
    let total = thread_cpu() - cpu;
    let wall = wall.elapsed().as_secs_f64();
    let calib = calibrate(&game.world.vm.system_dir);
    let vm = &game.world.vm;
    let (steps, calls, natives) = (vm.steps_total - s0, vm.calls_total - k0, vm.native_calls_total - n0);
    let warm: Vec<f64> = ms.iter().skip(frames.min(10)).copied().collect();
    let mut sorted = if warm.is_empty() { ms.clone() } else { warm.clone() };
    sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let pct = |p: f64| sorted[((sorted.len() as f64 - 1.0) * p) as usize];
    println!("map {map}: {actors} actors, load {t_load:.2}s CPU, begin-play {t_begin:.2}s CPU, {} live actors", game.live_actors());
    println!(
        "tick: {frames} frames, {total:.2}s CPU = {:.2} ms/frame ({wall:.2}s wall); wall per frame: first {:.2} ms; frames 10+: mean {:.3} ms  p50 {:.3}  p95 {:.3}  max {:.3}",
        total * 1000.0 / frames as f64,
        ms[0],
        sorted.iter().sum::<f64>() / sorted.len() as f64,
        pct(0.5),
        pct(0.95),
        sorted[sorted.len() - 1]
    );
    println!(
        "per frame: {:.0} statements, {:.0} calls ({:.0} native); throughput {:.2} M statements/s, {:.2} M calls/s",
        steps as f64 / frames as f64,
        calls as f64 / frames as f64,
        natives as f64 / frames as f64,
        steps as f64 / total / 1e6,
        calls as f64 / total / 1e6
    );
    println!("calibration: {calib0:.1} ns per native call on an empty VM before, {calib:.1} after");
    let mut errs: Vec<_> = vm.script_errors.iter().collect();
    errs.sort_by(|a, b| b.1.cmp(a.1));
    println!("script errors: {} distinct", errs.len());
    for (e, n) in errs.iter().take(5) {
        println!("  {n:>7}  {e}");
    }
    if vm.profile {
        for (title, m) in [("events", &vm.profile_events), ("natives", &vm.profile_natives)] {
            let mut v: Vec<_> = m.iter().collect();
            v.sort_by(|a, b| b.1.1.partial_cmp(&a.1.1).unwrap());
            println!("top {title} by inclusive wall time:");
            for (k, (n, s)) in v.iter().take(15) {
                println!("  {:>9.1} ms {n:>8} calls  {k}", s * 1000.0);
            }
        }
    }
}
