//! Turns emitter classes (from `.u` script packages) and Emitter actors placed in maps into
//! self-contained [`FxSystem`] definitions: decoded sub-emitter properties plus the RGBA pixels of
//! their textures and the geometry of mesh-particle static meshes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use upkg::emitter::{EmitterActor, EmitterKind, ParticleEmitter};
use upkg::library::{Library, Obj};
use upkg::staticmesh::read_static_mesh;
use upkg::texture::read_texture;
use upkg::{ObjRef, Package, Properties, begin_object};
use uscript::ScriptObject;

/// Decoded texture: RGBA8 mip chain (level 0 first), sRGB.
#[derive(Debug)]
pub struct TexData {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub levels: u32,
    pub rgba: Vec<u8>,
    /// Texture has meaningful alpha (masked or alpha format).
    pub has_alpha: bool,
}

/// One section of a mesh particle's static mesh.
#[derive(Debug)]
pub struct MeshSection {
    pub indices: Vec<u32>,
    pub texture: Option<Arc<TexData>>,
    /// The section material blends (FinalBlend/Shader with blending): drawn translucent.
    pub blended: bool,
    pub masked: bool,
    pub two_sided: bool,
    /// Draw style of the section's material when the emitter uses the mesh's own blending
    /// (UseMeshBlendMode): Translucent/Brighten/Modulated for non-alpha framebuffer ops,
    /// AlphaBlend for other blended materials, Regular otherwise.
    pub style: upkg::emitter::DrawStyle,
}

/// Static mesh geometry for MeshEmitter particles, in Unreal space.
#[derive(Debug)]
pub struct MeshData {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub sections: Vec<MeshSection>,
}

/// A fully resolved sub-emitter.
#[derive(Debug)]
pub struct FxEmitter {
    pub data: ParticleEmitter,
    pub texture: Option<Arc<TexData>>,
    pub mesh: Option<Arc<MeshData>>,
}

/// A fully resolved Emitter (actor class or placed actor).
#[derive(Debug)]
pub struct FxSystem {
    /// "Package.Class" or "Map.ActorName".
    pub name: String,
    pub actor: EmitterActor,
    pub emitters: Vec<FxEmitter>,
}

impl FxSystem {
    pub fn particle_capacity(&self) -> usize {
        self.emitters.iter().map(|e| e.data.max_particles.max(0) as usize).sum()
    }
}

/// Script packages that define emitter classes, searched for bare class names.
pub const FX_PACKAGES: &[&str] = &["FX", "FX2", "BaseFX", "AWEffects", "Postal2Game", "Postal2Extras", "Postal2Holidays", "Engine", "People", "Inventory", "AWInventory", "AWPStuff", "EDStuff", "MoreGame", "P2R"];

/// Loader with caches over a game install.
pub struct FxLibrary {
    pub lib: Library,
    textures: Mutex<HashMap<(String, usize), Option<Arc<TexData>>>>,
    meshes: Mutex<HashMap<(String, usize), Option<Arc<MeshData>>>>,
}

/// Class defaults from most-derived to root, each with its package.
pub struct ClassChain {
    pub names: Vec<String>,
    pub layers: Vec<(Arc<Package>, Properties)>,
}

impl ClassChain {
    pub fn is_a(&self, class: &str) -> bool {
        self.names.iter().any(|n| n.eq_ignore_ascii_case(class))
    }
}

impl FxLibrary {
    pub fn new(game: impl AsRef<std::path::Path>) -> upkg::Result<Self> {
        Ok(Self { lib: Library::new(game)?, textures: Mutex::default(), meshes: Mutex::default() })
    }

    pub fn from_library(lib: Library) -> Self {
        Self { lib, textures: Mutex::default(), meshes: Mutex::default() }
    }

    /// Finds a class by "Package.Class" or bare name (searching [`FX_PACKAGES`]).
    pub fn find_class(&self, name: &str) -> Option<Obj> {
        let (pkgs, leaf): (Vec<&str>, &str) = match name.split_once('.') {
            Some((p, l)) => (vec![p], l),
            None => (FX_PACKAGES.to_vec(), name),
        };
        pkgs.iter().find_map(|p| {
            let pkg = self.lib.package(p)?;
            let e = pkg.find_export(leaf, Some("Class"))?;
            Some(Obj { pkg, export: e })
        })
    }

    /// Defaults of a class and all its parents.
    pub fn class_chain(&self, class: &Obj) -> Option<ClassChain> {
        let mut chain = ClassChain { names: vec![], layers: vec![] };
        let mut cur = Some(class.clone());
        while let Some(c) = cur.take() {
            if chain.names.len() > 40 {
                break;
            }
            let Ok(Some(ScriptObject::Class(data))) = uscript::parse_export(&c.pkg, c.export) else { break };
            chain.names.push(c.name().to_string());
            cur = self.lib.resolve(&c.pkg, data.state.base.super_);
            chain.layers.push((c.pkg.clone(), data.defaults));
        }
        (!chain.names.is_empty()).then_some(chain)
    }

