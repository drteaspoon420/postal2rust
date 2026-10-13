//! `p2game --tour [--tour-from N] [--tour-count N] [--out DIR]`: windowed visit of every map.
//! Each map is entered through the normal travel path (`open <map>`), the tour waits for the
//! first interactive frame (10 consecutive frames under 50 ms, or 20 s), lets it settle, saves
//! `<out>/tour/<map>.png`, logs one row to `<out>/tour/tour.tsv` (load seconds, fps, actors,
//! rigs and rig failures, missing natives and script errors added on that map), then travels on.

use crate::window::Sim;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use std::io::Write as _;
use std::path::PathBuf;

pub(crate) struct TourPlugin;

#[derive(Resource)]
struct Tour {
    maps: Vec<String>,
    next: usize,
    dir: PathBuf,
    /// Current map: when travel was requested, frames since the scene appeared.
    started: std::time::Instant,
    smooth: u32,
    interactive_at: Option<f32>,
    frames: u32,
    shot_frame: Option<u32>,
    fps_acc: (f32, u32),
    natives0: u64,
    errors0: u64,
    map: String,
    pending: Option<String>,
    serial: u64,
}

impl Plugin for TourPlugin {
    fn build(&self, app: &mut App) {
        let a: Vec<String> = std::env::args().collect();
        if !a.iter().any(|x| x == "--tour") {
            return;
        }
        let flag = |f: &str| a.iter().position(|x| x == f).and_then(|i| a.get(i + 1)).cloned();
        let out = PathBuf::from(flag("--out").unwrap_or_else(|| "out".into())).join("tour");
        std::fs::create_dir_all(&out).ok();
        let from: usize = flag("--tour-from").and_then(|s| s.parse().ok()).unwrap_or(0);
        let count: usize = flag("--tour-count").and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
        let maps = crate::map_list().into_iter().skip(from).take(count).collect::<Vec<_>>();
        let tsv = out.join("tour.tsv");
        if from == 0 || !tsv.exists() {
            std::fs::write(&tsv, "map\tload_s\tfps\tlive_actors\trigs\trigs_failed\tnew_missing_natives\tnew_script_errors\ttop_natives\n").ok();
        }
        println!("tour: {} maps", maps.len());
        app.insert_resource(Tour {
            maps,
            next: 0,
            dir: out,
            started: std::time::Instant::now(),
            smooth: 0,
            interactive_at: None,
            frames: 0,
            shot_frame: None,
            fps_acc: (0.0, 0),
            natives0: 0,
            errors0: 0,
            map: String::new(),
            pending: None,
            serial: 0,
        })
        .add_systems(Last, step);
    }
}

fn step(mut tour: ResMut<Tour>, mut sim: NonSendMut<Sim>, time: Res<Time>, mut commands: Commands, mut exit: MessageWriter<AppExit>) {
    let t = &mut *tour;
    let map = sim.game.level.map.name.clone();
    // Waiting for a requested travel: the old level is still current until `travel` runs.
    if let Some(want) = &t.pending {
        if t.started.elapsed().as_secs_f32() > 60.0 {
            println!("tour: {want} never arrived (still on {map})");
            let row = format!("{want}\t-1\t0\t0\t0\t0\t0\t0\ttravel failed\n");
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(t.dir.join("tour.tsv")) {
                f.write_all(row.as_bytes()).ok();
            }
            t.pending = None;
            t.map = map.clone();
            t.shot_frame = Some(0);
            t.frames = 10;
        } else if sim.game.level_serial == t.serial {
            return;
        } else {
            t.pending = None;
            t.map.clear();
        }
    }
    if t.map != map {
        // Arrived (or the boot map): reset per-map counters.
        t.map = map.clone();
        t.frames = 0;
        t.smooth = 0;
        t.interactive_at = None;
        t.shot_frame = None;
        t.fps_acc = (0.0, 0);
        let vm = &sim.game.world.vm;
        t.natives0 = vm.missing_natives.values().sum();
        t.errors0 = vm.script_errors.values().sum();
    }
    t.frames += 1;
    // Skip intro cutscenes as a player would, so the shot shows the playable start.
    if let Some(pc) = sim.game.player_controller {
        sim.game.world.vm.set_prop(pc, "bWantsToSkip", uscript::vm::Value::Byte(1));
    }
    let dt = time.delta_secs();
    if t.interactive_at.is_none() {
        t.smooth = if dt < 0.05 { t.smooth + 1 } else { 0 };
        if t.smooth >= 10 || t.started.elapsed().as_secs_f32() > 20.0 {
            t.interactive_at = Some(t.started.elapsed().as_secs_f32());
            t.shot_frame = Some(t.frames + 30);
        }
    } else {
        t.fps_acc.0 += dt;
        t.fps_acc.1 += 1;
    }
    if t.shot_frame == Some(t.frames) && t.next > 0 {
        let path = t.dir.join(format!("{map}.png"));
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    // Write the row and move on a few frames after the shot (or straight away for the boot map).
    let done = t.shot_frame.is_some_and(|s| t.frames >= s + 5) || t.next == 0;
    if !done {
        return;
    }
    if t.next > 0 {
        let vm = &sim.game.world.vm;
        let natives: u64 = vm.missing_natives.values().sum();
        let errors: u64 = vm.script_errors.values().sum();
        let mut top: Vec<(&String, &u64)> = vm.missing_natives.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        let fps = if t.fps_acc.0 > 0.0 { t.fps_acc.1 as f32 / t.fps_acc.0 } else { 0.0 };
        let row = format!(
            "{map}\t{:.2}\t{fps:.0}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            t.interactive_at.unwrap_or(0.0),
            sim.game.live_actors(),
            sim.rig_count(),
            sim.rigs_failed(),
            natives.saturating_sub(t.natives0),
            errors.saturating_sub(t.errors0),
            top.iter().take(4).map(|(k, n)| format!("{k} x{n}")).collect::<Vec<_>>().join("; ")
        );
        print!("tour: {row}");
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(t.dir.join("tour.tsv")) {
            f.write_all(row.as_bytes()).ok();
        }
    }
    if t.next >= t.maps.len() {
        println!("tour: done");
        exit.write(AppExit::Success);
        return;
    }
    let next = t.maps[t.next].clone();
    t.next += 1;
    t.started = std::time::Instant::now();
    println!("tour: [{}/{}] {next}", t.next, t.maps.len());
    t.pending = Some(next.clone());
    t.serial = sim.game.level_serial;
    sim.request_travel(&format!("{next}?Game=GameTypes.GameSinglePlayer"));
}
