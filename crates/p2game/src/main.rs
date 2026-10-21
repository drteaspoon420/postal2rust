//! p2game: boots POSTAL 2 content from the user's install on the uscript VM.
//!
//! ```text
//! p2game --config                                   print the boot configuration from the inis
//! p2game --scan [--out DIR]                         instantiate every map, write level_scan.tsv
//! p2game --map X --headless [--frames N]            boot one level headless, print a native/error histogram
//! p2game --boot-all [--frames N] [--out DIR]        headless boot of every map, aggregate natives_needed.tsv
//! p2game --smoke [--intro-secs S] [--out DIR]       headless end-to-end: menu, New Game, intro, day map, walk, fire
//! p2game --smoke --windowed [--out DIR]            the same flow in the window, with a screenshot per stage
//! p2game [--map X]                                  windowed: render the level with VM-driven actor transforms
//! ```
//! Common options: `--game DIR` (install root), `--game-class Pkg.Class`, `--options "?k=v"`.

use p2game::boot::{Game, Tally};
use p2game::config::BootConfig;
use p2game::game_root;
use p2game::level::{Level, World};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use upkg::Package;

/// The script VM allocates heavily (values, frames, strings); mimalloc keeps that fast where the
/// Windows system heap degrades over a session.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod launch;
mod sampler;
#[cfg(feature = "window")]
mod cinema_view;
#[cfg(feature = "window")]
mod cursor;
#[cfg(feature = "window")]
mod debug;
#[cfg(feature = "window")]
mod devmenu;
#[cfg(feature = "window")]
mod frontend;
#[cfg(feature = "window")]
mod progress;
#[cfg(feature = "window")]
mod tour;
#[cfg(feature = "window")]
mod realinput;
#[cfg(feature = "window")]
mod pilot;
#[cfg(feature = "window")]
mod rigs;
#[cfg(feature = "window")]
mod dynmesh;
#[cfg(feature = "window")]
mod smoke;
#[cfg(feature = "window")]
mod ui;
#[cfg(feature = "window")]
mod settings;
#[cfg(feature = "window")]
mod splash;
#[cfg(feature = "window")]
mod weapons;
#[cfg(feature = "window")]
mod magicka;
#[cfg(feature = "window")]
mod window;
#[cfg(feature = "window")]
mod window_memory;
#[cfg(feature = "window")]
mod zone_fog;

pub struct Args {
    pub root: PathBuf,
    pub map: Option<String>,
    pub frames: u32,
    pub out: PathBuf,
    pub game_class: Option<String>,
    pub options: String,
    pub verbose: bool,
}

fn flag(a: &[String], f: &str) -> Option<String> {
    a.iter().position(|x| x.eq_ignore_ascii_case(f)).and_then(|i| a.get(i + 1)).cloned()
}

