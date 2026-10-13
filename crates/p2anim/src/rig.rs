//! Bevy side of characters: builds skinned meshes from decoded SkeletalMesh data (shared between
//! instances), spawns joint entities per instance, and animates them every frame with our own
//! keyframe sampler. Heads are separate skinned meshes attached to a body bone.
//!
//! All pose math is done in Unreal space (Z up, units) with standard column-vector matrices and
//! converted once per joint with C (swap Y/Z, scale to metres): a transform M becomes C*M*C^-1.

use crate::character::{Appearance, find_named};
use crate::notify::decode_anim_notifies;
use crate::runtime::{AnimEvent, AnimInstance, AnimSet, NoEvents, Skeleton};
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::collections::HashMap;
use std::sync::Arc;
use upkg::ObjRef;
use upkg::library::{Library, Obj};
use upkg::skeletal::{SkeletalMesh, read_mesh_animation, read_skeletal_mesh};
use upkg::texture::read_texture;

/// Unreal units per metre (same scale as p2view).
pub const UU_PER_M: f32 = 52.5;

/// Unreal space to Bevy space (swap Y/Z, scale to metres).
pub fn c_mat() -> Mat4 {
    Mat4::from_cols(Vec4::X / UU_PER_M, Vec4::Z / UU_PER_M, Vec4::Y / UU_PER_M, Vec4::W)
}

pub fn to_bevy(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[2], p[1]) / UU_PER_M
}

/// Unreal rotator (pitch, yaw, roll; 65536 = full turn) as a rotation matrix whose columns are the
/// rotated X, Y, Z axes.
pub fn rotator_mat(r: [i32; 3]) -> Mat4 {
    let k = std::f32::consts::TAU / 65536.0;
    let (sp, cp) = (r[0] as f32 * k).sin_cos();
    let (sy, cy) = (r[1] as f32 * k).sin_cos();
    let (sr, cr) = (r[2] as f32 * k).sin_cos();
    let x = Vec3::new(cp * cy, cp * sy, sp);
    let y = Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp);
    let z = Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp);
    Mat4::from_mat3(Mat3::from_cols(x, y, z))
}

/// Actor placement (Unreal space): translate, rotate, scale.
pub fn actor_mat(location: [f32; 3], rotation: [i32; 3], scale: [f32; 3]) -> Mat4 {
    Mat4::from_translation(Vec3::from(location)) * rotator_mat(rotation) * Mat4::from_scale(Vec3::from(scale))
}

/// Decoded mesh data needed per frame.
pub struct RigData {
    pub mesh: SkeletalMesh,
    pub skeleton: Arc<Skeleton>,
    /// Mesh space to actor space (LodMesh scale / origin / rotation).
    pub mesh_xform: Mat4,
}

impl RigData {
    pub fn bone(&self, name: &str) -> Option<usize> {
        self.skeleton.bone(name)
    }

    /// Bind-pose mesh-space bone matrices.
    pub fn bind_world(&self) -> Vec<Mat4> {
        let s = &self.skeleton;
        let mut w: Vec<Mat4> = Vec::with_capacity(s.len());
        for b in 0..s.len() {
            let l = s.ref_pose[b].mat();
            w.push(if b == 0 || s.parents[b] == b { l } else { w[s.parents[b]] * l });
        }
        w
    }
}

/// GPU resources for one (mesh, skins) combination, shared between instances.
pub struct RigAsset {
    pub data: Arc<RigData>,
    pub parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    pub inv_bind: Handle<SkinnedMeshInverseBindposes>,
}

pub struct Part {
    pub rig: Arc<RigData>,
    pub joints: Vec<Entity>,
    /// UE2 animation state (channels, controllers, attachments) for this mesh.
    pub inst: AnimInstance,
    /// Entities of the skinned mesh sections (for visibility toggles).
    pub sections: Vec<Entity>,
}

/// Attachment id used for a character's head on its body instance.
pub const HEAD_ATTACHMENT: u64 = u64::MAX;

