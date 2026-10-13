//! Rebuilding BSP lightmaps from the editor's saved light visibility, as UE2 does at load for
//! lightmap atlases saved without texel data (FLightMap::GetTextureData): every texel starts at
//! the surface zone's ambient, then each static light listed for the lightmap adds its
//! contribution where its visibility bit is set (sunlight: 2 * max(0, -L.N); point and spot
//! lights: the UE2 falloff 2 * (2f^3 - 3f^2 + 1) * N.L). Values are modulate-2x bytes
//! (128 = 1.0), the same as a saved atlas.

use crate::ObjRef;
use crate::lighting::{LightKind, MapLight, hsv_to_rgb};
use crate::model::{LightMap, Model};

/// Scale on the light term (colour * brightness/255 * this * intensity). Calibrated against
/// the saved atlases of suburbs-3, Industry, police and Church (examples/lm_rebuild_probe.rs:
/// per-lightmap mean 45.6/52.7/40.0/46.6 rebuilt vs 46.0/52.3/40.0/45.3 saved, correlation
/// 0.992-0.999).
pub const LIGHT_SCALE: f32 = 0.375;
/// Scale on the zone ambient term ([`zone_ambient`]), same calibration.
pub const AMBIENT_SCALE: f32 = 0.375;

/// Zone ambient light as a modulate-2x factor (FGetHSV: hue ramp toward white by saturation,
/// times sqrt(brightness * 1.4 / 255)), scaled by `scale`.
pub fn zone_ambient(hue: u8, saturation: u8, brightness: u8, scale: f32) -> [f32; 3] {
    let b = (brightness as f32 * 1.4 / 255.0).sqrt().min(1.0) * scale;
    hsv_to_rgb(hue, saturation).map(|c| c * b)
}

/// Size of an atlas inferred from the lightmaps placed in it (power of two covering every
/// texel rectangle). Atlases saved without data carry garbage dimensions.
pub fn infer_atlas_size(model: &Model, atlas: i32) -> (u32, u32) {
    let (mut w, mut h) = (1u32, 1u32);
    for l in model.light_maps.iter().filter(|l| l.texture == atlas) {
        w = w.max((l.offset[0] + l.size[0]).max(0) as u32);
        h = h.max((l.offset[1] + l.size[1]).max(0) as u32);
    }
    (w.next_power_of_two(), h.next_power_of_two())
}

/// Light intensity (0..2) of `l` at world point `p` with surface normal `n` (Unreal space).
pub fn intensity(l: &MapLight, p: [f32; 3], n: [f32; 3]) -> f32 {
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    if l.kind == LightKind::Sun {
        return (-2.0 * dot(l.direction(), n)).max(0.0);
    }
    let d = [l.location[0] - p[0], l.location[1] - p[1], l.location[2] - p[2]];
    let dist = dot(d, d).sqrt().max(1e-3);
    let f = dist / l.world_radius();
    if f >= 1.0 {
        return 0.0;
    }
    let u = [d[0] / dist, d[1] / dist, d[2] / dist];
    let cos = dot(u, n);
    if cos <= 0.0 {
        return 0.0;
    }
    2.0 * (2.0 * f * f * f - 3.0 * f * f + 1.0) * cos * spot_cone(l, [-u[0], -u[1], -u[2]])
}

/// UE2's lightmap spot term (UnRenderLight spot/static-spot path): with e = 1 - LightCone/256 and
/// c the cosine between the light's axis and the direction to the texel, ((c - e) / (1 - e))^2
/// inside the cone, 0 outside. LightCone 128 is a 60 degree half-angle.
pub fn spot_cone(l: &MapLight, to: [f32; 3]) -> f32 {
    if l.kind != LightKind::Spot {
        return 1.0;
    }
    let d = l.direction();
    let c = d[0] * to[0] + d[1] * to[1] + d[2] * to[2];
    let e = 1.0 - l.cone as f32 / 256.0;
    if c <= e || e >= 1.0 {
        return 0.0;
    }
    let t = (c - e) / (1.0 - e);
    t * t
}

/// Visibility of a light at lightmap texel (x, y), softened over the 3x3 neighbourhood like the
/// engine's filtered light bitmaps (hard 32-unit texel steps otherwise show as stairs).
fn soft_visible(b: &crate::model::LightBitmap, x: i32, y: i32) -> f32 {
    let mut n = 0;
    let mut v = 0;
    for dy in -1..=1 {
        for dx in -1..=1 {
            n += 1;
            if b.visible(x + dx, y + dy) {
                v += 1;
            }
        }
    }
    // Centre weighted: a lit texel stays mostly lit, an unlit one picks up a little.
    let c = if b.visible(x, y) { 1.0 } else { 0.0 };
    0.5 * c + 0.5 * v as f32 / n as f32
}

/// Writes the rebuilt texels of lightmap `lm` into an RGBA atlas of width `aw`.
/// `light` resolves a light actor; `ambient` gives the zone ambient (modulate-2x factor) of the
/// lightmap's zone number.
pub fn build_lightmap(
    model: &Model,
    lm: &LightMap,
    atlas: &mut [u8],
    aw: u32,
    light: &mut dyn FnMut(ObjRef) -> Option<MapLight>,
    ambient: &dyn Fn(i32) -> [f32; 3],
    light_scale: f32,
) {
    let surf = model.surfs.get(lm.fields[1].max(0) as usize);
    let n = surf.and_then(|s| model.vectors.get(s.normal as usize)).copied().unwrap_or([0.0, 0.0, 1.0]);
    let amb = ambient(lm.fields[2]);
    let (sx, sy) = (lm.size[0].max(0), lm.size[1].max(0));
    let mut acc = vec![amb; (sx * sy) as usize];
    for b in &lm.lights {
        let Some(l) = light(b.actor) else { continue };
        let c = hsv_to_rgb(l.hue, l.saturation).map(|v| v * l.brightness / 255.0 * light_scale);
        for y in b.min[1].max(0)..=b.max[1].min(sy - 1) {
            for x in b.min[0].max(0)..=b.max[0].min(sx - 1) {
                let vis = soft_visible(b, x, y);
                if vis <= 0.0 {
                    continue;
                }
                let p = [0, 1, 2].map(|k| lm.origin[k] + lm.x_axis[k] * x as f32 + lm.y_axis[k] * y as f32);
                let i = intensity(&l, p, n) * vis;
                if i > 0.0 {
                    let a = &mut acc[(y * sx + x) as usize];
                    for k in 0..3 {
                        a[k] += c[k] * i;
                    }
                }
            }
        }
    }
    let ah = atlas.len() as u32 / 4 / aw.max(1);
    for y in 0..sy {
        for x in 0..sx {
            let (ax, ay) = ((lm.offset[0] + x) as u32, (lm.offset[1] + y) as u32);
            if ax >= aw || ay >= ah {
                continue;
            }
            let a = acc[(y * sx + x) as usize];
            let o = ((ay * aw + ax) * 4) as usize;
            for k in 0..3 {
                atlas[o + k] = (a[k].clamp(0.0, 1.0) * 255.0) as u8;
            }
            atlas[o + 3] = 255;
        }
    }
}