    /// Resolves an emitter class ("FX.BloodSplashEmitter", "FireEmitter", ...).
    pub fn load_class(&self, name: &str) -> Result<FxSystem, String> {
        let class = self.find_class(name).ok_or_else(|| format!("class '{name}' not found"))?;
        let chain = self.class_chain(&class).ok_or_else(|| format!("cannot parse class '{name}'"))?;
        if !chain.is_a("Emitter") {
            return Err(format!("'{name}' is not an Emitter subclass (chain: {})", chain.names.join(" > ")));
        }
        let mut actor = EmitterActor { class: class.name().to_string(), object_name: class.name().to_string(), ..Default::default() };
        let mut emitters_from: Option<Arc<Package>> = None;
        for (pkg, props) in chain.layers.iter().rev() {
            if actor.apply(pkg, props) {
                emitters_from = Some(pkg.clone());
            }
        }
        let emitters = match &emitters_from {
            Some(pkg) => self.resolve_emitters(pkg, &actor.emitters),
            None => vec![],
        };
        Ok(FxSystem { name: format!("{}.{}", class.pkg.name, class.name()), actor, emitters })
    }

    /// Resolves an emitter actor placed in a map. Class defaults are applied first, then the
    /// actor's own properties (which normally carry their own `Emitters` sub-objects).
    pub fn load_actor(&self, map: &Arc<Package>, export: usize) -> Result<FxSystem, String> {
        let (_, props) = begin_object(map, export).map_err(|e| e.to_string())?;
        let class_name = map.export_class(export).to_string();
        let mut actor = EmitterActor { class: class_name.clone(), object_name: map.name(map.exports[export].name).to_string(), ..Default::default() };
        let mut emitters_from: Option<Arc<Package>> = None;
        if let Some(class) = self.lib.resolve(map, map.exports[export].class) {
            if let Some(chain) = self.class_chain(&class) {
                for (pkg, layer) in chain.layers.iter().rev() {
                    if actor.apply(pkg, layer) {
                        emitters_from = Some(pkg.clone());
                    }
                }
            }
        }
        if actor.apply(map, &props) {
            emitters_from = Some(map.clone());
        }
        let emitters = match &emitters_from {
            Some(pkg) => self.resolve_emitters(pkg, &actor.emitters),
            None => vec![],
        };
        Ok(FxSystem { name: format!("{}.{}", map.name, actor.object_name), actor, emitters })
    }