/// An animated character: a body and optionally a head attached to one of the body's bones.
/// Gameplay code drives `body.inst` (PlayAnim, AnimBlendParams, SetBoneRotation, ...).
#[derive(Component)]
pub struct Character {
    /// Actor-to-world transform (Unreal space).
    pub root: Mat4,
    pub body: Part,
    /// Head part, attached to the body through [`HEAD_ATTACHMENT`].
    pub head: Option<Part>,
    /// Extra time scale for all channels.
    pub rate: f32,
    /// Freeze at this time (seconds) instead of ticking: the base channel is set to that time.
    pub fixed_time: Option<f32>,
    /// Events emitted by the last tick (AnimEnd, notifies), for hosts to consume.
    pub events: Vec<AnimEvent>,
}

/// A non-skeletal (or separately animated) actor attached to a character bone through
/// `AnimInstance::attach_to_bone`. Its Transform is updated every frame.
#[derive(Component)]
pub struct BoneAttached {
    pub character: Entity,
    pub id: u64,
    /// Extra scale applied after the attachment transform (e.g. DrawScale).
    pub scale: Vec3,
}

impl Character {
    /// World (Unreal space) bounds of the bind-pose body.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let m = self.root * self.body.rig.mesh_xform;
        let pts = self.body.rig.mesh.lods.first().map(|l| l.points.as_slice()).unwrap_or(&[]);
        pts.iter().map(|p| m.transform_point3(Vec3::from(*p))).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(p), hi.max(p)))
    }

    /// World (Unreal space) transform of an attachment.
    pub fn attachment_world(&mut self, id: u64) -> Option<Mat4> {
        Some(self.root * self.body.inst.attachment_transform(id)?)
    }
}

/// Ticks every character's animation runtime and writes joint and attachment transforms.
pub fn animate_characters(time: Res<Time>, mut chars: Query<(Entity, &mut Character)>, mut tfs: Query<&mut Transform, (Without<Character>, Without<BoneAttached>)>, mut attached: Query<(&BoneAttached, &mut Transform), Without<Character>>) {
    let c = c_mat();
    let ci = c.inverse();
    let dt = time.delta_secs();
    let mut write = |joints: &[Entity], pose: &[Mat4], to_world: Mat4| {
        for (e, m) in joints.iter().zip(pose) {
            if let Ok(mut tf) = tfs.get_mut(*e) {
                *tf = Transform::from_matrix(c * to_world * *m * ci);
            }
        }
    };
    let mut attach_world: HashMap<(Entity, u64), Mat4> = HashMap::new();
    for (ent, mut ch) in &mut chars {
        let ch = &mut *ch;
        ch.events.clear();
        match ch.fixed_time {
            Some(t) => {
                for part in std::iter::once(&mut ch.body).chain(ch.head.as_mut()) {
                    for c in 0..part.inst.channels.len() {
                        if let Some((l, s)) = part.inst.channels[c].seq.clone() {
                            let n = l.set.frames(s);
                            let r = l.set.anim.sequences[s].rate.max(1.0) / n;
                            let f = (t * r * ch.rate).rem_euclid(1.0);
                            part.inst.freeze_anim_at(f, c);
                        }
                    }
                    part.inst.evaluate();
                }
            }
            None => {
                ch.body.inst.tick(dt * ch.rate, &mut ch.events);
                if let Some(h) = ch.head.as_mut() {
                    h.inst.tick(dt * ch.rate, &mut NoEvents);
                }
            }
        }
        let body_world = ch.root * ch.body.rig.mesh_xform;
        write(&ch.body.joints, ch.body.inst.bone_matrices(), body_world);
        if let Some(head_actor) = ch.body.inst.attachment_transform(HEAD_ATTACHMENT) {
            if let Some(h) = ch.head.as_mut() {
                let to_world = ch.root * head_actor * h.rig.mesh_xform;
                write(&h.joints, h.inst.bone_matrices(), to_world);
            }
        }
        let ids: Vec<u64> = ch.body.inst.attachments().map(|(id, _)| *id).collect();
        for id in ids {
            if let Some(m) = ch.body.inst.attachment_transform(id) {
                attach_world.insert((ent, id), ch.root * m);
            }
        }
    }
    for (a, mut tf) in &mut attached {
        if let Some(m) = attach_world.get(&(a.character, a.id)) {
            *tf = Transform::from_matrix(c * *m * Mat4::from_scale(a.scale) * ci);
        }
    }
}

