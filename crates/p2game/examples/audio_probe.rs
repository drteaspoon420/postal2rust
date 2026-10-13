//! Audio probe: boots a map as single player (like the windowed game: TravelPostAccept, day N),
//! runs it headless at 30 fps and mixes the audio engine in step with the game clock.
//!
//! Prints every `--every S` seconds the audible voices (name, kind, distance, gain), at the end a
//! histogram of voices started per sound and the loudest ones, and writes the mix to
//! `--wav PATH` (16-bit stereo) so it can be listened to / measured.
//!
//! `audio_probe [Map] [--day N] [--secs S] [--every S] [--wav out.wav] [--exec "Cmd;Cmd"] [--listen X,Y,Z]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::collections::BTreeMap;
use std::sync::Arc;

fn main() {
    std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
}

fn flag(a: &[String], k: &str) -> Option<String> {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned()
}

fn run() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let day: i32 = flag(&a, "--day").and_then(|s| s.parse().ok()).unwrap_or(1);
    let secs: f32 = flag(&a, "--secs").and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let every: f32 = flag(&a, "--every").and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let wav = flag(&a, "--wav");
    // `--listen X,Y,Z`: hold the listener there (check a sound source from a known distance).
    let listen = flag(&a, "--listen").and_then(|s| {
        let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        (v.len() == 3).then(|| glam::Vec3::new(v[0], v[1], v[2]))
    });
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::viewport::install();
    p2game::clock::install();
    p2game::ai::install();
    let audio = p2game::audio::install(root.clone(), None);
    p2game::cinema::install(root.clone());
    p2game::subtitles::install();
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let game_class = flag(&a, "--game").unwrap_or_else(|| "GameTypes.GameSinglePlayer".into());
    let mut game = Game::new(world, lv, Some(&game_class));
    game.begin_play(&format!("?SetDay={day}"));
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    let dt = 1.0 / 30.0;
    let rate = audio.with(|e| e.rate);
    let chunk = (rate as f32 * dt) as usize * 2;
    let mut mix: Vec<f32> = Vec::new();
    let mut started: BTreeMap<String, (u32, f32)> = BTreeMap::new();
    let mut seen_ids = std::collections::HashSet::new();
    let frames = (secs / dt) as usize;
    let mut next_dump = every;
    let (mut render_ms, mut render_max) = (0.0f64, 0.0f64);
    let execs: Vec<String> = flag(&a, "--exec").map(|s| s.split(';').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    for f in 0..frames {
        if let Some(p) = listen {
            p2game::audio::set_listener_override(Some((p, [0, 0, 0])));
        }
        if f == 15 {
            for c in &execs {
                let r = game.world.vm.console_get_set(c);
                println!("EXEC {c} -> {r:?}");
            }
            // `--break NAME`: 500 bullet damage to that actor (breaks a Window: glass sound check).
            if let Some(name) = flag(&a, "--break") {
                let vm = &mut game.world.vm;
                let x = vm.actors.iter().copied().find(|&o| !vm.objects[o].deleted && vm.object_name(o).eq_ignore_ascii_case(&name));
                if let Some(x) = x {
                    let l = vm.actor_loc(x);
                    let cls = vm.find_class_by_name("BulletDamage").ok();
                    let r = vm.call_event(x, "TakeDamage", vec![uscript::vm::Value::Int(500), uscript::vm::Value::Object(None), uscript::vm::Value::Vector(l), uscript::vm::Value::Vector([0.0, 1000.0, 0.0]), uscript::vm::Value::Class(cls)]);
                    println!("BREAK {name} -> {:?} deleted {}", r.err(), vm.objects[x].deleted);
                }
            }
        }
        game.tick(dt);
        game.drain_log(false);
        let mut buf = vec![0.0f32; chunk];
        let tr = std::time::Instant::now();
        audio.with(|e| e.render(&mut buf));
        let rt = tr.elapsed().as_secs_f64() * 1000.0;
        render_ms += rt;
        render_max = render_max.max(rt);
        mix.extend_from_slice(&buf);
        let t = (f + 1) as f32 * dt;
        let infos = audio.with(|e| e.voice_infos());
        for v in &infos {
            if seen_ids.insert(v.id) {
                let e = started.entry(format!("{}{}", if v.ambient { "[amb] " } else if v.music { "[music] " } else { "" }, v.name)).or_default();
                e.0 += 1;
            }
            let key = format!("{}{}", if v.ambient { "[amb] " } else if v.music { "[music] " } else { "" }, v.name);
            if let Some(e) = started.get_mut(&key) {
                e.1 = e.1.max(v.gain);
            }
        }
        if t >= next_dump {
            if next_dump == every {
                let vm = &game.world.vm;
                let pawn = game.player_controller.and_then(|pc| vm.get_prop(pc, "Pawn")).and_then(|v| v.as_obj());
                println!("player pawn {:?} at {:?}", pawn.map(|p| vm.object_name(p)), pawn.map(|p| vm.actor_loc(p)));
                let lp = audio.with(|e| e.listener.position);
                let mut amb: Vec<(f32, String)> = audio.with(|e| {
                    e.ambients()
                        .map(|(id, d)| {
                            let dist = d.location.distance(lp);
                            (dist, format!("dist {dist:>6.0} radius {:>6.0} vol {:>3} pitch {:>3} {} ({})", d.radius, d.volume, d.pitch, d.sound.name, vm.object_name(*id as usize)))
                        })
                        .collect()
                });
                amb.sort_by(|a, b| a.0.total_cmp(&b.0));
                for (_, s) in amb.iter().take(400) {
                    println!("  AMB {s}");
                }
            }
            next_dump += every;
            let l = audio.with(|e| e.listener);
            let rms = rms_db(&buf);
            println!("--- t={t:.1}s listener {:.0},{:.0},{:.0} voices {} ambients {} rms {rms:.1} dBFS", l.position.x, l.position.y, l.position.z, infos.len(), audio.with(|e| e.ambient_count()));
            let mut v: Vec<_> = infos.iter().filter(|v| v.gain > 1e-3).collect();
            v.sort_by(|a, b| b.gain.total_cmp(&a.gain));
            for v in v.iter().take(20) {
                println!(
                    "  {:<6} gain {:.3} vol {:.2} rad {:>6.0} dist {:>6} {}",
                    if v.music { "music" } else if v.ambient { "amb" } else { "shot" },
                    v.gain,
                    v.volume,
                    v.radius,
                    v.distance.map(|d| format!("{d:.0}")).unwrap_or_else(|| "flat".into()),
                    v.name
                );
            }
        }
    }
    println!("=== voices started (count, max gain) over {secs} s");
    let mut s: Vec<_> = started.into_iter().collect();
    s.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (k, (n, g)) in s {
        println!("  {n:>5}  max gain {g:.3}  {k}");
    }
    println!("mixer: {:.2} ms per 33 ms block avg, {:.2} ms max", render_ms / frames.max(1) as f64, render_max);
    let st = audio.stats.borrow().clone();
    println!("stats {st:?} rms total {:.1} dBFS peak {:.3}", rms_db(&mix), mix.iter().fold(0.0f32, |m, s| m.max(s.abs())));
    if let Some(p) = wav {
        std::fs::write(&p, p2sound::engine::stereo_to_wav(&mix, rate)).unwrap();
        println!("wrote {p}");
    }
}

fn rms_db(b: &[f32]) -> f32 {
    if b.is_empty() {
        return -120.0;
    }
    let r = (b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32).sqrt();
    20.0 * r.max(1e-6).log10()
}