fn has(a: &[String], f: &str) -> bool {
    a.iter().any(|x| x.eq_ignore_ascii_case(f))
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    launch::prepare(&a);
    // The shipped exe asks for the install folder when it cannot find one (or on --change-folder).
    let root = if launch::shipped() || has(&a, "--change-folder") {
        match launch::resolve_root(&a, flag(&a, "--game").as_deref()) {
            Some(r) => r,
            None => {
                println!("No POSTAL 2 folder selected.");
                return;
            }
        }
    } else {
        game_root(flag(&a, "--game").as_deref())
    };
    let args = Args {
        root,
        map: flag(&a, "--map"),
        frames: flag(&a, "--frames").and_then(|s| s.parse().ok()).unwrap_or(60),
        out: PathBuf::from(flag(&a, "--out").unwrap_or_else(|| "out".into())),
        game_class: flag(&a, "--game-class"),
        options: flag(&a, "--options").unwrap_or_default(),
        verbose: has(&a, "-v") || has(&a, "--verbose"),
    };
    let system = upkg::ci::join(&args.root, "System");
    if !upkg::ci::join(&system, "Postal2.ini").is_file() && !upkg::ci::join(&system, "Default.ini").is_file() {
        let example = if cfg!(windows) { r#"p2game.exe --game "C:\path\to\POSTAL2Complete""# } else { r#"./Postal2Rust --game "$HOME/.local/share/Steam/steamapps/common/POSTAL2Complete""# };
        let msg = format!(
            "POSTAL 2 was not found.\n\nThis engine needs your own installed copy of POSTAL 2 (e.g. Steam \"POSTAL 2\" / POSTAL2Complete).\n\nLooked in: {}\n\nFix: put the install folder path on one line in postal2dir.txt next to the game executable, or run\n  {example}",
            args.root.display()
        );
        eprintln!("{msg}");
        if launch::shipped() {
            launch::message(&msg, false);
        } else {
            #[cfg(windows)]
            {
                let script = format!("Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show(@'\n{msg}\n'@, 'Postal 2 Rust/Bevy') | Out-Null");
                let _ = std::process::Command::new("powershell").args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script]).status();
            }
        }
        std::process::exit(2);
    }
    p2game::host::register_level_hook(p2game::physics::level_hook);
    // Near a level exit, build the destination's collision world ahead (P2GAME_NO_PRELOAD=1: off).
    p2game::preload::install();
    // Sound (silent engine unless the window attaches a device) and movies/Matinee for every level.
    if std::env::var_os("P2GAME_NO_AUDIO").is_none() {
        // The windowed game plays through the default output device (`--mute`: silent).
        // Scripted runs (tests, screenshots, perf captures: any P2GAME_SHOTS/EXIT/EXEC/REAL/
        // EXIT_SECS, `--profile-secs`, `--screenshot`) and `P2GAME_MUTE=1` are always muted too; the
        // audio engine still runs, so sound traces keep working. Only interactive play is audible.
        #[cfg(feature = "window")]
        let scripted = ["P2GAME_MUTE", "P2GAME_SHOTS", "P2GAME_EXIT", "P2GAME_EXIT_SECS", "P2GAME_EXEC", "P2GAME_REAL", "P2GAME_DEMO_GOTO"]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
            || has(&a, "--profile-secs")
            || has(&a, "--screenshot");
        #[cfg(feature = "window")]
        let windowed = !scripted && !["--config", "--scan", "--boot-all", "--headless", "--smoke", "--progress", "--mute"].iter().any(|f| has(&a, f));
        #[cfg(not(feature = "window"))]
        let windowed = false;
        if windowed {
            #[cfg(feature = "window")]
            p2game::audio::install_with_device(args.root.clone());
        } else {
            p2game::audio::install(args.root.clone(), None);
        }
    }
    p2game::cinema::install(args.root.clone());
    // Karma ragdolls (KarmaData\*.ka) and SaveGame/LoadGame console commands (user Save dir).
    p2game::ragdoll::install(args.root.clone());
    // NPC perception and stasis (sight, hearing, LastRenderTime) for the script AI.
    p2game::ai::install();
    p2game::demo::install();
    p2game::save::install(None);
    // Resolution console queries (GetCurrentRes, ViewportManager ini) so the player FOV is not NaN.
    p2game::viewport::install();
    // Subtitles: sounds listed in Subtitles.ini call the player's CallSubtitle (P2HUD draws them).
    p2game::subtitles::install();
    // LevelInfo wall clock (Year/Month/Day/Hour/Minute...): save slot timestamps read it.
    p2game::clock::install();
    #[cfg(feature = "window")]
    p2game::fx_glue::install(&args.root);
    let cfg = BootConfig::load(&system).unwrap_or_else(|e| {
        eprintln!("cannot read ini files in {}: {e}", system.display());
        std::process::exit(1)
    });
    if has(&a, "--config") {
        cfg.print();
        return;
    }
    if has(&a, "--scan") {
        scan(cfg, &args);
        return;
    }
    if has(&a, "--boot-all") {
        boot_all(cfg, &args);
        return;
    }
    if has(&a, "--headless") {
        headless(cfg, &args);
        return;
    }
    #[cfg(feature = "window")]
    if has(&a, "--progress") {
        let max = flag(&a, "--max-frames").and_then(|s| s.parse().ok()).unwrap_or(30 * 60 * 30);
        let last_day = flag(&a, "--last-day").and_then(|s| s.parse().ok());
        let mut world = new_world(cfg);
        let url = args.map.clone().unwrap_or_else(|| "suburbs-3".into());
        let (name, opts) = p2game::boot::split_url(&url);
        let Some(map) = open_map(&world, &name) else {
            eprintln!("map {name} not found");
            std::process::exit(1)
        };
        let lv = world.load_level(map);
        let gc = args.game_class.clone().or_else(|| p2game::boot::url_option(&opts, "Game")).unwrap_or_else(|| "GameTypes.GameSinglePlayer".into());
        let mut game = Game::new(world, lv, Some(&gc));
        game.begin_play(&format!("{opts}{}", args.options));
        game.drain_log(false);
        std::process::exit(progress::run(game, &args.root, &args.out, max, last_day));
    }
    #[cfg(feature = "window")]
    if has(&a, "--smoke") && !has(&a, "--windowed") {
        let intro = flag(&a, "--intro-secs").and_then(|s| s.parse().ok()).unwrap_or(120.0);
        let max = flag(&a, "--max-frames").and_then(|s| s.parse().ok()).unwrap_or(30 * 300);
        let mut world = new_world(cfg);
        let url = args.map.clone().unwrap_or_else(|| world.cfg.startup_map.clone());
        let (name, opts) = p2game::boot::split_url(&url);
        let Some(map) = open_map(&world, &name) else {
            eprintln!("map {name} not found");
            std::process::exit(1)
        };
        let lv = world.load_level(map);
        let gc = args.game_class.clone().or_else(|| p2game::boot::url_option(&opts, "Game"));
        let mut game = Game::new(world, lv, gc.as_deref());
        game.begin_play(&format!("{opts}{}", args.options));
        game.drain_log(false);
        std::process::exit(smoke::run(game, &args.root, &args.out, intro, max));
    }
    #[cfg(feature = "window")]
    {
        window::run(cfg, args);
    }
    #[cfg(not(feature = "window"))]
    {
        eprintln!("built without the `window` feature; use --headless, --scan, --boot-all or --config");
        std::process::exit(2);
    }
}

