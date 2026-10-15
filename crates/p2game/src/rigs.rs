//! Windowed rendering of VM-driven skeletal actors.
//!
//! Every live actor with `DrawType == DT_Mesh` and a skeletal `Mesh` gets a GPU-skinned rig
//! (p2anim's `RigCache`, with the actor's `Skins`). Its joints are posed every frame from the
//! actor's `AnimInstance` in [`GameHost`] (the same instance the animation natives drive), placed
//! at the actor's Location/Rotation/DrawScale. Heads, weapons and other bone attachments are
//! separate actors that the host moves to their bones, so they follow automatically. Actors
//! spawned by script at runtime are picked up as they appear; destroyed or hidden ones are hidden.

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;
use p2anim::rig::{RigAssets, RigCache, c_mat};
use p2game::host::{GameHost, resolve_path, skel_actor_world};
use std::collections::{HashMap, HashSet};
use uscript::vm::{ObjId, Value, Vm};

/// `EDrawType::DT_Mesh`.
const DT_MESH: u8 = 2;

/// A rig posed from a VM actor's animation instance.
#[derive(Component)]
pub struct VmRig {
    pub actor: ObjId,
    pub joints: Vec<Entity>,
    pub sections: Vec<Entity>,
}

/// Signature of an actor's `Skins` array (resource paths), to notice script skin swaps
/// (burn victims, damage skins) without resolving materials every frame.
fn skins_sig(vm: &Vm, a: ObjId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(Value::Array(s)) = vm.get_prop(a, "Skins") {
        for v in s {
            v.as_resource().unwrap_or("").hash(&mut h);
        }
    }
    h.finish()
}

fn skins_of(vm: &Vm, host: &GameHost, a: ObjId) -> Vec<Option<upkg::library::Obj>> {
    match vm.get_prop(a, "Skins") {
        Some(Value::Array(s)) => s.iter().map(|v| v.as_resource().and_then(|p| resolve_path(&host.lib, p, None))).collect(),
        _ => Vec::new(),
    }
}

/// Every 15 frames: rigs whose actor's `Skins` changed get the new materials (same mesh and
/// skeleton), e.g. `SwapToBurnVictim` turning a corpse charred.
fn refresh_skins(host: &GameHost, vm: &Vm, tracker: &mut RigTracker, cache: &mut RigCache, assets: &mut RigAssets, mats: &mut Vec<(Entity, Handle<StandardMaterial>)>) {
    if tracker.frames % 15 != 7 {
        return;
    }
    let rigs: Vec<(ObjId, Entity)> = tracker.rigs.iter().map(|(&a, &e)| (a, e)).collect();
    for (a, e) in rigs {
        if vm.objects.get(a).is_none_or(|o| o.deleted) {
            continue;
        }
        let sig = skins_sig(vm, a);
        let old = tracker.skin_sigs.get(&a).copied();
        if old == Some(sig) {
            continue;
        }
        tracker.skin_sigs.insert(a, sig);
        if old.is_none() {
            continue;
        }
        let Some(mesh) = host.anims.borrow().get(&a).map(|x| x.mesh.clone()) else { continue };
        let skins = skins_of(vm, host, a);
        let Some(rig) = cache.rig(&host.lib, assets, &mesh.obj, &skins) else { continue };
        if let Some(sections) = tracker.sections.get(&e) {
            for (s, (_, mat)) in sections.iter().zip(rig.parts.iter()) {
                mats.push((*s, mat.clone()));
            }
        }
        if std::env::var_os("P2GAME_RIGS").is_some() {
            println!("rig {a}: skins changed, {} sections re-skinned", rig.parts.len());
        }
    }
}

