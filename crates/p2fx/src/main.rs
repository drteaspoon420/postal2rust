//! p2fx: shows POSTAL 2 particle effects (emitter classes or Emitter actors placed in maps) and
//! scans the install for emitters.
//!
//! Usage:
//!   p2fx [--game <dir>] --class <Pkg.Class|Class> [--shots <dir> --times 0.5,1,2,4] [--yaw <deg>]
//!   p2fx --map <Map> --actor <ActorName> [...]          one placed emitter actor
//!   p2fx --list [<Pkg>]                                 emitter classes in the script packages
//!   p2fx --map-list <Map>                               emitter actors placed in a map
//!   p2fx --scan                                         decode every emitter in the install, print counts
//!   p2fx --dump --class <...>                           print the decoded sub-emitter data
//! Options: --dist <m> (camera distance), --height <m>, --step <s> (fixed sim step, default 1/60),
//!          --size WxH, --bg r,g,b (0..1), --no-fit (do not frame the camera on the simulated particles).

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2fx::geom::{UU_PER_M, to_bevy};
use p2fx::load::{FX_PACKAGES, FxLibrary, FxSystem};
use p2fx::{FxClock, FxInstance, FxPlugin, actor_transform, spawn_emitter};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use upkg::emitter::{EmitterKind, ParticleEmitter};
use upkg::{Package, begin_object};

const DEFAULT_GAME: &str = r"G:\SteamLibrary\steamapps\common\POSTAL2Complete";

#[derive(Resource, Clone)]
struct Args {
    game: PathBuf,
    class: Option<String>,
    map: Option<String>,
    actor: Option<String>,
    shots: Option<PathBuf>,
    times: Vec<f32>,
    yaw: f32,
    pitch: f32,
    dist: Option<f32>,
    height: Option<f32>,
    step: f32,
    size: (u32, u32),
    bg: [f32; 3],
    fit: bool,
}

fn parse_args() -> (Args, Vec<String>) {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let size = get("--size").and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))).unwrap_or((960, 720));
    let bg = get("--bg").map(|s| {
        let v: Vec<f32> = s.split(',').filter_map(|x| x.parse().ok()).collect();
        [v.first().copied().unwrap_or(0.1), v.get(1).copied().unwrap_or(0.1), v.get(2).copied().unwrap_or(0.12)]
    });
    let args = Args {
        game: get("--game").map(PathBuf::from).or_else(|| std::env::var_os("POSTAL2_DIR").map(PathBuf::from)).unwrap_or_else(|| DEFAULT_GAME.into()),
        class: get("--class"),
        map: get("--map").or_else(|| get("--map-list")),
        actor: get("--actor"),
        shots: get("--shots").map(PathBuf::from),
        times: get("--times").map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_else(|| vec![0.25, 0.5, 1.0, 2.0, 4.0]),
        yaw: get("--yaw").and_then(|s| s.parse().ok()).unwrap_or(0.0),
        pitch: get("--pitch").and_then(|s| s.parse().ok()).unwrap_or(10.0),
        dist: get("--dist").and_then(|s| s.parse().ok()),
        height: get("--height").and_then(|s| s.parse().ok()),
        step: get("--step").and_then(|s| s.parse().ok()).unwrap_or(1.0 / 60.0),
        size,
        bg: bg.unwrap_or([0.12, 0.13, 0.15]),
        fit: !a.iter().any(|x| x == "--no-fit"),
    };
    (args, a)
}

