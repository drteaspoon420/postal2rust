//! Original local Magicka model/material loading, shared by either camera.
use crate::WizardWorld;
use magicka_content::{
    Content,
    assets::Asset,
    gltf::Primitive,
    mesh, scene,
    skinned::{self, Mat},
    xna::{Model, XnaObject},
};
use magicka_runtime::play_content;
use std::path::Path;

pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
pub struct Part {
    pub geometry: Primitive,
    /// None = robe skin. Some = equipment slot with a rigid attachment.
    pub slot: Option<i32>,
    pub color: [f32; 3],
    pub texture: Option<Texture>,
    pub cutout: bool,
}

fn external(object: &Option<XnaObject>) -> Option<&str> {
    match object.as_ref()? {
        XnaObject::Asset(a) => match a.as_ref() {
            Asset::External(p) => p.as_deref(),
            _ => None,
        },
        _ => None,
    }
}

/// The basic skinned shader blends detail with a separately tinted fabric map.
/// Alpha is coverage, including the overlay, rather than a recolour cutout.
fn fabric_pixel(
    detail: [f32; 4],
    overlay: [f32; 4],
    diffuse: [f32; 3],
    tint: [f32; 3],
    weight: f32,
    soft: bool,
) -> [u8; 4] {
    let a = overlay[3] * weight;
    let mut out = [0; 4];
    for i in 0..3 {
        let b = overlay[i];
        let t = tint[i];
        let fabric = if !soft {
            b * t
        } else if t < 0.5 {
            (1. - 2. * t) * b * b + 2. * b * t
        } else {
            (2. * t - 1.) * b.sqrt() + 2. * b * (1. - t)
        };
        out[i] =
            ((detail[i] * diffuse[i] * (1. - a) + fabric * a).clamp(0., 1.) * 255.).round() as u8;
    }
    out[3] = ((a + detail[3] * (1. - a)).clamp(0., 1.) * 255.).round() as u8;
    out
}

fn skinned_texture(
    content: &Path,
    asset: &str,
    effect: &magicka_content::assets::SkinnedModelBasicEffect,
    tint: [f32; 3],
) -> Option<Texture> {
    let load_map = |i: usize| -> Option<Texture> {
        if !effect.map_enabled[i] {
            return None;
        }
        texture(content, asset, external(effect.maps.get(i)?)?)
    };
    let detail = load_map(0);
    let overlay = load_map(1);
    let width = detail
        .as_ref()
        .map_or(1, |t| t.width)
        .max(overlay.as_ref().map_or(1, |t| t.width));
    let height = detail
        .as_ref()
        .map_or(1, |t| t.height)
        .max(overlay.as_ref().map_or(1, |t| t.height));
    let sample = |t: &Option<Texture>, x: u32, y: u32, fallback: [f32; 4]| {
        t.as_ref().map_or(fallback, |t| {
            let index = (((y * t.height / height) * t.width + x * t.width / width) * 4) as usize;
            std::array::from_fn(|i| t.rgba[index + i] as f32 / 255.)
        })
    };
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&fabric_pixel(
                sample(&detail, x, y, [1.; 4]),
                sample(&overlay, x, y, [0.; 4]),
                effect.diffuse_color,
                tint,
                effect.diffuse1_alpha,
                effect.use_soft_light_blend,
            ));
        }
    }
    Some(Texture {
        width,
        height,
        rgba,
    })
}
fn read(content: &Path, asset: &str) -> Result<magicka_content::Parsed, String> {
    let path = play_content::resolve(content, &Path::new(asset).with_extension("xnb"))
        .ok_or_else(|| format!("asset missing: {asset}"))?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let xnb = magicka_xnb::xnb::parse(&bytes).map_err(|e| format!("{e:?}"))?;
    magicka_content::parse_primary(&xnb)
        .ok_or("null content")?
        .map_err(|e| e.to_string())
}
fn texture(content: &Path, asset: &str, reference: &str) -> Option<Texture> {
    let name = mesh::resolve_reference(asset, reference);
    let Content::Xna(object) = read(content, &name).ok()?.content else {
        return None;
    };
    let XnaObject::Asset(asset) = *object else {
        return None;
    };
    let Asset::Texture2D(t) = *asset else {
        return None;
    };
    Some(Texture {
        width: t.width as u32,
        height: t.height as u32,
        rgba: magicka_content::texture::decode_rgba(
            t.format,
            t.width as usize,
            t.height as usize,
            t.mips.first()?,
        )?,
    })
}
fn material(effect: Option<&XnaObject>) -> ([f32; 3], Option<String>) {
    match effect {
        Some(XnaObject::Asset(a)) => match a.as_ref() {
            Asset::SkinnedModelBasicEffect(e) => {
                let tex = e
                    .maps
                    .first()
                    .and_then(Option::as_ref)
                    .and_then(|o| match o {
                        XnaObject::Asset(a) => match a.as_ref() {
                            Asset::External(p) => p.clone(),
                            _ => None,
                        },
                        _ => None,
                    });
                (e.diffuse_color, tex)
            }
            Asset::SkinnedModelDeferredNormalMappedEffect(e) => {
                (e.diffuse_color, e.maps[0].clone())
            }
            Asset::BasicEffect(e) => (e.diffuse, e.texture.clone()),
            _ => ([1.; 3], None),
        },
        Some(XnaObject::RenderDeferredEffect(e)) => {
            (e.layer0.diffuse_color, e.layer0.diffuse_texture.clone())
        }
        _ => ([1.; 3], None),
    }
}
fn parts(
    content: &Path,
    asset: &str,
    model: &Model,
    shared: &[Option<XnaObject>],
    slot: Option<i32>,
    tint: [f32; 3],
) -> Vec<Part> {
    let mut out = Vec::new();
    for mesh in &model.meshes {
        let (Some(vb), Some((sixteen, ib))) = (&mesh.vertex_buffer, &mesh.index_buffer) else {
            continue;
        };
        for part in &mesh.parts {
            let Some(decl) = model
                .vertex_declarations
                .get(part.vertex_declaration.max(0) as usize)
            else {
                continue;
            };
            let Some(geometry) = scene::primitive(
                decl,
                vb,
                scene::decl_stride(decl),
                part.stream_offset.max(0) as usize,
                part.base_vertex.max(0) as usize,
                (*sixteen, ib),
                part.start_index.max(0) as usize,
                part.primitive_count.max(0) as usize,
                &mut scene::Stats::default(),
            ) else {
                continue;
            };
            let effect = (part.effect as usize)
                .checked_sub(1)
                .and_then(|i| shared.get(i))
                .and_then(Option::as_ref);
            let (color, reference) = material(effect);
            let basic = match effect {
                Some(XnaObject::Asset(a)) => match a.as_ref() {
                    Asset::SkinnedModelBasicEffect(e) => Some(e),
                    _ => None,
                },
                _ => None,
            };
            let cutout = match effect {
                Some(XnaObject::RenderDeferredEffect(e)) => {
                    !e.layer0.alpha_mask_enabled && !e.layer0.diffuse_alpha_disabled
                }
                Some(XnaObject::Asset(a)) => matches!(
                    a.as_ref(),
                    Asset::SkinnedModelBasicEffect(_)
                        | Asset::SkinnedModelDeferredNormalMappedEffect(_)
                ),
                _ => false,
            };
            out.push(Part {
                geometry,
                slot,
                color: if basic.is_some() { [1.; 3] } else { color },
                texture: if let Some(e) = basic {
                    skinned_texture(content, asset, e, tint)
                } else {
                    reference.and_then(|p| texture(content, asset, &p))
                },
                cutout,
            });
        }
    }
    out
}

