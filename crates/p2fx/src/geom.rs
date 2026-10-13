//! CPU geometry for particles: billboards (with every UseDirectionAs mode), sub-UV frames and frame
//! blending, strips, spark trails, beams and mesh particles. Everything is built in Unreal space
//! and converted to Bevy space on output, so one vertex batch is produced per sub-emitter.

use crate::load::{FxEmitter, MeshData};
use crate::sim::{EmitterState, Owner, Particle, Rng, direction_rotator, rotator_matrix};
use bevy::math::{Mat3, Vec3};
use upkg::emitter::{EmitterKind, ParticleEmitter};

/// Unreal units per metre (same scale as p2view / p2anim).
pub const UU_PER_M: f32 = 52.5;

/// Unreal space to Bevy space (swap Y/Z, scale to metres).
#[inline]
pub fn to_bevy(p: Vec3) -> Vec3 {
    Vec3::new(p.x, p.z, p.y) / UU_PER_M
}

/// Bevy space to Unreal space.
#[inline]
pub fn to_unreal(p: Vec3) -> Vec3 {
    Vec3::new(p.x, p.z, p.y) * UU_PER_M
}

/// Bevy rotation matrix to the equivalent Unreal-space rotation.
pub fn rot_to_unreal(m: Mat3) -> Mat3 {
    let s = Mat3::from_cols(Vec3::X, Vec3::Z, Vec3::Y);
    s * m * s
}

/// Viewer in Unreal space.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub location: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
    pub right: Vec3,
}

/// Vertex arrays for one batch (Bevy space).
#[derive(Default, Clone, Debug)]
pub struct Batch {
    pub positions: Vec<[f32; 3]>,
    pub uv0: Vec<[f32; 2]>,
    pub uv1: Vec<[f32; 2]>,
    pub colors: Vec<[f32; 4]>,
    pub blend: Vec<f32>,
    pub indices: Vec<u32>,
}