fn main() {
    let (args, raw) = parse_args();
    let has = |k: &str| raw.iter().any(|x| x == k);
    let lib = FxLibrary::new(&args.game).unwrap_or_else(|e| panic!("cannot index game install at {}: {e}", args.game.display()));
    if has("--scan") {
        scan(&lib, &args);
        return;
    }
    if has("--list") {
        let pkg = raw.iter().position(|x| x == "--list").and_then(|i| raw.get(i + 1)).filter(|s| !s.starts_with("--")).cloned();
        list_classes(&lib, pkg.as_deref());
        return;
    }
    if has("--map-list") {
        let map = open_map(&lib, args.map.as_deref().unwrap());
        for (i, class) in FxLibrary::map_emitter_actors(&map) {
            match lib.load_actor(&map, i) {
                Ok(s) => println!("{:<28} {:<24} emitters={} particles={} loc={:?}", s.actor.object_name, class, s.emitters.len(), s.particle_capacity(), s.actor.location),
                Err(e) => println!("{:<28} {:<24} FAILED {e}", map.name(map.exports[i].name), class),
            }
        }
        return;
    }
    let system = match (&args.class, &args.map, &args.actor) {
        (Some(c), _, _) => lib.load_class(c),
        (None, Some(m), Some(a)) => {
            let map = open_map(&lib, m);
            let e = map.find_export(a, None).unwrap_or_else(|| panic!("actor '{a}' not found in {m}"));
            lib.load_actor(&map, e)
        }
        _ => {
            eprintln!("usage: p2fx --class <Class> | --map <Map> --actor <Name> | --list | --map-list <Map> | --scan  (see source header)");
            std::process::exit(2);
        }
    }
    .unwrap_or_else(|e| panic!("{e}"));
    describe(&system);
    if has("--dump") {
        for e in &system.emitters {
            println!("{:#?}", e.data);
        }
        return;
    }
    if system.emitters.is_empty() {
        eprintln!("no particle emitters in {}", system.name);
        std::process::exit(1);
    }
    run_viewer(args, Arc::new(system));
}

fn open_map(lib: &FxLibrary, name: &str) -> Arc<Package> {
    let path = lib.lib.map_path(name).unwrap_or_else(|| panic!("map '{name}' not found"));
    let pkg = Arc::new(Package::open(&path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display())));
    lib.lib.insert(pkg.clone());
    pkg
}

fn describe(s: &FxSystem) {
    println!("SYSTEM {} class={} emitters={} max_particles={} auto_destroy={}", s.name, s.actor.class, s.emitters.len(), s.particle_capacity(), s.actor.auto_destroy);
    for (i, e) in s.emitters.iter().enumerate() {
        let d = &e.data;
        println!(
            "  [{i}] {:<18} {:<20} max={:<4} life={:.2}..{:.2} pps={} style={:?} tex={} sub={}x{} coord={} respawn={} mesh={}{}",
            d.kind.class_name(),
            d.object_name,
            d.max_particles,
            d.lifetime_range.min,
            d.lifetime_range.max,
            d.particles_per_second,
            d.draw_style,
            e.texture.as_ref().map(|t| t.name.as_str()).unwrap_or("-"),
            d.texture_u_subdivisions,
            d.texture_v_subdivisions,
            d.coordinate_system,
            d.respawn_dead_particles,
            e.mesh.as_ref().map(|m| m.name.as_str()).unwrap_or("-"),
            if d.unknown.is_empty() { String::new() } else { format!(" unknown={:?}", d.unknown) }
        );
    }
}

fn list_classes(lib: &FxLibrary, only: Option<&str>) {
    let pkgs: Vec<&str> = match only {
        Some(p) => vec![p],
        None => FX_PACKAGES.to_vec(),
    };
    for p in pkgs {
        let Some(pkg) = lib.lib.package(p) else { continue };
        for i in pkg.exports_of_class("Class").collect::<Vec<_>>() {
            let name = format!("{}.{}", pkg.name, pkg.name(pkg.exports[i].name));
            if let Ok(s) = lib.load_class(&name) {
                if !s.emitters.is_empty() {
                    let kinds: Vec<&str> = s.emitters.iter().map(|e| e.data.kind.class_name()).collect();
                    println!("{name:<44} emitters={} [{}]", s.emitters.len(), kinds.join(", "));
                }
            }
        }
    }
}

