//! Windowed mode: the level rendered through p2view's scene builder, with every actor's render
//! entities driven by its VM state after each script tick.
//!
//! p2view builds each static mesh actor's transform from the map's saved properties. At load we
//! record, per actor, the VM placement (Location, Rotation, DrawScale * DrawScale3D) that transform
//! corresponds to. Each frame, if script changed the placement, the entity transform becomes
//! `placement(now) * placement(load)^-1 * transform(load)`, so p2view's own mesh conventions stay in
//! one place. `bHidden` maps to Bevy visibility.
//!
//! Controls: hold the right mouse button to look, WASD to move, Q/E down/up, Shift faster,
//! P pauses script ticking, F1 prints stats. `--screenshot FILE` captures after warm-up and exits.

use crate::{Args, open_map};
use bevy::image::{CompressedImageFormatSupport, CompressedImageFormats};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::math::{Affine3A, Mat3, Vec3};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World as P2World;
use p2view::{outdoor, scene, spawn};
use std::collections::HashMap;
use std::path::PathBuf;
use uscript::vm::{ObjId, Value};

/// Unreal angle units to radians.
const ROT: f32 = std::f32::consts::TAU / 65536.0;

/// Actor placement in Bevy space (metres, Y up) from Unreal Location / Rotation / scale.
fn placement(loc: [f32; 3], rot: [i32; 3], scale: [f32; 3]) -> Affine3A {
    let (sp, cp) = (rot[0] as f32 * ROT).sin_cos();
    let (sy, cy) = (rot[1] as f32 * ROT).sin_cos();
    let (sr, cr) = (rot[2] as f32 * ROT).sin_cos();
    // Unreal rotation: roll about X, then pitch about Y, then yaw about Z; columns are the
    // actor's forward, right and up axes in world space.
    let fwd = Vec3::new(cp * cy, cp * sy, sp);
    let right = Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp);
    let up = Vec3::new(-(cr * sp * cy + sr * sy), -(cr * sp * sy - sr * cy), cr * cp);
    let m = Mat3::from_cols(fwd * scale[0], right * scale[1], up * scale[2]);
    // Unreal (X, Y, Z) maps to Bevy (X, Z, Y); the swap is its own inverse.
    let swap = Mat3::from_cols(Vec3::X, Vec3::Z, Vec3::Y);
    Affine3A::from_mat3_translation(swap * m * swap, scene::to_bevy(loc))
}

#[derive(Clone, Copy, PartialEq)]
struct Pose {
    loc: [f32; 3],
    rot: [i32; 3],
    scale: [f32; 3],
    hidden: bool,
}

fn pose(vm: &uscript::vm::Vm, a: ObjId) -> Pose {
    pose_with(vm, a, &PoseSlots::of(vm, a))
}

/// Property slots read by [`pose`], resolved once per actor (an object's class never changes)
/// instead of hashing four property names per actor every frame.
#[derive(Clone, Copy)]
struct PoseSlots([Option<usize>; 6]);

impl PoseSlots {
    fn of(vm: &uscript::vm::Vm, a: ObjId) -> Self {
        let c = &vm.classes[vm.objects[a].class];
        PoseSlots(["DrawScale", "DrawScale3D", "Location", "Rotation", "bHidden", "AttachmentBone"].map(|n| c.slot(n)))
    }
}

fn pose_with(vm: &uscript::vm::Vm, a: ObjId, sl: &PoseSlots) -> Pose {
    let slots = &vm.objects[a].slots;
    let get = |i: usize| sl.0[i].and_then(|s| slots.get(s));
    let s = get(0).map(|v| v.as_float()).unwrap_or(1.0);
    let s3 = match get(1) {
        Some(Value::Vector(v)) => *v,
        _ => [1.0; 3],
    };
    Pose {
        loc: get(2).map(|v| v.as_vec()).unwrap_or_default(),
        rot: get(3).map(|v| v.as_rot()).unwrap_or_default(),
        scale: [s * s3[0], s * s3[1], s * s3[2]],
        hidden: {
            // `ai::render_hidden` with the actor's own step read through cached slots (this runs
            // for every level static mesh actor every frame): hidden if bHidden; only an actor on
            // a bone of its Base needs the Base-chain walk.
            let on_bone = matches!(get(5), Some(Value::Name(n)) if !n.is_empty() && !n.eq_ignore_ascii_case("None"));
            if get(4).is_some_and(|v| v.as_bool()) {
                true
            } else if on_bone {
                p2game::ai::render_hidden(vm, a)
            } else {
                false
            }
        },
    }
}

struct Driven {
    actor: ObjId,
    slots: PoseSlots,
    entities: Vec<(Entity, Option<Affine3A>)>,
    inv_initial: Affine3A,
    last: Pose,
}

/// The script world plus the actor-to-entity links. Holds `Rc`s, so it lives as a non-send resource.
pub(crate) struct Sim {
    pub(crate) game: Game,
    driven: Vec<Driven>,
    pub(crate) paused: bool,
    pub(crate) frames: u64,
    moved_total: u64,
    rigs: crate::rigs::RigTracker,
    /// Runtime-spawned static mesh actors (see `dynmesh`).
    dynmesh: crate::dynmesh::DynMeshTracker,
    /// Called with the new Game after every level change (for subsystems that register hooks).
    pub(crate) on_travel: Vec<Box<dyn FnMut(&mut Game)>>,
    /// Set after travel so the scene is rebuilt on the next frame.
    reload: bool,
    /// A save load replaced the level this frame: `travel` drops the old level's entities and rigs
    /// (their actor ids are stale) before anything else reads them.
    loaded_save: bool,
}

