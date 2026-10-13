//! UE2 texture-coordinate animation for world materials: TexPanner, TexRotator and TexOscillator
//! wrappers (sky clouds, water, signs). The scene keeps the wrappers per material (MatData.uv_anim);
//! entities drawn with such a material carry [`UvAnim`], and [`animate_uvs`] rewrites the material's
//! `uv_transform` every frame (same maths as the canvas evaluator, p2canvas eval.rs).

use bevy::math::{Affine2, Mat2, Vec2};
use bevy::prelude::*;
use std::sync::Arc;
use upkg::material::{Material as UMat, OscillationType, RotationType};

/// The UV wrappers of an entity's material (outermost first) and its texture size in texels.
#[derive(Component, Clone)]
pub struct UvAnim(pub Arc<(Vec<UMat>, (f32, f32))>);

impl UvAnim {
    pub fn new(mods: &[UMat], size: (f32, f32)) -> Option<Self> {
        (!mods.is_empty() && std::env::var_os("P2VIEW_NO_UV_ANIM").is_none()).then(|| UvAnim(Arc::new((mods.to_vec(), size))))
    }

    /// UV transform at time `t` seconds (texture coordinates in 0..1 units).
    pub fn at(&self, t: f32) -> Affine2 {
        let (mods, (w, h)) = (&self.0.0, self.0.1);
        let (w, h) = (w.max(1.0), h.max(1.0));
        let urot = |u: i32| u as f32 / 65536.0 * std::f32::consts::TAU;
        let rotate_about = |a: f32, px: f32, py: f32| Affine2::from_translation(Vec2::new(px, py)) * Affine2::from_mat2(Mat2::from_angle(a)) * Affine2::from_translation(Vec2::new(-px, -py));
        let scale_about = |sx: f32, sy: f32, px: f32, py: f32| Affine2::from_translation(Vec2::new(px, py)) * Affine2::from_scale(Vec2::new(sx, sy)) * Affine2::from_translation(Vec2::new(-px, -py));
        // The outermost wrapper transforms the coordinates first.
        let mut out = Affine2::IDENTITY;
        for m in mods {
            let x = match m {
                UMat::TexPanner { pan_direction, pan_rate, .. } => {
                    let (p, y) = (urot(pan_direction[0]), urot(pan_direction[1]));
                    let (dx, dy) = (p.cos() * y.cos(), p.cos() * y.sin());
                    let (x, y) = (dx * pan_rate * t, dy * pan_rate * t);
                    Affine2::from_translation(Vec2::new(x - x.trunc(), y - y.trunc()))
                }
                UMat::TexRotator { kind, rotation, u_offset, v_offset, oscillation_rate, oscillation_amplitude, oscillation_phase, .. } => {
                    let yaw = match kind {
                        RotationType::Fixed => rotation[1] as f32,
                        RotationType::ConstantlyRotating => ((rotation[1] as f32 * t) as i64 & 0xFFFF) as f32,
                        RotationType::Oscillating => {
                            let f = oscillation_rate[1] as f32 * t / 65536.0;
                            let s = (std::f32::consts::TAU * ((f - f.trunc()) + oscillation_phase[1] as f32 / 65536.0)).sin();
                            rotation[1] as f32 + oscillation_amplitude[1] as f32 * s
                        }
                    };
                    rotate_about(yaw / 65536.0 * std::f32::consts::TAU, u_offset / w, v_offset / h)
                }
                UMat::TexOscillator { u_rate, v_rate, u_phase, v_phase, u_amplitude, v_amplitude, u_kind, v_kind, u_offset, v_offset, .. } => {
                    let wave = |rate: f32, phase: f32, kind: OscillationType| {
                        let k = if kind == OscillationType::StretchRepeat { 0.5 } else { 2.0 };
                        let f = rate * t;
                        (((f - f.trunc()) + phase * std::f32::consts::TAU) * k * std::f32::consts::PI).sin()
                    };
                    let su = wave(*u_rate, *u_phase, *u_kind) * u_amplitude;
                    let sv = wave(*v_rate, *v_phase, *v_kind) * v_amplitude;
                    let (mut sx, mut sy, mut tx, mut ty) = (1.0, 1.0, 0.0, 0.0);
                    match u_kind {
                        OscillationType::Pan => tx = su,
                        _ => sx = 1.0 + su,
                    }
                    match v_kind {
                        OscillationType::Pan => ty = sv,
                        _ => sy = 1.0 + sv,
                    }
                    Affine2::from_translation(Vec2::new(tx, ty)) * scale_about(sx, sy, u_offset / w, v_offset / h)
                }
                _ => Affine2::IDENTITY,
            };
            out = x * out;
        }
        out
    }
}