/// Decodes every ParticleEmitter object and every emitter class / placed actor; prints counts.
fn scan(lib: &FxLibrary, args: &Args) {
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut style: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    let (mut objs, mut fails, mut tex_refs, mut tex_ok, mut mesh_refs, mut mesh_ok) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut check = |pkg: &Arc<Package>, i: usize, by_kind: &mut BTreeMap<String, usize>| {
        objs += 1;
        match ParticleEmitter::read(pkg, i) {
            Ok(d) => {
                *by_kind.entry(d.kind.class_name().to_string()).or_default() += 1;
                *style.entry(format!("{:?}", d.draw_style)).or_default() += 1;
                for u in &d.unknown {
                    *unknown.entry(u.clone()).or_default() += 1;
                }
                let obj = upkg::library::Obj { pkg: pkg.clone(), export: i };
                if let Ok((_, props)) = begin_object(pkg, i) {
                    if props.object("Texture").is_some_and(|r| r != upkg::ObjRef::Null) {
                        tex_refs += 1;
                    }
                    if props.object("StaticMesh").is_some_and(|r| r != upkg::ObjRef::Null) {
                        mesh_refs += 1;
                    }
                }
                if let Some(fx) = lib.load_sub_emitter(&obj) {
                    if fx.texture.is_some() && begin_object(pkg, i).ok().and_then(|(_, p)| p.object("Texture")).is_some_and(|r| r != upkg::ObjRef::Null) {
                        tex_ok += 1;
                    }
                    if fx.mesh.is_some() {
                        mesh_ok += 1;
                    }
                }
            }
            Err(e) => {
                fails += 1;
                println!("FAIL {}.{}: {e}", pkg.name, pkg.name(pkg.exports[i].name));
            }
        }
    };

    // Script packages: sub-emitter objects and emitter classes.
    let sys = args.game.join("System");
    let mut u_files: Vec<PathBuf> = std::fs::read_dir(&sys).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("u"))).collect()).unwrap_or_default();
    u_files.sort();
    let (mut classes, mut class_fail, mut class_particles) = (0usize, 0usize, 0usize);
    let mut class_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for f in &u_files {
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let Some(pkg) = lib.lib.package(&stem) else { continue };
        for i in 0..pkg.exports.len() {
            if EmitterKind::is_particle_emitter_class(pkg.export_class(i)) {
                check(&pkg, i, &mut by_kind);
            }
        }
        for i in pkg.exports_of_class("Class").collect::<Vec<_>>() {
            let name = format!("{}.{}", pkg.name, pkg.name(pkg.exports[i].name));
            let Some(class) = lib.find_class(&name) else { continue };
            let Some(chain) = lib.class_chain(&class) else { continue };
            if !chain.is_a("Emitter") {
                continue;
            }
            match lib.load_class(&name) {
                Ok(s) => {
                    classes += 1;
                    class_particles += s.particle_capacity();
                    for e in &s.emitters {
                        *class_kinds.entry(e.data.kind.class_name().to_string()).or_default() += 1;
                    }
                }
                Err(_) => class_fail += 1,
            }
        }
    }
    let script_objs = by_kind.values().sum::<usize>();

    // Maps: sub-emitter objects and placed emitter actors.
    let maps_dir = args.game.join("Maps");
    let mut maps: Vec<PathBuf> = std::fs::read_dir(&maps_dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk"))).collect()).unwrap_or_default();
    maps.sort();
    let (mut actors, mut actor_fail, mut actor_emitters, mut maps_with) = (0usize, 0usize, 0usize, 0usize);
    let mut actor_classes: BTreeMap<String, usize> = BTreeMap::new();
    for m in &maps {
        let Ok(pkg) = Package::open(m) else { continue };
        let pkg = Arc::new(pkg);
        lib.lib.insert(pkg.clone());
        for i in 0..pkg.exports.len() {
            if EmitterKind::is_particle_emitter_class(pkg.export_class(i)) {
                check(&pkg, i, &mut by_kind);
            }
        }
        let list = FxLibrary::map_emitter_actors(&pkg);
        if !list.is_empty() {
            maps_with += 1;
        }
        for (i, class) in list {
            match lib.load_actor(&pkg, i) {
                Ok(s) => {
                    actors += 1;
                    actor_emitters += s.emitters.len();
                    *actor_classes.entry(class).or_default() += 1;
                }
                Err(_) => actor_fail += 1,
            }
        }
    }

    println!("PARTICLE EMITTER OBJECTS: {objs} decoded ({script_objs} in {} script packages, {} in {} maps), {fails} failures", u_files.len(), objs - script_objs, maps.len());
    for (k, v) in &by_kind {
        println!("  {v:6} {k}");
    }
    println!("  draw styles: {}", style.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "));
    println!("  textures: {tex_ok}/{tex_refs} referenced textures decoded; static meshes: {mesh_ok}/{mesh_refs} decoded");
    println!(
        "  unmodelled properties: {}",
        if unknown.is_empty() { "none".to_string() } else { unknown.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ") }
    );
    println!("EMITTER CLASSES: {classes} resolved ({class_fail} failed), {} sub-emitters, {class_particles} particle slots", class_kinds.values().sum::<usize>());
    for (k, v) in &class_kinds {
        println!("  {v:6} {k}");
    }
    println!("PLACED EMITTER ACTORS: {actors} in {maps_with} of {} maps ({actor_fail} failed), {actor_emitters} sub-emitters", maps.len());
    for (k, v) in &actor_classes {
        println!("  {v:6} {k}");
    }
}

// -------------------------------------------------------------------------------------------------
// Viewer
// -------------------------------------------------------------------------------------------------

#[derive(Resource)]
struct Show {
    system: Arc<FxSystem>,
}

#[derive(Resource, Default)]
struct Capture {
    sim_time: f32,
    next: usize,
    pending: Option<(PathBuf, u32)>,
    frames: u32,
    framed: bool,
}

#[derive(Component)]
struct Orbit {
    target: Vec3,
    dist: f32,
    yaw: f32,
    pitch: f32,
}

fn run_viewer(args: Args, system: Arc<FxSystem>) {
    let capture = args.shots.is_some();
    let step = args.step;
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("p2fx - {}", system.name),
                resolution: (args.size.0, args.size.1).into(),
                present_mode: if capture { bevy::window::PresentMode::AutoNoVsync } else { bevy::window::PresentMode::AutoVsync },
                ..default()
            }),
            ..default()
        }))
        .add_plugins(FxPlugin)
        .insert_resource(ClearColor(Color::srgb(args.bg[0], args.bg[1], args.bg[2])))
        .insert_resource(FxClock { fixed_dt: capture.then_some(step), ..default() })
        .insert_resource(Show { system })
        .insert_resource(args)
        .init_resource::<Capture>()
        .add_systems(Startup, setup)
        .add_systems(Update, (orbit, capture_system, report))
        .run();
}

