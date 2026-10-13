//! SkeletalMesh and MeshAnimation decoding (UE2 package version 118, licensee 2).
//!
//! Layouts determined empirically from the shipped .ukx files: each field was located by walking
//! the native data after the tagged properties and checking that the counts agree with each other
//! (e.g. LOD face indices < wedge count, wedge vertex < point count, influence bone < bone count)
//! and that every object is consumed exactly to its end.

use crate::package::{ObjRef, Package};
use crate::props::{Properties, begin_object};
use crate::reader::Reader;
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    pub flags: u32,
    /// Local rotation relative to parent (x, y, z, w), as stored.
    pub rotation: [f32; 4],
    pub position: [f32; 3],
    pub length: f32,
    pub size: [f32; 3],
    pub num_children: u32,
    pub parent: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Influence {
    pub weight: f32,
    pub point: u16,
    pub bone: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Wedge {
    pub point: u16,
    pub uv: [f32; 2],
}

#[derive(Debug, Clone, Copy)]
pub struct Face {
    pub wedges: [u16; 3],
    pub material: u8,
    pub aux_material: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct SkinSection {
    pub material: u16,
    pub min_stream_index: u16,
    pub min_wedge: u16,
    pub max_wedge: u16,
    pub max_bones: u16,
    pub first_face: u16,
    pub num_faces: u16,
    pub extra: [u16; 2],
}

#[derive(Debug, Clone, Default)]
pub struct LodModel {
    /// Packed smooth-skinning stream (per dynamic wedge; see `unpack_skin_stream`).
    pub skin_stream: Vec<u32>,
    /// Skinning points (position, packed normal).
    pub skin_points: Vec<([f32; 3], u32)>,
    pub num_dyn_wedges: i32,
    pub smooth_sections: Vec<SkinSection>,
    pub rigid_sections: Vec<SkinSection>,
    pub smooth_indices: Vec<u16>,
    pub rigid_indices: Vec<u16>,
    pub influences: Vec<Influence>,
    pub wedges: Vec<Wedge>,
    pub faces: Vec<Face>,
    pub points: Vec<[f32; 3]>,
    pub display_factor: f32,
    pub hysteresis: f32,
    pub num_shared_verts: i32,
    pub max_influences: i32,
}

#[derive(Debug, Clone)]
pub struct SkeletalMesh {
    pub name: String,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    /// Mesh-to-actor transform from the LodMesh header.
    pub scale: [f32; 3],
    pub origin: [f32; 3],
    /// Pitch, yaw, roll (65536 = full turn).
    pub rotation: [i32; 3],
    /// Material per material index (from the `Textures` list).
    pub textures: Vec<ObjRef>,
    pub bones: Vec<Bone>,
    /// Default animation set (`MeshAnimation`) referenced natively.
    pub animation: ObjRef,
    pub skeletal_depth: i32,
    pub attach_aliases: Vec<String>,
    pub attach_bones: Vec<String>,
    pub lods: Vec<LodModel>,
    pub props: Properties,
}

fn bbox(r: &mut Reader) -> Result<([f32; 3], [f32; 3])> {
    let min = r.vec3()?;
    let max = r.vec3()?;
    let _valid = r.u8()?;
    Ok((min, max))
}

fn name_ref(pkg: &Package, r: &mut Reader) -> Result<String> {
    let i = r.compact()?;
    pkg.names.get(i as usize).cloned().ok_or(Error::Corrupt("name index"))
}

fn section(r: &mut Reader) -> Result<SkinSection> {
    let mut v = [0u16; 9];
    for x in &mut v {
        *x = r.u16()?;
    }
    Ok(SkinSection {
        material: v[0],
        min_stream_index: v[1],
        min_wedge: v[2],
        max_wedge: v[3],
        max_bones: v[4],
        extra: [v[5], v[6]],
        first_face: v[7],
        num_faces: v[8],
    })
}

fn read_lod(r: &mut Reader) -> Result<LodModel> {
    // Runtime skinning caches (dword blob, then point + packed normal per vertex).
    let skin_stream = r.array(|r| r.u32())?;
    let skin_points = r.array(|r| Ok((r.vec3()?, r.u32()?)))?;
    let num_dyn_wedges = r.i32()?;
    let smooth_sections = r.array(section)?;
    let rigid_sections = r.array(section)?;
    let smooth_indices = r.array(|r| r.u16())?;
    let _rev = r.i32()?;
    let rigid_indices = r.array(|r| r.u16())?;
    let _rev = r.i32()?;
    // Rigid vertex stream (32-byte vertices) + revision, stride, size.
    let _rigid_verts = r.array(|r| r.bytes(32).map(|_| ()))?;
    let _rev = r.i32()?;
    let _stride = r.i32()?;
    let _size = r.i32()?;
    let influences = r.lazy_array(|r| Ok(Influence { weight: r.f32()?, point: r.u16()?, bone: r.u16()? }))?;
    let wedges = r.lazy_array(|r| Ok(Wedge { point: r.u16()?, uv: [r.f32()?, r.f32()?] }))?;
    let faces = r.lazy_array(|r| Ok(Face { wedges: [r.u16()?, r.u16()?, r.u16()?], material: r.u8()?, aux_material: r.u8()? }))?;
    let points = r.lazy_array(|r| r.vec3())?;
    let display_factor = r.f32()?;
    let hysteresis = r.f32()?;
    let num_shared_verts = r.i32()?;
    let max_influences = r.i32()?;
    let _unk1 = r.i32()?;
    let _unk2 = r.i32()?;
    let mut lod = LodModel {
        skin_stream,
        skin_points,
        num_dyn_wedges,
        smooth_sections,
        rigid_sections,
        smooth_indices,
        rigid_indices,
        influences,
        wedges,
        faces,
        points,
        display_factor,
        hysteresis,
        num_shared_verts,
        max_influences,
    };
    lod.rebuild_stripped();
    Ok(lod)
}

/// One dynamic wedge decoded from the packed smooth-skin stream.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamWedge {
    pub point: u16,
    pub uv: [f32; 2],
    /// (bone, weight) for a new point; empty for a back-reference to an earlier point.
    pub influences: Vec<(u16, f32)>,
}

/// Decodes the packed smooth-skin stream (`SkinData`, LOD models of version 118): one entry per
/// dynamic wedge in index-buffer order, ending in 0xFFFFFFFF. An entry is a header word, extra
/// influence words, then U and V floats. Header: bits 0-11 bone * 6, bits 12-27 weight (65535 =
/// 1), bits 28-30 the number of extra influence words (same bone/weight layout), bit 31 set when
/// the point is kept in the shared-vertex cache. A header with the top nibble 0xF instead refers
/// back to cached point (bits 0-27) / 6. New points come in `Points` order.
/// Layout determined from meshes that ship both the stream and the plain wedge/influence lists.
pub fn unpack_skin_stream(stream: &[u32]) -> Option<Vec<StreamWedge>> {
    let mut out = Vec::new();
    let mut cache: Vec<u16> = Vec::new();
    let mut next_point: u32 = 0;
    let mut i = 0;
    let f = |w: u32| f32::from_bits(w);
    while i < stream.len() {
        let h = stream[i];
        if h == 0xFFFF_FFFF {
            return Some(out);
        }
        let infl = |w: u32| (((w & 0xFFF) / 6) as u16, ((w >> 12) & 0xFFFF) as f32 / 65535.0);
        if h >> 28 == 0xF {
            let p = *cache.get(((h & 0x0FFF_FFFF) / 6) as usize)?;
            let uv = [f(*stream.get(i + 1)?), f(*stream.get(i + 2)?)];
            out.push(StreamWedge { point: p, uv, influences: Vec::new() });
            i += 3;
            continue;
        }
        let extra = ((h >> 28) & 7) as usize;
        let mut influences = vec![infl(h)];
        for k in 0..extra {
            influences.push(infl(*stream.get(i + 1 + k)?));
        }
        let uv = [f(*stream.get(i + 1 + extra)?), f(*stream.get(i + 2 + extra)?)];
        let p = u16::try_from(next_point).ok()?;
        next_point += 1;
        if h & 0x8000_0000 != 0 {
            cache.push(p);
        }
        out.push(StreamWedge { point: p, uv, influences });
        i += 3 + extra;
    }
    None
}

impl LodModel {
    /// Some shipped meshes (the AW7/ED "_NEW" first-person weapons, P2R anims) were saved with
    /// the editable wedge/face/influence lists stripped, keeping only the render data: the smooth
    /// sections, their index buffer and the packed skin stream. Rebuild the plain lists from those.
    fn rebuild_stripped(&mut self) {
        if !self.faces.is_empty() || self.smooth_sections.is_empty() || !self.rigid_sections.is_empty() {
            return;
        }
        if self.points.is_empty() {
            self.points = self.skin_points.iter().map(|p| p.0).collect();
        }
        if self.wedges.is_empty() || self.influences.is_empty() {
            let Some(sw) = unpack_skin_stream(&self.skin_stream) else { return };
            if sw.iter().any(|w| w.point as usize >= self.points.len()) {
                return;
            }
            if self.wedges.is_empty() {
                self.wedges = sw.iter().map(|w| Wedge { point: w.point, uv: w.uv }).collect();
            }
            if self.influences.is_empty() {
                for w in &sw {
                    // Repeated bones in one entry are split weights: merge them.
                    let mut merged: Vec<(u16, f32)> = Vec::new();
                    for &(b, wt) in &w.influences {
                        match merged.iter_mut().find(|m| m.0 == b) {
                            Some(m) => m.1 += wt,
                            None => merged.push((b, wt)),
                        }
                    }
                    for (bone, weight) in merged {
                        self.influences.push(Influence { weight, point: w.point, bone });
                    }
                }
            }
        }
        let nw = self.wedges.len();
        let mut faces = Vec::new();
        for s in &self.smooth_sections {
            for f in s.first_face as usize..s.first_face as usize + s.num_faces as usize {
                let Some(ix) = self.smooth_indices.get(f * 3..f * 3 + 3) else { return };
                if ix.iter().any(|&w| w as usize >= nw) {
                    return;
                }
                faces.push(Face { wedges: [ix[0], ix[1], ix[2]], material: s.material as u8, aux_material: 0 });
            }
        }
        self.faces = faces;
    }
}

pub fn read_skeletal_mesh(pkg: &Package, export: usize) -> Result<SkeletalMesh> {
    read_skeletal_mesh_impl(pkg, export, false)
}

/// Like [`read_skeletal_mesh`] but prints the bytes that follow the LOD models (research aid).
pub fn trace(pkg: &Package, export: usize) -> Result<()> {
    read_skeletal_mesh_impl(pkg, export, true).map(|_| ())
}

fn read_skeletal_mesh_impl(pkg: &Package, export: usize, trace: bool) -> Result<SkeletalMesh> {
    let (mut r, props) = begin_object(pkg, export)?;
    let end = pkg.exports[export].offset + pkg.exports[export].size;
    // UPrimitive bounds.
    let (bounds_min, bounds_max) = bbox(&mut r)?;
    let _sphere = (r.vec3()?, r.f32()?);
    // ULodMesh header.
    let _version = r.i32()?;
    let _vertex_count = r.i32()?;
    let _verts = r.array(|r| r.u32())?;
    let textures = r.array(|r| r.compact().map(ObjRef::from_raw))?;
    let scale = r.vec3()?;
    let origin = r.vec3()?;
    let rotation = [r.i32()?, r.i32()?, r.i32()?];
    let _face_level = r.array(|r| r.u16())?;
    let _faces = r.array(|r| r.bytes(8).map(|_| ()))?;
    let _collapse_wedge = r.array(|r| r.u16())?;
    let _wedges = r.array(|r| r.bytes(10).map(|_| ()))?;
    let _materials = r.array(|r| Ok((r.u32()?, r.i32()?)))?;
    let _mesh_scale_max = r.f32()?;
    let _lod_strength = r.f32()?;
    let _lod_min_verts = r.i32()?;
    let _lod_morph = r.f32()?;
    let _lod_z_displace = r.f32()?;
    let _lod_hysteresis = r.f32()?;
    // USkeletalMesh.
    let _points2 = r.array(|r| r.vec3())?;
    let bones = r.array(|r| {
        Ok(Bone {
            name: name_ref(pkg, r)?,
            flags: r.u32()?,
            rotation: [r.f32()?, r.f32()?, r.f32()?, r.f32()?],
            position: r.vec3()?,
            length: r.f32()?,
            size: r.vec3()?,
            num_children: r.u32()?,
            parent: r.u32()?,
        })
    })?;
    let animation = ObjRef::from_raw(r.compact()?);
    let skeletal_depth = r.i32()?;
    let _weight_indices = r.array(|r| r.bytes(1).map(|_| ()))?;
    let _bone_influences = r.array(|r| r.bytes(1).map(|_| ()))?;
    let attach_aliases = r.array(|r| name_ref(pkg, r))?;
    let attach_bones = r.array(|r| name_ref(pkg, r))?;
    let _attach_coords = r.array(|r| r.bytes(48).map(|_| ()))?;
    let lods = r.array(read_lod)?;
    // Original (pre-LOD) import data kept for re-processing: unknown array (always empty in the
    // shipped files), points, wedges, triangles (with smoothing groups), influences, and the
    // per-wedge / per-face collapse tables.
    let unk_count = r.count()?;
    if unk_count != 0 {
        return Err(Error::Unsupported(format!("skeletal mesh raw-data prefix array of {unk_count}")));
    }
    let _raw_points = r.lazy_array(|r| r.vec3())?;
    let _raw_wedges = r.lazy_array(|r| r.bytes(10).map(|_| ()))?;
    let _raw_faces = r.lazy_array(|r| r.bytes(12).map(|_| ()))?;
    let _raw_influences = r.lazy_array(|r| r.bytes(8).map(|_| ()))?;
    let _raw_collapse_wedges = r.lazy_array(|r| r.u16())?;
    let _raw_face_levels = r.lazy_array(|r| r.u16())?;
    if r.pos() != end {
        return Err(Error::Corrupt("skeletal mesh not fully consumed"));
    }
    if trace {
        println!(
            "  raw: points={} wedges={} faces={} infl={} collapse={} facelevel={}",
            _raw_points.len(),
            _raw_wedges.len(),
            _raw_faces.len(),
            _raw_influences.len(),
            _raw_collapse_wedges.len(),
            _raw_face_levels.len()
        );
        println!("  bones={} lods={} tail={} bytes", bones.len(), lods.len(), end - r.pos());
        for (i, l) in lods.iter().enumerate() {
            println!(
                "   lod{i}: points={} wedges={} faces={} infl={} smooth={:?} rigid={} idx={} maxinfl={} shared={}",
                l.points.len(),
                l.wedges.len(),
                l.faces.len(),
                l.influences.len(),
                l.smooth_sections.iter().map(|s| (s.material, s.first_face, s.num_faces)).collect::<Vec<_>>(),
                l.rigid_sections.len(),
                l.smooth_indices.len(),
                l.max_influences,
                l.num_shared_verts
            );
        }
    }
    let mesh = SkeletalMesh {
        name: pkg.name(pkg.exports[export].name).to_string(),
        bounds_min,
        bounds_max,
        scale,
        origin,
        rotation,
        textures,
        bones,
        animation,
        skeletal_depth,
        attach_aliases,
        attach_bones,
        lods,
        props,
    };
    mesh.validate()?;
    Ok(mesh)
}

impl SkeletalMesh {
    fn validate(&self) -> Result<()> {
        let nb = self.bones.len();
        if nb == 0 {
            return Err(Error::Corrupt("skeletal mesh without bones"));
        }
        for (i, b) in self.bones.iter().enumerate() {
            if b.parent as usize >= nb || (i > 0 && b.parent as usize >= i) {
                return Err(Error::Corrupt("bone parent out of order"));
            }
        }
        for l in &self.lods {
            let (np, nw) = (l.points.len(), l.wedges.len());
            if l.wedges.iter().any(|w| w.point as usize >= np) {
                return Err(Error::Corrupt("wedge point out of range"));
            }
            if l.faces.iter().any(|f| f.wedges.iter().any(|&w| w as usize >= nw)) {
                return Err(Error::Corrupt("face wedge out of range"));
            }
            if l.influences.iter().any(|i| i.point as usize >= np || i.bone as usize >= nb) {
                return Err(Error::Corrupt("influence out of range"));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// MeshAnimation

/// Keyframes for one bone. Times are in frames from the start of the sequence; when a list has a
/// single key it holds for the whole sequence.
#[derive(Debug, Clone, Default)]
pub struct AnalogTrack {
    pub flags: u32,
    /// (x, y, z, w)
    pub rotations: Vec<[f32; 4]>,
    pub positions: Vec<[f32; 3]>,
    pub times: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct MotionChunk {
    pub root_speed: [f32; 3],
    /// Length of the chunk in frames.
    pub track_time: f32,
    pub start_bone: i32,
    pub flags: u32,
    pub bone_indices: Vec<i32>,
    /// One track per bone in [`MeshAnimation::bones`].
    pub tracks: Vec<AnalogTrack>,
    pub root_track: AnalogTrack,
}

#[derive(Debug, Clone)]
pub struct AnimNotify {
    pub time: f32,
    pub function: String,
    pub notify: ObjRef,
}

#[derive(Debug, Clone)]
pub struct AnimSeq {
    /// POSTAL 2 specific value stored before the name (unknown meaning; 0 for most sequences).
    pub extra: f32,
    pub name: String,
    pub groups: Vec<String>,
    pub start_frame: i32,
    pub num_frames: i32,
    pub notifies: Vec<AnimNotify>,
    /// Frames per second.
    pub rate: f32,
}

#[derive(Debug, Clone)]
pub struct AnimBone {
    pub name: String,
    pub flags: u32,
    pub parent: i32,
}

#[derive(Debug, Clone)]
pub struct MeshAnimation {
    pub name: String,
    pub version: i32,
    pub bones: Vec<AnimBone>,
    /// Parallel to `sequences`.
    pub moves: Vec<MotionChunk>,
    pub sequences: Vec<AnimSeq>,
    pub props: Properties,
}

fn analog_track(r: &mut Reader) -> Result<AnalogTrack> {
    Ok(AnalogTrack {
        flags: r.u32()?,
        rotations: r.array(|r| Ok([r.f32()?, r.f32()?, r.f32()?, r.f32()?]))?,
        positions: r.array(|r| r.vec3())?,
        times: r.array(|r| r.f32())?,
    })
}

pub fn read_mesh_animation(pkg: &Package, export: usize) -> Result<MeshAnimation> {
    let (mut r, props) = begin_object(pkg, export)?;
    let end = pkg.exports[export].offset + pkg.exports[export].size;
    let version = r.i32()?;
    let bones = r.array(|r| Ok(AnimBone { name: name_ref(pkg, r)?, flags: r.u32()?, parent: r.i32()? }))?;
    let moves = r.array(|r| {
        Ok(MotionChunk {
            root_speed: r.vec3()?,
            track_time: r.f32()?,
            start_bone: r.i32()?,
            flags: r.u32()?,
            bone_indices: r.array(|r| r.i32())?,
            tracks: r.array(analog_track)?,
            root_track: analog_track(r)?,
        })
    })?;
    let sequences = r.array(|r| {
        Ok(AnimSeq {
            extra: r.f32()?,
            name: name_ref(pkg, r)?,
            groups: r.array(|r| name_ref(pkg, r))?,
            start_frame: r.i32()?,
            num_frames: r.i32()?,
            notifies: r.array(|r| Ok(AnimNotify { time: r.f32()?, function: name_ref(pkg, r)?, notify: ObjRef::from_raw(r.compact()?) }))?,
            rate: r.f32()?,
        })
    })?;
    if r.pos() != end {
        return Err(Error::Corrupt("mesh animation not fully consumed"));
    }
    let anim = MeshAnimation { name: pkg.name(pkg.exports[export].name).to_string(), version, bones, moves, sequences, props };
    anim.validate()?;
    Ok(anim)
}

impl MeshAnimation {
    fn validate(&self) -> Result<()> {
        if self.moves.len() != self.sequences.len() {
            return Err(Error::Corrupt("motion chunk count != sequence count"));
        }
        for m in &self.moves {
            // Usually `bone_indices` is empty and track i animates bone i. Otherwise it maps each
            // track to a bone. A few re-imported sets have more tracks than bones; extra tracks
            // are ignored when sampling.
            if !m.bone_indices.is_empty() && m.tracks.len() != m.bone_indices.len() {
                return Err(Error::Corrupt("track count != bone index count"));
            }
            for t in &m.tracks {
                let n = t.times.len();
                if t.rotations.len() > n.max(1) || t.positions.len() > n.max(1) {
                    return Err(Error::Corrupt("track key count exceeds time count"));
                }
            }
        }
        Ok(())
    }

    pub fn find(&self, seq: &str) -> Option<usize> {
        self.sequences.iter().position(|s| s.name.eq_ignore_ascii_case(seq))
    }

    /// Local pose (rotation xyzw, position) of every animation bone for sequence `seq` at `frame`
    /// (in frames from the sequence start). Bones without a track keep the identity.
    pub fn sample_pose(&self, seq: usize, frame: f32, looping: bool) -> Vec<([f32; 4], [f32; 3])> {
        let mut pose = vec![([0.0, 0.0, 0.0, 1.0], [0.0; 3]); self.bones.len()];
        let Some(m) = self.moves.get(seq) else { return pose };
        let len = m.track_time.max(self.sequences[seq].num_frames as f32);
        for (i, t) in m.tracks.iter().enumerate() {
            let b = m.bone_indices.get(i).map_or(i, |&b| b as usize);
            if let Some(slot) = pose.get_mut(b) {
                *slot = t.sample(frame, len, looping);
            }
        }
        pose
    }
}

impl AnalogTrack {
    /// Samples the track at `frame` (0..track_time), interpolating linearly between keys and
    /// wrapping from the last key back to the first when `looping`. Returns (rotation, position).
    pub fn sample(&self, frame: f32, track_time: f32, looping: bool) -> ([f32; 4], [f32; 3]) {
        let rot = sample_keys(&self.rotations, &self.times, frame, track_time, looping, nlerp, [0.0, 0.0, 0.0, 1.0]);
        let pos = sample_keys(&self.positions, &self.times, frame, track_time, looping, lerp3, [0.0; 3]);
        (rot, pos)
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn nlerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    // Spherical interpolation along the shorter arc (falls back to normalized lerp for nearly
    // identical keys).
    let mut dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let s = if dot < 0.0 { -1.0 } else { 1.0 };
    dot *= s;
    let (wa, wb) = if dot > 0.9995 {
        (1.0 - t, t)
    } else {
        let theta = dot.clamp(-1.0, 1.0).acos();
        let sin = theta.sin();
        (((1.0 - t) * theta).sin() / sin, (t * theta).sin() / sin)
    };
    let mut q = [0.0; 4];
    for i in 0..4 {
        q[i] = a[i] * wa + b[i] * s * wb;
    }
    let len = (q.iter().map(|x| x * x).sum::<f32>()).sqrt().max(1e-12);
    q.map(|x| x / len)
}

fn sample_keys<T: Copy>(keys: &[T], times: &[f32], frame: f32, track_time: f32, looping: bool, mix: fn(T, T, f32) -> T, default: T) -> T {
    match keys.len() {
        0 => return default,
        1 => return keys[0],
        _ => {}
    }
    let n = keys.len().min(times.len());
    if n < 2 {
        return keys[0];
    }
    // Index of the last key at or before `frame`.
    let i = times[..n].partition_point(|&t| t <= frame).saturating_sub(1);
    if i + 1 < n {
        let (t0, t1) = (times[i], times[i + 1]);
        let f = if t1 > t0 { ((frame - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { 0.0 };
        mix(keys[i], keys[i + 1], f)
    } else if looping && track_time > times[n - 1] {
        let f = ((frame - times[n - 1]) / (track_time - times[n - 1])).clamp(0.0, 1.0);
        mix(keys[n - 1], keys[0], f)
    } else {
        keys[n - 1]
    }
}
