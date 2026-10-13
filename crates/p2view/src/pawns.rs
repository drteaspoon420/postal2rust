//! Characters placed in the map: every actor whose script class derives from `Pawn` and ends up
//! with a skeletal mesh is spawned as a GPU-skinned character (body + head) playing an idle
//! animation. Appearance comes from the class defaults (via `uscript`) overlaid with the
//! properties saved on the placed actor.

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;
use p2anim::character::{ClassDb, PropStack, resolve_appearance, seed_of};
use p2anim::rig::{RigAssets, RigCache, SpawnOpts, actor_mat};
use std::sync::Arc;
use upkg::library::{Library, Obj};
use upkg::{ObjRef, Package, Value, begin_object};

/// Idle sequences tried in order (POSTAL 2 humans, then animals and others).
const IDLE: &[&str] = &["s_base1", "s_idle_survey", "sf_base1", "stand", "Idle", "base", "cb_idle"];

#[derive(Default, Debug)]
pub struct PawnStats {
    pub placed: usize,
    pub spawned: usize,
    pub no_mesh: usize,
    pub failed: usize,
    pub with_head: usize,
}

/// Spawns all placed pawns of `map`. Returns statistics; prints failures by class.
pub fn spawn_pawns(commands: &mut Commands, lib: &Library, map: &Arc<Package>, assets: &mut RigAssets, cache: &mut RigCache) -> PawnStats {
    let mut db = ClassDb::default();
    let mut st = PawnStats::default();
    let mut failed_classes = std::collections::BTreeMap::<String, usize>::new();
    for i in 0..map.exports.len() {
        let class_ref = map.exports[i].class;
        if !matches!(class_ref, ObjRef::Import(_)) {
            continue;
        }
        let Some(class) = lib.resolve(map, class_ref) else { continue };
        if !class.class().eq_ignore_ascii_case("Class") {
            continue;
        }
        let Some(defaults) = db.defaults(lib, &class) else { continue };
        if !defaults.is_a("Pawn") {
            continue;
        }
        let Ok((_, props)) = begin_object(map, i) else { continue };
        st.placed += 1;
        if props.bool("bHidden") == Some(true) {
            continue;
        }
        // Placed actor properties override the class defaults.
        let mut stack = PropStack { layers: vec![(map.clone(), props.clone())], chain: defaults.chain.clone() };
        stack.layers.extend(defaults.layers.iter().cloned());
        let actor_name = map.name(map.exports[i].name).to_string();
        let Some(mut app) = resolve_appearance(lib, &stack, seed_of(&actor_name)) else {
            st.no_mesh += 1;
            continue;
        };
        // RepSkin records the body skin the level designer saw; prefer it when it fits the mesh.
        if let Some(Value::Object(r)) = props.get("RepSkin") {
            if let Some(skin) = lib.resolve(map, *r) {
                if skin_matches(&skin, &app.body) {
                    app.body_skins = vec![Some(skin)];
                }
            }
        }
        let loc = props.vector("Location").unwrap_or([0.0; 3]);
        let rot = props.rotator("Rotation").unwrap_or([0; 3]);
        // Pawns stand with their origin at the collision cylinder centre; meshes are authored
        // with their own origin offset (PrePivot is applied by the mesh transform).
        let pre = stack.vector("PrePivot").unwrap_or([0.0; 3]);
        let root = actor_mat([loc[0] + pre[0], loc[1] + pre[1], loc[2] + pre[2]], [0, rot[1], 0], app.draw_scale);
        let seed = seed_of(&actor_name);
        let opts = SpawnOpts { seq_pref: IDLE, animset: None, time_offset: (seed % 1000) as f32 * 0.01, rate: 1.0, fixed_time: None };
        match cache.spawn_character(commands, assets, lib, &app, root, &opts) {
            Some(_) => {
                st.spawned += 1;
                st.with_head += app.head.is_some() as usize;
                if std::env::var_os("P2VIEW_LIST_PAWNS").is_some() {
                    let b = p2anim::rig::to_bevy(loc);
                    let yaw = rot[1] as f32 * 360.0 / 65536.0;
                    println!("PAWN {actor_name} class={} at bevy=({:.1},{:.1},{:.1}) yaw={yaw:.0}", class.name(), b.x, b.y, b.z);
                }
            }
            None => {
                st.failed += 1;
                *failed_classes.entry(class.name().to_string()).or_default() += 1;
            }
        }
    }
    if !failed_classes.is_empty() {
        println!("PAWNS failed by class: {failed_classes:?}");
    }
    st
}

/// True if a chameleon skin name targets this mesh (or carries no mesh code at all).
fn skin_matches(skin: &Obj, mesh: &Obj) -> bool {
    match p2anim::character::SkinCode::parse(skin.name()) {
        Some(c) => c.mesh.eq_ignore_ascii_case(mesh.name()),
        None => true,
    }
}

/// Adds placed characters to the scene. main.rs only has to add this plugin.
pub struct PawnsPlugin {
    pub game: std::path::PathBuf,
    pub map: String,
}

#[derive(Resource, Clone)]
struct PawnSource {
    game: std::path::PathBuf,
    map: String,
}

impl Plugin for PawnsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(PawnSource { game: self.game.clone(), map: self.map.clone() })
            .init_resource::<RigCache>()
            .add_systems(Startup, load_pawns)
            .add_systems(Update, p2anim::rig::animate_characters);
    }
}

fn load_pawns(
    mut commands: Commands,
    src: Res<PawnSource>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut cache: ResMut<RigCache>,
) {
    let t0 = std::time::Instant::now();
    let Ok(lib) = Library::new(&src.game) else { return };
    let Some(path) = lib.map_path(&src.map) else { return };
    let Ok(map) = Package::open(&path) else { return };
    let map = Arc::new(map);
    lib.insert(map.clone());
    let mut assets = RigAssets { meshes: &mut meshes, materials: &mut materials, images: &mut images, bindposes: &mut bindposes };
    let st = spawn_pawns(&mut commands, &lib, &map, &mut assets, &mut cache);
    println!(
        "PAWNS placed={} spawned={} with_head={} no_mesh={} failed={} rigs={} anims={} textures={} in {} ms",
        st.placed,
        st.spawned,
        st.with_head,
        st.no_mesh,
        st.failed,
        cache.stats.rigs,
        cache.stats.anims,
        cache.stats.textures,
        t0.elapsed().as_millis()
    );
}
