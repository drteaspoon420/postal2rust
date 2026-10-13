//! p2anim: viewer for POSTAL 2 characters, loading content from the user's own install.
//! Skinning runs on the GPU (Bevy `SkinnedMesh`); poses come from the UE2 animation runtime in
//! `p2anim::runtime` (channels, tweening, blending, bone controllers, attachments).
//!
//! Usage: p2anim [--game <dir>] (--class <PawnClass> | --mesh <name>) [--anim <sequence>]
//!               [--blend <sequence>[@<bone>[@<alpha>]]] [--weapon <WeaponClass>]
//!               [--class <WeaponClass> --fp [--anim Fire|Idle|...]]
//!               [--animset <Pkg.MeshAnimation>] [--skin <Pkg.Texture>] [--seed <n>]
//!               [--time <seconds>] [--rate <mult>] [--yaw <deg>] [--screenshot <file.png>] [--list]
//!
//! --blend plays a second sequence on channel 1 restricted to the bone subtree (default
//! "MALE01 spine1", the game's upper-body firing blend bone) at the given alpha (default 1).
//! --weapon attaches the weapon class's third-person attachment (AttachmentClass StaticMesh)
//! to the pawn's weapon bone ("MALE01 r hand") with its RelativeLocation/RelativeRotation.
//! --fp shows the weapon class's first-person mesh (Mesh default, hands included) from a
//! first-person camera, playing the weapon's sequences.

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use p2anim::character::{Appearance, ClassDb, PropStack, find_named, resolve_appearance};
use p2anim::rig::{BoneAttached, Character, RigAssets, RigCache, SpawnOpts, animate_characters, c_mat};
use p2anim::runtime::AnimEvent;
use std::path::PathBuf;
use std::sync::Arc;
use upkg::library::{Library, Obj};
use upkg::skeletal::{read_mesh_animation, read_skeletal_mesh};
use upkg::{ObjRef, Package};

const DEFAULT_GAME: &str = r"G:\SteamLibrary\steamapps\common\POSTAL2Complete";

#[derive(Resource, Clone)]
struct Args {
    game: PathBuf,
    mesh: Option<String>,
    class: Option<String>,
    anim: Option<String>,
    blend: Option<String>,
    weapon: Option<String>,
    fp: bool,
    animset: Option<String>,
    skin: Option<String>,
    seed: u64,
    time: Option<f32>,
    rate: f32,
    yaw: f32,
    pitch: f32,
    zoom: f32,
    screenshot: Option<PathBuf>,
    list: bool,
}

fn parse_args() -> Args {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let class = get("--class");
    Args {
        game: get("--game").map(PathBuf::from).or_else(|| std::env::var_os("POSTAL2_DIR").map(PathBuf::from)).unwrap_or_else(|| DEFAULT_GAME.into()),
        mesh: get("--mesh").or_else(|| class.is_none().then(|| "Avg_M_SS_Shorts_PB_D".into())),
        class,
        anim: get("--anim"),
        blend: get("--blend"),
        weapon: get("--weapon"),
        fp: a.iter().any(|x| x == "--fp"),
        animset: get("--animset"),
        skin: get("--skin"),
        seed: get("--seed").and_then(|s| s.parse().ok()).unwrap_or(1),
        time: get("--time").and_then(|s| s.parse().ok()),
        rate: get("--rate").and_then(|s| s.parse().ok()).unwrap_or(1.0),
        yaw: get("--yaw").and_then(|s| s.parse().ok()).unwrap_or(0.0),
        pitch: get("--pitch").and_then(|s| s.parse().ok()).unwrap_or(-7.0),
        zoom: get("--zoom").and_then(|s| s.parse().ok()).unwrap_or(1.0),
        screenshot: get("--screenshot").map(PathBuf::from),
        list: a.iter().any(|x| x == "--list"),
    }
}

/// All skeletal packages (.ukx) of the install, opened.
fn animation_packages(game: &std::path::Path) -> Vec<Arc<Package>> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(game.join("Animations")) else { return out };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ukx"))).collect();
    paths.sort();
    for p in paths {
        if let Ok(pkg) = Package::open(&p) {
            out.push(Arc::new(pkg));
        }
    }
    out
}

fn find_object(pkgs: &[Arc<Package>], name: &str, class: &str) -> Option<Obj> {
    let (want_pkg, leaf) = match name.rsplit_once('.') {
        Some((p, l)) => (Some(p.to_lowercase()), l),
        None => (None, name),
    };
    pkgs.iter().filter(|p| want_pkg.as_ref().is_none_or(|w| p.name == *w)).find_map(|p| p.find_export(leaf, Some(class)).map(|e| Obj { pkg: p.clone(), export: e }))
}