    /// Emitter actors placed in a map: (export index, class name). An actor counts when it has
    /// an `Emitters` list or its class name ends in "Emitter".
    pub fn map_emitter_actors(map: &Package) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for i in 0..map.exports.len() {
            let class = map.export_class(i);
            if EmitterKind::is_particle_emitter_class(class) || matches!(class, "Class" | "Function" | "State" | "Struct" | "Texture" | "Model" | "Polys" | "StaticMesh") {
                continue;
            }
            let lc = class.to_ascii_lowercase();
            let maybe = lc.contains("emitter") || lc.contains("fire") || lc.contains("smoke") || lc.contains("rain") || lc.contains("snow");
            if !maybe {
                continue;
            }
            if let Ok((_, props)) = begin_object(map, i) {
                if props.get("Emitters").is_some() || lc.ends_with("emitter") {
                    out.push((i, class.to_string()));
                }
            }
        }
        out
    }

    fn resolve_emitters(&self, pkg: &Arc<Package>, refs: &[ObjRef]) -> Vec<FxEmitter> {
        let mut out = Vec::new();
        for r in refs {
            let Some(obj) = self.lib.resolve(pkg, *r) else { continue };
            if !EmitterKind::is_particle_emitter_class(obj.class()) {
                continue;
            }
            if let Some(e) = self.load_sub_emitter(&obj) {
                out.push(e);
            }
        }
        out
    }

    /// Decodes one ParticleEmitter object and its texture / static mesh.
    pub fn load_sub_emitter(&self, obj: &Obj) -> Option<FxEmitter> {
        let data = ParticleEmitter::read(&obj.pkg, obj.export).ok()?;
        let (_, props) = begin_object(&obj.pkg, obj.export).ok()?;
        // Texture: the object's own reference, else the engine default (S_Emitter).
        let texture = match props.object("Texture") {
            Some(ObjRef::Null) => None,
            Some(r) => self.lib.resolve(&obj.pkg, r).and_then(|t| self.texture(&t)),
            None => self.lib.package("Engine").and_then(|p| p.find_export("S_Emitter", Some("Texture")).map(|e| Obj { pkg: p, export: e })).and_then(|t| self.texture(&t)),
        };
        let mesh = match props.object("StaticMesh") {
            Some(r) if r != ObjRef::Null => self.lib.resolve(&obj.pkg, r).and_then(|m| self.mesh(&m)),
            _ => None,
        };
        Some(FxEmitter { data, texture, mesh })
    }

    /// Decoded texture by full object path (`Pkg.Group.Name`, as the VM names resources a script
    /// assigned, e.g. xMuzzleFlashEmitter.SetEmitterTexture). Cached per path.
    pub fn texture_by_path(&self, path: &str) -> Option<Arc<TexData>> {
        let key = (format!("path:{}", path.to_ascii_lowercase()), 0);
        if let Some(t) = self.textures.lock().unwrap().get(&key) {
            return t.clone();
        }
        let mut parts = path.split('.');
        let pkg = parts.next().and_then(|p| self.lib.package(p));
        let rest: Vec<&str> = parts.collect();
        let t = pkg.and_then(|pkg| {
            let (leaf, groups) = rest.split_last()?;
            // Exports named `leaf` whose outer chain matches the groups (outermost first in the path).
            let export = (0..pkg.exports.len()).find(|&i| {
                if !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(leaf) {
                    return false;
                }
                let mut outer = pkg.exports[i].outer;
                for g in groups.iter().rev() {
                    match outer {
                        upkg::ObjRef::Export(o) if pkg.name(pkg.exports[o].name).eq_ignore_ascii_case(g) => outer = pkg.exports[o].outer,
                        _ => return false,
                    }
                }
                outer == upkg::ObjRef::Null
            })?;
            self.texture(&Obj { pkg, export })
        });
        self.textures.lock().unwrap().insert(key, t.clone());
        t
    }

    /// Decoded texture (cached). Follows material wrappers down to a Texture.
    pub fn texture(&self, obj: &Obj) -> Option<Arc<TexData>> {
        let key = obj.key();
        if let Some(t) = self.textures.lock().unwrap().get(&key) {
            return t.clone();
        }
        let tex_obj = if obj.class().eq_ignore_ascii_case("Texture") { Some(obj.clone()) } else { self.lib.material_texture(obj).and_then(|m| m.texture) };
        let t = tex_obj.and_then(|t| decode_texture(&t)).map(Arc::new);
        self.textures.lock().unwrap().insert(key, t.clone());
        t
    }

    /// Static mesh geometry with section textures (cached).
    pub fn mesh(&self, obj: &Obj) -> Option<Arc<MeshData>> {
        let key = obj.key();
        if let Some(m) = self.meshes.lock().unwrap().get(&key) {
            return m.clone();
        }
        let m = (|| {
            let sm = read_static_mesh(&obj.pkg, obj.export).ok()?;
            let mut sections = Vec::new();
            for (s, _) in sm.sections.iter().enumerate() {
                let mat = sm.materials.get(s).copied().unwrap_or(ObjRef::Null);
                let info = self.lib.resolve(&obj.pkg, mat).and_then(|m| self.lib.material_texture(&m));
                let texture = info.as_ref().and_then(|i| i.texture.as_ref()).and_then(|t| self.texture(t));
                let blended = info.as_ref().is_some_and(|i| i.blended);
                let style = match info.as_ref().and_then(|i| i.op) {
                    Some(upkg::library::BlendOp::Translucent) => upkg::emitter::DrawStyle::Translucent,
                    Some(upkg::library::BlendOp::Brighten) => upkg::emitter::DrawStyle::Brighten,
                    Some(upkg::library::BlendOp::Modulate) => upkg::emitter::DrawStyle::Modulated,
                    Some(upkg::library::BlendOp::Darken) => upkg::emitter::DrawStyle::Darken,
                    None if blended => upkg::emitter::DrawStyle::AlphaBlend,
                    None => upkg::emitter::DrawStyle::Regular,
                };
                sections.push(MeshSection {
                    indices: sm.section_indices(s).iter().map(|&i| i as u32).collect(),
                    texture,
                    blended,
                    masked: info.as_ref().is_some_and(|i| i.masked),
                    two_sided: info.as_ref().is_some_and(|i| i.two_sided),
                    style,
                });
            }
            let uvs = if sm.uvs.len() == sm.positions.len() { sm.uvs.clone() } else { vec![[0.0; 2]; sm.positions.len()] };
            Some(Arc::new(MeshData { name: sm.name.clone(), positions: sm.positions, normals: sm.normals, uvs, sections }))
        })();
        self.meshes.lock().unwrap().insert(key, m.clone());
        m
    }
}

fn decode_texture(t: &Obj) -> Option<TexData> {
    let tex = read_texture(&t.pkg, t.export).ok()?;
    let (w, h) = (tex.mips.first()?.width.max(1), tex.mips.first()?.height.max(1));
    let mut rgba = Vec::new();
    let mut levels = 0u32;
    let (mut ew, mut eh) = (w, h);
    for level in 0..tex.mips.len() {
        let m = &tex.mips[level];
        if m.width.max(1) != ew || m.height.max(1) != eh {
            break;
        }
        let Ok((_, _, px)) = tex.decode_rgba(level) else { break };
        rgba.extend_from_slice(&px);
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
    let has_alpha = tex.masked || tex.alpha_texture || !matches!(tex.format, upkg::texture::TexFormat::Dxt1 | upkg::texture::TexFormat::Rgb8 | upkg::texture::TexFormat::L8 | upkg::texture::TexFormat::G16);
    Some(TexData { name: format!("{}.{}", t.pkg.name, tex.name), width: w, height: h, levels, rgba, has_alpha })
}
