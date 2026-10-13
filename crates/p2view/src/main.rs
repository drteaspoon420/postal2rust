//! p2view: fly-through viewer for POSTAL 2 maps, loading content from the user's own install.
//!
//! Usage: p2view [--game <install dir>] [--map <name>] [--no-bc] [--unlit] [--dynamic]
//!               [--screenshot <file.png>] [--bench <frames>] [--cam x,y,z,yaw_deg,pitch_deg]
//! Controls: hold right mouse + move to look, WASD move, Q/E down/up, Shift fast, F1 stats.

mod ambient;
mod fx;
mod pawns;
mod player;

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::image::{CompressedImageFormatSupport, CompressedImageFormats};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2view::{outdoor, scene, spawn};
use scene::{Options, SceneData};
use std::path::PathBuf;
use std::sync::Arc;
use upkg::library::Library;

const DEFAULT_GAME: &str = r"G:\SteamLibrary\steamapps\common\POSTAL2Complete";

#[derive(Resource, Clone)]
struct Args {
    game: PathBuf,
    map: String,
    allow_bc: bool,
    unlit: bool,
    /// Ignore baked lighting (lightmaps, vertex lighting) and light everything dynamically.
    dynamic: bool,
    screenshot: Option<PathBuf>,
    bench: Option<u32>,
    cam: Option<[f32; 5]>,
}

fn parse_args() -> Args {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let has = |k: &str| a.iter().any(|x| x == k);
    Args {
        game: get("--game").map(PathBuf::from).or_else(|| std::env::var_os("POSTAL2_DIR").map(PathBuf::from)).unwrap_or_else(|| DEFAULT_GAME.into()),
        map: get("--map").unwrap_or_else(|| "Junkyard".into()),
        allow_bc: !has("--no-bc"),
        unlit: has("--unlit"),
        dynamic: has("--dynamic"),
        screenshot: get("--screenshot").map(PathBuf::from),
        bench: get("--bench").and_then(|s| s.parse().ok()),
        cam: get("--cam").and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (v.len() == 5).then(|| [v[0], v[1], v[2], v[3], v[4]])
        }),
    }
}

fn main() {
    let args = parse_args();
    let headless_capture = args.screenshot.is_some() || args.bench.is_some();
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("p2view - {}", args.map),
                resolution: (1600u32, 900u32).into(),
                present_mode: if headless_capture { bevy::window::PresentMode::AutoNoVsync } else { bevy::window::PresentMode::AutoVsync },
                ..default()
            }),
            ..default()
        }))
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        .add_plugins(outdoor::OutdoorPlugin)
        .add_plugins(p2view::baked::BakedPlugin)
        .add_plugins(pawns::PawnsPlugin { game: args.game.clone(), map: args.map.clone() })
        .add_plugins(player::PlayerPlugin { game: args.game.clone(), map: args.map.clone() })
        .add_plugins(ambient::AmbientPlugin { game: args.game.clone(), map: args.map.clone() })
        .add_plugins(fx::FxMapPlugin { game: args.game.clone(), map: args.map.clone() })
        .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.85)))
        .insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 900.0, ..default() })
        .insert_resource(args)
        .init_resource::<FrameCounter>()
        .add_systems(Startup, load_map)
        .add_systems(Update, (fly_camera, report, capture))
        .run();
}

#[derive(Component)]
struct FlyCam {
    yaw: f32,
    pitch: f32,
}

#[derive(Resource, Default)]
struct FrameCounter {
    frames: u32,
    start: Option<std::time::Instant>,
    bench_start: Option<std::time::Instant>,
    bench_frames: u32,
    last_report: f64,
}