#[derive(Default)]
pub struct RigTracker {
    /// Actors already handled (with or without a rig).
    seen: HashSet<ObjId>,
    pub rigs: HashMap<ObjId, Entity>,
    pub failed: usize,
    /// Highest actor id scanned so far; new actors get higher ids (the object table only grows
    /// within a level), so each frame only looks at the tail.
    scanned_max: Option<ObjId>,
    frames: u32,
    /// Last seen `Skins` signature per rigged actor (see [`refresh_skins`]).
    skin_sigs: HashMap<ObjId, u64>,
    /// Section entities per rig entity.
    sections: HashMap<Entity, Vec<Entity>>,
    /// Material swaps decided this frame (section entity, new material); applied by the caller.
    pub material_swaps: Vec<(Entity, Handle<StandardMaterial>)>,
}

impl RigTracker {
    pub fn clear(&mut self) {
        self.seen.clear();
        self.rigs.clear();
        self.failed = 0;
        self.scanned_max = None;
        self.skin_sigs.clear();
        self.sections.clear();
        self.material_swaps.clear();
    }
}

fn is_mesh_actor(vm: &Vm, a: ObjId) -> bool {
    // Skip first-person-only meshes (weapons, hands). The player's own `bOwnerNoSee` body still
    // gets a rig: the death/behind view shows it, and `ui::hide_own_body` hides it while the
    // camera looks through the pawn's eyes.
    let flag = |n: &str| vm.get_prop(a, n).is_some_and(|v| v.as_bool());
    if flag("bOnlyOwnerSee") {
        return false;
    }
    matches!(vm.get_prop(a, "DrawType"), Some(Value::Byte(DT_MESH))) && vm.get_prop(a, "Mesh").and_then(|v| v.as_resource()).is_some()
}

use p2game::ai::render_hidden;

/// A rigged non-player pawn to frame with the camera (`--view-npc`): (location, rotation).
pub fn pick_npc(vm: &Vm, tracker: &RigTracker, skip: usize) -> Option<([f32; 3], [i32; 3])> {
    let mut v: Vec<ObjId> = tracker
        .rigs
        .keys()
        .copied()
        .filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn") && vm.get_prop(a, "Controller").and_then(|c| c.as_obj()).is_some_and(|c| !vm.class_is_a(vm.objects[c].class, "PlayerController")))
        .collect();
    v.sort();
    let a = *v.get(skip.min(v.len().saturating_sub(1)))?;
    Some((vm.actor_loc(a), vm.actor_rot(a)))
}