/// Animated texture (UE2 AnimNext frame chain: TV static, arcade screens): the frame images,
/// head first, and the playback rate in frames per second. [`animate_textures`] swaps the
/// material's base colour texture when the frame changes.
#[derive(Component, Clone)]
pub struct TexAnim(pub Arc<(Vec<Handle<Image>>, f32)>);

impl TexAnim {
    pub fn new(frames: &[u32], rate: f32, images: &[Handle<Image>]) -> Option<Self> {
        (frames.len() > 1 && rate > 0.0).then(|| TexAnim(Arc::new((frames.iter().map(|&f| images[f as usize].clone()).collect(), rate))))
    }

    /// Frame image at time `t` seconds.
    pub fn at(&self, t: f32) -> &Handle<Image> {
        let (frames, rate) = (&self.0.0, self.0.1);
        &frames[((t * rate) as usize) % frames.len()]
    }
}

/// Advances every animated texture (each material asset once per frame, written only when the
/// frame changes so unchanged materials are not re-prepared).
pub fn animate_textures(
    time: Res<Time>,
    std_q: Query<(&TexAnim, &MeshMaterial3d<StandardMaterial>)>,
    baked_q: Query<(&TexAnim, &MeshMaterial3d<crate::baked::BakedMaterial>)>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut baked_mats: ResMut<Assets<crate::baked::BakedMaterial>>,
    mut seen: Local<std::collections::HashSet<AssetId<StandardMaterial>>>,
    mut seen_b: Local<std::collections::HashSet<AssetId<crate::baked::BakedMaterial>>>,
) {
    let t = time.elapsed_secs_wrapped();
    seen.clear();
    seen_b.clear();
    for (a, m) in &std_q {
        let frame = a.at(t);
        if seen.insert(m.0.id()) && std_mats.get(&m.0).is_some_and(|mat| mat.base_color_texture.as_ref() != Some(frame)) {
            if let Some(mut mat) = std_mats.get_mut(&m.0) {
                mat.base_color_texture = Some(frame.clone());
            }
        }
    }
    for (a, m) in &baked_q {
        let frame = a.at(t);
        if seen_b.insert(m.0.id()) && baked_mats.get(&m.0).is_some_and(|mat| mat.base.base_color_texture.as_ref() != Some(frame)) {
            if let Some(mut mat) = baked_mats.get_mut(&m.0) {
                mat.base.base_color_texture = Some(frame.clone());
            }
        }
    }
}

/// Rewrites the uv_transform of every animated material (each material asset once per frame).
pub fn animate_uvs(
    time: Res<Time>,
    std_q: Query<(&UvAnim, &MeshMaterial3d<StandardMaterial>)>,
    baked_q: Query<(&UvAnim, &MeshMaterial3d<crate::baked::BakedMaterial>)>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut baked_mats: ResMut<Assets<crate::baked::BakedMaterial>>,
    mut seen: Local<std::collections::HashSet<AssetId<StandardMaterial>>>,
    mut seen_b: Local<std::collections::HashSet<AssetId<crate::baked::BakedMaterial>>>,
) {
    let t = time.elapsed_secs_wrapped();
    seen.clear();
    seen_b.clear();
    for (a, m) in &std_q {
        if seen.insert(m.0.id()) {
            if let Some(mut mat) = std_mats.get_mut(&m.0) {
                mat.uv_transform = a.at(t);
            }
        }
    }
    for (a, m) in &baked_q {
        if seen_b.insert(m.0.id()) {
            if let Some(mut mat) = baked_mats.get_mut(&m.0) {
                mat.base.uv_transform = a.at(t);
            }
        }
    }
}