fn load_map(
    mut commands: Commands,
    args: Res<Args>,
    bc_support: Option<Res<CompressedImageFormatSupport>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let gpu_bc = args.allow_bc && bc_support.as_ref().is_some_and(|s| s.0.contains(CompressedImageFormats::BC));
    let lib = Library::new(&args.game).unwrap_or_else(|e| panic!("cannot index game install at {}: {e}", args.game.display()));
    let path = lib.map_path(&args.map).unwrap_or_else(|| panic!("map '{}' not found under {}", args.map, args.game.display()));
    let map = Arc::new(upkg::Package::open(&path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display())));
    let data = scene::build_scene(&lib, &map, &Options { gpu_bc, projectors: true }).unwrap_or_else(|e| panic!("cannot build scene: {e}"));
    let s = &data.stats;
    println!(
        "LOADED {} in {} ms: bsp_polys={} static_mesh_actors={} unique_meshes={} mesh_sections={} instances={} world_batches={} materials={} textures={} (gpu-bc={}) unresolved_refs={}",
        args.map,
        s.load_ms,
        s.bsp_polys,
        s.static_mesh_actors,
        s.unique_meshes,
        data.meshes.len(),
        data.instances.len(),
        data.world.len(),
        data.materials.len(),
        s.textures,
        s.bc_textures,
        s.unresolved
    );
    println!(
        "TERRAIN terrains={} layer_meshes={} tris={} deco={} sky_polys={} sky_actors={} lightmaps={} lightmapped_tris={} baked_actors={} projectors={} movers={} lights={} (cap {}) sun={} zone_fog={:?}",
        s.terrains,
        data.terrain.len(),
        s.terrain_tris,
        s.deco_instances,
        s.sky_polys,
        s.sky_actors,
        s.lightmaps,
        s.lightmapped_polys,
        s.baked_actors,
        s.projectors,
        data.movers.len(),
        s.lights,
        scene::MAX_LIGHTS,
        data.sun.is_some(),
        data.zone.as_ref().and_then(|z| z.fog)
    );
    spawn_scene(&mut commands, &args, data, &mut meshes, &mut materials, &mut images);
}

/// Bounding box of all static mesh instance origins (metres, Bevy space).
fn instance_bounds(data: &SceneData) -> Option<(Vec3, Vec3)> {
    let mut it = data.instances.iter().map(|i| i.transform.w_axis.truncate());
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p))))
}

fn spawn_scene(
    commands: &mut Commands,
    args: &Args,
    mut data: SceneData,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    let spawned = spawn::spawn_scene_with(commands, &mut data, spawn::SpawnOptions { unlit: args.unlit, baked: !args.dynamic }, images, meshes, materials);
    let (fog_color, fog_falloff) = spawned.fog.unwrap_or((Color::srgb(0.55, 0.68, 0.85), FogFalloff::Linear { start: 250.0, end: 1400.0 }));

    let bounds = instance_bounds(&data);
    println!("SPAWN found={} at {:?}  instance bounds {:?}", data.spawn_found, data.spawn.0, bounds);
    let (pos, yaw, pitch) = match args.cam {
        Some(c) => (Vec3::new(c[0], c[1], c[2]), c[3].to_radians(), c[4].to_radians()),
        None if data.spawn_found => {
            let (p, dir) = data.spawn;
            (p + Vec3::Y * 1.2, (-dir.z).atan2(dir.x), 0.0)
        }
        None => {
            // No PlayerStart: look at the centre of the placed props from above and to the side.
            let (lo, hi) = bounds.unwrap_or((Vec3::ZERO, Vec3::ZERO));
            let c = (lo + hi) * 0.5;
            let r = (hi - lo).length().max(10.0) * 0.6;
            (c + Vec3::new(-r * 0.7, r * 0.5, 0.0), 0.0, -0.6)
        }
    };
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { fov: 75f32.to_radians(), far: 1500.0, ..default() }),
        DistanceFog { color: fog_color, falloff: fog_falloff, ..default() },
        Transform::from_translation(pos).with_rotation(cam_rot(yaw, pitch)),
        FlyCam { yaw, pitch },
    ));
}