fn list(pkgs: &[Arc<Package>]) {
    for p in pkgs {
        for i in p.exports_of_class("SkeletalMesh").collect::<Vec<_>>() {
            match read_skeletal_mesh(p, i) {
                Ok(m) => println!(
                    "MESH {}.{} bones={} faces={} anim={}",
                    p.name,
                    m.name,
                    m.bones.len(),
                    m.lods.first().map_or(0, |l| l.faces.len()),
                    if m.animation == ObjRef::Null { "-".into() } else { p.full_path(m.animation) }
                ),
                Err(e) => println!("MESH {}.{} FAILED {e}", p.name, p.name(p.exports[i].name)),
            }
        }
        for i in p.exports_of_class("MeshAnimation").collect::<Vec<_>>() {
            if let Ok(a) = read_mesh_animation(p, i) {
                let names: Vec<String> = a.sequences.iter().map(|s| format!("{}({})", s.name, s.num_frames)).collect();
                println!("ANIM {}.{} bones={} seqs={}: {}", p.name, a.name, a.bones.len(), a.sequences.len(), names.join(" "));
            }
        }
    }
}

/// Third-person weapon attachment resolved from a weapon class.
#[derive(Clone)]
struct WeaponAttach {
    mesh: Obj,
    skins: Vec<Option<Obj>>,
    rel_location: [f32; 3],
    rel_rotation: [i32; 3],
    scale: f32,
}

#[derive(Resource)]
struct Setup {
    lib: Library,
    app: Appearance,
    animset: Option<Obj>,
    weapon: Option<WeaponAttach>,
    /// First-person: weapon class property stack.
    fp: Option<Arc<PropStack>>,
}

fn weapon_attachment(lib: &Library, db: &mut ClassDb, class: &str) -> Option<WeaponAttach> {
    let w = db.defaults_by_name(lib, class)?;
    let att_class = w.object(lib, "AttachmentClass")?;
    let att = db.defaults(lib, &att_class)?;
    println!("WEAPON {class} attachment {} chain: {}", att_class.name(), att.chain.join(" > "));
    let mesh = att.object(lib, "StaticMesh")?;
    Some(WeaponAttach {
        mesh,
        skins: att.object_array(lib, "Skins"),
        rel_location: att.vector("RelativeLocation").unwrap_or([0.0; 3]),
        rel_rotation: att.rotator("RelativeRotation").unwrap_or([0; 3]),
        scale: att.float("DrawScale").unwrap_or(1.0),
    })
}

fn main() {
    let args = parse_args();
    let pkgs = animation_packages(&args.game);
    if pkgs.is_empty() {
        panic!("no .ukx packages under {}", args.game.join("Animations").display());
    }
    if args.list {
        list(&pkgs);
        return;
    }
    let lib = Library::new(&args.game).unwrap_or_else(|e| panic!("cannot index game install at {}: {e}", args.game.display()));
    let mut db = ClassDb::default();
    let mut fp = None;
    let mut app = match (&args.class, args.fp) {
        (Some(c), true) => {
            // First-person weapon: the class's Mesh default is the FP mesh (arms + weapon).
            let stack = db.defaults_by_name(&lib, c).unwrap_or_else(|| panic!("class '{c}' not found"));
            println!("CLASS {} chain: {}", c, stack.chain.join(" > "));
            let body = stack.object(&lib, "Mesh").filter(|o| o.class().eq_ignore_ascii_case("SkeletalMesh")).unwrap_or_else(|| panic!("class '{c}' has no skeletal Mesh"));
            let skins = stack.object_array(&lib, "Skins");
            fp = Some(stack.clone());
            Appearance { class_name: c.clone(), body, body_skins: skins, head: None, anim_sets: vec![], draw_scale: [1.0; 3], body_code: None }
        }
        (Some(c), false) => {
            let stack = db.defaults_by_name(&lib, c).unwrap_or_else(|| panic!("class '{c}' not found"));
            println!("CLASS {} chain: {}", c, stack.chain.join(" > "));
            resolve_appearance(&lib, &stack, args.seed).unwrap_or_else(|| panic!("class '{c}' has no skeletal Mesh default"))
        }
        (None, _) => {
            let m = args.mesh.as_deref().unwrap();
            let body = find_object(&pkgs, m, "SkeletalMesh").unwrap_or_else(|| panic!("skeletal mesh '{m}' not found (try --list)"));
            Appearance { class_name: String::new(), body, body_skins: vec![], head: None, anim_sets: vec![], draw_scale: [1.0; 3], body_code: None }
        }
    };
    if let Some(s) = &args.skin {
        app.body_skins = vec![Some(find_named(&lib, s, None).unwrap_or_else(|| panic!("skin '{s}' not found")))];
    }
    let weapon = args.weapon.as_ref().map(|w| weapon_attachment(&lib, &mut db, w).unwrap_or_else(|| panic!("weapon class '{w}' has no static mesh attachment")));
    let animset = args.animset.as_ref().map(|n| find_object(&pkgs, n, "MeshAnimation").unwrap_or_else(|| panic!("animation set '{n}' not found")));
    let name = |o: &Obj| format!("{}.{}", o.pkg.name, o.name());
    println!(
        "APPEARANCE body={} skins=[{}] head={} head_skin={} weapon={}",
        name(&app.body),
        app.body_skins.iter().map(|s| s.as_ref().map(name).unwrap_or_else(|| "-".into())).collect::<Vec<_>>().join(", "),
        app.head.as_ref().map(|h| name(&h.mesh)).unwrap_or_else(|| "-".into()),
        app.head.as_ref().and_then(|h| h.skin.as_ref()).map(name).unwrap_or_else(|| "-".into()),
        weapon.as_ref().map(|w| name(&w.mesh)).unwrap_or_else(|| "-".into()),
    );
    let capture = args.screenshot.is_some();
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("p2anim - {}", args.class.clone().or(args.mesh.clone()).unwrap_or_default()),
                resolution: (1280u32, 960u32).into(),
                present_mode: if capture { bevy::window::PresentMode::AutoNoVsync } else { bevy::window::PresentMode::AutoVsync },
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.32, 0.36, 0.42)))
        .insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 600.0, ..default() })
        .insert_resource(args)
        .insert_resource(Setup { lib, app, animset, weapon, fp })
        .init_resource::<RigCache>()
        .init_resource::<Frames>()
        .add_plugins(PendingPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (animate_characters, log_events.after(animate_characters), orbit, capture_system))
        .run();
}

