//! Cross-package object resolution over the user's game installation.
//!
//! Packages are opened lazily and cached; imports are resolved by walking the import's outer
//! chain (Package.Group.Object) and matching it against the target package's exports.

use crate::package::{ObjRef, Package};
use crate::props::{Value, begin_object};
use crate::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Content directories searched under the game root, in priority order.
pub const CONTENT_DIRS: &[&str] = &["Textures", "StaticMeshes", "Animations", "Sounds", "System", "Maps"];
const EXTENSIONS: &[&str] = &["utx", "usx", "ukx", "uax", "u", "fuk"];

/// A resolved object: the package that exports it and the export index.
#[derive(Clone)]
pub struct Obj {
    pub pkg: Arc<Package>,
    pub export: usize,
}

impl std::fmt::Debug for Obj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{} ({})", self.pkg.name, self.name(), self.class())
    }
}

impl Obj {
    pub fn class(&self) -> &str {
        self.pkg.export_class(self.export)
    }

    pub fn name(&self) -> &str {
        self.pkg.name(self.pkg.exports[self.export].name)
    }

    /// Stable key for caches: "package.export_index".
    pub fn key(&self) -> (String, usize) {
        (self.pkg.name.clone(), self.export)
    }
}

pub struct Library {
    pub root: PathBuf,
    files: HashMap<String, PathBuf>,
    cache: Mutex<HashMap<String, Option<Arc<Package>>>>,
}

