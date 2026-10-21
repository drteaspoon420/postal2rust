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
            out.push(Part {
                geometry,
                slot,
                color: std::array::from_fn(|i| color[i] * tint[i]),
                texture: reference.and_then(|p| texture(content, asset, &p)),
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
                entry.tint,
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
/// are Magicka world metres; the Bevy host scales them to Postal world units.
pub fn pose(wizard: &WizardWorld) -> Option<(Vec<Mat>, Vec<(i32, Mat)>)> {
    let avatar = wizard.avatar();
    let rig = avatar.rig.as_ref()?;
    let body = wizard.body();
    let world = magicka_runtime::character_pose::character_orientation(
        &rig.static_transform,
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