impl Batch {
    pub fn clear(&mut self) {
        self.positions.clear();
        self.uv0.clear();
        self.uv1.clear();
        self.colors.clear();
        self.blend.clear();
        self.indices.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn push(&mut self, p: Vec3, uv0: [f32; 2], uv1: [f32; 2], color: [f32; 4], blend: f32) -> u32 {
        let i = self.positions.len() as u32;
        self.positions.push(to_bevy(p).to_array());
        self.uv0.push(uv0);
        self.uv1.push(uv1);
        self.colors.push(color);
        self.blend.push(blend);
        i
    }

    fn quad(&mut self, corners: [Vec3; 4], uv: [[f32; 2]; 4], uv_b: [[f32; 2]; 4], color: [f32; 4], blend: f32) {
        let b = self.push(corners[0], uv[0], uv_b[0], color, blend);
        for k in 1..4 {
            self.push(corners[k], uv[k], uv_b[k], color, blend);
        }
        self.indices.extend_from_slice(&[b, b + 1, b + 2, b + 2, b + 1, b + 3]);
    }
}

fn rgba(c: [u8; 4]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
}

fn safe_normal(v: Vec3) -> Vec3 {
    let l = v.length_squared();
    if l < 1e-8 { Vec3::ZERO } else { v / l.sqrt() }
}

/// A vector not parallel to `v`.
fn non_parallel(v: Vec3) -> Vec3 {
    if v.z.abs() < 1.0 - 1e-4 { Vec3::Z } else { Vec3::Y }
}

/// Rotates `v` by `angle` (Unreal units) about the unit `axis`.
fn rotate_angle_axis(v: Vec3, angle: f32, axis: Vec3) -> Vec3 {
    let a = angle * std::f32::consts::TAU / 65536.0;
    let (s, c) = a.sin_cos();
    v * c + axis.cross(v) * s + axis * axis.dot(v) * (1.0 - c)
}

/// Sub-image rectangle (u0, v0, u1, v1) for frame `f`. Frames advance down each column first
/// (the engine's `frame % VSubdivisions` picks the row and `frame / VSubdivisions` the column).
fn frame_rect(def: &ParticleEmitter, f: i32) -> [f32; 4] {
    let (u, v) = (def.texture_u_subdivisions.max(1), def.texture_v_subdivisions.max(1));
    let (du, dv) = (1.0 / u as f32, 1.0 / v as f32);
    let u0 = (f / v) as f32 * du;
    let v0 = (f % v) as f32 * dv;
    [u0, v0, (u0 + du).min(1.0), (v0 + dv).min(1.0)]
}

/// Current frame, next frame and blend factor for a particle.
pub fn frames(def: &ParticleEmitter, p: &Particle) -> ([f32; 4], [f32; 4], f32) {
    let full = [0.0, 0.0, 1.0, 1.0];
    if def.texture_u_subdivisions == 0 || def.texture_v_subdivisions == 0 {
        return (full, full, 0.0);
    }
    if p.subdivision >= 0 {
        let r = frame_rect(def, p.subdivision);
        return (r, r, 0.0);
    }
    if p.max_lifetime == 0.0 {
        let r = frame_rect(def, 0);
        return (r, r, 0.0);
    }
    let rel = p.relative_time();
    let total = def.texture_u_subdivisions * def.texture_v_subdivisions;
    let (count, start) = EmitterState::subdivision_span(def);
    let frame = if def.use_subdivision_scale {
        def.subdivision_scale.iter().position(|&s| rel <= s).unwrap_or(def.subdivision_scale.len()) as i32
    } else {
        (count as f32 * rel).floor() as i32 + start
    };
    let frame = frame.clamp(0, total - 1);
    let r0 = frame_rect(def, frame);
    if !def.blend_between_subdivisions || def.use_random_subdivision {
        return (r0, r0, 0.0);
    }
    let next = if frame + 1 != count + start { frame + 1 } else { frame };
    let r1 = frame_rect(def, next.min(total - 1));
    let blend = if def.use_subdivision_scale && (def.subdivision_scale.len() as i32) > count {
        let prev = if frame > 0 { def.subdivision_scale.get(frame as usize - 1).copied().unwrap_or(0.0) } else { 0.0 };
        let cur = def.subdivision_scale.get(frame as usize).copied().unwrap_or(1.0).min(1.0);
        if cur > prev { (rel - prev) / (cur - prev) } else { 0.0 }
    } else {
        let x = count as f32 * rel;
        x - x.floor()
    };
    (r0, r1, blend.clamp(0.0, 1.0))
}

fn rect_uvs(r: [f32; 4]) -> [[f32; 2]; 4] {
    // Corner order: -A-B, -A+B, +A-B, +A+B with A pointing "down" the texture and B to the right.
    [[r[0], r[1]], [r[2], r[1]], [r[0], r[3]], [r[2], r[3]]]
}

/// Particle location in world (Unreal) space.
fn world_loc(def: &ParticleEmitter, owner: &Owner, p: Vec3) -> Vec3 {
    if def.coordinate_system == 1 { owner.location + owner.rotation * p } else { p }
}

/// Builds the camera-facing / oriented quads of a sprite emitter.
pub fn build_sprites(def: &ParticleEmitter, st: &EmitterState, owner: &Owner, view: &View, out: &mut Batch) {
    let mut proj_normal = Vec3::from(def.projection_normal);
    if def.use_rotation_from == 1 {
        proj_normal = owner.rotation * proj_normal;
    }
    let relative = def.coordinate_system == 1;
    for p in st.particles.iter().take(st.active_particles) {
        if !p.alive() {
            continue;
        }
        let loc = world_loc(def, owner, p.location);
        let old = world_loc(def, owner, p.old_location);
        let mode = def.use_direction_as;
        // Sprites laid flat on a surface (fluid trails, puddles, ripples, splats) sit exactly on
        // the collision surface; the visible terrain/BSP is often a touch above it, so lift them
        // off along the projection normal to avoid being buried or z-fighting.
        let loc = if mode == 4 { loc + safe_normal(proj_normal) * surface_lift() } else { loc };
        let mut dir = Vec3::ZERO;
        let mut side = Vec3::ZERO;
        match mode {
            0 => {}
            4 => dir = proj_normal,
            _ => {
                dir = safe_normal(loc - old);
                if dir == Vec3::ZERO {
                    dir = safe_normal(if relative { owner.rotation * p.velocity } else { p.velocity });
                }
                if mode != 3 {
                    side = if mode == 5 || mode == 6 { dir.cross(proj_normal) } else { safe_normal(dir.cross(loc - view.location)) };
                }
            }
        }
        let size = p.size;
        // Billboard axes: `a` runs down the texture, `b` to the right.
        let (mut a, mut b, axis) = match mode {
            0 => (-view.up * size.x, view.right * size.x, view.forward),
            1 | 5 => (dir * size.y, side * size.x, view.forward),
            2 | 6 => (side * size.y, dir * size.x, view.forward),
            3 | 4 => {
                let a = safe_normal(non_parallel(dir).cross(dir)) * size.y;
                let b = safe_normal(dir.cross(a)) * size.x;
                (a, b, dir)
            }
            _ => (Vec3::ZERO, Vec3::ZERO, view.forward),
        };
        if def.spin_particles {
            let ang = p.start_spin.x + p.spins_per_second.x * p.time;
            a = rotate_angle_axis(a, ang, axis);
            b = rotate_angle_axis(b, ang, axis);
        }
        let (r0, r1, blend) = frames(def, p);
        let color = rgba(p.color);
        out.quad([loc - a - b, loc - a + b, loc + a - b, loc + a + b], rect_uvs(r0), rect_uvs(r1), color, blend);
    }
}

/// Builds a StripEmitter ribbon through its particles, newest first.
/// Lift (uu) for surface-projected sprites; `P2FX_SURFACE_LIFT` overrides for tuning.
fn surface_lift() -> f32 {
    static LIFT: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *LIFT.get_or_init(|| std::env::var("P2FX_SURFACE_LIFT").ok().and_then(|s| s.parse().ok()).unwrap_or(1.5))
}

pub fn build_strip(def: &ParticleEmitter, st: &EmitterState, owner: &Owner, view: &View, out: &mut Batch) {
    let pts: Vec<&Particle> = st.strip_order.iter().filter_map(|&i| st.particles.get(i)).filter(|p| p.alive()).collect();
    if pts.len() < 2 {
        return;
    }
    let locs: Vec<Vec3> = pts.iter().map(|p| world_loc(def, owner, p.location)).collect();
    let mut prev_left = None;
    for (k, p) in pts.iter().enumerate() {
        let here = locs[k];
        let along = if k + 1 < locs.len() { locs[k + 1] - here } else { here - locs[k - 1] };
        let n = if def.use_direction_as == 1 { Vec3::from(def.projection_normal) } else { here - view.location };
        let w = safe_normal(along.cross(n)) * p.size.x;
        let color = rgba(p.color);
        let v = k as f32 / (locs.len() - 1) as f32;
        let l = out.push(here - w, [0.0, v], [0.0, v], color, 0.0);
        out.push(here + w, [1.0, v], [1.0, v], color, 0.0);
        if let Some(pl) = prev_left {
            out.indices.extend_from_slice(&[pl, pl + 1, l, l, pl + 1, l + 1]);
        }
        prev_left = Some(l);
    }
}

/// Builds spark trails as line segments (index pairs; drawn with a line-list topology).
pub fn build_sparks(def: &ParticleEmitter, st: &EmitterState, owner: &Owner, out: &mut Batch) {
    let segs = def.line_segments_range.max.max(2.0) as usize;
    let accel = Vec3::from(def.acceleration);
    for (i, p) in st.particles.iter().enumerate().take(st.active_particles) {
        if !p.alive() {
            continue;
        }
        let Some(sd) = st.spark.get(i) else { continue };
        let age = p.time - p.spawn_time;
        if age < sd.time_before_visible {
            continue;
        }
        let step = if sd.time_between_segments > 0.0 { sd.time_between_segments } else { (age / segs as f32).max(1e-3) };
        let color = rgba(p.color);
        let mut prev: Option<u32> = None;
        for k in 0..=segs {
            let t = (age - k as f32 * step).max(sd.time_before_visible.max(0.0));
            let pos = world_loc(def, owner, sd.start_location + sd.start_velocity * t + accel * (0.5 * t * t));
            let fade = 1.0 - k as f32 / segs as f32;
            let c = [color[0] * fade, color[1] * fade, color[2] * fade, color[3] * fade];
            let idx = out.push(pos, [0.0, 0.0], [0.0, 0.0], c, 0.0);
            if let Some(pv) = prev {
                out.indices.extend_from_slice(&[pv, idx]);
            }
            prev = Some(idx);
            if t <= sd.time_before_visible {
                break;
            }
        }
    }
}

/// Beam end point for a particle (DetermineEndPointBy).
fn beam_end(def: &ParticleEmitter, p: &Particle, owner: &Owner, rng: &mut Rng) -> Vec3 {
    let start = world_loc(def, owner, p.start_location);
    match def.determine_end_point_by {
        1 => start + safe_normal(p.velocity) * def.beam_distance_range.center(),
        2 | 4 | 5 => {
            let off = def.beam_end_points.first().map(|e| rng.range_vector(&e.offset)).unwrap_or(Vec3::ZERO);
            if def.determine_end_point_by == 5 { off } else { start + off }
        }
        _ => world_loc(def, owner, p.location),
    }
}

/// Builds beams: camera-facing ribbons from each particle's start to its end point, displaced by
/// low-frequency noise (high-frequency noise is applied on top between those points).
pub fn build_beams(def: &ParticleEmitter, st: &EmitterState, owner: &Owner, view: &View, out: &mut Batch) {
    for (i, p) in st.particles.iter().enumerate().take(st.active_particles) {
        if !p.alive() {
            continue;
        }
        // Stable per-particle noise so beams do not flicker every frame.
        let mut rng = Rng(0x5eed ^ (i as u64) << 20 ^ (p.spawn_time.to_bits() as u64));
        let start = world_loc(def, owner, p.start_location);
        let end = beam_end(def, p, owner, &mut rng);
        let lf = def.low_frequency_points.max(2) as usize;
        let hf = def.high_frequency_points.max(0) as usize;
        let mut lf_pts: Vec<Vec3> = (0..lf).map(|k| start + (end - start) * (k as f32 / (lf - 1) as f32)).collect();
        for k in 1..lf - 1 {
            lf_pts[k] += rng.range_vector(&def.low_frequency_noise_range);
        }
        if def.noise_determines_end_point {
            let last = lf - 1;
            lf_pts[last] += rng.range_vector(&def.low_frequency_noise_range);
        }
        let mut pts = Vec::new();
        for k in 0..lf - 1 {
            let (a, b) = (lf_pts[k], lf_pts[k + 1]);
            let n = (hf / (lf - 1)).max(1);
            for j in 0..n {
                let t = j as f32 / n as f32;
                let mut q = a + (b - a) * t;
                if j > 0 {
                    q += rng.range_vector(&def.high_frequency_noise_range);
                }
                pts.push(q);
            }
        }
        pts.push(lf_pts[lf - 1]);
        let len = (end - start).length().max(1e-3);
        let color = rgba(p.color);
        let mut prev: Option<u32> = None;
        let mut dist = 0.0;
        for k in 0..pts.len() {
            if k > 0 {
                dist += (pts[k] - pts[k - 1]).length();
            }
            let along = if k + 1 < pts.len() { pts[k + 1] - pts[k] } else { pts[k] - pts[k - 1] };
            let w = safe_normal(along.cross(pts[k] - view.location)) * p.size.x;
            let v = dist / len * def.beam_texture_u_scale;
            let l = out.push(pts[k] - w, [0.0, v], [0.0, v], color, 0.0);
            out.push(pts[k] + w, [def.beam_texture_v_scale, v], [def.beam_texture_v_scale, v], color, 0.0);
            if let Some(pl) = prev {
                out.indices.extend_from_slice(&[pl, pl + 1, l, l, pl + 1, l + 1]);
            }
            prev = Some(l);
        }
    }
}

/// Transforms of mesh particles (location, rotation, scale) in Unreal space.
pub fn mesh_particle_transform(def: &ParticleEmitter, p: &Particle, owner: &Owner) -> (Vec3, Mat3, Vec3) {
    let loc = world_loc(def, owner, p.location);
    let rot = if def.spin_particles {
        // Yaw from X, pitch from Y, roll from Z.
        let r = [
            (p.spins_per_second.y * p.time + p.start_spin.y) as i32,
            (p.spins_per_second.x * p.time + p.start_spin.x) as i32,
            (p.spins_per_second.z * p.time + p.start_spin.z) as i32,
        ];
        rotator_matrix(r)
    } else if def.use_direction_as != 0 || def.use_rotation_from == 3 {
        rotator_matrix(direction_rotator(Vec3::from(def.rotation_normal)))
    } else {
        owner.rotation
    };
    let rot = if def.coordinate_system == 1 && def.spin_particles { owner.rotation * rot } else { rot };
    (loc, rot, p.size)
}

/// Builds the geometry of one static-mesh section for every live particle.
pub fn build_mesh_section(def: &ParticleEmitter, st: &EmitterState, owner: &Owner, mesh: &MeshData, section: usize, out: &mut Batch) {
    let Some(sec) = mesh.sections.get(section) else { return };
    let use_color = def.use_particle_color || !def.use_mesh_blend_mode;
    for p in st.particles.iter().take(st.active_particles) {
        if !p.alive() {
            continue;
        }
        let (loc, rot, scale) = mesh_particle_transform(def, p, owner);
        let color = if use_color { rgba(p.color) } else { [1.0; 4] };
        let base = out.positions.len() as u32;
        // Only the vertices this section uses are emitted (remapped).
        let mut remap = std::collections::HashMap::new();
        for &i in &sec.indices {
            let next = base + remap.len() as u32;
            let idx = *remap.entry(i).or_insert_with(|| {
                let v = Vec3::from(mesh.positions[i as usize]);
                let uv = mesh.uvs[i as usize];
                out.positions.push(to_bevy(loc + rot * (v * scale)).to_array());
                out.uv0.push(uv);
                out.uv1.push(uv);
                out.colors.push(color);
                out.blend.push(0.0);
                next
            });
            out.indices.push(idx);
        }
    }
}

/// Builds the batch for a non-mesh sub-emitter.
pub fn build_emitter(fx: &FxEmitter, st: &EmitterState, owner: &Owner, view: &View, out: &mut Batch) {
    build_emitter_def(&fx.data, st, owner, view, out);
}

/// Builds the batch for a non-mesh sub-emitter from its (possibly script-modified) definition.
pub fn build_emitter_def(def: &ParticleEmitter, st: &EmitterState, owner: &Owner, view: &View, out: &mut Batch) {
    out.clear();
    match def.kind {
        EmitterKind::Strip => build_strip(def, st, owner, view, out),
        EmitterKind::Spark => build_sparks(def, st, owner, out),
        EmitterKind::Beam => build_beams(def, st, owner, view, out),
        EmitterKind::Mesh => {}
        _ => build_sprites(def, st, owner, view, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::F_ALIVE;
    use upkg::emitter::{EmitterKind, ParticleEmitter};

    #[test]
    fn subdivision_frames_and_blend() {
        let mut d = ParticleEmitter::new(EmitterKind::Sprite);
        d.texture_u_subdivisions = 2;
        d.texture_v_subdivisions = 2;
        d.blend_between_subdivisions = true;
        let p = Particle { max_lifetime: 1.0, time: 0.375, subdivision: -1, flags: F_ALIVE, ..Default::default() };
        let (r0, r1, blend) = frames(&d, &p);
        // 4 frames, age 0.375 -> frame 1 (row 1 of column 0), blending half way to frame 2.
        assert_eq!(r0, [0.0, 0.5, 0.5, 1.0]);
        assert_eq!(r1, [0.5, 0.0, 1.0, 0.5]);
        assert!((blend - 0.5).abs() < 1e-5);
    }

    #[test]
    fn camera_facing_quad_is_twice_the_size() {
        let d = ParticleEmitter::new(EmitterKind::Sprite);
        let mut st = EmitterState::new(&d);
        st.particles = vec![Particle { size: Vec3::splat(10.0), flags: F_ALIVE, color: [255; 4], subdivision: -1, ..Default::default() }];
        st.active_particles = 1;
        let view = View { location: Vec3::new(-100.0, 0.0, 0.0), forward: Vec3::X, up: Vec3::Z, right: Vec3::Y };
        let mut b = Batch::default();
        build_sprites(&d, &st, &Owner::default(), &view, &mut b);
        assert_eq!(b.positions.len(), 4);
        let p0 = to_unreal(Vec3::from(b.positions[0]));
        let p3 = to_unreal(Vec3::from(b.positions[3]));
        assert!(((p3 - p0).length() - (2.0f32 * 20.0 * 20.0).sqrt()).abs() < 1e-3);
    }

    #[test]
    fn space_conversion_round_trip() {
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert!((to_unreal(to_bevy(p)) - p).length() < 1e-5);
    }
}
