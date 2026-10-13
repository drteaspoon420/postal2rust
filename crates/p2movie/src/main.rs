//! p2movie: inspect, dump and play POSTAL 2 RoQ movies.
//!
//! ```text
//! p2movie --info intro.RoQ
//! p2movie --dump intro.RoQ --frames 0,60,300 --out out\frames     # PNG per listed frame
//! p2movie intro.RoQ [--sound Movies.Movie_EndOfDay1] [--loop] [--screenshot out\m.png --at 5]
//! ```
//! File names resolve against `<game>\Movies` case-insensitively.

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2movie::bevy_plugin::{MoviePlugin, Movies};
use p2movie::{Decoder, Event, find_movie};
use p2sound::SoundCache;
use p2sound::bevy_plugin::SoundPlugin;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DEFAULT_GAME: &str = r"G:\SteamLibrary\steamapps\common\POSTAL2Complete";

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let has = |k: &str| a.iter().any(|x| x == k);
    let game = get("--game").map(PathBuf::from).or_else(|| std::env::var_os("POSTAL2_DIR").map(PathBuf::from)).unwrap_or_else(|| DEFAULT_GAME.into());
    if let Some(name) = get("--info") {
        return info(&game, &name);
    }
    if let Some(name) = get("--dump") {
        let frames: Vec<u32> = get("--frames").unwrap_or_else(|| "0".into()).split(',').filter_map(|s| s.trim().parse().ok()).collect();
        let out = PathBuf::from(get("--out").unwrap_or_else(|| "out".into()));
        return dump(&game, &name, &frames, &out);
    }
    if has("--list") {
        for e in std::fs::read_dir(game.join("Movies")).into_iter().flatten().flatten() {
            info(&game, &e.path().to_string_lossy());
        }
        return;
    }
    let Some(name) = a.iter().find(|x| !x.starts_with("--") && Some(*x) != get("--sound").as_ref() && Some(*x) != get("--screenshot").as_ref() && Some(*x) != get("--at").as_ref() && Some(*x) != get("--game").as_ref()).cloned() else {
        eprintln!("usage: p2movie <movie> [--sound Pkg.Sound] [--loop] [--screenshot file --at secs] | --info <movie> | --dump <movie> --frames a,b --out dir | --list");
        std::process::exit(2);
    };
    play(game, name, get("--sound"), has("--loop"), get("--screenshot").map(PathBuf::from), get("--at").and_then(|s| s.parse().ok()).unwrap_or(3.0));
}

