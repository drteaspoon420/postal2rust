//! Drawing script-driven systems ([`crate::vm::FxWorld`]) in Bevy.
//!
//! The game host owns the simulation on its own thread-local VM; each frame it hands a Send
//! snapshot ([`FxFrame`]) to the [`FxFrameQueue`] resource. [`FxVmPlugin`] then keeps one mesh
//! entity per (system, sub-emitter / mesh section) and one per decal, rebuilds their vertices for
//! the current camera, and despawns entities of systems or decals that are gone.

use crate::geom::{Batch, View, build_emitter_def, build_mesh_section, to_unreal};
use crate::vm::FxFrame;
use crate::{FxAssets, ParticleKey, ParticleMaterial, empty_mesh, fill_mesh};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use upkg::emitter::{DrawStyle, EmitterKind};

/// The latest frame from the game host (replace each frame).
#[derive(Resource, Clone, Default)]
pub struct FxFrameQueue(pub Arc<Mutex<Option<FxFrame>>>);

/// Render layer of the front end's first-person overlay (weapon) camera. Systems attached to
/// the held weapon (muzzle flashes) are drawn there, facing that camera, so they appear in front
/// of the weapon model like in the engine.
#[derive(Resource, Clone, Copy, Default)]
pub struct FxFirstPersonLayer(pub Option<usize>);

#[derive(Component)]
pub struct VmFxBatch;

/// (system id, sub-emitter, section) -> entity + mesh.
#[derive(Resource, Default)]
struct VmEntities {
    systems: HashMap<(u64, usize, usize), (Entity, Handle<Mesh>)>,
    decals: HashMap<u64, Entity>,
    last: Option<FxFrame>,
    /// First batch of entities spawned last frame. A new entity is not drawn on the frame it is
    /// spawned, so an effect that lives a single frame (pistol / shotgun muzzle flash, life
    /// 0.05 s) never reached the screen; its first batch is shown on the next frame instead,
    /// if the system has nothing newer to draw by then (FIDELITY #9, p10 shotgun cloud).
    fresh: HashMap<(u64, usize, usize), Batch>,
}

pub struct FxVmPlugin;

impl Plugin for FxVmPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FxFrameQueue>()
            .init_resource::<FxFirstPersonLayer>()
            .init_resource::<VmEntities>()
            .add_systems(PostUpdate, (apply_camera.before(TransformSystems::Propagate), draw_vm_frame.after(TransformSystems::Propagate)));
    }
}