fn setup(mut commands: Commands, args: Res<Args>, show: Res<Show>, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let sys = show.system.clone();
    // Placed actors keep their map transform; classes spawn at the origin.
    let tf = if args.map.is_some() { actor_transform(&sys.actor) } else { Transform::IDENTITY };
    // Frame from the emitters' spawn volume and expected travel.
    let mut extent = 64.0f32;
    let mut center_up = 0.0f32;
    for e in &sys.emitters {
        let d = &e.data;
        let life = d.lifetime_range.max.max(0.1);
        let v = d.start_velocity_range.max_abs() + d.start_velocity_radial_range.max.abs();
        let acc = Vec3::from(d.acceleration);
        let travel = v * life + 0.5 * acc.length() * life * life;
        let size = d.start_size_range.max_abs() * d.size_scale.iter().map(|k| k.size).fold(1.0, f32::max);
        extent = extent.max((d.start_location_range.max_abs() + travel.min(1500.0) + size).min(3000.0));
        center_up += (d.start_velocity_range.z.center() * life + 0.5 * acc.z * life * life).clamp(-800.0, 800.0) * 0.4;
    }
    center_up /= sys.emitters.len().max(1) as f32;
    let mut floor_y = tf.translation.y - 0.02;
    let mut target = tf.translation + Vec3::Y * (center_up / UU_PER_M);
    let mut dist = args.dist.unwrap_or(extent / UU_PER_M * 2.4).max(1.0);
    if args.fit && args.dist.is_none() {
        // Pre-simulate on the CPU to the middle of the shot times and frame the live particles.
        let mut probe = FxInstance::new(sys.clone(), 1);
        let t_fit = args.times.iter().copied().fold(0.0f32, f32::max).min(4.0) * 0.6;
        let mut pts: Vec<Vec3> = Vec::new();
        let mut t = 0.0;
        while t < t_fit.max(0.2) {
            probe.advance(0.1, args.step);
            t += 0.1;
            pts.extend(probe.particle_points());
        }
        // Robust bounds: 5th..95th percentile per axis (a few far-flung particles do not
        // shrink the effect to a dot).
        let union = (!pts.is_empty()).then(|| {
            let pct = |axis: usize, q: f32| {
                let mut v: Vec<f32> = pts.iter().map(|p| p[axis]).collect();
                v.sort_by(|a, b| a.total_cmp(b));
                v[((v.len() - 1) as f32 * q) as usize]
            };
            let lo = Vec3::new(pct(0, 0.05), pct(1, 0.05), pct(2, 0.05));
            let hi = Vec3::new(pct(0, 0.95), pct(1, 0.95), pct(2, 0.95));
            let pad = Vec3::splat(probe.typical_size());
            (lo - pad, hi + pad)
        });
        if let Some((lo, hi)) = union {
            let (blo, bhi) = (to_bevy(lo), to_bevy(hi));
            let (mn, mx) = (blo.min(bhi), blo.max(bhi));
            target = tf.translation + (mn + mx) * 0.5;
            dist = ((mx - mn).length() * 1.3).clamp(0.6, 150.0);
            floor_y = floor_y.min(tf.translation.y + mn.y - 0.02);
        }
    }
    let o = Orbit { target: target + Vec3::Y * args.height.unwrap_or(0.0), dist, yaw: args.yaw.to_radians(), pitch: args.pitch.to_radians() };
    println!("CAMERA target={:?} dist={dist:.2} m (extent {extent:.0} uu)", o.target);
    commands.spawn((Camera3d::default(), orbit_transform(&o), o));
    // Floor grid (1 m squares) for scale.
    let floor = mats.add(StandardMaterial { base_color: Color::srgb(0.2, 0.21, 0.22), perceptual_roughness: 1.0, ..default() });
    let line = mats.add(StandardMaterial { base_color: Color::srgb(0.32, 0.33, 0.35), unlit: true, ..default() });
    commands.spawn((Mesh3d(meshes.add(Plane3d::default().mesh().size(60.0, 60.0))), MeshMaterial3d(floor), Transform::from_xyz(tf.translation.x, floor_y - 0.01, tf.translation.z)));
    let bar = meshes.add(Cuboid::new(60.0, 0.005, 0.02));
    for k in -30..=30 {
        let k = k as f32;
        commands.spawn((Mesh3d(bar.clone()), MeshMaterial3d(line.clone()), Transform::from_xyz(tf.translation.x, floor_y, tf.translation.z + k)));
        commands.spawn((Mesh3d(bar.clone()), MeshMaterial3d(line.clone()), Transform::from_xyz(tf.translation.x + k, floor_y, tf.translation.z).with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))));
    }
    commands.spawn((DirectionalLight { illuminance: 6000.0, ..default() }, Transform::from_xyz(2.0, 5.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y)));
    let _ = to_bevy;
    spawn_emitter(&mut commands, sys, tf);
}