#[derive(Component)]
struct Orbit {
    target: Vec3,
    dist: f32,
    yaw: f32,
    pitch: f32,
}

#[derive(Resource, Default)]
struct Frames(u32);

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    args: Res<Args>,
    setup: Res<Setup>,
    mut cache: ResMut<RigCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let fp = setup.fp.is_some();
    let default_pref: &[&str] = if fp { &["Idle", "idle"] } else { &["s_base1", "s_idle_survey", "base", "stand"] };
    let seq_pref: Vec<&str> = match &args.anim {
        Some(s) => vec![s.as_str()],
        None => default_pref.to_vec(),
    };
    // With a fixed time the base channel is driven directly; otherwise it plays from the offset.
    let opts = SpawnOpts { seq_pref: &seq_pref, animset: setup.animset.clone(), time_offset: 0.0, rate: args.rate, fixed_time: args.time };
    let mut assets = RigAssets { meshes: &mut meshes, materials: &mut materials, images: &mut images, bindposes: &mut bindposes };
    let root = Mat4::from_scale(Vec3::from(setup.app.draw_scale));
    let ent = cache.spawn_character(&mut commands, &mut assets, &setup.lib, &setup.app, root, &opts).expect("cannot build character");

    // The character component was queued; build blend / attachment state on a local copy is not
    // possible, so configure through a one-shot system below using these parameters.
    commands.insert_resource(PendingSetup { ent, blend: args.blend.clone(), weapon: setup.weapon.clone() });

    // Frame the bind-pose body.
    let rig = cache.rig(&setup.lib, &mut assets, &setup.app.body, &setup.app.body_skins).unwrap();
    let m = c_mat() * root * rig.data.mesh_xform;
    let pts = &rig.data.mesh.lods[0].points;
    let (lo, mut hi) = pts.iter().map(|p| m.transform_point3(Vec3::from(*p))).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(p), hi.max(p)));
    if setup.app.head.is_some() {
        hi.y += (hi.y - lo.y) * 0.15;
    }
    commands.spawn((DirectionalLight { illuminance: 7000.0, shadow_maps_enabled: false, ..default() }, Transform::from_xyz(2.0, 4.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y)));
    if fp {
        // First-person view: Unreal weapon meshes are authored relative to the eye looking down +X.
        // PlayerViewOffset moves the weapon relative to the eye; the camera sits at the origin.
        let stack = setup.fp.as_ref().unwrap();
        let off = stack.vector("PlayerViewOffset").unwrap_or([0.0; 3]);
        let fov = stack.float("DisplayFOV").unwrap_or(70.0);
        let eye = c_mat().transform_point3(-Vec3::from(off));
        let fwd = c_mat().transform_vector3(Vec3::X).normalize();
        println!("FP view offset {off:?} fov {fov}");
        commands.spawn((
            Camera3d::default(),
            Projection::Perspective(PerspectiveProjection { fov: fov.to_radians() * 0.75, near: 0.01, ..default() }),
            Transform::from_translation(eye).looking_to(fwd, Vec3::Y),
        ));
        return;
    }
    let target = (lo + hi) * 0.5;
    let dist = (hi - lo).length() * 1.3 * args.zoom;
    commands.spawn((
        Mesh3d(assets.meshes.add(Plane3d::default().mesh().size(dist * 1.2, dist * 1.2))),
        MeshMaterial3d(assets.materials.add(StandardMaterial { base_color: Color::srgb(0.25, 0.27, 0.25), perceptual_roughness: 1.0, ..default() })),
        Transform::from_xyz(target.x, lo.y, target.z),
    ));
    let o = Orbit { target, dist, yaw: args.yaw.to_radians(), pitch: args.pitch.to_radians() };
    println!("FRAME bounds {lo:?}..{hi:?} camera dist {dist:.2}");
    commands.spawn((Camera3d::default(), orbit_transform(&o), o));
}