/// Alpha handling of a skin material as UE2 draws it: alpha-tested only when the material asks for
/// it (Shader OB_Masked / AlphaTest, Opacity = Diffuse, a bMasked texture, or a blending wrapper),
/// else opaque. Texture alpha alone is not coverage: xPatchTex.BaliShad (the butterfly
/// knife) uses BaliTex's alpha (under 128 everywhere) as its SpecularityMask, so a blanket alpha
/// test cut the whole knife away.
pub fn skin_alpha_mode(lib: &Library, mat: &Obj) -> AlphaMode {
    let Some(info) = lib.material_texture(mat) else { return AlphaMode::Mask(0.5) };
    let tex_masked = info.texture.as_ref().and_then(|t| read_texture(&t.pkg, t.export).ok()).is_some_and(|t| t.masked);
    // Blending wrappers keep the alpha test the rigs always used (no sorting for skinned parts).
    if info.blended || info.masked || tex_masked {
        AlphaMode::Mask(0.5)
    } else {
        AlphaMode::Opaque
    }
}

pub struct RigAssets<'a> {
    pub meshes: &'a mut Assets<Mesh>,
    pub materials: &'a mut Assets<StandardMaterial>,
    pub images: &'a mut Assets<Image>,
    pub bindposes: &'a mut Assets<SkinnedMeshInverseBindposes>,
}

/// Caches decoded meshes, animations and textures across spawns.
#[derive(Default, Resource)]
pub struct RigCache {
    rigs: HashMap<String, Option<Arc<RigAsset>>>,
    anims: HashMap<(String, usize), Option<Arc<AnimSet>>>,
    textures: HashMap<(String, usize), Option<Handle<Image>>>,
    pub stats: RigStats,
}

#[derive(Default, Debug, Clone)]
pub struct RigStats {
    pub rigs: usize,
    pub anims: usize,
    pub textures: usize,
}

pub struct SpawnOpts<'s> {
    /// Preferred sequence names for the looping base animation (channel 0), first match wins.
    /// Empty: no animation is started.
    pub seq_pref: &'s [&'s str],
    /// Extra animation set searched first (otherwise the body mesh's own set, then the class's).
    pub animset: Option<Obj>,
    /// Start offset in seconds for the base animation.
    pub time_offset: f32,
    pub rate: f32,
    pub fixed_time: Option<f32>,
}

impl RigCache {
    fn texture(&mut self, lib: &Library, images: &mut Assets<Image>, mat: &Obj) -> Option<Handle<Image>> {
        let key = mat.key();
        if let Some(h) = self.textures.get(&key) {
            return h.clone();
        }
        let h = lib.material_texture(mat).and_then(|mi| mi.texture).and_then(|t| {
            let tex = read_texture(&t.pkg, t.export).ok()?;
            let (w, h) = (tex.mips.first()?.width, tex.mips.first()?.height);
            let mut data = Vec::new();
            let mut levels = 0u32;
            let (mut ew, mut eh) = (w, h);
            for level in 0..tex.mips.len() {
                let m = &tex.mips[level];
                if m.width != ew || m.height != eh {
                    break;
                }
                let Ok((_, _, px)) = tex.decode_rgba(level) else { break };
                data.extend_from_slice(&px);
                levels += 1;
                if ew == 1 && eh == 1 {
                    break;
                }
                ew = (ew / 2).max(1);
                eh = (eh / 2).max(1);
            }
            if levels == 0 {
                return None;
            }
            self.stats.textures += 1;
            Some(images.add(make_image(w, h, levels, data)))
        });
        self.textures.insert(key, h.clone());
        h
    }