pub fn load(wizard: &WizardWorld) -> Result<Vec<Part>, String> {
    let content = &wizard.content;
    let template = play_content::load_character(content, &wizard.loadout.robe)
        .map_err(|e| format!("{e:?}"))?;
    let mut out = Vec::new();
    let character_asset = format!("Data/Characters/{}", wizard.loadout.robe);
    for entry in &template.models {
        let Some(reference) = &entry.model else {
            continue;
        };
        let asset = mesh::resolve_reference(&character_asset, reference);
        let model =
            play_content::load_skinned_model(content, &Path::new(&asset).with_extension("xnb"))
                .map_err(|e| format!("{e:?}"))?;
        if let Some(geometry) = model.model.as_ref() {
            out.extend(parts(
                content,
                &asset,
                geometry,
                &model.shared,
                None,
                std::array::from_fn(|i| entry.tint[i] * wizard.robe_colour[i]),
            ));
        }
    }
    for (slot, item) in &wizard.avatar().items {
        if item.definition.hide_model {
            continue;
        }
        let Some(reference) = &item.definition.model else {
            continue;
        };
        let asset = mesh::resolve_reference(
            &format!("Data/Items/Wizard/{}", item.definition.name),
            reference,
        );
        let parsed = read(content, &asset)?;
        if let Content::Xna(object) = parsed.content {
            if let XnaObject::Model(model) = *object {
                out.extend(parts(
                    content,
                    &asset,
                    &model,
                    &parsed.shared,
                    Some(*slot),
                    [1.; 3],
                ));
            }
        }
    }
    if out.is_empty() {
        return Err("robe has no renderable mesh".into());
    }
    Ok(out)
}