impl Sim {
    pub(crate) fn rig_count(&self) -> usize {
        self.rigs.rigs.len()
    }

    pub(crate) fn rigs_failed(&self) -> usize {
        self.rigs.failed
    }

    /// Requests a level change (`Map?Options`), processed at the end of the frame.
    #[allow(dead_code)]
    pub(crate) fn request_travel(&self, url: &str) {
        *self.game.host.pending_travel.borrow_mut() = Some(url.to_string());
    }
}

/// Entities belonging to the current level (despawned on travel).
#[derive(Component)]
struct LevelEntity;

#[derive(Resource)]
struct WinArgs {
    map: String,
    unlit: bool,
    screenshot: Option<PathBuf>,
    /// Frame the N-th rigged NPC with the camera once rigs exist.
    view_npc: Option<usize>,
}

#[derive(Component)]
pub(crate) struct FlyCam {
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
}

#[derive(Resource, Default)]
struct Frames {
    n: u32,
    start: Option<std::time::Instant>,
    last_report: f64,
    /// Rendered frames since the last report.
    wall_frames: u64,
    interactive: bool,
    smooth: u32,
    smooth_travel: u32,
    /// Travels requested by `--travel-after`.
    travels: u32,
    /// Frame times (ms) since the last report, for the percentile/hitch line.
    dts: Vec<f32>,
}

pub fn run(cfg: BootConfig, args: Args) {
    let _ = BOOT.set(std::time::Instant::now());
    // The original's red WARNING disclaimer (System\Postal2Splash.bmp) while the engine boots.
    crate::splash::show(&cfg.system_dir);
    let a: Vec<String> = std::env::args().collect();
    let screenshot = a.iter().position(|x| x == "--screenshot").and_then(|i| a.get(i + 1)).map(PathBuf::from);
    let unlit = a.iter().any(|x| x == "--unlit");
    let view_npc = a.iter().position(|x| x == "--view-npc").map(|i| a.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0));
    let url = args.map.clone().unwrap_or_else(|| cfg.startup_map.clone());
    let (map, url_opts) = p2game::boot::split_url(&url);
    let options = format!("{url_opts}{}", args.options);
    let game_class = args.game_class.clone().or_else(|| p2game::boot::url_option(&url_opts, "Game"));

    // Script world first: if the map cannot boot there is nothing to show.
    let mut world = P2World::new(cfg).unwrap_or_else(|e| panic!("VM init failed: {e}"));
    let pkg = open_map(&world, &map).unwrap_or_else(|| panic!("map {map} not found"));
    p2game::physics::prefetch(world.library(), pkg.clone());
    let level = world.load_level(pkg);
    println!("{map}: {} actors instantiated ({:.2} s since boot)", level.stats.actors, boot_secs());
    let mut game = Game::new(world, level, game_class.as_deref());
    println!("load: game set up ({:.2} s since boot)", boot_secs());
    game.begin_play(&options);
    game.drain_log(false);
    println!("begin play done: {} live actors, GameInfo {} ({:.2} s since boot)", game.live_actors(), game.game_class, boot_secs());
    let mut sim = Sim { game, driven: Vec::new(), paused: false, frames: 0, moved_total: 0, rigs: Default::default(), dynmesh: Default::default(), on_travel: Vec::new(), reload: true, loaded_save: false };

    let mut app = App::new();
    let t_plugins = std::time::Instant::now();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        // Size, window mode and vsync from the saved video settings (settings.rs; env overrides
        // P2GAME_RES=WxH, P2GAME_FULLSCREEN=1, P2GAME_NOVSYNC=1, P2GAME_FPS_LIMIT=N).
        primary_window: Some(crate::settings::initial_window(&mut sim.game.world.vm, "Postal 2 Rust/Bevy".to_string())),
        ..default()
    }));
    crate::settings::install(&mut sim);
    app.add_plugins(crate::settings::SettingsPlugin);
    println!("load: bevy plugins added in {:.2} s ({:.2} s since boot)", t_plugins.elapsed().as_secs_f32(), boot_secs());
    // HUD, menus, console and player input from script (ui.rs).
    crate::ui::install(&mut app, &mut sim, &args.root);
    crate::weapons::install(&mut sim);
    println!("load: ui/weapons installed ({:.2} s since boot)", boot_secs());
    // Particles, fluids and decals from script (fx_glue.rs): the VM side pushes a snapshot per frame.
    let fxq = p2fx::FxFrameQueue::default();
    p2game::fx_glue::set_frame_queue(fxq.0.clone());
    app.add_plugins((p2fx::FxPlugin, p2fx::FxVmPlugin)).insert_resource(fxq).insert_resource(p2fx::render_vm::FxFirstPersonLayer(Some(crate::weapons::FP_LAYER)));
    app.add_plugins(crate::ui::UiPlugin)
        .add_plugins(crate::debug::DebugOverlayPlugin)
        .add_plugins(crate::devmenu::DevMenuPlugin)
        .add_plugins(crate::cursor::CursorPlugin)
        .add_plugins(crate::cinema_view::CinemaViewPlugin)
        .add_plugins(crate::weapons::WeaponsPlugin)
        .add_plugins(crate::realinput::RealInputPlugin)
        .add_plugins(crate::pilot::PilotPlugin)
        .add_plugins(crate::tour::TourPlugin)
        .add_plugins(outdoor::OutdoorPlugin)
        .add_plugins(p2view::baked::BakedPlugin)
        .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.85)))
        .insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 900.0, ..default() })
        .insert_resource(WinArgs { map, unlit, screenshot, view_npc })
        .init_resource::<Frames>()
        .init_resource::<p2anim::rig::RigCache>()
        .init_resource::<LevelSnapshot>()
        .insert_non_send(sim)
        .add_systems(Startup, cluster_capacity)
        .add_systems(
            Update,
            (load_scene, tag_level_entities, tick_script, travel, crate::ui::ui_frame, spawn_rigs, sync_transforms, dyn_meshes, pose_rigs, crate::ui::hide_own_body, frame_npc, fly_camera, crate::ui::follow_player, crate::cinema_view::apply_cinema_view, crate::zone_fog::zone_fog, crate::debug::update_overlay, report, capture).chain(),
        )
        .run();
    crate::cursor::release_cursor();
    // Close the audio stream while the process is still whole (see `audio::shutdown_device`).
    p2game::audio::shutdown_device();
}