/// Spawns rigs for mesh actors that do not have one yet.
pub fn spawn_new_rigs(
    commands: &mut Commands,
    host: &GameHost,
    vm: &Vm,
    _player: Option<ObjId>,
    tracker: &mut RigTracker,
    cache: &mut RigCache,
    assets: &mut RigAssets,
) -> usize {
    let mut n = 0;
    // Newly spawned actors have ids above everything seen; a periodic full pass catches actors
    // whose Mesh/DrawType script set later.
    tracker.frames = tracker.frames.wrapping_add(1);
    let full = tracker.frames % 60 == 1;
    let floor = if full { None } else { tracker.scanned_max };
    for &a in &vm.actors {
        if floor.is_some_and(|m| a <= m) {
            continue;
        }
        tracker.scanned_max = Some(tracker.scanned_max.map_or(a, |m| m.max(a)));
        if vm.objects[a].deleted || tracker.rigs.contains_key(&a) || (!full && tracker.seen.contains(&a)) {
            continue;
        }
        tracker.seen.insert(a);
        if !is_mesh_actor(vm, a) || !host.ensure(vm, a) {
            if std::env::var_os("P2GAME_RIGS").is_some() && vm.class_is_a(vm.objects[a].class, "Pawn") {
                println!(
                    "skip pawn {a} {}: mesh_actor={} ensure={} DrawType={:?} Mesh={:?}",
                    vm.classes[vm.objects[a].class].name,
                    is_mesh_actor(vm, a),
                    host.ensure(vm, a),
                    vm.get_prop(a, "DrawType"),
                    vm.get_prop(a, "Mesh")
                );
            }
            continue;
        }
        let Some(mesh) = host.anims.borrow().get(&a).map(|x| x.mesh.clone()) else { continue };
        let skins = skins_of(vm, host, a);
        let Some(rig) = cache.rig(&host.lib, assets, &mesh.obj, &skins) else {
            tracker.failed += 1;
            continue;
        };
        // The part's own AnimInstance is unused: the VM-side instance in GameHost drives the pose.
        let part = cache.spawn_part(commands, &host.lib, &rig, &[]);
        let sections = part.sections.clone();
        let e = commands.spawn((VmRig { actor: a, joints: part.joints, sections: part.sections }, Transform::IDENTITY, Visibility::default())).id();
        tracker.sections.insert(e, sections);
        tracker.skin_sigs.insert(a, skins_sig(vm, a));
        if std::env::var_os("P2GAME_RIGS").is_some() {
            let l = vm.actor_loc(a);
            let owner = vm.get_prop(a, "Owner").and_then(|v| v.as_obj()).map(|o| vm.classes[vm.objects[o].class].name.clone()).unwrap_or_default();
            println!(
                "rig {a}: {} mesh {} at ({:.0},{:.0},{:.0}) scale {:?} owner {owner} hidden {}",
                vm.classes[vm.objects[a].class].name,
                mesh.path,
                l[0],
                l[1],
                l[2],
                vm.get_prop(a, "DrawScale"),
                vm.get_prop(a, "bHidden").is_some_and(|v| v.as_bool())
            );
        }
        tracker.rigs.insert(a, e);
        n += 1;
    }
    let mut swaps = std::mem::take(&mut tracker.material_swaps);
    refresh_skins(host, vm, tracker, cache, assets, &mut swaps);
    for (s, mat) in swaps.drain(..) {
        if let Ok(mut ec) = commands.get_entity(s) {
            ec.insert(MeshMaterial3d(mat));
        }
    }
    tracker.material_swaps = swaps;
    n
}

/// Poses every rig from its actor's animation instance and placement.
pub fn pose_rigs(host: &GameHost, vm: &Vm, rigs: &Query<&VmRig>, tfs: &mut Query<&mut Transform, Without<VmRig>>, vis: &mut Query<&mut Visibility, Without<VmRig>>) {
    let c = c_mat();
    let ci = c.inverse();
    let mut anims = host.anims.borrow_mut();
    for r in rigs.iter() {
        let hidden = render_hidden(vm, r.actor);
        for &s in &r.sections {
            if let Ok(mut v) = vis.get_mut(s) {
                let want = if hidden { Visibility::Hidden } else { Visibility::Inherited };
                if *v != want {
                    *v = want;
                }
            }
        }
        if hidden {
            continue;
        }
        let Some(x) = anims.get_mut(&r.actor) else {
            // `P2GAME_RIG_TRACE=1`: a visible rig whose actor has no anim instance keeps its
            // last joint transforms (frozen in place).
            if std::env::var_os("P2GAME_RIG_TRACE").is_some() {
                println!("RIG frozen {} {} loc {:?} base {:?}", r.actor, vm.object_name(r.actor), vm.actor_loc(r.actor), vm.get_prop(r.actor, "Base").and_then(|v| v.as_obj()).map(|b| vm.object_name(b)));
            }
            continue;
        };
        let to_world = skel_actor_world(vm, r.actor, x.inst.skeleton.mesh_xform) * x.inst.skeleton.mesh_xform;
        for (e, m) in r.joints.iter().zip(x.inst.bone_matrices()) {
            if let Ok(mut tf) = tfs.get_mut(*e) {
                *tf = Transform::from_matrix(c * to_world * *m * ci);
            }
        }
    }
}

/// Bundles the asset collections p2anim's rig builder needs.
pub fn rig_assets<'a>(
    meshes: &'a mut Assets<Mesh>,
    materials: &'a mut Assets<StandardMaterial>,
    images: &'a mut Assets<Image>,
    bindposes: &'a mut Assets<SkinnedMeshInverseBindposes>,
) -> RigAssets<'a> {
    RigAssets { meshes, materials, images, bindposes }
}