fn orbit_transform(o: &Orbit) -> Transform {
    let dir = Vec3::new(o.yaw.sin() * o.pitch.cos(), o.pitch.sin(), o.yaw.cos() * o.pitch.cos());
    Transform::from_translation(o.target + dir * o.dist).looking_at(o.target, Vec3::Y)
}

fn orbit(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    scroll: Res<bevy::input::mouse::AccumulatedMouseScroll>,
    mut q: Query<(&mut Transform, &mut Orbit)>,
    mut fx: Query<&mut FxInstance>,
) {
    let Ok((mut tf, mut o)) = q.single_mut() else { return };
    if buttons.pressed(MouseButton::Left) || buttons.pressed(MouseButton::Right) {
        o.yaw -= motion.delta.x * 0.005;
        o.pitch = (o.pitch + motion.delta.y * 0.005).clamp(-1.4, 1.4);
    }
    o.dist = (o.dist * (1.0 - scroll.delta.y * 0.1)).clamp(0.2, 500.0);
    *tf = orbit_transform(&o);
    if keys.just_pressed(KeyCode::KeyK) {
        for mut f in &mut fx {
            f.kill();
        }
    }
    if keys.just_pressed(KeyCode::KeyT) {
        for mut f in &mut fx {
            f.trigger();
        }
    }
}