/// Process start of the windowed host, for load-time reports.
static BOOT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
/// Start of the last travel, until its first interactive frame is reported.
static TRAVEL_START: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

fn boot_secs() -> f32 {
    BOOT.get().map_or(0.0, |t| t.elapsed().as_secs_f32())
}

/// Process memory in MB: (working set, peak working set, private commit). Windows only.
pub(crate) fn process_memory_mb() -> Option<(f64, f64, f64)> {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct Counters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set: usize,
            working_set: usize,
            quota_peak_paged_pool: usize,
            quota_paged_pool: usize,
            quota_peak_non_paged_pool: usize,
            quota_non_paged_pool: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
        }
        let mut c = Counters { cb: std::mem::size_of::<Counters>() as u32, ..Default::default() };
        // SAFETY: plain Win32 call filling a correctly sized PROCESS_MEMORY_COUNTERS.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0;
        let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
        ok.then(|| (mb(c.working_set), mb(c.peak_working_set), mb(c.pagefile_usage)))
    }
    #[cfg(not(windows))]
    None
}

/// The level's many point lights overflow Bevy's default GPU clustering buffers, which then grow
/// at runtime ("Resizing the view clustering Z slice list", lighting glitches for a few frames).
/// Start them large enough for POSTAL 2 maps.
fn cluster_capacity(settings: Option<ResMut<bevy::light::cluster::GlobalClusterSettings>>) {
    if let Some(gpu) = settings.and_then(|s| s.into_inner().gpu_clustering.as_mut()) {
        // Only the Z slice list overflowed. The index list is re-uploaded per view every frame
        // at its full capacity, so it stays at Bevy's default.
        gpu.initial_z_slice_list_capacity = gpu.initial_z_slice_list_capacity.max(4096);
    }
}

/// Entities that existed before the current level was spawned (everything newer belongs to it).
#[derive(Resource, Default)]
struct LevelSnapshot {
    before: Option<std::collections::HashSet<Entity>>,
}

/// Marks the entities spawned by the last level load (one frame later, once commands applied).
// Resources are entities in Bevy 0.19: ones created by a level load (GlobalAmbientLight from spawn_lights) must not be tagged, or the next travel despawns them and Bevy panics.
fn tag_level_entities(mut commands: Commands, mut snap: ResMut<LevelSnapshot>, all: Query<Entity, (Without<LevelEntity>, Without<bevy::ecs::resource::IsResource>)>) {
    let Some(before) = snap.before.take() else { return };
    for e in &all {
        if !before.contains(&e) {
            commands.entity(e).insert(LevelEntity);
        }
    }
}

/// Full-screen "Loading..." overlay shown while a travel loads the next level.
#[derive(Component)]
struct LoadingScreen;

