//! Character appearance from UnrealScript class defaults.
//!
//! A pawn's look comes from its class defaults (merged down the inheritance chain) plus the
//! properties saved on the placed actor: `Mesh`, `Skins`, `HeadMesh`, `HeadSkin`, `HeadScale`,
//! `HeadBone`, the `ADJUST_RELATIVE_HEAD_*` offsets and `CoreMeshAnim`. Classes that randomize
//! their look list candidate skins in `ChameleonSkins` / `ChamelHeadSkins`. Those skin names
//! encode the choice: `GR__NNN__MeshName`, where G is the gender (M/F), R the race letter, NNN a
//! unique id and MeshName the mesh the skin was painted for. Bodies and heads are paired by
//! matching gender, race and build (meshes whose name starts with "Fat" are fat).

use std::collections::HashMap;
use std::sync::Arc;
use upkg::library::{Library, Obj};
use upkg::{ObjRef, Package, Properties, Reader, Value};
use uscript::ScriptObject;

/// Property lists from most specific (placed actor) to least specific (root class), each with
/// the package its references resolve in.
#[derive(Clone, Default)]
pub struct PropStack {
    pub layers: Vec<(Arc<Package>, Properties)>,
    /// Class names from most derived to root, e.g. ["Bystanders", "Bystander", ..., "Actor", "Object"].
    pub chain: Vec<String>,
}

impl PropStack {
    fn find(&self, name: &str, index: u32) -> Option<(&Arc<Package>, &Value)> {
        self.layers.iter().find_map(|(p, props)| props.get_at(name, index).map(|v| (p, v)))
    }

    pub fn has(&self, name: &str) -> bool {
        self.find(name, 0).is_some()
    }

    pub fn object(&self, lib: &Library, name: &str) -> Option<Obj> {
        match self.find(name, 0)? {
            (p, Value::Object(r)) => lib.resolve(p, *r),
            _ => None,
        }
    }

    pub fn object_is_set(&self, name: &str) -> bool {
        matches!(self.find(name, 0), Some((_, Value::Object(r))) if *r != ObjRef::Null)
    }