fn open(game: &Path, name: &str) -> Decoder {
    let path = find_movie(game, name).unwrap_or_else(|| panic!("movie '{name}' not found"));
    Decoder::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn info(game: &Path, name: &str) {
    let path = find_movie(game, name).unwrap_or_else(|| PathBuf::from(name));
    let data = std::fs::read(&path).unwrap_or_default();
    let Ok(mut d) = Decoder::new(data.clone()) else {
        println!("{}: not a RoQ file", path.display());
        return;
    };
    let chunks = Decoder::chunks(&data);
    let count = |id| chunks.iter().filter(|c| c.id == id).count();
    let t0 = std::time::Instant::now();
    let (mut frames, mut audio_samples, mut err) = (0u32, 0usize, None);
    loop {
        match d.next_event() {
            Ok(Some(Event::Frame)) => frames += 1,
            Ok(Some(Event::Audio(a))) => audio_samples += a.samples.len() / a.channels as usize,
            Ok(None) => break,
            Err(e) => {
                err = Some(e.to_string());
                break;
            }
        }
    }
    println!(
        "{}: {}x{} @ {} fps, frames={} ({:.1} s), codebooks={} audio_chunks={} ({:.1} s) other_chunks={} decoded in {} ms{}",
        path.file_name().unwrap().to_string_lossy(),
        d.width,
        d.height,
        d.fps,
        frames,
        frames as f32 / d.fps as f32,
        count(p2movie::roq::QUAD_CODEBOOK),
        count(p2movie::roq::AUDIO_MONO) + count(p2movie::roq::AUDIO_STEREO),
        audio_samples as f32 / p2movie::roq::AUDIO_RATE as f32,
        d.skipped,
        t0.elapsed().as_millis(),
        err.map(|e| format!(" ERROR {e}")).unwrap_or_default()
    );
}

fn dump(game: &Path, name: &str, frames: &[u32], out: &Path) {
    std::fs::create_dir_all(out).unwrap();
    let mut d = open(game, name);
    let stem = Path::new(name).file_stem().unwrap().to_string_lossy().to_string();
    let last = frames.iter().copied().max().unwrap_or(0);
    let mut i = 0;
    while d.next_frame().unwrap() {
        if frames.contains(&i) {
            let p = out.join(format!("{stem}_{i:05}.png"));
            write_png(&p, d.width, d.height, &d.rgba());
            println!("FRAME {} -> {}", i, p.display());
        }
        if i >= last {
            break;
        }
        i += 1;
    }
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(rgba).unwrap();
}

#[derive(Resource)]
struct Cli {
    name: String,
    sound: Option<String>,
    looping: bool,
    screenshot: Option<PathBuf>,
    at: f32,
    started: bool,
    shot: bool,
}

fn play(game: PathBuf, name: String, sound: Option<String>, looping: bool, screenshot: Option<PathBuf>, at: f32) {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: format!("p2movie - {name}"), resolution: (1280u32, 720u32).into(), ..default() }),
            ..default()
        }))
        .add_plugins(SoundPlugin { uu_per_unit: 52.5, output: screenshot.is_none() })
        .add_plugins(MoviePlugin { game })
        .insert_resource(ClearColor(Color::BLACK))
        .insert_resource(Cli { name, sound, looping, screenshot, at, started: false, shot: false })
        .add_systems(Startup, |mut c: Commands| {
            c.spawn(Camera2d);
        })
        .add_systems(Update, drive)
        .run();
}

fn drive(mut commands: Commands, mut cli: ResMut<Cli>, mut movies: ResMut<Movies>, time: Res<Time>, keys: Res<ButtonInput<KeyCode>>, mut exit: MessageWriter<AppExit>) {
    if !cli.started {
        cli.started = true;
        let track = cli.sound.as_ref().and_then(|s| load_sound(&movies.game, s));
        if cli.sound.is_some() && track.is_none() {
            eprintln!("sound {:?} not found", cli.sound);
        }
        if let Err(e) = movies.play_scaled(&cli.name.clone(), cli.looping, track) {
            eprintln!("{e}");
            exit.write(AppExit::error());
            return;
        }
        println!("PLAY {} {}x{} @ {} fps", movies.player.filename, movies.player.width(), movies.player.height(), movies.player.fps());
        return;
    }
    if keys.just_pressed(KeyCode::Space) {
        let p = !movies.player.is_paused();
        movies.pause(p);
    }
    if keys.just_pressed(KeyCode::Escape) {
        movies.stop();
    }
    if let Some(path) = cli.screenshot.clone() {
        if !cli.shot && time.elapsed_secs() >= cli.at {
            cli.shot = true;
            println!("SHOT t={:.2} movie_time={:.2} uploads={}", time.elapsed_secs(), movies.player.time(), movies.uploads);
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        }
        if cli.shot && cli.screenshot.as_ref().is_some_and(|p| p.exists()) {
            exit.write(AppExit::Success);
        }
        return;
    }
    if !movies.is_playing() {
        println!("END after {:.1} s", time.elapsed_secs());
        exit.write(AppExit::Success);
    }
}

fn load_sound(game: &Path, full: &str) -> Option<Arc<p2sound::SoundData>> {
    let lib = upkg::library::Library::new(game).ok()?;
    let (pkg, name) = full.split_once('.')?;
    let p = lib.package(pkg)?;
    let e = (0..p.exports.len()).find(|&i| p.name(p.exports[i].name).eq_ignore_ascii_case(name))?;
    let obj = upkg::library::Obj { pkg: p, export: e };
    SoundCache::new().resolve(&lib, &obj).map(|r| r.data)
}