/// Handles a travel request from script: despawns the level's entities, boots the new map and
/// schedules a scene rebuild.
#[allow(clippy::too_many_arguments)]
fn travel(
    mut commands: Commands,
    mut sim: NonSendMut<Sim>,
    level: Query<Entity, With<LevelEntity>>,
    vm_rigs: Query<(Entity, &crate::rigs::VmRig)>,
    mut windows: Query<&mut Window>,
    mut pending: Local<Option<(String, String, u8)>>,
    loading: Query<Entity, With<LoadingScreen>>,
) {
    // A save load (in tick_script) already replaced the level: drop the old level's render
    // entities and actor rigs the same way a travel does.
    if sim.loaded_save {
        sim.loaded_save = false;
        drop_level_entities(&mut commands, &level, &vm_rigs);
        sim.driven.clear();
        sim.rigs.clear();
        sim.reload = true;
        if let Ok(mut w) = windows.single_mut() {
            w.title = "Postal 2 Rust/Bevy".to_string();
        }
    }
    // The level load blocks for a second or more, so first put a loading screen up and let it
    // reach the display (two frames: queued, then presented), then load. `?load=N` save loads
    // are handled elsewhere and go straight through.
    if pending.is_none() {
        let Some((map, opts)) = sim.game.take_travel() else { return };
        let is_level = !map.split('#').next().unwrap_or("").is_empty();
        if is_level {
            // The original keeps the last game frame on screen while LoadMap blocks and draws its
            // level-loading progress bar over it: a thin red bar centred near the bottom
            // (measured at 1600x900: x 568..1032, y 766..772, rgb 181,12,16; `orig_tr_00.png`).
            commands.spawn((
                LoadingScreen,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(35.5),
                    width: Val::Percent(29.0),
                    top: Val::Percent(85.1),
                    height: Val::Percent(0.78),
                    ..default()
                },
                BackgroundColor(Color::srgb_u8(181, 12, 16)),
                GlobalZIndex(i32::MAX - 1),
            ));
        }
        *pending = Some((map, opts, if is_level { 0 } else { 2 }));
    }
    let Some((_, _, waited)) = pending.as_mut() else { return };
    if *waited < 2 {
        *waited += 1;
        return;
    }
    let Some((map, opts, _)) = pending.take() else { return };
    for e in &loading {
        commands.entity(e).despawn();
    }
    println!("travel: {map}{opts}");
    let sim = &mut *sim;
    // `?load=N` (no map): the Load Game menu asks for a save slot, not a level.
    if map.split('#').next().unwrap_or("").is_empty() {
        match p2game::boot::url_option(&opts, "load").and_then(|s| s.trim_end_matches('#').parse::<i32>().ok()) {
            Some(n) => p2game::save::request_load(n),
            None => eprintln!("travel with no map and no ?load: ignored ({opts})"),
        }
        return;
    }
    let t_travel = std::time::Instant::now();
    *TRAVEL_START.lock().unwrap_or_else(|p| p.into_inner()) = Some(t_travel);
    if !sim.game.travel(&map, &opts) {
        eprintln!("travel to {map} failed: map not found");
        return;
    }
    println!("load: travel boot {:.2} s", t_travel.elapsed().as_secs_f32());
    sim.game.drain_log(false);
    drop_level_entities(&mut commands, &level, &vm_rigs);
    sim.driven.clear();
    sim.rigs.clear();
    sim.reload = true;
    for f in sim.on_travel.iter_mut() {
        f(&mut sim.game);
    }
    println!("load: travel hooks done {:.2} s", t_travel.elapsed().as_secs_f32());
    if let Ok(mut w) = windows.single_mut() {
        w.title = "Postal 2 Rust/Bevy".to_string();
    }
}

/// Despawns the current level's render entities and the actor rigs spawned during it.
fn drop_level_entities(commands: &mut Commands, level: &Query<Entity, With<LevelEntity>>, vm_rigs: &Query<(Entity, &crate::rigs::VmRig)>) {
    for e in level {
        commands.entity(e).despawn();
    }
    // Actor rigs spawned after the level's first frame are not LevelEntity; they hold actor ids of
    // the old level (the object table was truncated, so those ids are now out of range or point at
    // new objects). Drop them with their joints and mesh sections.
    for (e, r) in vm_rigs {
        if level.contains(e) {
            continue;
        }
        for &x in r.joints.iter().chain(&r.sections) {
            if !level.contains(x) {
                if let Ok(mut ec) = commands.get_entity(x) {
                    ec.despawn();
                }
            }
        }
        commands.entity(e).despawn();
    }
}