#[derive(Resource)]
struct PendingSetup {
    ent: Entity,
    blend: Option<String>,
    weapon: Option<WeaponAttach>,
}

/// Applies --blend and --weapon once the character entity exists.
#[allow(clippy::too_many_arguments)]
fn apply_pending(
    mut commands: Commands,
    pending: Option<Res<PendingSetup>>,
    setup: Res<Setup>,
    mut chars: Query<&mut Character>,
    mut cache: ResMut<RigCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let Some(p) = pending else { return };
    let Ok(mut ch) = chars.get_mut(p.ent) else { return };
    if let Some(b) = &p.blend {
        let mut it = b.split('@');
        let seq = it.next().unwrap_or_default();
        let bone = it.next().unwrap_or("MALE01 spine1");
        let alpha: f32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
        let ok = ch.body.inst.anim_blend_params(1, alpha, 0.0, 0.0, Some(bone), false) && ch.body.inst.loop_anim(seq, 1.0, 0.0, 1);
        println!("BLEND channel 1 seq={seq} bone={bone} alpha={alpha} ok={ok}");
    }
    if let Some(w) = &p.weapon {
        let mut assets = RigAssets { meshes: &mut meshes, materials: &mut materials, images: &mut images, bindposes: &mut bindposes };
        let ok = cache.attach_static_mesh(
            &mut commands,
            &mut assets,
            &setup.lib,
            p.ent,
            &mut ch,
            1,
            "MALE01 r hand",
            Vec3::from(w.rel_location),
            w.rel_rotation,
            &w.mesh,
            &w.skins,
            Vec3::splat(w.scale),
        );
        println!("ATTACH {} to 'MALE01 r hand' rel_loc={:?} rel_rot={:?} ok={ok}", w.mesh.name(), w.rel_location, w.rel_rotation);
    }
    commands.remove_resource::<PendingSetup>();
}

/// Prints animation events (AnimEnd, notifies) as they happen.
fn log_events(chars: Query<&Character>) {
    for ch in &chars {
        for e in &ch.events {
            match e {
                AnimEvent::AnimEnd { channel } => println!("EVENT AnimEnd({channel})"),
                AnimEvent::Notify { channel, sequence, notify } => println!("EVENT Notify ch={channel} seq={sequence} t={:.3} {:?}", notify.time, notify.kind),
            }
        }
    }
}

fn orbit_transform(o: &Orbit) -> Transform {
    let dir = Vec3::new(o.yaw.sin() * o.pitch.cos(), -o.pitch.sin(), o.yaw.cos() * o.pitch.cos());
    Transform::from_translation(o.target + dir * o.dist).looking_at(o.target, Vec3::Y)
}

fn orbit(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    scroll: Res<bevy::input::mouse::AccumulatedMouseScroll>,
    mut q: Query<(&mut Transform, &mut Orbit), (Without<Character>, Without<BoneAttached>)>,
) {
    let Ok((mut tf, mut o)) = q.single_mut() else { return };
    if buttons.pressed(MouseButton::Left) || buttons.pressed(MouseButton::Right) {
        o.yaw -= motion.delta.x * 0.005;
        o.pitch = (o.pitch + motion.delta.y * 0.005).clamp(-1.4, 1.4);
    }
    if keys.pressed(KeyCode::ArrowLeft) {
        o.yaw += 0.03;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        o.yaw -= 0.03;
    }
    o.dist = (o.dist * (1.0 - scroll.delta.y * 0.1)).clamp(0.3, 100.0);
    *tf = orbit_transform(&o);
}

fn capture_system(mut commands: Commands, args: Res<Args>, mut frames: ResMut<Frames>, mut exit: MessageWriter<AppExit>) {
    let Some(path) = &args.screenshot else { return };
    frames.0 += 1;
    const WARMUP: u32 = 120;
    if frames.0 == WARMUP {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path.clone()));
    }
    if frames.0 > WARMUP + 10 && (path.exists() || frames.0 > WARMUP + 600) {
        exit.write(AppExit::Success);
    }
}

/// Registers the deferred setup system (runs before animation so the first frame is correct).
pub struct PendingPlugin;

impl Plugin for PendingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, apply_pending.before(animate_characters));
    }
}