    /// Loads an animation set with its notifies (cached).
    pub fn anim_set(&mut self, lib: &Library, obj: &Obj) -> Option<Arc<AnimSet>> {
        let key = obj.key();
        if let Some(a) = self.anims.get(&key) {
            return a.clone();
        }
        let a = if obj.class().eq_ignore_ascii_case("MeshAnimation") {
            read_mesh_animation(&obj.pkg, obj.export).ok().map(|anim| {
                let notifies = decode_anim_notifies(Some(lib), &obj.pkg, &anim);
                Arc::new(AnimSet::new(Arc::new(anim), notifies))
            })
        } else {
            None
        };
        if a.is_some() {
            self.stats.anims += 1;
        }
        self.anims.insert(key, a.clone());
        a
    }

    /// Builds (or reuses) the GPU resources for a mesh with optional per-material skin overrides.
    pub fn rig(&mut self, lib: &Library, assets: &mut RigAssets, mesh_obj: &Obj, skins: &[Option<Obj>]) -> Option<Arc<RigAsset>> {
        let key = format!(
            "{}:{}|{}",
            mesh_obj.pkg.name,
            mesh_obj.export,
            skins.iter().map(|s| s.as_ref().map(|o| format!("{}:{}", o.pkg.name, o.export)).unwrap_or_default()).collect::<Vec<_>>().join(",")
        );
        if let Some(r) = self.rigs.get(&key) {
            return r.clone();
        }
        let built = self.build_rig(lib, assets, mesh_obj, skins).map(Arc::new);
        if built.is_some() {
            self.stats.rigs += 1;
        }
        self.rigs.insert(key, built.clone());
        built
    }