fn load_scene(
    mut commands: Commands,
    args: Res<WinArgs>,
    mut sim: NonSendMut<Sim>,
    mut snap: ResMut<LevelSnapshot>,
    existing: Query<Entity>,
    bc: Option<Res<CompressedImageFormatSupport>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !sim.reload {
        return;
    }
    sim.reload = false;
    snap.before = Some(existing.iter().collect());
    let gpu_bc = bc.as_ref().is_some_and(|s| s.0.contains(CompressedImageFormats::BC));
    let lib = sim.game.host.lib.clone();
    // After a save load the level package is the save (its actors), but the geometry, terrain and
    // lightmaps are the original map's: the host's map package (the save loader picks it from the
    // save's level URL, as it does for the nav graph).
    let map = sim.game.host.map.borrow().clone().unwrap_or_else(|| sim.game.level.map.clone());
    lib.insert(map.clone());
    let t0 = std::time::Instant::now();
    let mut data = scene::build_scene(&lib, &map, &scene::Options { gpu_bc, projectors: false }).unwrap_or_else(|e| panic!("scene: {e}"));
    let t_build = t0.elapsed().as_secs_f32();
    let spawned = spawn::spawn_scene_with(&mut commands, &mut data, spawn::SpawnOptions { unlit: args.unlit, baked: true }, &mut images, &mut meshes, &mut materials);
    println!("load: scene built in {t_build:.2} s, spawned in {:.2} s ({:.2} s since boot)", t0.elapsed().as_secs_f32() - t_build, boot_secs());

    // Link render entities to VM actors by map export index. spawn_scene_with spawns one entity per
    // instance in instance order, so an actor's instances and entities pair up in order.
    // After a save load the actors came from the save package: pair them with the map's exports
    // by object name (saved actors keep their map names).
    let level_actors: Vec<(usize, ObjId)> = if std::sync::Arc::ptr_eq(&map, &sim.game.level.map) {
        sim.game.level.actors.clone()
    } else {
        let lm = &sim.game.level.map;
        let by_name: HashMap<String, ObjId> = sim.game.level.actors.iter().map(|&(e, o)| (lm.name(lm.exports[e].name).to_ascii_lowercase(), o)).collect();
        map.exports.iter().enumerate().filter_map(|(i, x)| by_name.get(&map.name(x.name).to_ascii_lowercase()).map(|&o| (i, o))).collect()
    };
    let by_export: HashMap<usize, ObjId> = level_actors.iter().map(|&(e, o)| (e, o)).collect();
    let mut inst_by_export: HashMap<usize, Vec<Mat4>> = HashMap::new();
    for i in &data.instances {
        inst_by_export.entry(i.export).or_default().push(i.transform);
    }
    let mut driven = Vec::new();
    let link = |export: usize, ents: &[Entity], driven: &mut Vec<Driven>| {
        let Some(&actor) = by_export.get(&export) else { return };
        let p = pose(&sim.game.world.vm, actor);
        let initial = placement(p.loc, p.rot, p.scale);
        let mats = inst_by_export.get(&export).cloned().unwrap_or_default();
        // Static mesh instances carry their transform; for other entities (movers) the spawned
        // Transform is captured on the first sync.
        let mut it = mats.into_iter();
        let tfs = ents.iter().map(|&e| (e, it.next().map(Affine3A::from_mat4))).collect();
        driven.push(Driven { actor, slots: PoseSlots::of(&sim.game.world.vm, actor), entities: tfs, inv_initial: initial.inverse(), last: p });
    };
    for (export, ents) in &spawned.actors {
        link(*export, ents, &mut driven);
    }
    let static_groups = driven.len();
    for (m, ents) in &spawned.movers {
        link(m.export, ents, &mut driven);
    }
    println!(
        "render: {} actor entity groups linked to VM actors ({static_groups} static mesh actors, {} movers; {} static mesh instances)",
        driven.len(),
        driven.len() - static_groups,
        data.instances.len()
    );
    sim.driven = driven;
    let mut rendered: std::collections::HashSet<ObjId> = spawned.actors.keys().chain(spawned.movers.iter().map(|(m, _)| &m.export)).filter_map(|e| by_export.get(e).copied()).collect();
    // Every actor the map saved a StaticMesh for is drawn by p2view (main world, sky zone or
    // terrain decoration); only class-default static meshes are left to dynmesh.
    for (e, o) in &level_actors {
        if upkg::begin_object(&map, *e).ok().is_some_and(|(_, p)| p.object("StaticMesh").is_some()) {
            rendered.insert(*o);
        }
    }
    let mut dm = std::mem::take(&mut sim.dynmesh);
    dm.level_loaded(&sim.game.world.vm, &rendered, sim.game.player_controller);
    dm.set_lighting(crate::dynmesh::ActorLighting::new(&map, &level_actors));
    println!("render: {} level actors with class-default static meshes drawn at runtime", dm.level_extra_count());
    if std::env::var_os("P2GAME_DYN_TRACE").is_some() {
        for (e, o) in &level_actors {
            let vm = &sim.game.world.vm;
            if map.name(map.exports[*e].name).to_ascii_lowercase().contains("shovel") {
                println!("DYN {} draw {:?} mesh {:?} hidden {:?} rendered {} loc {:?} rot {:?} scale {:?} phys {:?}", map.name(map.exports[*e].name), vm.get_prop(*o, "DrawType"), vm.get_prop(*o, "StaticMesh"), vm.get_prop(*o, "bHidden"), rendered.contains(o), vm.actor_loc(*o), vm.actor_rot(*o), vm.get_prop(*o, "DrawScale"), vm.get_prop(*o, "Physics"));
            }
        }
    }
    sim.dynmesh = dm;

    let (fog_color, fog_falloff) = spawned.fog.unwrap_or((Color::srgb(0.55, 0.68, 0.85), FogFalloff::Linear { start: 250.0, end: 1400.0 }));
    let (pos, dir) = if data.spawn_found { data.spawn } else { (Vec3::new(0.0, 10.0, 0.0), Vec3::X) };
    let yaw = (-dir.z).atan2(dir.x);
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { fov: 75f32.to_radians(), far: 1500.0, ..default() }),
        DistanceFog { color: fog_color, falloff: fog_falloff, ..default() },
        Transform::from_translation(pos + Vec3::Y * 1.2).with_rotation(cam_rot(yaw, 0.0)),
        FlyCam { yaw, pitch: 0.0 },
    ));
}

fn tick_script(time: Res<Time>, keys: Res<ButtonInput<KeyCode>>, ui: Res<crate::ui::UiInput>, mut sim: NonSendMut<Sim>) {
    // Debug pause (P), only for free-camera viewing: with a player, P is a game key (and typing
    // "packnheat" in the console must not freeze the world).
    if keys.just_pressed(KeyCode::KeyP) && !ui.captured && sim.game.player_controller.is_none() {
        sim.paused = !sim.paused;
        println!("script ticking {}", if sim.paused { "paused" } else { "resumed" });
    }
    if sim.paused {
        return;
    }
    // The engine sends TravelPostAccept at login, before the level's first tick; our window
    // front end sends it from its first frame. Hold the world until then, so level-start script
    // (AW intro cinematics unpossessing the player) runs after the day-start setup (#54).
    if !crate::frontend::travel_accepted(sim.game.level_serial, sim.game.player_controller) {
        return;
    }
    // Clamp long frames (asset upload stalls) the way the engine caps its tick delta.
    let dt = time.delta_secs().clamp(0.0, 0.2);
    sim.game.tick(dt);
    sim.game.drain_log(false);
    // SaveGame / LoadGame console commands (a load replaces the level: rebuild the scene).
    for l in p2game::save::process_requests(&mut sim.game) {
        println!("{l}");
        if l.starts_with("LoadGame") && !l.contains("failed") {
            sim.loaded_save = true;
        }
    }
    sim.frames += 1;
}