impl Library {
    /// Indexes all package files under the game root.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let mut files = HashMap::new();
        for dir in CONTENT_DIRS {
            let Ok(rd) = std::fs::read_dir(crate::ci::join(&root, dir)) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let ext = p.extension().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                if !EXTENSIONS.contains(&ext.as_str()) {
                    continue;
                }
                let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                files.entry(stem).or_insert(p);
            }
        }
        if files.is_empty() {
            return Err(Error::Unsupported(format!("no Unreal packages found under {}", root.display())));
        }
        Ok(Self { root, files, cache: Mutex::new(HashMap::new()) })
    }

    pub fn package_count(&self) -> usize {
        self.files.len()
    }

    /// Path of a map by name ("Junkyard") or a direct path.
    pub fn map_path(&self, name: &str) -> Option<PathBuf> {
        let p = Path::new(name);
        if p.is_file() {
            return Some(p.to_path_buf());
        }
        let stem = p.file_stem()?.to_string_lossy().to_lowercase();
        self.files.get(&stem).cloned()
    }

    /// Opens (or returns the cached) package by name.
    pub fn package(&self, name: &str) -> Option<Arc<Package>> {
        let key = name.to_lowercase();
        if let Some(p) = self.cache.lock().unwrap().get(&key) {
            return p.clone();
        }
        let loaded = self.files.get(&key).and_then(|path| Package::open(path).ok()).map(Arc::new);
        self.cache.lock().unwrap().insert(key, loaded.clone());
        loaded
    }

    /// Registers an already opened package (e.g. the current map) so references back into it resolve.
    pub fn insert(&self, pkg: Arc<Package>) {
        self.cache.lock().unwrap().insert(pkg.name.clone(), Some(pkg));
    }

    /// Resolves a reference made from `pkg` to the package/export that defines it.
    pub fn resolve(&self, pkg: &Arc<Package>, r: ObjRef) -> Option<Obj> {
        match r {
            ObjRef::Null => None,
            ObjRef::Export(i) => Some(Obj { pkg: pkg.clone(), export: i }),
            ObjRef::Import(i) => {
                // Collect the name chain from leaf to root.
                let mut chain = Vec::new();
                let mut cur = ObjRef::Import(i);
                while let ObjRef::Import(j) = cur {
                    chain.push(pkg.name(pkg.imports[j].name).to_string());
                    cur = pkg.imports[j].outer;
                    if chain.len() > 16 {
                        return None;
                    }
                }
                let root = chain.pop()?;
                let target = self.package(&root)?;
                let leaf = chain.first()?.clone();
                let groups: Vec<&str> = chain[1..].iter().map(|s| s.as_str()).collect();
                let class = pkg.name(pkg.imports[i].class_name);
                let export = find_by_path(&target, &leaf, &groups, Some(class)).or_else(|| find_by_path(&target, &leaf, &groups, None))?;
                Some(Obj { pkg: target, export })
            }
        }
    }

    /// Follows material wrappers (Shader, FinalBlend, Combiner, TexModifier, ...) down to a Texture.
    pub fn material_texture(&self, mat: &Obj) -> Option<MaterialInfo> {
        let mut info = MaterialInfo::default();
        let mut cur = mat.clone();
        for _ in 0..8 {
            let class = cur.class().to_ascii_lowercase();
            if class == "texture" {
                info.texture = Some(cur);
                return Some(info);
            }
            let (_, props) = begin_object(&cur.pkg, cur.export).ok()?;
            // UV animation wrappers (sky clouds, water): collected outermost first.
            if matches!(class.as_str(), "texpanner" | "texrotator" | "texoscillator") {
                if let Ok(m) = crate::material::read_material(&cur.pkg, cur.export) {
                    info.uv_anim.push(m);
                }
            }
            if props.bool("TwoSided") == Some(true) {
                info.two_sided = true;
            }
            if props.bool("AlphaTest") == Some(true) {
                info.masked = true;
            }
            // Shader OutputBlending: 0 normal, 1 masked (alpha test), 2+ blended. Foliage shaders
            // use 1; treating it as blended made tree canopies soft and see-through.
            let opacity = props.object("Opacity").filter(|o| *o != ObjRef::Null);
            match props.byte("OutputBlending") {
                Some(1) => info.masked = true,
                Some(b) if b != 0 => info.blended = true,
                // Shader with an Opacity input and OB_Normal: alpha comes from the Opacity
                // material. When Opacity is the Diffuse itself (CowPasture's zo_grassline2) we
                // alpha-test it: blending sorted and dithered the grass into dark blotches.
                // A separate Opacity texture (suburbs window glass: window_house_outermask,
                // alpha 163 = 64% on the panes, 255 on the frame) is a real translucent blend,
                // the diffuse's own alpha is 0 on the panes.
                _ if class == "shader" && opacity.is_some() => {
                    if opacity == props.object("Diffuse") {
                        // ...unless the texture has no cut-out texels: AW door/bank glass
                        // (zo_greenbelt.zo_doorglass, Opacity = Diffuse = zo_doorglass_opaque,
                        // alpha ~176 everywhere, none clear) alpha-tested to solid white panes
                        // (VincesHouse, FIDELITY #56). Blend it like a separate Opacity mask.
                        // P2_GLASS_SELF_OPACITY_MASKED=1 restores the old alpha test.
                        let tex = opacity.and_then(|o| self.resolve(&cur.pkg, o)).and_then(|o| self.material_texture(&o)).and_then(|m| m.texture);
                        // P2_SELF_OPACITY_BLEND=name,...: probe, blend these self-opacity shaders anyway.
                        let force = std::env::var("P2_SELF_OPACITY_BLEND").ok().is_some_and(|v| {
                            let n = cur.pkg.name(cur.pkg.exports[cur.export].name).to_ascii_lowercase();
                            v.split(',').any(|f| !f.is_empty() && n.contains(&f.to_ascii_lowercase()))
                        });
                        match tex.filter(|t| force || (std::env::var_os("P2_GLASS_SELF_OPACITY_MASKED").is_none() && translucent_alpha(t))) {
                            Some(t) => {
                                info.opacity = Some(t);
                                info.blended = true;
                            }
                            None => info.masked = true,
                        }
                    } else if let Some(o) = opacity.and_then(|o| self.resolve(&cur.pkg, o)) {
                        info.opacity = self.material_texture(&o).and_then(|m| m.texture);
                        if info.opacity.is_some() {
                            info.blended = true;
                        } else {
                            info.masked = true;
                        }
                    }
                }
                _ => {}
            }
            if props.byte("FrameBufferBlending").is_some_and(|b| b != 0) {
                info.blended = true;
            }
            // Framebuffer ops that are not alpha blends (outermost wins): Shader OutputBlending
            // OB_Modulate 2 / OB_Brighten 5 / OB_Darken 6, FinalBlend FB_Modulate 1 / FB_Darken 5 /
            // FB_Brighten 6. Drawn as an alpha blend of an opaque texture they cover what is
            // below: suburbs-3's interstate sign has a sign_grime_dark (OB_Darken) section over
            // the destination slots that showed as a solid grey board.
            if info.op.is_none() {
                info.op = match (class.as_str(), props.byte("OutputBlending"), props.byte("FrameBufferBlending")) {
                    ("shader", Some(2), _) | ("finalblend", _, Some(1)) => Some(BlendOp::Modulate),
                    ("shader", Some(6), _) | ("finalblend", _, Some(5)) => Some(BlendOp::Darken),
                    ("shader", Some(5), _) | ("finalblend", _, Some(6)) => Some(BlendOp::Brighten),
                    // OB_Translucent / FB_Translucent ignore the texture alpha (UE2 D3D: ONE,
                    // INVSRCCOLOR): black is clear. Muzzle-flash meshes (timb.pistol_corona_add,
                    // alpha 255 everywhere) drew as an opaque brown card when alpha-blended.
                    ("shader", Some(3), _) | ("finalblend", _, Some(4)) => Some(BlendOp::Translucent),
                    _ => None,
                };
            }
            // Combiner CO_AlphaBlend_With_Mask without a Mask: Material1's alpha selects between
            // the layers and Material1 is the overlay (ben_skins fuzzyScreen_layered: scrolling
            // black bars, white and opaque except the bars, over the TV static in Material2).
            // Following Material1 drew the big-screen TV as a white panel; the static is the
            // visible layer.
            let combiner_base = (class == "combiner" && props.byte("CombineOperation") == Some(5) && props.object("Mask").is_none_or(|m| m == ObjRef::Null))
                .then(|| props.object("Material2").filter(|o| *o != ObjRef::Null))
                .flatten();
            let next = combiner_base
                .or_else(|| ["Diffuse", "Material", "Material1", "SelfIllumination", "Material2", "FallbackMaterial"].iter().find_map(|k| props.object(k).filter(|o| *o != ObjRef::Null)))
                .or_else(|| first_array_object(&props, "Materials"))?;
            cur = self.resolve(&cur.pkg, next)?;
        }
        None
    }
}