fn cam_rot(yaw: f32, pitch: f32) -> Quat {
    // yaw 0 looks along +X; Bevy cameras look along -Z by default.
    Quat::from_rotation_y(yaw - std::f32::consts::FRAC_PI_2) * Quat::from_rotation_x(pitch)
}

fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut q: Query<(&mut Transform, &mut FlyCam)>,
) {
    let Ok((mut tf, mut cam)) = q.single_mut() else { return };
    if buttons.pressed(MouseButton::Right) {
        cam.yaw -= motion.delta.x * 0.003;
        cam.pitch = (cam.pitch - motion.delta.y * 0.003).clamp(-1.54, 1.54);
        tf.rotation = cam_rot(cam.yaw, cam.pitch);
    }
    let mut v = Vec3::ZERO;
    let (fwd, right) = (*tf.forward(), *tf.right());
    if keys.pressed(KeyCode::KeyW) {
        v += fwd;
    }
    if keys.pressed(KeyCode::KeyS) {
        v -= fwd;
    }
    if keys.pressed(KeyCode::KeyD) {
        v += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        v -= right;
    }
    if keys.pressed(KeyCode::KeyE) {
        v += Vec3::Y;
    }
    if keys.pressed(KeyCode::KeyQ) {
        v -= Vec3::Y;
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) { 40.0 } else { 8.0 };
    tf.translation += v.normalize_or_zero() * speed * time.delta_secs();
}

fn report(time: Res<Time>, diag: Res<DiagnosticsStore>, keys: Res<ButtonInput<KeyCode>>, mut fc: ResMut<FrameCounter>, q: Query<&Transform, With<FlyCam>>) {
    let now = time.elapsed_secs_f64();
    if keys.just_pressed(KeyCode::F1) || now - fc.last_report > 5.0 {
        fc.last_report = now;
        let fps = diag.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
        if let Ok(t) = q.single() {
            println!("fps={fps:.0} cam=({:.1},{:.1},{:.1})", t.translation.x, t.translation.y, t.translation.z);
        }
    }
}

/// Screenshot / benchmark mode: warm up (asset upload), optionally measure frames, capture, then exit.
fn capture(mut commands: Commands, args: Res<Args>, mut fc: ResMut<FrameCounter>, mut exit: MessageWriter<AppExit>) {
    if args.screenshot.is_none() && args.bench.is_none() {
        return;
    }
    fc.frames += 1;
    // Pipelines (lit/shadowed variants especially) compile asynchronously; hold the warmup until
    // both a frame count and a minimum wall time have passed so captures are never blank.
    let started = *fc.start.get_or_insert_with(std::time::Instant::now);
    if fc.frames == WARMUP && started.elapsed().as_secs_f32() < 4.0 {
        fc.frames -= 1;
        return;
    }
    const WARMUP: u32 = 60;
    let bench = args.bench.unwrap_or(0);
    if fc.frames == WARMUP {
        fc.bench_start = Some(std::time::Instant::now());
    }
    if fc.frames > WARMUP {
        fc.bench_frames += 1;
    }
    let done_bench = fc.frames >= WARMUP + bench;
    if done_bench && fc.frames == WARMUP + bench {
        if bench > 0 {
            let secs = fc.bench_start.map(|s| s.elapsed().as_secs_f64()).unwrap_or(1.0);
            println!("BENCH frames={} avg_frame_ms={:.2} avg_fps={:.0}", fc.bench_frames, secs * 1000.0 / fc.bench_frames as f64, fc.bench_frames as f64 / secs);
        }
        if let Some(path) = &args.screenshot {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
        }
    }
    // Give the screenshot time to be read back and written before exiting (the file save
    // runs on an async task, so wait until the file actually exists).
    let shot_done = args.screenshot.as_ref().is_none_or(|p| p.exists());
    if fc.frames >= WARMUP + bench + 15 && (shot_done || fc.frames >= WARMUP + bench + 600) {
        exit.write(AppExit::Success);
    }
}