fn sync_transforms(mut sim: NonSendMut<Sim>, mut q: Query<(&mut Transform, &mut Visibility)>) {
    let _t = SysTimer::new("sync");
    let sim = &mut *sim;
    let vm = &sim.game.world.vm;
    let mut moved = 0;
    for d in sim.driven.iter_mut() {
        let now = if vm.objects[d.actor].deleted { Pose { hidden: true, ..d.last } } else { pose_with(vm, d.actor, &d.slots) };
        if now == d.last {
            continue;
        }
        let delta = placement(now.loc, now.rot, now.scale) * d.inv_initial;
        for (e, base) in d.entities.iter_mut() {
            if let Ok((mut tf, mut vis)) = q.get_mut(*e) {
                let b = *base.get_or_insert_with(|| Affine3A::from_mat4(tf.to_matrix()));
                *tf = Transform::from_matrix(Mat4::from(delta * b));
                *vis = if now.hidden { Visibility::Hidden } else { Visibility::Inherited };
            }
        }
        d.last = now;
        moved += 1;
    }
    sim.moved_total += moved;
}

/// Renders static mesh actors that script spawned at runtime (see `dynmesh`).
fn dyn_meshes(
    mut commands: Commands,
    mut sim: NonSendMut<Sim>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut q: Query<(&mut Transform, &mut Visibility)>,
) {
    let _t = SysTimer::new("dyn_meshes");
    let sim = &mut *sim;
    let lib = sim.game.host.lib.clone();
    crate::dynmesh::update(&mut commands, &lib, &sim.game.world.vm, sim.game.player_controller, &mut sim.dynmesh, &mut meshes, &mut materials, &mut images, &mut q);
}

/// Creates rigs for skeletal actors that appeared since the last frame (map pawns at load,
/// then anything script spawns).
#[allow(clippy::too_many_arguments)]
fn spawn_rigs(
    mut commands: Commands,
    mut sim: NonSendMut<Sim>,
    mut cache: ResMut<p2anim::rig::RigCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>,
) {
    let _t = SysTimer::new("spawn_rigs");
    let sim = &mut *sim;
    let mut assets = crate::rigs::rig_assets(&mut meshes, &mut materials, &mut images, &mut bindposes);
    let host = sim.game.host.clone();
    let n = crate::rigs::spawn_new_rigs(&mut commands, &host, &sim.game.world.vm, sim.game.player_controller, &mut sim.rigs, &mut cache, &mut assets);
    if n > 0 && sim.frames < 3 {
        println!("rigs: {n} skeletal actors spawned ({} total, {} failed) in {:.0} ms", sim.rigs.rigs.len(), sim.rigs.failed, _t.1.elapsed().as_secs_f64() * 1000.0);
    }
}

/// `--view-npc N`: once rigs exist, puts the free camera in front of an NPC, looking at it.
fn frame_npc(args: Res<WinArgs>, sim: NonSend<Sim>, mut done: Local<bool>, mut cam: Query<(&mut Transform, &mut FlyCam)>) {
    let Some(n) = args.view_npc else { return };
    if *done || sim.rigs.rigs.is_empty() {
        return;
    }
    let Some((loc, rot)) = crate::rigs::pick_npc(&sim.game.world.vm, &sim.rigs, n) else { return };
    let Ok((mut tf, mut fc)) = cam.single_mut() else { return };
    *done = true;
    let yaw = rot[1] as f32 * std::f32::consts::TAU / 65536.0;
    // 160 units in front of the pawn along its facing, at eye height, looking back at it.
    let eye = [loc[0] + yaw.cos() * 160.0, loc[1] + yaw.sin() * 160.0, loc[2] + 20.0];
    let target = p2view::scene::to_bevy(loc);
    tf.translation = p2view::scene::to_bevy(eye);
    let d = (target - tf.translation).normalize_or_zero();
    fc.yaw = (-d.z).atan2(d.x);
    fc.pitch = d.y.asin();
    tf.rotation = cam_rot(fc.yaw, fc.pitch);
    println!("camera framing NPC #{n} at ({:.0},{:.0},{:.0})", loc[0], loc[1], loc[2]);
}

fn pose_rigs(
    sim: NonSend<Sim>,
    rigs: Query<&crate::rigs::VmRig>,
    mut tfs: Query<&mut Transform, Without<crate::rigs::VmRig>>,
    mut vis: Query<&mut Visibility, Without<crate::rigs::VmRig>>,
) {
    let _t = SysTimer::new("pose_rigs");
    crate::rigs::pose_rigs(&sim.game.host, &sim.game.world.vm, &rigs, &mut tfs, &mut vis);
}