    fn build_rig(&mut self, lib: &Library, assets: &mut RigAssets, mesh_obj: &Obj, skins: &[Option<Obj>]) -> Option<RigAsset> {
        let mesh = read_skeletal_mesh(&mesh_obj.pkg, mesh_obj.export).ok()?;
        let lod = mesh.lods.first()?;
        if lod.faces.is_empty() {
            return None;
        }
        let skeleton = Arc::new(Skeleton::from_mesh(&mesh));
        let mesh_xform = skeleton.mesh_xform;
        let data = RigData { mesh, skeleton, mesh_xform };
        let bind_world = data.bind_world();
        let c = c_mat();
        let ci = c.inverse();
        let inv: Vec<Mat4> = bind_world.iter().map(|m| c * m.inverse() * ci).collect();
        let inv_bind = assets.bindposes.add(SkinnedMeshInverseBindposes::from(inv));

        let mesh = &data.mesh;
        let lod = &mesh.lods[0];
        // Per-point top-4 influences.
        let mut infl: Vec<Vec<(f32, u16)>> = vec![Vec::new(); lod.points.len()];
        for i in &lod.influences {
            infl[i.point as usize].push((i.weight, i.bone));
        }
        let (joint_idx, joint_w): (Vec<[u16; 4]>, Vec<[f32; 4]>) = infl
            .iter_mut()
            .map(|v| {
                v.sort_by(|a, b| b.0.total_cmp(&a.0));
                v.truncate(4);
                let sum: f32 = v.iter().map(|x| x.0).sum::<f32>().max(1e-6);
                let mut idx = [0u16; 4];
                let mut w = [0f32; 4];
                for (k, &(wt, b)) in v.iter().enumerate() {
                    idx[k] = b;
                    w[k] = wt / sum;
                }
                if v.is_empty() {
                    w[0] = 1.0;
                }
                (idx, w)
            })
            .unzip();

        let pos: Vec<Vec3> = lod.points.iter().map(|&p| to_bevy(p)).collect();
        let centroid = pos.iter().copied().sum::<Vec3>() / pos.len().max(1) as f32;
        // The Y/Z swap mirrors the mesh, so pick the winding that makes faces point outward.
        let mut outward = 0.0;
        for f in &lod.faces {
            let [a, b, cc] = f.wedges.map(|w| pos[lod.wedges[w as usize].point as usize]);
            outward += (b - a).cross(cc - a).dot((a + b + cc) / 3.0 - centroid);
        }
        let flip = outward < 0.0;
        let tri = |f: &upkg::skeletal::Face| if flip { [f.wedges[0], f.wedges[2], f.wedges[1]] } else { f.wedges };
        let mut pnorm = vec![Vec3::ZERO; pos.len()];
        for f in &lod.faces {
            let [a, b, cc] = tri(f).map(|w| lod.wedges[w as usize].point as usize);
            let n = (pos[b] - pos[a]).cross(pos[cc] - pos[a]);
            for p in [a, b, cc] {
                pnorm[p] += n;
            }
        }

        let nmat = lod.faces.iter().map(|f| f.material as usize + 1).max().unwrap_or(1);
        let mut parts = Vec::new();
        for mat in 0..nmat {
            let faces: Vec<&upkg::skeletal::Face> = lod.faces.iter().filter(|f| f.material as usize == mat).collect();
            if faces.is_empty() {
                continue;
            }
            let mut remap = vec![u32::MAX; lod.wedges.len()];
            let (mut p, mut n, mut uv, mut ji, mut jw, mut idx) = (vec![], vec![], vec![], vec![], vec![], vec![]);
            for f in &faces {
                for w in tri(f) {
                    let w = w as usize;
                    if remap[w] == u32::MAX {
                        remap[w] = p.len() as u32;
                        let we = lod.wedges[w];
                        let pt = we.point as usize;
                        p.push(pos[pt].to_array());
                        n.push(pnorm[pt].normalize_or_zero().to_array());
                        uv.push(we.uv);
                        ji.push(joint_idx[pt]);
                        jw.push(joint_w[pt]);
                    }
                    idx.push(remap[w]);
                }
            }
            let bevy_mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, p)
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, n)
                .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
                .with_inserted_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(ji))
                .with_inserted_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, jw)
                .with_inserted_indices(Indices::U32(idx));
            // Material: explicit skin, else the mesh's own Skins property, else its texture list.
            let mat_obj = skins.get(mat).cloned().flatten().or_else(|| {
                let r = mesh.props.object_at("Skins", mat as u32).filter(|o| *o != ObjRef::Null).or_else(|| mesh.textures.get(mat).copied())?;
                lib.resolve(&mesh_obj_pkg(lib, mesh_obj), r)
            });
            let tex = mat_obj.as_ref().and_then(|m| self.texture(lib, assets.images, m));
            let alpha_mode = mat_obj.as_ref().map(|m| skin_alpha_mode(lib, m)).unwrap_or(AlphaMode::Opaque);
            let material = assets.materials.add(StandardMaterial {
                base_color: if tex.is_some() { Color::WHITE } else { Color::srgb(0.7, 0.6, 0.5) },
                base_color_texture: tex,
                alpha_mode,
                perceptual_roughness: 0.9,
                reflectance: 0.1,
                ..default()
            });
            parts.push((assets.meshes.add(bevy_mesh), material));
        }
        Some(RigAsset { data: Arc::new(data), parts, inv_bind })
    }

    /// Spawns joints and skinned sections for a rig and creates its animation instance with every
    /// usable set linked (sets whose bones cover at least half the skeleton).
    pub fn spawn_part(&mut self, commands: &mut Commands, lib: &Library, rig: &RigAsset, anims: &[Obj]) -> Part {
        let joints: Vec<Entity> = (0..rig.data.mesh.bones.len()).map(|_| commands.spawn(Transform::IDENTITY).id()).collect();
        let mut sections = Vec::new();
        for (mesh, mat) in &rig.parts {
            sections.push(
                commands
                    .spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(mat.clone()),
                        SkinnedMesh { inverse_bindposes: rig.inv_bind.clone(), joints: joints.clone() },
                        NoFrustumCulling,
                        Transform::IDENTITY,
                    ))
                    .id(),
            );
        }
        let mut inst = AnimInstance::new(rig.data.skeleton.clone());
        for o in anims {
            let Some(set) = self.anim_set(lib, o) else { continue };
            let hits = set.anim.bones.iter().filter(|b| rig.data.bone(&b.name).is_some()).count();
            if hits * 2 >= rig.data.skeleton.len() && !set.anim.sequences.is_empty() {
                inst.add_anim_set(set);
            }
        }
        Part { rig: rig.data.clone(), joints, inst, sections }
    }

    /// Spawns a full character (body + head) and returns the entity carrying [`Character`].
    /// The base channel loops the first available sequence of `opts.seq_pref`.
    pub fn spawn_character(&mut self, commands: &mut Commands, assets: &mut RigAssets, lib: &Library, app: &Appearance, root: Mat4, opts: &SpawnOpts) -> Option<Entity> {
        let body_rig = self.rig(lib, assets, &app.body, &app.body_skins)?;
        let mut anims: Vec<Obj> = Vec::new();
        if let Some(a) = &opts.animset {
            anims.push(a.clone());
        }
        if let Some(a) = lib.resolve(&app.body.pkg, body_rig.data.mesh.animation) {
            anims.push(a);
        }
        anims.extend(app.anim_sets.iter().cloned());
        let mut body = self.spawn_part(commands, lib, &body_rig, &anims);
        if let Some(seq) = opts.seq_pref.iter().find(|s| body.inst.has_anim(s)) {
            body.inst.loop_anim(seq, 1.0, 0.0, 0);
            body.inst.tick(opts.time_offset, &mut NoEvents);
        } else if !opts.seq_pref.is_empty() {
            // Fall back to the first sequence of the primary set.
            let first = body.inst.anim_sets().next().map(|s| s.anim.sequences[0].name.clone());
            if let Some(first) = first {
                body.inst.loop_anim(&first, 1.0, 0.0, 0);
                body.inst.tick(opts.time_offset, &mut NoEvents);
            }
        }

        let head = app.head.as_ref().and_then(|h| {
            let rig = self.rig(lib, assets, &h.mesh, std::slice::from_ref(&h.skin))?;
            let rel = Mat4::from_translation(Vec3::from(h.offset)) * rotator_mat(h.rotation) * Mat4::from_scale(Vec3::from(h.scale));
            if !body.inst.attach_with_matrix(HEAD_ATTACHMENT, &h.bone, rel) {
                return None;
            }
            let head_anims: Vec<Obj> = lib.resolve(&h.mesh.pkg, rig.data.mesh.animation).into_iter().collect();
            let mut part = self.spawn_part(commands, lib, &rig, &head_anims);
            for s in ["Base", "base"] {
                if part.inst.loop_anim(s, 1.0, 0.0, 0) {
                    break;
                }
            }
            Some(part)
        });
        let ch = Character { root, body, head, rate: opts.rate, fixed_time: opts.fixed_time, events: Vec::new() };
        Some(commands.spawn(ch).id())
    }

    /// Spawns a skeletal mesh (e.g. a weapon) as its own animated "character" without a head.
    pub fn spawn_skeletal(&mut self, commands: &mut Commands, assets: &mut RigAssets, lib: &Library, mesh: &Obj, skins: &[Option<Obj>], anims: &[Obj], root: Mat4) -> Option<Entity> {
        let rig = self.rig(lib, assets, mesh, skins)?;
        let mut anims = anims.to_vec();
        if let Some(a) = lib.resolve(&mesh.pkg, rig.data.mesh.animation) {
            anims.push(a);
        }
        let body = self.spawn_part(commands, lib, &rig, &anims);
        Some(commands.spawn(Character { root, body, head: None, rate: 1.0, fixed_time: None, events: Vec::new() }).id())
    }

    /// Spawns a static mesh actor attached to a character bone (weapon attachments, hats, ...).
    /// `rel_location` / `rel_rotation` are the attachment's RelativeLocation / RelativeRotation.
    #[allow(clippy::too_many_arguments)]
    pub fn attach_static_mesh(
        &mut self,
        commands: &mut Commands,
        assets: &mut RigAssets,
        lib: &Library,
        character: Entity,
        ch: &mut Character,
        id: u64,
        bone: &str,
        rel_location: Vec3,
        rel_rotation: [i32; 3],
        mesh_obj: &Obj,
        skins: &[Option<Obj>],
        scale: Vec3,
    ) -> bool {
        if !ch.body.inst.attach_to_bone(id, bone, rel_location, rel_rotation) {
            return false;
        }
        let Ok(sm) = upkg::staticmesh::read_static_mesh(&mesh_obj.pkg, mesh_obj.export) else { return false };
        // Static mesh vertices are in actor units; convert per vertex with the same C as bones,
        // so the attachment Transform (C * M * C^-1) composes correctly.
        let pos: Vec<[f32; 3]> = sm.positions.iter().map(|p| to_bevy(*p).to_array()).collect();
        let nrm: Vec<[f32; 3]> = sm.normals.iter().map(|n| Vec3::new(n[0], n[2], n[1]).normalize_or_zero().to_array()).collect();
        let uvs = if sm.uvs.len() == pos.len() { sm.uvs.clone() } else { vec![[0.0; 2]; pos.len()] };
        if std::env::var_os("P2ANIM_DEBUG").is_some() {
            let w = ch.attachment_world(id);
            eprintln!("attach {} sections={} verts={} bounds={:?}..{:?} world={:?}", mesh_obj.name(), sm.sections.len(), pos.len(), sm.bounds_min, sm.bounds_max, w.map(|m| m.w_axis));
        }
        for (s, _) in sm.sections.iter().enumerate() {
            // The Y/Z swap mirrors geometry: reverse the winding.
            let idx: Vec<u32> = sm.section_indices(s).chunks(3).flat_map(|t| [t[0] as u32, t[2] as u32, t[1] as u32]).collect();
            if idx.is_empty() {
                continue;
            }
            let m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos.clone())
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm.clone())
                .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone())
                .with_inserted_indices(Indices::U32(idx));
            let mat_obj = skins.get(s).cloned().flatten().or_else(|| sm.materials.get(s).and_then(|r| lib.resolve(&mesh_obj.pkg, *r)));
            let tex = mat_obj.as_ref().and_then(|o| self.texture(lib, assets.images, o));
            // Weapon textures often keep specular masks in alpha: only alpha-test real masked materials.
            let masked = mat_obj.as_ref().and_then(|o| lib.material_texture(o)).is_some_and(|mi| mi.masked);
            let material = assets.materials.add(StandardMaterial {
                base_color: if tex.is_some() { Color::WHITE } else { Color::srgb(0.5, 0.5, 0.5) },
                base_color_texture: tex,
                alpha_mode: if masked { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
                double_sided: true,
                cull_mode: None,
                perceptual_roughness: 0.6,
                ..default()
            });
            commands.spawn((Mesh3d(assets.meshes.add(m)), MeshMaterial3d(material), Transform::IDENTITY, BoneAttached { character, id, scale }));
        }
        true
    }
}

/// The package a mesh's own references resolve against.
fn mesh_obj_pkg(_lib: &Library, o: &Obj) -> Arc<upkg::Package> {
    o.pkg.clone()
}

pub fn make_image(w: u32, h: u32, levels: u32, px: Vec<u8>) -> Image {
    let mut img = Image::new_uninit(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
    img.data = Some(px);
    img.texture_descriptor.mip_level_count = levels;
    // 16x anisotropic like the original's D3D renderer (Postal2.ini LevelOfAnisotropy=16).
    let mut sampler = ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    };
    sampler.set_anisotropic_filter(16);
    img.sampler = ImageSampler::Descriptor(sampler);
    img
}

/// Resolves "Package.Name" to an object, for command-line overrides.
pub fn named(lib: &Library, full: &str) -> Option<Obj> {
    find_named(lib, full, None)
}