/// A driver-requested camera (demos / screenshots) wins over the front end's camera systems.
fn apply_camera(queue: Res<FxFrameQueue>, ents: Res<VmEntities>, cams: crate::ViewCams, mut tfs: Query<&mut Transform, (With<Camera3d>, Without<VmFxBatch>)>) {
    let pending = queue.0.lock().unwrap_or_else(|p| p.into_inner()).as_ref().and_then(|f| f.camera);
    let Some((eye, target)) = pending.or_else(|| ents.last.as_ref().and_then(|f| f.camera)) else { return };
    let (e, t) = (crate::geom::to_bevy(Vec3::from(eye)), crate::geom::to_bevy(Vec3::from(target)));
    let Some((main, _)) = crate::main_view(&cams) else { return };
    if let Ok(mut tf) = tfs.get_mut(main) {
        *tf = Transform::from_translation(e).looking_at(t, Vec3::Y);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_vm_frame(
    mut commands: Commands,
    queue: Res<FxFrameQueue>,
    mut ents: ResMut<VmEntities>,
    cams: crate::ViewCams,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<ParticleMaterial>>,
    mut cache: ResMut<FxAssets>,
    mut vis: Query<&mut Visibility, With<VmFxBatch>>,
    fp_layer: Res<FxFirstPersonLayer>,
    all_cams: Query<(&GlobalTransform, &bevy::camera::visibility::RenderLayers), With<Camera3d>>,
) {
    if let Some(f) = queue.0.lock().unwrap_or_else(|p| p.into_inner()).take() {
        ents.last = Some(f);
    }
    let Some(frame) = ents.last.take() else { return };
    // Billboards face the main view: the active 3D camera with the lowest order (front ends may
    // add overlay cameras, e.g. the first-person weapon camera, which must not drive this).
    let Some((_, cam)) = crate::main_view(&cams) else {
        ents.last = Some(frame);
        return;
    };
    let (_, rot, pos) = cam.to_scale_rotation_translation();
    let to_u = |v: Vec3| Vec3::new(v.x, v.z, v.y);
    let view = View { location: to_unreal(pos), forward: to_u(rot * Vec3::NEG_Z), up: to_u(rot * Vec3::Y), right: to_u(rot * Vec3::X) };
    // First-person overlay view (if the front end has one).
    let fp = fp_layer.0.filter(|_| std::env::var_os("P2FX_NO_FP_LAYER").is_none()).and_then(|l| {
        let layer = bevy::camera::visibility::RenderLayers::layer(l);
        all_cams.iter().find(|(_, rl)| rl.intersects(&layer)).map(|(t, _)| {
            let (_, r, p) = t.to_scale_rotation_translation();
            (layer, View { location: to_unreal(p), forward: to_u(r * Vec3::NEG_Z), up: to_u(r * Vec3::Y), right: to_u(r * Vec3::X) })
        })
    });

    let mut alive: HashSet<(u64, usize, usize)> = HashSet::new();
    let mut batch = Batch::default();
    let debug = std::env::var_os("P2FX_DEBUG").is_some();
    let held = std::mem::take(&mut ents.fresh);
    for sys in &frame.systems {
        let (view, layer) = match (&fp, sys.first_person) {
            (Some((l, v)), true) => (*v, Some(l.clone())),
            _ => (view, None),
        };
        for (i, def) in sys.defs.iter().enumerate() {
            let st = &sys.state.emitters[i];
            let res = &sys.res[i];
            let sections: Vec<(usize, Option<usize>)> = match (&res.mesh, def.kind == EmitterKind::Mesh) {
                (Some(m), true) => (0..m.sections.len()).map(|s| (s, Some(s))).collect(),
                (None, true) => vec![],
                _ => vec![(0, None)],
            };
            for (s, section) in sections {
                let key = (sys.id, i, s);
                batch.clear();
                if !(st.disabled && st.live == 0) {
                    match (section, &res.mesh) {
                        (Some(s), Some(m)) => build_mesh_section(def, st, &sys.owner, m, s, &mut batch),
                        _ => build_emitter_def(def, st, &sys.owner, &view, &mut batch),
                    }
                }
                let created = !ents.systems.contains_key(&key);
                if batch.is_empty() {
                    if let Some(b) = held.get(&key) {
                        batch.clone_from(b);
                    }
                }
                alive.insert(key);
                if let Ok(tc) = std::env::var("P2FX_TRACE_CLASS") {
                    if sys.class.contains(tc.as_str()) {
                        let span = if batch.positions.len() >= 2 { (Vec3::from(batch.positions[0]) - Vec3::from(batch.positions[batch.positions.len() - 1])).length() } else { 0.0 };
                        let fwd = cam.forward();
                        let ahead: Vec<f32> = batch.positions.iter().map(|p| (Vec3::from(*p) - cam.translation()).dot(*fwd)).collect();
                        eprintln!("p2fx trace ahead(m)={ahead:?} uv={:?} idx={:?}", batch.uv0, batch.indices);
                        let es = &sys.state.emitters[i];
                        eprintln!(
                            "p2fx trace-state disabled={} active={} max={} pps={} ipps={} auto={} allow={} respawn={} kill={} init={} sys_t={:.2}",
                            es.disabled, es.active_particles, es.max_active, es.particles_per_second, es.initial_particles_per_second,
                            es.automatic_initial_spawning, es.allow_particle_spawn, es.respawn_dead_particles, es.kill_pending, es.initialized, sys.state.time
                        );
                        eprintln!("p2fx trace {} [{i}] new={} live={} verts={} color={:?} span(m)={span:.3} size={:?} owner={:?} first={:?} cam={:.1} fpview={:?} layer={:?} tex={:?} style={:?}", sys.class, !ents.systems.contains_key(&key), st.live, batch.positions.len(), batch.colors.first(), def.start_size_range.x, sys.owner.location, batch.positions.first(), cam.translation(), sys.first_person.then_some(view.location), layer.is_some(), res.texture.as_ref().map(|t| &t.name), def.draw_style);
                    }
                }
                if debug && !batch.positions.is_empty() {
                    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                    for p in &batch.positions {
                        lo = lo.min(Vec3::from(*p));
                        hi = hi.max(Vec3::from(*p));
                    }
                    let ext = hi - lo;
                    let near = batch.positions.iter().map(|p| (Vec3::from(*p) - cam.translation()).length()).fold(f32::MAX, f32::min);
                    if ext.max_element() > 3.0 || near < 1.0 {
                        eprintln!("p2fx vm batch {} [{i}] {:?} dir={} verts={} extent(m)={ext:.1} near(m)={near:.2} cam={:.1} live={}", sys.class, def.kind, def.use_direction_as, batch.positions.len(), cam.translation(), st.live);
                    }
                }
                let lines = def.kind == EmitterKind::Spark;
                let (entity, mesh) = ents
                    .systems
                    .entry(key)
                    .or_insert_with(|| {
                        let pk = ParticleKey { style: def.draw_style as u8, z_test: def.z_test, z_write: def.z_write };
                        let (tex, pk, alpha_test, repeat) = match (section, &res.mesh) {
                            (Some(s), Some(m)) => {
                                let sec = &m.sections[s];
                                let k = if def.use_mesh_blend_mode {
                                    ParticleKey { style: sec.style as u8, z_test: true, z_write: sec.style == DrawStyle::Regular }
                                } else {
                                    pk
                                };
                                (sec.texture.clone(), k, def.alpha_test || sec.masked, true)
                            }
                            _ => (res.texture.clone(), pk, def.alpha_test, def.kind == EmitterKind::Beam),
                        };
                        let mesh = meshes.add(empty_mesh(lines));
                        let mat = cache.material(&mut images, &mut mats, tex.as_ref(), repeat, pk, alpha_test, def.alpha_ref, i as i32);
                        // Visible from the first frame: one-frame effects (muzzle flashes) never get
                        // a second frame, and the visibility query below cannot see an entity whose
                        // spawn command has not been applied yet.
                        let vis0 = if batch.is_empty() { Visibility::Hidden } else { Visibility::Inherited };
                        let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), Transform::IDENTITY, vis0, NoFrustumCulling, NotShadowCaster, NotShadowReceiver, VmFxBatch));
                        if let Some(l) = &layer {
                            ec.insert(l.clone());
                        }
                        let e = ec.id();
                        (e, mesh)
                    })
                    .clone();
                let empty = batch.is_empty();
                if let Ok(mut v) = vis.get_mut(entity) {
                    let want = if empty { Visibility::Hidden } else { Visibility::Inherited };
                    if *v != want {
                        *v = want;
                    }
                }
                if !empty {
                    // A mesh spawned this frame is not yet in Assets until commands apply, but
                    // `meshes.add` inserted it synchronously, so get_mut works; re-insert if not.
                    let found = if let Some(mut m) = meshes.get_mut(mesh.id()) {
                        fill_mesh(&mut m, &batch);
                        true
                    } else {
                        false
                    };
                    if !found {
                        let mut m = empty_mesh(lines);
                        fill_mesh(&mut m, &batch);
                        let _ = meshes.insert(mesh.id(), m);
                    }
                    if created {
                        ents.fresh.insert(key, batch.clone());
                    }
                }
            }
        }
    }
    ents.systems.retain(|k, (e, _)| {
        let keep = alive.contains(k);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });

    // Decals: geometry is static, built once.
    let mut live_decals = HashSet::new();
    let no_decals = std::env::var_os("P2FX_NO_DECALS").is_some();
    for d in frame.decals.iter().filter(|_| !no_decals) {
        live_decals.insert(d.id);
        if ents.decals.contains_key(&d.id) {
            continue;
        }
        let mut m = empty_mesh(false);
        fill_mesh(&mut m, &d.batch);
        if debug {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for p in &d.batch.positions {
                lo = lo.min(Vec3::from(*p));
                hi = hi.max(Vec3::from(*p));
            }
            let bad_uv = d.batch.uv0.iter().filter(|u| !(u[0].is_finite() && u[1].is_finite()) || u[0].abs() > 4.0 || u[1].abs() > 4.0).count();
            eprintln!("p2fx draw decal {} verts={} idx={} lo={lo:.1} hi={hi:.1} bad_uv={bad_uv} cam={:.1}", d.id, d.batch.positions.len(), d.batch.indices.len(), cam.translation());
        }
        let mesh = meshes.add(m);
        let key = ParticleKey { style: d.style as u8, z_test: true, z_write: false };
        let mat = cache.material(&mut images, &mut mats, d.texture.as_ref(), false, key, false, 0, -50);
        let e = commands.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::IDENTITY, Visibility::Inherited, NoFrustumCulling, NotShadowCaster, NotShadowReceiver)).id();
        ents.decals.insert(d.id, e);
    }
    ents.decals.retain(|id, e| {
        let keep = live_decals.contains(id);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
    ents.last = Some(frame);
}