/// True when a texture's alpha is a translucency (glass), not a cut-out: under 1% of texels
/// clear (alpha < 8) and at least half of them partial (8..248), on the top mip (small mips
/// blur a cut-out lattice such as zo_kennel_fence_mask into partial alpha).
/// zo_doorglass_opaque: 0% clear, 100% partial; CowPasture zo_grassline2_base: 32% clear.
pub fn translucent_alpha(tex: &Obj) -> bool {
    let Ok(t) = crate::texture::read_texture(&tex.pkg, tex.export) else { return false };
    let Ok((_, _, px)) = t.decode_rgba(0) else { return false };
    let n = px.len() / 4;
    let clear = px.chunks_exact(4).filter(|c| c[3] < 8).count();
    let partial = px.chunks_exact(4).filter(|c| (8..248).contains(&c[3])).count();
    if std::env::var_os("P2_ALPHA_DEBUG").is_some() {
        let low = t.decode_rgba(3).ok().map(|(_, _, p)| {
            let m = p.len() / 4;
            (p.chunks_exact(4).filter(|c| (32..224).contains(&c[3])).count() * 100 / m.max(1), p.chunks_exact(4).filter(|c| c[3] >= 128).count() * 100 / m.max(1))
        });
        let under: f32 = px.chunks_exact(4).filter(|c| c[3] < 8).map(|c| (c[0] as f32 + c[1] as f32 + c[2] as f32) / 3.0).sum::<f32>() / clear.max(1) as f32;
        let solid: Vec<&[u8]> = px.chunks_exact(4).filter(|c| c[3] >= 248).collect();
        let over: f32 = solid.iter().map(|c| (c[0] as f32 + c[1] as f32 + c[2] as f32) / 3.0).sum::<f32>() / solid.len().max(1) as f32;
        eprintln!("translucent_alpha {}: {}x{} n {n} clear {clear} partial {partial} rgb_under_clear {under:.0} rgb_solid {over:.0} mip3 (partial%, >=128%) {low:?}", tex.pkg.name(tex.pkg.exports[tex.export].name), t.width, t.height);
    }
    n > 0 && clear * 100 < n && partial * 2 >= n
}

fn first_array_object(props: &crate::Properties, name: &str) -> Option<ObjRef> {
    let Some(Value::Array { raw }) = props.get(name) else { return None };
    let mut r = crate::Reader::new(raw);
    let n = r.count().ok()?;
    if n == 0 {
        return None;
    }
    Some(ObjRef::from_raw(r.compact().ok()?)).filter(|o| *o != ObjRef::Null)
}

#[derive(Default, Clone)]
pub struct MaterialInfo {
    pub texture: Option<Obj>,
    pub two_sided: bool,
    pub masked: bool,
    pub blended: bool,
    /// Shader Opacity texture whose alpha replaces the diffuse alpha (glass).
    pub opacity: Option<Obj>,
    /// TexPanner/TexRotator/TexOscillator wrappers on the way to the texture, outermost first.
    pub uv_anim: Vec<crate::material::Material>,
    /// A non-alpha framebuffer operation (multiply / darken / add), if any wrapper sets one.
    pub op: Option<BlendOp>,
}

/// Framebuffer operations other than alpha blending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendOp {
    /// dst * src * 2 (UE2 modulate-2x).
    Modulate,
    /// dst * (1 - src).
    Darken,
    /// dst + src.
    Brighten,
    /// src + dst * (1 - src): additive that saturates (texture alpha unused).
    Translucent,
}

/// Finds an export by name whose outer chain matches `groups` (innermost first).
fn find_by_path(pkg: &Package, leaf: &str, groups: &[&str], class: Option<&str>) -> Option<usize> {
    'outer: for (i, e) in pkg.exports.iter().enumerate() {
        if !pkg.name(e.name).eq_ignore_ascii_case(leaf) {
            continue;
        }
        if let Some(c) = class {
            if !pkg.export_class(i).eq_ignore_ascii_case(c) {
                continue;
            }
        }
        let mut outer = e.outer;
        for g in groups {
            match outer {
                ObjRef::Export(o) if pkg.name(pkg.exports[o].name).eq_ignore_ascii_case(g) => outer = pkg.exports[o].outer,
                _ => continue 'outer,
            }
        }
        if outer == ObjRef::Null {
            return Some(i);
        }
    }
    None
}