thread_local! {
    /// Milliseconds spent per p2game Bevy system since the last report.
    static SYS_TIMES: std::cell::RefCell<Vec<(&'static str, f64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Adds the lifetime of the guard to the named system's total (profile report).
pub(crate) struct SysTimer(&'static str, std::time::Instant);

impl SysTimer {
    pub(crate) fn new(name: &'static str) -> Self {
        SysTimer(name, std::time::Instant::now())
    }
}

impl Drop for SysTimer {
    fn drop(&mut self) {
        let ms = self.1.elapsed().as_secs_f64() * 1000.0;
        // `P2GAME_SPIKES=ms`: log single system runs over that many ms (warmup hitches).
        thread_local!(static SPIKE: Option<f64> = std::env::var("P2GAME_SPIKES").ok().and_then(|s| s.parse().ok()));
        if SPIKE.with(|s| s.is_some_and(|lim| ms > lim)) {
            println!("SPIKE {} {ms:.1} ms", self.0);
        }
        SYS_TIMES.with(|t| {
            let mut t = t.borrow_mut();
            match t.iter_mut().find(|e| e.0 == self.0) {
                Some(e) => e.1 += ms,
                None => t.push((self.0, ms)),
            }
        });
    }
}

pub(crate) fn cam_rot(yaw: f32, pitch: f32) -> Quat {
    Quat::from_rotation_y(yaw - std::f32::consts::FRAC_PI_2) * Quat::from_rotation_x(pitch)
}

fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut q: Query<(&mut Transform, &mut FlyCam)>,
    ui: Res<crate::ui::UiInput>,
) {
    // Menus own the mouse/keys, and in game the camera follows the player (ui::follow_player).
    if ui.captured || ui.game_look {
        return;
    }
    let Ok((mut tf, mut cam)) = q.single_mut() else { return };
    if buttons.pressed(MouseButton::Right) {
        cam.yaw -= motion.delta.x * 0.003;
        cam.pitch = (cam.pitch - motion.delta.y * 0.003).clamp(-1.54, 1.54);
        tf.rotation = cam_rot(cam.yaw, cam.pitch);
    }
    let (fwd, right) = (*tf.forward(), *tf.right());
    let mut v = Vec3::ZERO;
    for (k, d) in [(KeyCode::KeyW, fwd), (KeyCode::KeyS, -fwd), (KeyCode::KeyD, right), (KeyCode::KeyA, -right), (KeyCode::KeyE, Vec3::Y), (KeyCode::KeyQ, -Vec3::Y)] {
        if keys.pressed(k) {
            v += d;
        }
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) { 40.0 } else { 8.0 };
    tf.translation += v.normalize_or_zero() * speed * time.delta_secs();
}

#[allow(clippy::too_many_arguments)]
fn report(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut fr: ResMut<Frames>,
    sim: NonSend<Sim>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    materials: Res<Assets<StandardMaterial>>,
    entities: Query<Entity>,
    unowned: Query<(Entity, Option<&Name>, Has<Mesh3d>, Has<Node>, Has<ChildOf>), Without<LevelEntity>>,
    rq: (Query<&Camera>, Query<&ViewVisibility, With<Mesh3d>>, Query<(), Or<(With<PointLight>, With<SpotLight>)>>),
) {
    let (cams, mesh_q, lights_q) = rq;
    let std_mats = &materials;
    let now = time.elapsed_secs_f64();
    fr.wall_frames += 1;
    if fr.interactive {
        fr.dts.push(time.delta_secs() * 1000.0);
    }
    // Load time: the first of 10 consecutive frames under 50 ms after the scene exists counts as
    // the first interactive frame (GPU uploads and pipeline compiles stall the frames before it).
    if !fr.interactive && sim.frames > 0 {
        if std::env::var_os("P2GAME_BOOT_TIMING").is_some() {
            println!("load: frame {} took {:.1} ms ({:.2} s since boot)", sim.frames, time.delta_secs() * 1000.0, boot_secs());
        }
        if time.delta_secs() < 0.05 {
            fr.smooth += 1;
        } else {
            fr.smooth = 0;
        }
        if fr.smooth == 10 {
            fr.interactive = true;
            crate::splash::close();
            println!("load: first interactive frame {:.2} s after boot (script frame {})", boot_secs(), sim.frames);
        }
    }
    {
        let mut ts = TRAVEL_START.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = *ts {
            if time.delta_secs() < 0.05 {
                fr.smooth_travel += 1;
            } else {
                fr.smooth_travel = 0;
            }
            if fr.smooth_travel == 10 {
                println!("load: travel to first interactive frame {:.2} s", t.elapsed().as_secs_f32());
                *ts = None;
                fr.smooth_travel = 0;
            }
        }
    }
    if keys.just_pressed(KeyCode::F1) || now - fr.last_report > 5.0 {
        let span = now - fr.last_report;
        fr.last_report = now;
        if let Some((ws, peak, commit)) = process_memory_mb() {
            let tex_mb = images.iter().map(|(_, i)| i.data.as_ref().map_or(0, |d| d.len())).sum::<usize>() as f64 / 1048576.0;
            println!(
                "memory: working set {ws:.0} MB (peak {peak:.0}), commit {commit:.0} MB; assets: {} meshes, {} images ({tex_mb:.0} MB CPU-side), {} materials; {} entities",
                meshes.len(),
                images.len(),
                materials.len(),
                entities.iter().len()
            );
            if std::env::var_os("P2GAME_MEM_DETAIL").is_some() {
                let (mut mesh, mut node, mut child, mut other) = (0, 0, 0, 0);
                for (_, _, m, n, c) in &unowned {
                    if m {
                        mesh += 1;
                    } else if n {
                        node += 1;
                    } else if c {
                        child += 1;
                    } else {
                        other += 1;
                    }
                }
                println!("memory: non-level entities: {mesh} meshes, {node} ui nodes, {child} other children, {other} other roots");
            }
            if std::env::var_os("P2GAME_SPIKES").is_some() {
                let blend = std_mats.iter().filter(|(_, m)| matches!(m.alpha_mode, AlphaMode::Blend | AlphaMode::Add | AlphaMode::Multiply | AlphaMode::Premultiplied)).count();
                println!("render: cameras {} active {}, Mesh3d {} (visible {}), std materials {} (blended {}), point/spot lights {}", cams.iter().len(), cams.iter().filter(|c| c.is_active).count(), mesh_q.iter().len(), mesh_q.iter().filter(|v| v.get()).count(), std_mats.len(), blend, lights_q.iter().len());
            }
        }
        let vm = &sim.game.world.vm;
        println!(
            "script frame {}  live actors {}  transforms synced {}  missing-native calls {}  script errors {}  nav stuck {} deflect {}",
            sim.frames,
            sim.game.live_actors(),
            sim.moved_total,
            vm.missing_natives.values().sum::<u64>(),
            vm.script_errors.values().sum::<u64>(),
            p2game::physics::NAV_STUCK.with(|c| c.get()),
            p2game::physics::NAV_DEFLECT.with(|c| c.get())
        );
        // Frame time split: game phases (script, anim, each frame hook) vs the whole frame.
        let frame_ms = span * 1000.0 / fr.wall_frames.max(1) as f64;
        fr.wall_frames = 0;
        let phases = sim.game.take_frame_times();
        let game_ms: f64 = phases.iter().map(|p| p.1).sum();
        let list = phases.iter().map(|(n, ms)| format!("{n} {ms:.2}")).collect::<Vec<_>>().join("  ");
        let frames = (span * 1000.0 / frame_ms.max(0.001)).max(1.0);
        let sys = SYS_TIMES.with(|t| std::mem::take(&mut *t.borrow_mut()));
        let sys_ms: f64 = sys.iter().map(|s| s.1).sum::<f64>() / frames;
        let sys_list = sys.iter().map(|(n, ms)| format!("{n} {:.2}", ms / frames)).collect::<Vec<_>>().join("  ");
        println!(
            "frame {frame_ms:.1} ms ({:.0} fps): game {game_ms:.2} ms [{list}]  systems {sys_ms:.2} ms [{sys_list}]  render+other {:.1} ms",
            1000.0 / frame_ms.max(0.001),
            frame_ms - game_ms - sys_ms
        );
        // Frame-time distribution since the last report (smoothness / hitches).
        if !fr.dts.is_empty() {
            let mut d = std::mem::take(&mut fr.dts);
            d.sort_by(f32::total_cmp);
            let pct = |p: f32| d[((d.len() - 1) as f32 * p).round() as usize];
            let over = |ms: f32| d.iter().filter(|&&x| x > ms).count();
            println!("frametimes: n {} p50 {:.1} p95 {:.1} p99 {:.1} max {:.1} ms, over 25 ms {} over 33 ms {} over 50 ms {}", d.len(), pct(0.5), pct(0.95), pct(0.99), d[d.len() - 1], over(25.0), over(33.3), over(50.0));
        }
    }
}

fn capture(mut commands: Commands, args: Res<WinArgs>, mut fr: ResMut<Frames>, mut exit: MessageWriter<AppExit>, time: Res<Time>, sim: NonSend<Sim>, mut travelled: Local<bool>) {
    // `--travel-after SECS URL[,URL2,...]`: requests a level change every SECS seconds, through
    // the list in order (load/travel stress and leak test).
    if !*travelled {
        let a: Vec<String> = std::env::args().collect();
        *travelled = true;
        if let Some(i) = a.iter().position(|x| x == "--travel-after") {
            if let (Some(secs), Some(urls)) = (a.get(i + 1).and_then(|s| s.parse::<f32>().ok()), a.get(i + 2)) {
                let urls: Vec<&str> = urls.split(',').filter(|s| !s.is_empty()).collect();
                let k = fr.travels as usize;
                if k < urls.len() {
                    *travelled = false;
                    if time.elapsed_secs() > secs * (k + 1) as f32 {
                        fr.travels += 1;
                        sim.request_travel(urls[k]);
                    }
                }
            }
        }
    }
    // `--profile-secs N`: run N seconds (frame-time reports every 5 s), then quit.
    if let Some(secs) = std::env::args().skip_while(|a| a != "--profile-secs").nth(1).and_then(|s| s.parse::<f32>().ok()) {
        if time.elapsed_secs() > secs {
            exit.write(AppExit::Success);
        }
    }
    let Some(path) = &args.screenshot else { return };
    fr.n += 1;
    let started = *fr.start.get_or_insert_with(std::time::Instant::now);
    const WARMUP: u32 = 90;
    if fr.n == WARMUP && started.elapsed().as_secs_f32() < 5.0 {
        fr.n -= 1;
        return;
    }
    if fr.n == WARMUP {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
    }
    if fr.n > WARMUP + 15 && (path.exists() || fr.n > WARMUP + 600) {
        println!("screenshot {} ({})", path.display(), args.map);
        exit.write(AppExit::Success);
    }
}