pub fn open_map(world: &World, name: &str) -> Option<Arc<Package>> {
    let p = Path::new(name);
    let path = if p.is_file() { Some(p.to_path_buf()) } else { world.files.path(p.file_stem()?.to_str()?).cloned() };
    let path = path?;
    Package::open(&path).ok().map(Arc::new)
}

/// Names of every map in the install (for `--tour`), sorted.
#[cfg(feature = "window")]
pub(crate) fn map_list() -> Vec<String> {
    let root = p2game::game_root(None);
    let Ok(cfg) = BootConfig::load(&upkg::ci::join(root, "System")) else { return Vec::new() };
    let ext = cfg.map_ext.to_ascii_lowercase();
    let files = p2game::pkgindex::PackageFiles::new(&cfg.system_dir, &cfg.paths);
    let mut v: Vec<String> = files
        .files
        .values()
        .filter(|p| p.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(&ext)))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    v.sort_by_key(|s| s.to_ascii_lowercase());
    v.dedup_by_key(|s| s.to_ascii_lowercase());
    v
}

/// All map files from the ini search paths, sorted by name.
fn all_maps(world: &World) -> Vec<(String, PathBuf)> {
    let ext = world.cfg.map_ext.to_ascii_lowercase();
    let mut v: Vec<(String, PathBuf)> = world
        .files
        .files
        .iter()
        .filter(|(_, p)| p.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(&ext)))
        .map(|(k, p)| (p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| k.clone()), p.clone()))
        .collect();
    v.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
    v
}

fn new_world(cfg: BootConfig) -> World {
    World::new(cfg).unwrap_or_else(|e| {
        eprintln!("VM init failed: {e}");
        std::process::exit(1)
    })
}