    pub fn float(&self, name: &str) -> Option<f32> {
        match self.find(name, 0)? {
            (_, Value::Float(f)) => Some(*f),
            _ => None,
        }
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.find(name, 0)? {
            (_, Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn name(&self, name: &str) -> Option<String> {
        match self.find(name, 0)? {
            (_, Value::Name(n)) => Some(n.clone()),
            _ => None,
        }
    }

    pub fn vector(&self, name: &str) -> Option<[f32; 3]> {
        match self.find(name, 0)? {
            (_, Value::Vector(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn rotator(&self, name: &str) -> Option<[i32; 3]> {
        match self.find(name, 0)? {
            (_, Value::Rotator(r)) => Some(*r),
            _ => None,
        }
    }

    pub fn byte(&self, name: &str) -> Option<u8> {
        match self.find(name, 0)? {
            (_, Value::Byte(b)) => Some(*b),
            _ => None,
        }
    }

    /// Dynamic array of object references (e.g. `Skins`). Entries that fail to resolve are None.
    pub fn object_array(&self, lib: &Library, name: &str) -> Vec<Option<Obj>> {
        let Some((p, Value::Array { raw })) = self.find(name, 0) else { return vec![] };
        let mut r = Reader::new(raw);
        let Ok(n) = r.count() else { return vec![] };
        (0..n).map_while(|_| r.compact().ok()).map(|i| lib.resolve(p, ObjRef::from_raw(i))).collect()
    }

    /// Dynamic array of names (e.g. `ChameleonSkins`).
    pub fn name_array(&self, name: &str) -> Vec<String> {
        let Some((p, Value::Array { raw })) = self.find(name, 0) else { return vec![] };
        let mut r = Reader::new(raw);
        let Ok(n) = r.count() else { return vec![] };
        (0..n).map_while(|_| r.compact().ok()).filter_map(|i| p.names.get(i as usize).cloned()).collect()
    }

    /// Dynamic array of strings (e.g. `ChameleonMeshPkgs`).
    pub fn string_array(&self, name: &str) -> Vec<String> {
        let Some((_, Value::Array { raw })) = self.find(name, 0) else { return vec![] };
        let mut r = Reader::new(raw);
        let Ok(n) = r.count() else { return vec![] };
        (0..n).map_while(|_| r.fstring().ok()).collect()
    }

    pub fn is_a(&self, class: &str) -> bool {
        self.chain.iter().any(|c| c.eq_ignore_ascii_case(class))
    }
}

/// Resolves and caches class defaults (with inheritance) for script classes.
#[derive(Default)]
pub struct ClassDb {
    cache: HashMap<(String, usize), Option<Arc<PropStack>>>,
}

impl ClassDb {
    /// Defaults for a class object (an export of class "Class" in a script package).
    pub fn defaults(&mut self, lib: &Library, class: &Obj) -> Option<Arc<PropStack>> {
        let key = class.key();
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        let v = build_stack(lib, class).map(Arc::new);
        self.cache.insert(key, v.clone());
        v
    }

    /// Defaults for a class named "Package.Class" (or a bare class name searched in common
    /// gameplay packages).
    pub fn defaults_by_name(&mut self, lib: &Library, name: &str) -> Option<Arc<PropStack>> {
        let obj = find_class(lib, name)?;
        self.defaults(lib, &obj)
    }
}

/// Script packages searched when a class name is given without its package.
pub const CLASS_PACKAGES: &[&str] = &["People", "BasePeople", "AWPawns", "Postal2Game", "AWGame", "GameTypes", "MoreGame", "EDStuff", "AWPStuff", "Postal2Holidays", "P2R", "FPSGame", "Inventory", "AWInventory", "Engine"];

pub fn find_class(lib: &Library, name: &str) -> Option<Obj> {
    let (pkgs, leaf): (Vec<&str>, &str) = match name.split_once('.') {
        Some((p, l)) => (vec![p], l),
        None => (CLASS_PACKAGES.to_vec(), name),
    };
    pkgs.iter().find_map(|p| {
        let pkg = lib.package(p)?;
        let e = pkg.find_export(leaf, Some("Class"))?;
        Some(Obj { pkg, export: e })
    })
}

fn build_stack(lib: &Library, class: &Obj) -> Option<PropStack> {
    let mut stack = PropStack::default();
    let mut cur = Some(class.clone());
    while let Some(c) = cur.take() {
        if stack.chain.len() > 40 {
            break;
        }
        let Ok(Some(ScriptObject::Class(data))) = uscript::parse_export(&c.pkg, c.export) else { break };
        stack.chain.push(c.name().to_string());
        stack.layers.push((c.pkg.clone(), data.defaults.clone()));
        cur = lib.resolve(&c.pkg, data.state.base.super_);
    }
    (!stack.chain.is_empty()).then_some(stack)
}

/// A parsed chameleon skin name (`GR__NNN__MeshName`).
#[derive(Debug, Clone, PartialEq)]
pub struct SkinCode {
    pub gender: char,
    pub race: char,
    pub id: u32,
    pub mesh: String,
}

impl SkinCode {
    pub fn parse(full: &str) -> Option<Self> {
        // Body skins use a 2-letter prefix (gender, race), head skins a 3-letter one (gender,
        // race, build A/F); the unique id is always 3 digits between double underscores.
        let item = full.rsplit('.').next()?;
        let s1 = item.find("__")?;
        if !(2..=3).contains(&s1) || item.len() < s1 + 7 || &item[s1 + 5..s1 + 7] != "__" {
            return None;
        }
        let b = item.as_bytes();
        let gender = (b[0] as char).to_ascii_uppercase();
        let race = (b[1] as char).to_ascii_uppercase();
        if !matches!(gender, 'M' | 'F') || !"WBMAHF".contains(race) {
            return None;
        }
        let id = item[s1 + 2..s1 + 5].parse().ok()?;
        Some(SkinCode { gender, race, id, mesh: item[s1 + 7..].to_string() })
    }

    pub fn fat(&self) -> bool {
        self.mesh.to_ascii_lowercase().starts_with("fat")
    }
}

/// Finds an object by a dotted name ("Package.Group.Name"), trying the leaf in the package.
pub fn find_named(lib: &Library, full: &str, class: Option<&str>) -> Option<Obj> {
    let (pkg_name, rest) = full.split_once('.')?;
    let pkg = lib.package(pkg_name)?;
    let leaf = rest.rsplit('.').next()?;
    let e = pkg.find_export(leaf, class)?;
    Some(Obj { pkg, export: e })
}

fn find_mesh(lib: &Library, pkgs: &[String], fallback: &[&str], name: &str) -> Option<Obj> {
    pkgs.iter()
        .map(|s| s.as_str())
        .chain(fallback.iter().copied())
        .find_map(|p| find_named(lib, &format!("{p}.{name}"), Some("SkeletalMesh")))
}

#[derive(Clone)]
pub struct HeadPart {
    pub mesh: Obj,
    pub skin: Option<Obj>,
    pub bone: String,
    /// Relative location to the head bone (Unreal units).
    pub offset: [f32; 3],
    /// Relative rotation to the head bone.
    pub rotation: [i32; 3],
    pub scale: [f32; 3],
}

#[derive(Clone)]
pub struct Appearance {
    pub class_name: String,
    pub body: Obj,
    /// Material override per material index (None keeps the mesh's own material).
    pub body_skins: Vec<Option<Obj>>,
    pub head: Option<HeadPart>,
    /// Animation sets to try, in priority order.
    pub anim_sets: Vec<Obj>,
    pub draw_scale: [f32; 3],
    /// Chameleon code of the chosen body skin, when known.
    pub body_code: Option<SkinCode>,
}

/// Small deterministic hash so each placed actor keeps the same random look between runs.
pub fn seed_of(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Works out body, skins and head for a pawn from its merged property stack.
pub fn resolve_appearance(lib: &Library, stack: &PropStack, seed: u64) -> Option<Appearance> {
    let mut body = stack.object(lib, "Mesh").filter(|o| o.class().eq_ignore_ascii_case("SkeletalMesh"))?;
    let mut body_skins = stack.object_array(lib, "Skins");
    let mesh_pkgs = stack.name_array("ChameleonMeshPkgs");
    let head_mesh_pkgs = stack.name_array("ChamelHeadMeshPkgs");

    // Body: chameleon randomization unless switched off for this class/actor.
    let mut body_code = None;
    let candidates: Vec<(String, SkinCode)> = stack.name_array("ChameleonSkins").into_iter().filter_map(|n| SkinCode::parse(&n).map(|c| (n, c))).collect();
    let randomize = !candidates.is_empty() && stack.bool("bStartupRandomization") != Some(false);
    if randomize {
        let start = (seed % candidates.len() as u64) as usize;
        for k in 0..candidates.len() {
            let (name, code) = &candidates[(start + k) % candidates.len()];
            let (Some(skin), Some(mesh)) = (find_named(lib, name, None), find_mesh(lib, &mesh_pkgs, &["Characters"], &code.mesh)) else { continue };
            body = mesh;
            body_skins = vec![Some(skin)];
            body_code = Some(code.clone());
            break;
        }
    }
    if body_code.is_none() {
        body_code = body_skins.first().and_then(|s| s.as_ref()).and_then(|s| SkinCode::parse(s.name()));
    }

    // Head.
    let mut head = None;
    if stack.object_is_set("HeadClass") {
        let explicit = (!randomize).then(|| stack.object(lib, "HeadMesh")).flatten().filter(|o| o.class().eq_ignore_ascii_case("SkeletalMesh"));
        let (mesh, skin) = match explicit {
            Some(m) => (Some(m), stack.object(lib, "HeadSkin")),
            None => match pick_head(lib, stack, &head_mesh_pkgs, body_code.as_ref(), seed) {
                Some((m, s)) => (Some(m), s),
                None => (None, None),
            },
        };
        let skin = skin;
        if let Some(mesh) = mesh {
            let offset = [stack.float("ADJUST_RELATIVE_HEAD_X").unwrap_or(0.0), stack.float("ADJUST_RELATIVE_HEAD_Y").unwrap_or(0.0), stack.float("ADJUST_RELATIVE_HEAD_Z").unwrap_or(0.0)];
            head = Some(HeadPart {
                mesh,
                skin,
                bone: stack.name("HeadBone").unwrap_or_else(|| "MALE01 head".into()),
                offset,
                // Heads are authored facing a different axis than the neck bone; the game turns
                // them by this fixed relative rotation when attaching.
                rotation: [0, -16384, 16384],
                scale: stack.vector("HeadScale").unwrap_or([1.0; 3]),
            });
        }
    }

    let mut anim_sets = Vec::new();
    if let Some(a) = stack.object(lib, "CoreMeshAnim") {
        anim_sets.push(a);
    }
    let ds = stack.float("DrawScale").unwrap_or(1.0);
    let ds3 = stack.vector("DrawScale3D").unwrap_or([1.0; 3]);
    Some(Appearance {
        class_name: stack.chain.first().cloned().unwrap_or_default(),
        body,
        body_skins,
        head,
        anim_sets,
        draw_scale: [ds3[0] * ds, ds3[1] * ds, ds3[2] * ds],
        body_code,
    })
}

/// Picks a head skin from `ChamelHeadSkins` matching the body's gender, race and build.
fn pick_head(lib: &Library, stack: &PropStack, pkgs: &[String], body: Option<&SkinCode>, seed: u64) -> Option<(Obj, Option<Obj>)> {
    let heads: Vec<(String, SkinCode)> = stack.name_array("ChamelHeadSkins").into_iter().filter_map(|n| SkinCode::parse(&n).map(|c| (n, c))).collect();
    if std::env::var_os("P2ANIM_DEBUG").is_some() {
        eprintln!("head candidates={} pkgs={pkgs:?} body={body:?}", heads.len());
    }
    let filters: [&dyn Fn(&SkinCode) -> bool; 4] = [
        &|h| body.is_some_and(|b| h.gender == b.gender && h.race == b.race && h.fat() == b.fat()),
        &|h| body.is_some_and(|b| h.gender == b.gender && h.race == b.race),
        &|h| body.is_some_and(|b| h.gender == b.gender),
        &|_| true,
    ];
    for f in filters {
        let pool: Vec<&(String, SkinCode)> = heads.iter().filter(|(_, c)| f(c)).collect();
        if pool.is_empty() {
            continue;
        }
        let start = ((seed >> 17) % pool.len() as u64) as usize;
        for k in 0..pool.len() {
            let (name, code) = pool[(start + k) % pool.len()];
            if let Some(mesh) = find_mesh(lib, pkgs, &["heads", "MoreHeads"], &code.mesh) {
                return Some((mesh, find_named(lib, name, None)));
            }
        }
    }
    None
}
