//! Level instantiation: turns the exports of a map package into VM objects.
//!
//! Every export whose class is a loadable script class becomes a VM object: placed actors, the
//! LevelInfo, and the script subobjects that actors point at (Matinee actions, ScriptedTrigger
//! actions, ...). Each object starts from its class defaults (with `config` values from the boot
//! ini applied to those defaults) and is then overlaid with the tagged properties saved in the
//! map. Object references are resolved in a second pass so actors can point at each other, at the
//! LevelInfo, at the Level, and at objects in other packages:
//! - export in this map that became a VM object: `Value::Object(Some(id))`
//! - class reference: `Value::Class(Some(class))` loaded through the VM
//! - anything else (textures, meshes, sounds, ...): `Value::Resource("Package.Group.Name")`,
//!   after checking through the package tables that the target really exists.
//!
//! Struct property values are stored either as tagged property lists or, for engine structs such
//! as `PointRegion`, as raw binary in field order. The decoder tries tagged first and falls back to
//! binary, and only accepts a decode that consumes the value exactly.

use crate::config::BootConfig;
use crate::ini::ClassRef;
use crate::pkgindex::PackageFiles;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use upkg::{ObjRef, Package, Reader, begin_object};
use uscript::vm::{ClassId, ObjId, Ty, Value, Vm};

#[derive(Debug, Default, Clone)]
pub struct LevelStats {
    pub exports: usize,
    pub actors: usize,
    pub subobjects: usize,
    /// Exports of non-script classes (Model, Polys, StaticMeshInstance data, ...).
    pub native_only: usize,
    pub distinct_classes: usize,
    pub props_applied: usize,
    /// Saved properties with no matching slot in the class.
    pub props_unknown: usize,
    /// Struct/array values that could not be decoded exactly.
    pub props_undecoded: usize,
    pub refs_internal: usize,
    pub refs_external: usize,
    pub refs_unresolved: usize,
    pub classes_unresolved: usize,
    pub prop_parse_failures: usize,
}

pub struct Level {
    pub map: Arc<Package>,
    /// `vm.objects.len()` before this level was instantiated (objects from here on belong to it).
    pub base_objects: usize,
    /// Export index to VM object, for every instantiated export.
    pub objects: HashMap<usize, ObjId>,
    /// Actor objects in export order (the engine's actor list order).
    pub actors: Vec<(usize, ObjId)>,
    pub level_info: Option<ObjId>,
    pub xlevel: Option<ObjId>,
    pub class_counts: BTreeMap<String, usize>,
    pub unresolved: BTreeMap<String, usize>,
    pub stats: LevelStats,
}

/// Shared state used while loading levels: the boot config, package tables and class caches.
pub struct World {
    pub cfg: BootConfig,
    pub vm: Vm,
    pub files: PackageFiles,
    actor_class: ClassId,
    class_cache: HashMap<(String, String), Option<ClassId>>,
    lib: Option<Arc<upkg::library::Library>>,
}

impl World {
    pub fn new(cfg: BootConfig) -> Result<Self, String> {
        let mut vm = Vm::new(&cfg.system_dir).map_err(|e| e.to_string())?;
        // The shipped inis are the VM's base config layer; SaveConfig goes to the VM's user dir.
        vm.set_config_source(Box::new(crate::config::IniConfig::new(&cfg)));
        let files = PackageFiles::new(&cfg.system_dir, &cfg.paths);
        let actor_class = vm.find_class("Engine", "Actor").map_err(|e| e.to_string())?;
        Ok(Self { cfg, vm, files, actor_class, class_cache: HashMap::new(), lib: None })
    }

    pub fn is_actor_class(&self, c: ClassId) -> bool {
        self.vm.is_child_of(c, self.actor_class)
    }

    /// The content library over the game install (shared, created on first use).
    pub fn library(&mut self) -> Arc<upkg::library::Library> {
        if self.lib.is_none() {
            let root = self.cfg.system_dir.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let lib = upkg::library::Library::new(&root).unwrap_or_else(|e| panic!("cannot index {}: {e}", root.display()));
            self.lib = Some(Arc::new(lib));
        }
        self.lib.clone().unwrap()
    }

    /// Finds a script class by package and name, following `[PackageRemap]`.
    pub fn class(&mut self, package: &str, name: &str) -> Option<ClassId> {
        let key = (package.to_ascii_lowercase(), name.to_ascii_lowercase());
        if let Some(c) = self.class_cache.get(&key) {
            return *c;
        }
        let mut found = None;
        for p in self.cfg.package_candidates(package) {
            if let Ok(c) = self.vm.find_class(&p, name) {
                found = Some(c);
                break;
            }
        }
        if let Some(c) = found {
            self.vm.apply_class_config(c);
        }
        self.class_cache.insert(key, found);
        found
    }