/// Sample the exact animation controller used for cast/melee timing. Matrices
/// are world metres, matching the Postal renderer after its UU conversion.
pub fn pose(wizard: &WizardWorld) -> Option<(Vec<Mat>, Vec<(i32, Mat)>)> {
    let avatar = wizard.avatar();
    let rig = avatar.rig.as_ref()?;
    let body = wizard.body();
    let world = magicka_runtime::character_pose::character_orientation(
        &avatar
            .character
            .scaled_static_transform(&rig.static_transform),
        &body.orientation,
        body.position,
        body.height_offset(),
    );
    let skin = rig.skeleton.skinned_for(&avatar.motion.controller, &world);
    let items = rig
        .attachments
        .iter()
        .filter(|a| a.has_item)
        .map(|a| (a.slot, a.item_transform(&skin)))
        .collect();
    Some((skin, items))
}
pub fn transform_point(p: [f32; 3], m: &Mat) -> [f32; 3] {
    std::array::from_fn(|i| p[0] * m[i] + p[1] * m[4 + i] + p[2] * m[8 + i] + m[12 + i])
}
pub fn positions(part: &Part, skin: &[Mat], items: &[(i32, Mat)]) -> Vec<[f32; 3]> {
    if let Some(slot) = part.slot {
        let transform = items
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, m)| m)
            .unwrap_or(&skinned::IDENTITY);
        return part
            .geometry
            .positions
            .iter()
            .map(|p| transform_point(*p, transform))
            .collect();
    }
    match (&part.geometry.joints, &part.geometry.weights) {
        (Some(j), Some(w)) => part
            .geometry
            .positions
            .iter()
            .enumerate()
            .map(|(i, p)| skinned::skin_vertex(*p, j[i], w[i], skin))
            .collect(),
        _ => part.geometry.positions.clone(),
    }
}

fn asset(content: &Path, name: &str) -> Result<Asset, String> {
    let Content::Xna(object) = read(content, name)?.content else {
        return Err(format!("{name}: expected an XNA object"));
    };
    let XnaObject::Asset(asset) = *object else {
        return Err(format!("{name}: expected an asset"));
    };
    Ok(*asset)
}
/// A Texture2D from Content (e.g. `EffectTextures/Beams`), base level only.
pub fn texture2d(content: &Path, name: &str) -> Result<Texture, String> {
    let Asset::Texture2D(t) = asset(content, name)? else {
        return Err(format!("{name}: not a Texture2D"));
    };
    Ok(Texture {
        width: t.width as u32,
        height: t.height as u32,
        rgba: magicka_content::texture::decode_rgba(
            t.format,
            t.width as usize,
            t.height as usize,
            t.mips.first().ok_or("no mips")?,
        )
        .ok_or_else(|| format!("{name}: undecodable format {}", t.format))?,
    })
}

/// Original rigid model parts, including material textures, at authored size.
pub fn model_parts(content: &Path, name: &str) -> Result<Vec<Part>, String> {
    let parsed = read(content, name)?;
    let Content::Xna(object) = parsed.content else {
        return Err(format!("{name}: not an XNA model"));
    };
    let XnaObject::Model(model) = *object else {
        return Err(format!("{name}: not a model"));
    };
    Ok(parts(
        content,
        name,
        &model,
        &parsed.shared,
        Some(0),
        [1.; 3],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fabric_overlay_restores_coverage_and_preserves_detail() {
        // Transparent detail is the fabric region, not a hole in the robe.
        assert_eq!(
            fabric_pixel(
                [0.; 4],
                [0.5, 0.5, 0.5, 1.],
                [1.; 3],
                [1., 0., 0.],
                1.,
                false
            ),
            [128, 0, 0, 255]
        );
        assert_eq!(
            fabric_pixel(
                [0.2, 0.4, 0.8, 1.],
                [1., 1., 1., 0.],
                [1.; 3],
                [1., 0., 0.],
                1.,
                true
            ),
            [51, 102, 204, 255]
        );
        assert_eq!(
            fabric_pixel([0.; 4], [1., 1., 1., 0.], [1.; 3], [1.; 3], 1., false)[3],
            0
        );
    }
}
/// A particle sprite sheet volume: RGBA bytes of every mip level in order.
pub struct Volume {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub mip_count: u32,
    pub rgba: Vec<u8>,
}
/// EffectTextures/ParticlesA..D (Texture3D, SurfaceFormat.Color as BGRA).
pub fn particle_sheets(content: &Path) -> Result<Vec<Volume>, String> {
    ["A", "B", "C", "D"]
        .iter()
        .map(|k| {
            let name = format!("EffectTextures/Particles{k}");
            let Asset::Texture3D {
                format,
                width,
                height,
                depth,
                mips,
            } = asset(content, &name)?
            else {
                return Err(format!("{name}: not a Texture3D"));
            };
            if format != 1 {
                return Err(format!("{name}: surface format {format}"));
            }
            let mut rgba = Vec::new();
            for mip in &mips {
                for c in mip.chunks_exact(4) {
                    rgba.extend_from_slice(&[c[2], c[1], c[0], c[3]]);
                }
            }
            Ok(Volume {
                width: width as u32,
                height: height as u32,
                depth: depth as u32,
                mip_count: mips.len() as u32,
                rgba,
            })
        })
        .collect()
}