fn report(time: Res<Time>, q: Query<&FxInstance>, mut acc: Local<f32>) {
    *acc += time.delta_secs();
    if *acc >= 2.0 {
        *acc = 0.0;
        for f in &q {
            println!("t={:.1}s live particles={}", f.state.time, f.live_particles());
        }
    }
}

/// Takes a screenshot whenever the simulation clock passes one of `--times`, then exits.
fn capture_system(
    mut commands: Commands,
    args: Res<Args>,
    mut cap: ResMut<Capture>,
    mut clock: ResMut<FxClock>,
    fx: Query<&FxInstance>,
    mut cam: Query<&mut Orbit>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(dir) = &args.shots else { return };
    cap.frames += 1;
    // Hold the simulation until shaders/pipelines are ready so early shots are not blank.
    const WARMUP: u32 = 90;
    if cap.frames < WARMUP {
        clock.paused = true;
        return;
    }
    clock.paused = false;
    let t = fx.iter().next().map(|f| f.state.time).unwrap_or(0.0);
    cap.sim_time = t;
    if let Some((path, frame)) = &cap.pending {
        if path.exists() || cap.frames > frame + 600 {
            cap.pending = None;
        } else {
            clock.paused = true;
            return;
        }
    }
    if cap.next >= args.times.len() {
        if cap.frames > 5 {
            exit.write(AppExit::Success);
        }
        return;
    }
    if false {
        if let Some((lo, hi)) = fx.iter().next().and_then(|f| f.particle_bounds()) {
            if t >= args.times[0] * 0.5 {
                if let Ok(mut o) = cam.single_mut() {
                    let (blo, bhi) = (to_bevy(lo), to_bevy(hi));
                    let (mn, mx) = (blo.min(bhi), blo.max(bhi));
                    o.target = (mn + mx) * 0.5;
                    o.dist = ((mx - mn).length() * 1.1).clamp(0.5, 200.0);
                }
                cap.framed = true;
            }
        }
    }
    let want = args.times[cap.next];
    if t + 1e-4 >= want {
        std::fs::create_dir_all(dir).ok();
        let path = dir.join(format!("t{:05.2}.png", t));
        let _ = std::fs::remove_file(&path);
        let live = fx.iter().next().map(|f| f.live_particles()).unwrap_or(0);
        println!("SHOT {} sim_t={t:.3}s live={live}", path.display());
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
        cap.pending = Some((path, cap.frames));
        clock.paused = true;
        cap.next += 1;
    }
}