    pub fn class_ref(&mut self, r: &ClassRef) -> Option<ClassId> {
        self.class(&r.package, &r.name)
    }

    /// Package name of the script package defining a class.
    pub fn class_package(&self, c: ClassId) -> String {
        let f = self.vm.classes[c].field;
        self.vm.pkgs[f.pkg as usize].pkg.name.clone()
    }

    pub fn class_path(&self, c: ClassId) -> String {
        format!("{}.{}", self.class_package(c), self.vm.classes[c].name)
    }

    /// Class of an import reference (`Package.Name` chain) made from a map.
    fn import_class(&mut self, pkg: &Package, r: ObjRef) -> Option<ClassId> {
        let ObjRef::Import(i) = r else { return None };
        let name = pkg.name(pkg.imports[i].name).to_string();
        let mut root = pkg.imports[i].outer;
        while let ObjRef::Import(j) = root {
            if pkg.imports[j].outer == ObjRef::Null {
                break;
            }
            root = pkg.imports[j].outer;
        }
        let package = pkg.ref_name(root).to_string();
        self.class(&package, &name)
    }

    /// Instantiates every script-class export of a map.
    pub fn load_level(&mut self, map: Arc<Package>) -> Level {
        let mut lv = Level {
            map: map.clone(),
            base_objects: self.vm.objects.len(),
            objects: HashMap::new(),
            actors: Vec::new(),
            level_info: None,
            xlevel: None,
            class_counts: BTreeMap::new(),
            unresolved: BTreeMap::new(),
            stats: LevelStats { exports: map.exports.len(), ..Default::default() },
        };
        // Pass 1: allocate objects from class defaults.
        let level_info_class = self.class("Engine", "LevelInfo");
        for (i, e) in map.exports.iter().enumerate() {
            let class = match e.class {
                ObjRef::Import(_) => self.import_class(&map, e.class),
                ObjRef::Export(_) => {
                    // A class defined inside the map itself (MyLevel script).
                    lv.stats.classes_unresolved += 1;
                    *lv.unresolved.entry(format!("map-local class {}", map.ref_name(e.class))).or_default() += 1;
                    continue;
                }
                ObjRef::Null => {
                    lv.stats.native_only += 1;
                    continue;
                }
            };
            let Some(c) = class else {
                let cls = map.class_of(e.class).to_string();
                let cname = map.ref_name(e.class).to_string();
                // Native-only engine types are expected; anything else is a missing script class.
                if is_native_only(&cname) || cls != "Class" {
                    lv.stats.native_only += 1;
                } else {
                    lv.stats.classes_unresolved += 1;
                    *lv.unresolved.entry(format!("class {}", map.full_path(e.class))).or_default() += 1;
                }
                continue;
            };
            let o = self.vm.new_object(c);
            self.vm.set_prop(o, "Name", Value::Name(map.name(e.name).to_string()));
            self.vm.set_prop(o, "Class", Value::Class(Some(c)));
            lv.objects.insert(i, o);
            *lv.class_counts.entry(self.class_path(c)).or_default() += 1;
            if self.is_actor_class(c) {
                lv.actors.push((i, o));
                if level_info_class.is_some_and(|li| self.vm.is_child_of(c, li)) && lv.level_info.is_none() {
                    lv.level_info = Some(o);
                }
            } else {
                lv.stats.subobjects += 1;
                if self.vm.classes[c].name.eq_ignore_ascii_case("Level") {
                    lv.xlevel = Some(o);
                }
            }
        }
        lv.stats.actors = lv.actors.len();
        lv.stats.distinct_classes = lv.class_counts.len();
        // Pass 2: overlay saved properties.
        let mut ids: Vec<(usize, ObjId)> = lv.objects.iter().map(|(&k, &v)| (k, v)).collect();
        ids.sort();
        for (i, o) in ids {
            let props = match begin_object(&map, i) {
                Ok((_, p)) => p,
                Err(_) => {
                    lv.stats.prop_parse_failures += 1;
                    continue;
                }
            };
            if let Some(e) = map.exports[i].outer.export_index() {
                if let Some(&outer) = lv.objects.get(&e) {
                    self.vm.set_prop(o, "Outer", Value::Object(Some(outer)));
                }
            }
            let class = self.vm.objects[o].class;
            for p in &props.0 {
                let Some(slot) = self.vm.classes[class].slot(&p.name) else {
                    lv.stats.props_unknown += 1;
                    continue;
                };
                let ty = self.vm.classes[class].slots[slot].ty.clone();
                let elem_ty = match &ty {
                    Ty::Fixed(t, _) => (**t).clone(),
                    t => t.clone(),
                };
                let mut cx = Cx { w: self, map: &map, lv: &mut lv };
                let Some(v) = cx.convert(&p.value, &elem_ty) else {
                    lv.stats.props_undecoded += 1;
                    continue;
                };
                lv.stats.props_applied += 1;
                let cell = &mut self.vm.objects[o].slots[slot];
                match (&ty, cell) {
                    (Ty::Fixed(..), Value::Array(a)) => {
                        if let Some(e) = a.get_mut(p.index as usize) {
                            *e = v;
                        }
                    }
                    (_, cell) => *cell = v,
                }
            }
            if let Some(x) = lv.xlevel {
                if self.is_actor_class(class) {
                    self.vm.set_prop(o, "XLevel", Value::Object(Some(x)));
                }
            }
        }
        // The engine points every actor's Level at the level's LevelInfo on load. Some maps carry
        // stray extra LevelInfos (Brewery: LevelInfo11) or unset refs; those actors then saw
        // Level.Game == None (Actor.PreBeginPlay Accessed None on ~70 maps).
        if let Some(li) = lv.level_info {
            for &(_, a) in &lv.actors {
                self.vm.set_prop(a, "Level", Value::Object(Some(li)));
            }
        }
        lv
    }
}