fn scan(cfg: BootConfig, args: &Args) {
    let t0 = Instant::now();
    let mut world = new_world(cfg);
    let maps = all_maps(&world);
    std::fs::create_dir_all(&args.out).ok();
    let mut tsv = String::from("map\texports\tactors\tsubobjects\tclasses\tprops\tprops_unknown\tprops_undecoded\trefs_internal\trefs_external\trefs_unresolved\tclasses_unresolved\tlevelinfo\n");
    let mut total = p2game::level::LevelStats::default();
    let mut unresolved: BTreeMap<String, usize> = BTreeMap::new();
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let (mut ok, mut failed) = (0, 0);
    for (name, path) in &maps {
        let Ok(pkg) = Package::open(path) else {
            println!("{name:<28} FAILED to open");
            failed += 1;
            continue;
        };
        let objects_before = world.vm.objects.len();
        let lv = world.load_level(Arc::new(pkg));
        let s = &lv.stats;
        println!(
            "{name:<28} actors {:>5}  sub {:>4}  classes {:>3}  props {:>6} (unknown {:>3}, undecoded {:>3})  refs int {:>6} ext {:>5} unresolved {:>3}  classes unresolved {:>2}{}",
            s.actors,
            s.subobjects,
            s.distinct_classes,
            s.props_applied,
            s.props_unknown,
            s.props_undecoded,
            s.refs_internal,
            s.refs_external,
            s.refs_unresolved,
            s.classes_unresolved,
            if lv.level_info.is_none() { "  NO LEVELINFO" } else { "" }
        );
        tsv += &format!(
            "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            s.exports,
            s.actors,
            s.subobjects,
            s.distinct_classes,
            s.props_applied,
            s.props_unknown,
            s.props_undecoded,
            s.refs_internal,
            s.refs_external,
            s.refs_unresolved,
            s.classes_unresolved,
            lv.level_info.is_some()
        );
        total.actors += s.actors;
        total.subobjects += s.subobjects;
        total.props_applied += s.props_applied;
        total.props_unknown += s.props_unknown;
        total.props_undecoded += s.props_undecoded;
        total.refs_internal += s.refs_internal;
        total.refs_external += s.refs_external;
        total.refs_unresolved += s.refs_unresolved;
        total.classes_unresolved += s.classes_unresolved;
        total.prop_parse_failures += s.prop_parse_failures;
        for (k, v) in &lv.unresolved {
            *unresolved.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &lv.class_counts {
            *classes.entry(k.clone()).or_default() += v;
        }
        if lv.level_info.is_some() {
            ok += 1;
        } else {
            failed += 1;
        }
        // Each map is independent; drop its objects so memory stays flat across the scan.
        world.vm.truncate_objects(objects_before);
    }
    std::fs::write(args.out.join("level_scan.tsv"), tsv).ok();
    let mut ct = String::from("class\tinstances\n");
    let mut cv: Vec<_> = classes.iter().collect();
    cv.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in &cv {
        ct += &format!("{k}\t{v}\n");
    }
    std::fs::write(args.out.join("level_classes.tsv"), ct).ok();
    println!(
        "\n{} maps: {ok} instantiated with a LevelInfo, {failed} failed. {} actors, {} subobjects, {} distinct classes",
        maps.len(),
        total.actors,
        total.subobjects,
        classes.len()
    );
    println!(
        "properties: {} applied, {} with no slot, {} undecoded, {} unparsable objects",
        total.props_applied, total.props_unknown, total.props_undecoded, total.prop_parse_failures
    );
    println!(
        "references: {} internal, {} external resolved, {} unresolved; {} unresolved classes",
        total.refs_internal, total.refs_external, total.refs_unresolved, total.classes_unresolved
    );
    let mut uv: Vec<_> = unresolved.iter().collect();
    uv.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in uv.iter().take(25) {
        println!("  unresolved x{v:<5} {k}");
    }
    println!("wrote {} and level_classes.tsv in {:.1}s", args.out.join("level_scan.tsv").display(), t0.elapsed().as_secs_f32());
}

/// Resets the VM's per-world state so the next map starts clean (classes stay loaded).
fn reset_world(world: &mut World, base_objects: usize) {
    let vm = &mut world.vm;
    vm.truncate_objects(base_objects);
    vm.actors.clear();
    vm.level = None;
    vm.log.clear();
    vm.missing_natives.clear();
    vm.script_errors.clear();
}

fn boot_one(world: World, lv: Level, args: &Args, echo: bool) -> (Game, f32) {
    let t0 = Instant::now();
    let mut game = Game::new(world, lv, args.game_class.as_deref());
    game.begin_play(&args.options);
    // The window frontend sends TravelPostAccept to the local player (P2Player: PostTravel,
    // GameState, day setup); do the same headless so the sweep sees the real startup path.
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    game.drain_log(echo);
    // `P2GAME_PROFILE_TICKS=1`: profile only the world ticks (not begin play) and print each tick's time.
    let tick_times = std::env::var_os("P2GAME_PROFILE_TICKS").is_some();
    if tick_times {
        game.world.vm.profile_natives.clear();
        game.world.vm.profile_events.clear();
    }
    for i in 0..args.frames {
        let t = Instant::now();
        game.tick(1.0 / 30.0);
        if tick_times {
            println!("tick {i}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
        game.drain_log(echo);
        for l in p2game::save::process_requests(&mut game) {
            println!("{l}");
        }
        if let Some((map, opts)) = game.take_travel() {
            println!("travel requested: {map}{opts}");
            if !game.travel(&map, &opts) {
                eprintln!("travel to {map} failed: map not found");
            }
        }
    }
    (game, t0.elapsed().as_secs_f32())
}

fn print_report(game: &Game, tally: &Tally, top: usize) {
    println!(
        "GameInfo: {}  PlayerController: {}  live actors: {}  Accessed None warnings: {}",
        game.game_class,
        if game.player_controller.is_some() { "logged in" } else { "none" },
        game.live_actors(),
        game.accessed_none
    );
    {
        let s = game.host.stats.borrow();
        let playing = game.host.anims.borrow().values().filter(|x| x.inst.is_animating(0)).count();
        println!(
            "animation: {} instances ({playing} playing on channel 0), {} AnimEnd delivered, notifies {:?}, {} unresolved meshes, {} sounds requested",
            s.instances,
            s.anim_ends,
            s.notifies,
            s.unresolved_meshes.len(),
            game.host.sounds.borrow().len()
        );
    }
    let mut an: Vec<_> = game.accessed_none_by.iter().collect();
    an.sort_by(|a, b| b.1.cmp(a.1));
    for (msg, n) in an.iter().take(top) {
        println!("  AN {n:>9}  {msg}");
    }
    let nv = tally.sorted_natives();
    println!("\nunimplemented natives hit: {} distinct, {} calls", nv.len(), nv.iter().map(|x| x.1.0).sum::<u64>());
    for (name, (calls, _)) in nv.iter().take(top) {
        println!("  {calls:>9}  {name}");
    }
    let ev = tally.sorted_errors();
    println!("\nscript errors: {} distinct, {} total", ev.len(), ev.iter().map(|x| x.1.0).sum::<u64>());
    for (msg, (n, _)) in ev.iter().take(top) {
        println!("  {n:>9}  {msg}");
    }
}

fn headless(cfg: BootConfig, args: &Args) {
    let mut world = new_world(cfg);
    let url = args.map.clone().unwrap_or_else(|| world.cfg.startup_map.clone());
    // `--map intro?Game=GameTypes.GameSinglePlayer` works like a travel URL.
    let (name, url_opts) = p2game::boot::split_url(&url);
    let mut args2 = Args { root: args.root.clone(), map: args.map.clone(), frames: args.frames, out: args.out.clone(), game_class: args.game_class.clone(), options: format!("{}{}", url_opts, args.options), verbose: args.verbose };
    if args2.game_class.is_none() {
        args2.game_class = p2game::boot::url_option(&url_opts, "Game");
    }
    let args = &args2;
    let Some(map) = open_map(&world, &name) else {
        eprintln!("map {name} not found");
        std::process::exit(1)
    };
    let t0 = Instant::now();
    let lv = world.load_level(map);
    println!(
        "{name}: {} actors, {} subobjects, {} classes, {} props applied, {} unresolved refs ({:.2}s)",
        lv.stats.actors,
        lv.stats.subobjects,
        lv.stats.distinct_classes,
        lv.stats.props_applied,
        lv.stats.refs_unresolved,
        t0.elapsed().as_secs_f32()
    );
    let profile = std::env::args().any(|a| a == "--profile");
    let mut world = world;
    world.vm.profile = profile;
    let (game, secs) = boot_one(world, lv, args, args.verbose);
    println!("begin play + {} frames in {:.2}s", args.frames, secs);
    // `P2GAME_PRINT_ACTORS=Name1,Name2`: final location and physics of the named actors.
    if let Ok(names) = std::env::var("P2GAME_PRINT_ACTORS") {
        let vm = &game.world.vm;
        for n in names.split(',').filter(|s| !s.is_empty()) {
            match vm.actors.iter().copied().find(|&a| !vm.objects[a].deleted && vm.object_name(a).eq_ignore_ascii_case(n)) {
                Some(a) => println!("actor {n}: at {:?} physics {:?}", vm.actor_loc(a), vm.get_prop(a, "Physics").map(|v| v.as_int())),
                None => println!("actor {n}: not found"),
            }
        }
    }
    let phases = game.take_frame_times();
    println!(
        "per frame: {}",
        phases.iter().map(|(n, ms)| format!("{n} {ms:.2} ms")).collect::<Vec<_>>().join(", ")
    );
    if profile {
        let vm = &game.world.vm;
        for (title, m) in [("natives", &vm.profile_natives), ("events", &vm.profile_events)] {
            let mut v: Vec<_> = m.iter().collect();
            v.sort_by(|a, b| b.1.1.total_cmp(&a.1.1));
            println!("top {title} by total time:");
            for (k, (n, t)) in v.iter().take(25) {
                println!("  {:>9.1} ms {:>9} calls  {k}", t * 1000.0, n);
            }
        }
    }
    let mut tally = Tally::default();
    tally.add(&game);
    print_report(&game, &tally, 40);
    match tally.write(&args.out) {
        Ok(()) => println!("\nwrote {} and script_errors.tsv", args.out.join("natives_needed.tsv").display()),
        Err(e) => eprintln!("writing reports: {e}"),
    }
}

/// Boots every map in turn (fresh VM world per map) and aggregates the histograms.
fn boot_all(cfg: BootConfig, args: &Args) {
    let t0 = Instant::now();
    let mut world = new_world(cfg);
    let maps = all_maps(&world);
    let mut tally = Tally::default();
    let mut per_map = String::from("map\tactors\tlive_after\tmissing_native_calls\tscript_errors\tsecs\n");
    for (name, path) in &maps {
        let Ok(pkg) = Package::open(path) else { continue };
        let base = world.vm.objects.len();
        let lv = world.load_level(Arc::new(pkg));
        let actors = lv.stats.actors;
        let (game, secs) = boot_one(world, lv, args, false);
        tally.add(&game);
        let calls: u64 = game.world.vm.missing_natives.values().sum();
        let errs: u64 = game.world.vm.script_errors.values().sum();
        let live = game.live_actors();
        println!("{name:<28} actors {actors:>5}  live {live:>5}  missing-native calls {calls:>8}  script errors {errs:>6}  {secs:.1}s");
        per_map += &format!("{name}\t{actors}\t{live}\t{calls}\t{errs}\t{secs:.2}\n");
        world = game.world;
        reset_world(&mut world, base);
    }
    std::fs::create_dir_all(&args.out).ok();
    std::fs::write(args.out.join("boot_maps.tsv"), per_map).ok();
    if let Err(e) = tally.write(&args.out) {
        eprintln!("writing reports: {e}");
    }
    let nv = tally.sorted_natives();
    println!("\n{} maps booted in {:.1}s. Unimplemented natives hit: {}", maps.len(), t0.elapsed().as_secs_f32(), nv.len());
    for (k, (calls, maps)) in nv.iter().take(30) {
        println!("  {calls:>10}  {k:<44} {maps} maps");
    }
    let ev = tally.sorted_errors();
    println!("script errors: {} distinct", ev.len());
    for (msg, (n, m)) in ev.iter().take(20) {
        println!("  {n:>10}  {msg}  ({m} maps)");
    }
    println!("wrote natives_needed.tsv, script_errors.tsv, boot_maps.tsv in {}", args.out.display());
}