trait ExportIndex {
    fn export_index(self) -> Option<usize>;
}
impl ExportIndex for ObjRef {
    fn export_index(self) -> Option<usize> {
        match self {
            ObjRef::Export(i) => Some(i),
            _ => None,
        }
    }
}

/// Engine classes that only carry native data (no script-visible object needed for gameplay).
fn is_native_only(class: &str) -> bool {
    matches!(
        class.to_ascii_lowercase().as_str(),
        "level" | "package" | "kmeshprops" | "model" | "polys" | "staticmeshinstance" | "levelsummary" | "texture" | "shader" | "combiner" | "finalblend" | "texpanner" | "texrotator"
            | "texscaler" | "texoscillator" | "constantcolor" | "fadecolor" | "texenvmap" | "cubemap" | "staticmesh" | "sound" | "palette"
            | "terrainsector" | "terrainprimitive" | "convexvolume" | "projectorprimitive" | "bspnode"
    )
}

struct Cx<'a> {
    w: &'a mut World,
    map: &'a Arc<Package>,
    lv: &'a mut Level,
}

impl Cx<'_> {
    fn object_ref(&mut self, r: ObjRef, ty: &Ty) -> Value {
        match r {
            ObjRef::Null => {
                if matches!(ty, Ty::Class) {
                    Value::Class(None)
                } else {
                    Value::Object(None)
                }
            }
            ObjRef::Export(i) => {
                self.lv.stats.refs_internal += 1;
                match self.lv.objects.get(&i) {
                    Some(&o) => Value::Object(Some(o)),
                    None => Value::Resource(self.map.full_path(r)),
                }
            }
            ObjRef::Import(_) => {
                if matches!(ty, Ty::Class) || self.map.class_of(r) == "Class" {
                    let c = self.w.import_class(self.map, r);
                    if c.is_none() {
                        self.lv.stats.refs_unresolved += 1;
                        *self.lv.unresolved.entry(format!("class ref {}", self.map.full_path(r))).or_default() += 1;
                    } else {
                        self.lv.stats.refs_external += 1;
                    }
                    return Value::Class(c);
                }
                match self.w.files.resolve_import(self.map, r) {
                    Ok(path) => {
                        self.lv.stats.refs_external += 1;
                        Value::Resource(path)
                    }
                    Err(e) => {
                        self.lv.stats.refs_unresolved += 1;
                        *self.lv.unresolved.entry(e).or_default() += 1;
                        Value::Resource(self.map.full_path(r))
                    }
                }
            }
        }
    }

    fn convert(&mut self, v: &upkg::Value, ty: &Ty) -> Option<Value> {
        Some(match v {
            upkg::Value::Byte(b) => Value::Byte(*b),
            upkg::Value::Int(i) => Value::Int(*i),
            upkg::Value::Bool(b) => Value::Bool(*b),
            upkg::Value::Float(f) => Value::Float(*f),
            upkg::Value::Name(n) => Value::Name(n.clone()),
            upkg::Value::Str(s) => Value::Str(s.clone()),
            upkg::Value::Vector(v) => Value::Vector(*v),
            upkg::Value::Rotator(r) => Value::Rotator(*r),
            upkg::Value::Object(r) => self.object_ref(*r, ty),
            upkg::Value::Color(c) => match ty {
                Ty::Struct(fs) if fs.len() == 4 => Value::Struct(fs.iter().zip(c).map(|((n, _), b)| (n.clone(), Value::Byte(*b))).collect()),
                _ => return None,
            },
            upkg::Value::Struct { raw, .. } => self.decode_exact(raw, ty)?,
            upkg::Value::Array { raw } => self.decode_exact(raw, ty)?,
            upkg::Value::Unknown { .. } => return None,
        })
    }

    /// Decodes a raw struct or array value, requiring every byte to be consumed.
    fn decode_exact(&mut self, raw: &[u8], ty: &Ty) -> Option<Value> {
        // Tagged struct first (non-native structs), then binary field layout.
        if let Ty::Struct(fields) = ty {
            let mut r = Reader::new(raw);
            if let Ok(props) = upkg::read_properties(&mut r, self.map) {
                if r.remaining() == 0 && props.0.iter().all(|p| fields.iter().any(|(n, _)| n.eq_ignore_ascii_case(&p.name))) {
                    let mut out: Vec<(String, Value)> = fields.iter().map(|(n, t)| (n.clone(), t.zero())).collect();
                    for p in &props.0 {
                        let k = out.iter().position(|(n, _)| n.eq_ignore_ascii_case(&p.name))?;
                        let ft = fields[k].1.clone();
                        let et = match &ft {
                            Ty::Fixed(t, _) => (**t).clone(),
                            t => t.clone(),
                        };
                        let v = self.convert(&p.value, &et)?;
                        match &mut out[k].1 {
                            Value::Array(a) if matches!(ft, Ty::Fixed(..)) => {
                                if let Some(c) = a.get_mut(p.index as usize) {
                                    *c = v;
                                }
                            }
                            cell => *cell = v,
                        }
                    }
                    return Some(Value::Struct(out));
                }
            }
        }
        let mut r = Reader::new(raw);
        let v = self.bin(&mut r, ty, 0)?;
        (r.remaining() == 0).then_some(v)
    }

    fn bin(&mut self, r: &mut Reader, ty: &Ty, depth: u32) -> Option<Value> {
        if depth > 8 {
            return None;
        }
        Some(match ty {
            Ty::Byte => Value::Byte(r.u8().ok()?),
            Ty::Int => Value::Int(r.i32().ok()?),
            Ty::Bool => Value::Bool(r.u32().ok()? != 0),
            Ty::Float => Value::Float(r.f32().ok()?),
            Ty::Name => Value::Name(self.map.names.get(usize::try_from(r.compact().ok()?).ok()?)?.clone()),
            Ty::Str => Value::Str(r.fstring().ok()?),
            Ty::Object | Ty::Class => {
                let o = ObjRef::from_raw(r.compact().ok()?);
                if let ObjRef::Export(i) = o {
                    if i >= self.map.exports.len() {
                        return None;
                    }
                }
                if let ObjRef::Import(i) = o {
                    if i >= self.map.imports.len() {
                        return None;
                    }
                }
                self.object_ref(o, ty)
            }
            Ty::Vector => Value::Vector(r.vec3().ok()?),
            Ty::Rotator => Value::Rotator([r.i32().ok()?, r.i32().ok()?, r.i32().ok()?]),
            Ty::Struct(fs) => {
                // Structs nested in arrays are tagged lists; try that before the binary layout.
                let save = r.pos();
                let tagged = if depth > 0 { upkg::read_properties(r, self.map).ok() } else { None };
                if let Some(props) = tagged {
                    if props.0.iter().all(|p| fs.iter().any(|(n, _)| n.eq_ignore_ascii_case(&p.name))) {
                        let mut out: Vec<(String, Value)> = fs.iter().map(|(n, t)| (n.clone(), t.zero())).collect();
                        let mut ok = true;
                        for p in &props.0 {
                            let k = out.iter().position(|(n, _)| n.eq_ignore_ascii_case(&p.name)).unwrap();
                            let et = match &fs[k].1 {
                                Ty::Fixed(t, _) => (**t).clone(),
                                t => t.clone(),
                            };
                            match self.convert(&p.value, &et) {
                                Some(v) => match &mut out[k].1 {
                                    Value::Array(a) if matches!(fs[k].1, Ty::Fixed(..)) => {
                                        if let Some(c) = a.get_mut(p.index as usize) {
                                            *c = v;
                                        }
                                    }
                                    cell => *cell = v,
                                },
                                None => ok = false,
                            }
                        }
                        if ok {
                            return Some(Value::Struct(out));
                        }
                    }
                }
                r.seek(save);
                let mut out = Vec::with_capacity(fs.len());
                for (n, t) in fs {
                    out.push((n.clone(), self.bin(r, t, depth + 1)?));
                }
                Value::Struct(out)
            }
            Ty::Fixed(t, n) => {
                let mut a = Vec::with_capacity(*n);
                for _ in 0..*n {
                    a.push(self.bin(r, t, depth + 1)?);
                }
                Value::Array(a)
            }
            Ty::Array(t) => {
                let n = r.count().ok()?;
                let mut a = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    a.push(self.bin(r, t, depth + 1)?);
                }
                Value::Array(a)
            }
            Ty::Delegate | Ty::Other => return None,
        })
    }
}
