//! Save games: `SaveGame N` writes the live level to a `.usa` package, `LoadGame N` / `?load=N`
//! restores it into the VM. Both our saves and the original game's saves load.
//!
//! # File layout written (matching the original saves closely)
//!
//! A UE2 package (version 118, licensee 2) named after the slot (`Save3`), written with
//! [`upkg::writer`]:
//! - Export 0 `LevelInfo0` (the level's LevelInfo), then `MyLevel` (class `Engine.Level`, no
//!   native data from us; the original stores the actor list and URL there), then every live
//!   actor in actor-list order, then every non-actor VM object they reference (subobjects such as
//!   ScriptedAction lists, the GameState, inventory, controllers' non-actor helpers). Actors are
//!   outered to `MyLevel`, other objects to the package root.
//! - Each object: script state frame when it is in a state (flag 0x02000000: state node, state,
//!   probe mask, latent action number, code offset), then a tagged property list of every
//!   property whose value differs from the class default (transient / native properties are
//!   skipped, as the engine does), terminated by `None`.
//! - Class references and assets (meshes, textures, sounds) are imports: `Package.Class` with class
//!   `Core.Class`, assets with their class read from the package table where known.
//! - Values: the regular tag encodings; structs are written as nested tagged lists (except Vector
//!   / Rotator / Color, which are binary as the engine writes them); dynamic arrays as count plus
//!   elements (structs inside arrays as tagged lists).
//!
//! Next to the package we keep a small text sidecar (`SaveN.p2r`) with what the package format
//! has no place for in our engine: latent action arguments and elapsed time, our statement index
//! of state code, and the map the level came from. Original saves have no sidecar; their state
//! code restarts from the state's `Begin` label (the engine resumes at the stored code offset; we
//! map that offset back to our statement index when it matches one).
//!
//! Slot names go to `SavedGameInfo.ini` in the user directory with the same `Slots=(...)` syntax
//! (see `p2save`), never to the install.
//!
//! # Loading
//!
//! The package is opened as the level: the original map supplies static geometry (looked up from
//! `LevelInfo.Title`'s map file through the `Level.URL` stored by the engine or our sidecar), and
//! the save's exports become the VM objects instead of the map's. Every object is created from its
//! class defaults and overlaid with the saved properties (the same decoding as map loading), state
//! frames are restored without `BeginState`, `bScriptInitialized` stays as saved so BeginPlay is
//! not rerun, then `PostLoadGame` is sent to every actor (as the engine does).

use crate::boot::Game;
use crate::host::GameHost;
use crate::level::{Level, LevelStats};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use upkg::writer::{Bytes, PackageWriter, RF_HAS_STACK, RF_LOAD, T_ARRAY, T_BOOL, T_BYTE, T_FLOAT, T_INT, T_NAME, T_OBJECT, T_STR, T_STRUCT, raw_ref, write_none, write_tag};
use upkg::{ObjRef, Package};
use uscript::host::Host;
use uscript::objects::prop_flags;
use uscript::vm::{ClassId, ObjId, Ty, Value, Vm};

/// Where saves go (never the install): `<user dir>\Save`.
pub fn user_save_dir() -> PathBuf {
    uscript::config::default_user_dir().unwrap_or_else(|| PathBuf::from("user")).join("Save")
}

pub fn slot_path(dir: &Path, slot: i32) -> PathBuf {
    dir.join(format!("Save{slot}.usa"))
}

#[derive(Debug, Default, Clone)]
pub struct SaveStats {
    pub objects: usize,
    pub actors: usize,
    pub properties: usize,
    pub states: usize,
    pub latents: usize,
    pub imports_skipped: usize,
    pub bytes: usize,
}

#[derive(Debug, Default, Clone)]
pub struct LoadStats {
    pub exports: usize,
    pub objects: usize,
    pub actors: usize,
    pub props_applied: usize,
    pub props_unknown: usize,
    pub props_undecoded: usize,
    pub states: usize,
    pub states_resumed_at_code: usize,
    pub latents: usize,
    pub classes_unresolved: usize,
    pub map: String,
}

// ---------------------------------------------------------------------------------------------
// Writing

/// Builds the package for the current level of `game`.
pub fn write_save(game: &Game, slot_name: &str) -> (Vec<u8>, String, SaveStats) {
    write_save_with(game, slot_name, std::env::var_os("P2GAME_SAVE_NO_CACHE").is_none())
}

/// [`write_save`] with the resource lookup cache on or off (same bytes either way).
pub fn write_save_with(game: &Game, slot_name: &str, use_cache: bool) -> (Vec<u8>, String, SaveStats) {
    let vm = &game.world.vm;
    let lap0 = std::time::Instant::now();
    let mut res_cache: HashMap<String, i32> = HashMap::new();
    let mut pkg_index: HashMap<String, Option<PkgIndex>> = HashMap::new();
    let mut st = SaveStats::default();
    let mut w = PackageWriter::new();
    let mut sidecar = String::new();
    // A loaded level's package is SaveN. Geometry still comes from the map
    // installed on the host; keep that identity when saving a loaded game.
    let source_map = game.host.map_name.borrow().clone();
    sidecar += &format!("map={}\n", if source_map.is_empty() { &game.level.map.name } else { &source_map });
    for (namespace, payload) in &game.save_extensions {
        sidecar += &format!("extension {} {}\n", hex_text(namespace), hex_text(payload));
    }

    // Objects to write: LevelInfo, MyLevel, actors, then referenced non-actor objects.
    let mut order: Vec<ObjId> = Vec::new();
    let mut seen: HashSet<ObjId> = HashSet::new();
    let li = game.level.level_info;
    let xlevel = game.level.xlevel;
    for o in li.into_iter().chain(xlevel) {
        if seen.insert(o) {
            order.push(o);
        }
    }
    for &a in &vm.actors {
        if !vm.objects[a].deleted && seen.insert(a) {
            order.push(a);
        }
    }
    let mut k = 0;
    while k < order.len() {
        let o = order[k];
        let mut refs = Vec::new();
        for v in &vm.objects[o].slots {
            collect_refs(v, &mut refs);
        }
        for r in refs {
            if r < vm.objects.len() && !vm.objects[r].deleted && seen.insert(r) {
                order.push(r);
            }
        }
        k += 1;
    }
    let lap_order = lap0.elapsed();
    // Names must be unique per outer; make them so (spawned actors share class-based names).
    let mut used: HashSet<String> = HashSet::new();
    let mut names: HashMap<ObjId, String> = HashMap::new();
    let mut counters: HashMap<String, usize> = HashMap::new();
    for &o in &order {
        let base = match vm.get_prop(o, "Name") {
            Some(Value::Name(n)) if !n.is_empty() && !n.eq_ignore_ascii_case("None") => n.clone(),
            _ => vm.classes[vm.objects[o].class].name.clone(),
        };
        let mut n = base.clone();
        while !used.insert(n.to_ascii_lowercase()) {
            let c = counters.entry(base.to_ascii_lowercase()).or_insert(0);
            n = format!("{base}{c}");
            *c += 1;
        }
        names.insert(o, n);
    }
    let lap_names = lap0.elapsed();
    // Exports (indices fixed before properties so references can be written).
    let mut export_of: HashMap<ObjId, usize> = HashMap::new();
    let level_ref = xlevel.map(|x| ObjRef::Export(order.iter().position(|&o| o == x).unwrap()));
    for &o in &order {
        let c = vm.objects[o].class;
        let cls = class_import(&mut w, vm, c);
        let is_actor = vm.class_is_a(c, "Actor");
        let outer = if is_actor { level_ref.unwrap_or(ObjRef::Null) } else { ObjRef::Null };
        let e = w.add_export(cls, outer, &names[&o], RF_LOAD);
        let ObjRef::Export(i) = e else { unreachable!() };
        export_of.insert(o, i);
    }
    let lap_exports = lap0.elapsed();
    // Data.
    for &o in &order {
        let i = export_of[&o];
        let mut out = Bytes::default();
        let ob = &vm.objects[o];
        let mut flags = RF_LOAD;
        if let Some(state) = ob.exec.state {
            flags |= RF_HAS_STACK;
            st.states += 1;
            let state_ref = state_import(&mut w, vm, state);
            // StateFrame: node (code owner), state, probe mask, latent action, code offset.
            let node = match ob.exec.code {
                Some((sf, _)) => state_import(&mut w, vm, sf),
                None => ObjRef::Null,
            };
            out.compact(raw_ref(node));
            out.compact(raw_ref(state_ref));
            out.u64(!ob.exec.disabled);
            out.i32(ob.exec.latent.as_ref().map(|l| l.native as i32).unwrap_or(0));
            if node != ObjRef::Null {
                let off = ob.exec.code.and_then(|(sf, idx)| stmt_offset(vm, sf, idx)).unwrap_or(0);
                out.compact(off as i32);
            }
            // Sidecar: exact resume point and latent action.
            if let Some((sf, idx)) = ob.exec.code {
                sidecar += &format!("code {} {} {}\n", names[&o], vm.field_path(sf), idx);
            }
            if let Some(l) = &ob.exec.latent {
                st.latents += 1;
                let args: Vec<String> = l.args.iter().map(|v| encode_value(vm, v, &names)).collect();
                sidecar += &format!("latent {} {} {} {} {}\n", names[&o], l.name, l.native, l.elapsed, args.join("\u{1f}"));
            }
            if ob.exec.disabled != 0 {
                sidecar += &format!("disabled {} {}\n", names[&o], ob.exec.disabled);
            }
        }
        let mut cx = WCx { vm, names: &names, export_of: &export_of, w: &mut w, game, res_cache: use_cache.then_some(&mut res_cache), pkg_index: &mut pkg_index };
        st.properties += cx.properties(o, &mut out);
        write_none(cx.w, &mut out);
        w.set_data(i, out.0, flags);
        st.objects += 1;
        if vm.class_is_a(ob.class, "Actor") {
            st.actors += 1;
        }
    }
    sidecar += &format!("slot_name={slot_name}\n");
    let lap_data = lap0.elapsed();
    let bytes = w.finish();
    if std::env::var_os("P2GAME_BOOT_TIMING").is_some() {
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        eprintln!("save build: order {:.1}, names {:.1}, exports {:.1}, data {:.1}, finish {:.1} ms; {} resource lookups {:.1} ms", ms(lap_order), ms(lap_names - lap_order), ms(lap_exports - lap_names), ms(lap_data - lap_exports), ms(lap0.elapsed() - lap_data), RES_N.with(|c| c.replace(0)), RES_NS.with(|c| c.replace(0)) as f64 / 1e6);
    }
    st.bytes = bytes.len();
    (bytes, sidecar, st)
}

thread_local! {
    static RES_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static RES_N: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn collect_refs(v: &Value, out: &mut Vec<ObjId>) {
    match v {
        Value::Object(Some(o)) => out.push(*o),
        Value::Struct(f) => f.iter().for_each(|(_, v)| collect_refs(v, out)),
        Value::Array(a) => a.iter().for_each(|v| collect_refs(v, out)),
        Value::DelegateTo { obj: Some(o), .. } => out.push(*o),
        _ => {}
    }
}

fn class_import(w: &mut PackageWriter, vm: &Vm, c: ClassId) -> ObjRef {
    let pkg = vm.classes[c].package.clone();
    let name = vm.classes[c].name.clone();
    w.import_path(&format!("{pkg}.{name}"), "Core", "Class")
}

fn state_import(w: &mut PackageWriter, vm: &Vm, f: uscript::vm::FieldId) -> ObjRef {
    // Package.Class.State
    let path = vm.field_path(f);
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() < 3 {
        return w.import_path(&path, "Core", "State");
    }
    let class = w.import_path(&parts[..parts.len() - 1].join("."), "Core", "Class");
    w.import("Core", "State", class, parts[parts.len() - 1])
}

fn stmt_offset(vm: &Vm, sf: uscript::vm::FieldId, idx: usize) -> Option<u32> {
    match vm.pkgs[sf.pkg as usize].objects.get(sf.export as usize)?.as_ref()? {
        uscript::ScriptObject::State(s) => s.base.script.stmts.get(idx).map(|s| s.offset),
        _ => None,
    }
}

/// Text encoding of a value for the sidecar (latent arguments).
fn encode_value(vm: &Vm, v: &Value, names: &HashMap<ObjId, String>) -> String {
    match v {
        Value::Void => "v".into(),
        Value::Byte(b) => format!("b{b}"),
        Value::Int(i) => format!("i{i}"),
        Value::Bool(b) => format!("z{}", *b as u8),
        Value::Float(f) => format!("f{f}"),
        Value::Name(n) => format!("n{n}"),
        Value::Str(s) => format!("s{s}"),
        Value::Vector([x, y, z]) => format!("V{x},{y},{z}"),
        Value::Rotator([p, y, r]) => format!("R{p},{y},{r}"),
        Value::Object(Some(o)) => names.get(o).map(|n| format!("o{n}")).unwrap_or_else(|| "o".into()),
        Value::Object(None) => "o".into(),
        Value::Class(Some(c)) => format!("c{}.{}", vm.classes[*c].package, vm.classes[*c].name),
        Value::Resource(p) => format!("r{p}"),
        _ => "v".into(),
    }
}

struct WCx<'a> {
    vm: &'a Vm,
    names: &'a HashMap<ObjId, String>,
    export_of: &'a HashMap<ObjId, usize>,
    w: &'a mut PackageWriter,
    game: &'a Game,
    /// Resource path -> raw import ref, per save (`resolve_path` scans the whole package). `None`
    /// when disabled (`P2GAME_SAVE_NO_CACHE`). Imports are deduplicated, so this cannot change the
    /// bytes written.
    res_cache: Option<&'a mut HashMap<String, i32>>,
    /// Per package: lowercase export name -> export indices in table order (for [`resolve_indexed`]).
    pkg_index: &'a mut HashMap<String, Option<PkgIndex>>,
}

type PkgIndex = (std::sync::Arc<Package>, HashMap<String, Vec<usize>>);

/// `host::resolve_path(lib, path, None)` through a name index: the same candidates in the same
/// order, so the same result, without scanning every export per lookup.
fn resolve_indexed(lib: &upkg::library::Library, index: &mut HashMap<String, Option<PkgIndex>>, path: &str) -> Option<(String, String)> {
    // Package contents never change for a path, so the answer is kept across saves.
    if let Some(r) = CLASS_OF.with(|c| c.borrow().get(path).cloned()) {
        return r;
    }
    let r = resolve_indexed_uncached(lib, index, path);
    CLASS_OF.with(|c| c.borrow_mut().insert(path.to_string(), r.clone()));
    r
}

thread_local! {
    /// Resource path -> (class, class package) as [`resolve_indexed`] found it.
    static CLASS_OF: std::cell::RefCell<HashMap<String, Option<(String, String)>>> = std::cell::RefCell::new(HashMap::new());
}

fn resolve_indexed_uncached(lib: &upkg::library::Library, index: &mut HashMap<String, Option<PkgIndex>>, path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = path.split('.').collect();
    let (pkg_name, rest) = parts.split_first()?;
    let leaf = rest.last()?;
    let groups: Vec<&str> = rest[..rest.len() - 1].iter().rev().copied().collect();
    let entry = index.entry(pkg_name.to_ascii_lowercase()).or_insert_with(|| {
        lib.package(pkg_name).map(|pkg| {
            let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
            for (i, e) in pkg.exports.iter().enumerate() {
                by_name.entry(pkg.name(e.name).to_ascii_lowercase()).or_default().push(i);
            }
            (pkg, by_name)
        })
    });
    let (pkg, by_name) = entry.as_ref()?;
    let cands = by_name.get(&leaf.to_ascii_lowercase())?;
    let mut fallback = None;
    for &i in cands {
        let mut outer = pkg.exports[i].outer;
        let mut ok = true;
        for g in &groups {
            match outer {
                ObjRef::Export(o) if pkg.name(pkg.exports[o].name).eq_ignore_ascii_case(g) => outer = pkg.exports[o].outer,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && outer == ObjRef::Null {
            fallback = Some(i);
            break;
        }
        fallback.get_or_insert(i);
    }
    let i = fallback?;
    let cn = pkg.export_class(i).to_string();
    let cp = class_package_of(&cn);
    Some((cn, cp))
}

impl WCx<'_> {
    /// Writes every non-default, savable property. Returns the number written.
    fn properties(&mut self, o: ObjId, out: &mut Bytes) -> usize {
        let vm = self.vm;
        let c = vm.objects[o].class;
        let ci = &vm.classes[c];
        let mut n = 0;
        for (s, slot) in ci.slots.iter().enumerate() {
            if slot.flags & (prop_flags::TRANSIENT | prop_flags::NATIVE) != 0 && !keep_transient(&slot.name) {
                continue;
            }
            if slot.name.eq_ignore_ascii_case("Name") || slot.name.eq_ignore_ascii_case("Class") || slot.name.eq_ignore_ascii_case("Outer") {
                continue;
            }
            let v = &vm.objects[o].slots[s];
            let d = ci.defaults.get(s);
            match (&slot.ty, v) {
                (Ty::Fixed(t, _), Value::Array(a)) => {
                    let da = match d {
                        Some(Value::Array(x)) => Some(x),
                        _ => None,
                    };
                    for (k, e) in a.iter().enumerate() {
                        if da.and_then(|x| x.get(k)).is_some_and(|de| de == e) {
                            continue;
                        }
                        if self.prop(out, &slot.name, t, e, k as u32) {
                            n += 1;
                        }
                    }
                }
                (t, v) => {
                    if d == Some(v) {
                        continue;
                    }
                    if self.prop(out, &slot.name, t, v, 0) {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    fn obj_ref(&mut self, o: Option<ObjId>) -> i32 {
        match o.and_then(|o| self.export_of.get(&o)) {
            Some(&i) => i as i32 + 1,
            None => 0,
        }
    }

    fn resource_ref(&mut self, path: &str) -> i32 {
        if let Some(&r) = self.res_cache.as_ref().and_then(|c| c.get(path)) {
            return r;
        }
        let r = {
            let t = std::time::Instant::now();
            let r = self.resource_ref_uncached(path);
            RES_NS.with(|c| c.set(c.get() + t.elapsed().as_nanos() as u64));
            RES_N.with(|c| c.set(c.get() + 1));
            r
        };
        if let Some(c) = self.res_cache.as_mut() {
            c.insert(path.to_string(), r);
        }
        r
    }

    fn resource_ref_uncached(&mut self, path: &str) -> i32 {
        // Class of the asset from the content library's package table.
        let class = if self.res_cache.is_some() {
            resolve_indexed(&self.game.host.lib, self.pkg_index, path)
        } else {
            crate::host::resolve_path(&self.game.host.lib, path, None).map(|o| (o.pkg.export_class(o.export).to_string(), class_package_of(o.pkg.export_class(o.export))))
        };
        match class {
            Some((cn, cp)) => raw_ref(self.w.import_path(path, &cp, &cn)),
            None => raw_ref(self.w.import_path(path, "Engine", "Texture")),
        }
    }

    fn class_ref(&mut self, c: Option<ClassId>) -> i32 {
        match c {
            Some(c) => raw_ref(class_import(self.w, self.vm, c)),
            None => 0,
        }
    }

    /// Encodes one value as a tagged property. False when the value cannot be saved.
    fn prop(&mut self, out: &mut Bytes, name: &str, ty: &Ty, v: &Value, index: u32) -> bool {
        let mut b = Bytes::default();
        let (kind, sname): (u8, Option<&str>) = match (ty, v) {
            (Ty::Bool, Value::Bool(x)) => {
                write_tag(self.w, out, name, T_BOOL, None, index, &[], *x);
                return true;
            }
            (Ty::Byte, v) => {
                b.u8(v.as_int() as u8);
                (T_BYTE, None)
            }
            (Ty::Int, v) => {
                b.i32(v.as_int());
                (T_INT, None)
            }
            (Ty::Float, v) => {
                b.f32(v.as_float());
                (T_FLOAT, None)
            }
            (Ty::Name, v) => {
                let n = self.w.name(&v.as_name());
                b.compact(n as i32);
                (T_NAME, None)
            }
            (Ty::Str, v) => {
                b.fstring(&v.as_str());
                (T_STR, None)
            }
            (Ty::Object, Value::Object(o)) => {
                let r = self.obj_ref(*o);
                b.compact(r);
                (T_OBJECT, None)
            }
            (Ty::Object, Value::Resource(p)) => {
                let r = self.resource_ref(p);
                b.compact(r);
                (T_OBJECT, None)
            }
            (Ty::Class, Value::Class(c)) => {
                let r = self.class_ref(*c);
                b.compact(r);
                (T_OBJECT, None)
            }
            (Ty::Vector, Value::Vector(x)) => {
                b.vec3(*x);
                (T_STRUCT, Some("Vector"))
            }
            (Ty::Rotator, Value::Rotator(r)) => {
                for c in r {
                    b.i32(*c);
                }
                (T_STRUCT, Some("Rotator"))
            }
            (Ty::Struct(fields), Value::Struct(vals)) => {
                if is_color(fields) {
                    for (_, fv) in vals {
                        b.u8(fv.as_int() as u8);
                    }
                    (T_STRUCT, Some("Color"))
                } else {
                    for ((fname, ft), (_, fv)) in fields.iter().zip(vals) {
                        if is_zero(fv, ft) {
                            continue;
                        }
                        match (ft, fv) {
                            (Ty::Fixed(t, _), Value::Array(a)) => {
                                for (k, e) in a.iter().enumerate() {
                                    if !is_zero(e, t) {
                                        self.prop(&mut b, fname, t, e, k as u32);
                                    }
                                }
                            }
                            _ => {
                                self.prop(&mut b, fname, ft, fv, 0);
                            }
                        }
                    }
                    write_none(self.w, &mut b);
                    (T_STRUCT, Some("Struct"))
                }
            }
            (Ty::Array(t), Value::Array(a)) => {
                b.compact(a.len() as i32);
                for e in a {
                    if !self.elem(&mut b, t, e) {
                        return false;
                    }
                }
                (T_ARRAY, None)
            }
            _ => return false,
        };
        write_tag(self.w, out, name, kind, sname, index, &b.0, false);
        true
    }

    /// Binary array element (structs as tagged lists, as array elements are stored).
    fn elem(&mut self, b: &mut Bytes, ty: &Ty, v: &Value) -> bool {
        match (ty, v) {
            (Ty::Byte, v) => b.u8(v.as_int() as u8),
            (Ty::Int, v) => b.i32(v.as_int()),
            (Ty::Bool, v) => b.u32(v.as_bool() as u32),
            (Ty::Float, v) => b.f32(v.as_float()),
            (Ty::Name, v) => {
                let n = self.w.name(&v.as_name());
                b.compact(n as i32)
            }
            (Ty::Str, v) => b.fstring(&v.as_str()),
            (Ty::Object, Value::Object(o)) => {
                let r = self.obj_ref(*o);
                b.compact(r)
            }
            (Ty::Object, Value::Resource(p)) => {
                let r = self.resource_ref(p);
                b.compact(r)
            }
            (Ty::Object, _) => b.compact(0),
            (Ty::Class, Value::Class(c)) => {
                let r = self.class_ref(*c);
                b.compact(r)
            }
            (Ty::Vector, Value::Vector(x)) => b.vec3(*x),
            (Ty::Rotator, Value::Rotator(r)) => r.iter().for_each(|c| b.i32(*c)),
            (Ty::Struct(fields), Value::Struct(vals)) => {
                if is_color(fields) {
                    vals.iter().for_each(|(_, fv)| b.u8(fv.as_int() as u8));
                } else {
                    for ((fname, ft), (_, fv)) in fields.iter().zip(vals) {
                        if !is_zero(fv, ft) {
                            self.prop(b, fname, ft, fv, 0);
                        }
                    }
                    write_none(self.w, b);
                }
            }
            _ => return false,
        }
        true
    }
}

fn is_color(fields: &[(String, Ty)]) -> bool {
    fields.len() == 4 && fields.iter().all(|(_, t)| *t == Ty::Byte) && fields.iter().map(|(n, _)| n.to_ascii_lowercase()).collect::<Vec<_>>() == ["b", "g", "r", "a"]
}

fn is_zero(v: &Value, t: &Ty) -> bool {
    *v == t.zero()
}

/// Transient properties the engine keeps in saves anyway (state needed to resume).
fn keep_transient(_name: &str) -> bool {
    false
}

/// Package of a native asset class.
fn class_package_of(class: &str) -> String {
    match class.to_ascii_lowercase().as_str() {
        "sound" | "texture" | "shader" | "staticmesh" | "skeletalmesh" | "meshanimation" | "finalblend" | "combiner" | "texpanner" | "texrotator" | "texscaler" | "texoscillator" | "cubemap" | "font" | "material" | "proceduralsound" | "soundgroup" => "Engine".into(),
        _ => "Engine".into(),
    }
}

// ---------------------------------------------------------------------------------------------
// Reading

/// Decodes the `Level` export's native data of an original save: the URL map name.
pub fn level_url_map(pkg: &Package) -> Option<String> {
    let lv = (0..pkg.exports.len()).find(|&i| pkg.export_class(i).eq_ignore_ascii_case("Level"))?;
    let data = pkg.export_data(lv);
    // The URL block ends the export: Protocol "unreal"? (absent), Host, Map, Portal, Options...
    // Look for the first length-prefixed ASCII string after the actor list that names a map
    // in the install: search backwards for "<len>Map\0" patterns.
    let mut found = None;
    let mut i = 0;
    while i + 2 < data.len() {
        let n = data[i] as usize;
        if (2..64).contains(&n) && i + 1 + n <= data.len() && data[i + n] == 0 {
            let s = &data[i + 1..i + n];
            if s.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
                let t = String::from_utf8_lossy(s).to_string();
                // Protocol is "postal2"-like; Host empty; the next string is the map.
                if found.is_none() && (t.eq_ignore_ascii_case("postal2") || t.eq_ignore_ascii_case("unreal")) {
                    found = Some(String::new());
                } else if matches!(found.as_deref(), Some("")) {
                    return Some(t);
                }
                i += n + 1;
                continue;
            }
        }
        if found.as_deref() == Some("") && data[i] == 0 {
            // empty Host string (length 0)
        }
        i += 1;
    }
    None
}

#[derive(Default)]
struct Sidecar {
    map: Option<String>,
    code: HashMap<String, (String, usize)>,
    latent: HashMap<String, (String, u16, f32, Vec<String>)>,
    disabled: HashMap<String, u64>,
    extensions: BTreeMap<String, String>,
}

fn hex_text(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len() * 2);
    for byte in text.bytes() { write!(out, "{byte:02x}").unwrap(); }
    out
}
fn unhex_text(text: &str) -> Option<String> {
    if text.len() % 2 != 0 { return None }
    let bytes = text.as_bytes().chunks_exact(2).map(|pair| {
        let digit = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
        Some(digit(pair[0])? * 16 + digit(pair[1])?)
    }).collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

fn read_sidecar(path: &Path) -> Sidecar {
    let mut s = Sidecar::default();
    let Ok(t) = std::fs::read_to_string(path) else { return s };
    for l in t.lines() {
        if let Some(m) = l.strip_prefix("map=") {
            s.map = Some(m.to_string());
        } else if let Some(r) = l.strip_prefix("extension ") {
            if let Some((namespace, payload)) = r.split_once(' ') {
                if let Some(namespace) = unhex_text(namespace) {
                    // Retain corrupt known payloads as invalid data so their
                    // owning adapter reports an error instead of resetting it.
                    s.extensions.insert(namespace, unhex_text(payload)
                        .unwrap_or_else(|| "invalid save extension encoding".into()));
                }
            }
        } else if let Some(r) = l.strip_prefix("code ") {
            let p: Vec<&str> = r.splitn(3, ' ').collect();
            if p.len() == 3 {
                s.code.insert(p[0].to_ascii_lowercase(), (p[1].to_string(), p[2].parse().unwrap_or(0)));
            }
        } else if let Some(r) = l.strip_prefix("latent ") {
            let p: Vec<&str> = r.splitn(5, ' ').collect();
            if p.len() >= 4 {
                let args = p.get(4).map(|a| a.split('\u{1f}').map(str::to_string).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
                s.latent.insert(p[0].to_ascii_lowercase(), (p[1].to_string(), p[2].parse().unwrap_or(0), p[3].parse().unwrap_or(0.0), args));
            }
        } else if let Some(r) = l.strip_prefix("disabled ") {
            if let Some((n, v)) = r.split_once(' ') {
                s.disabled.insert(n.to_ascii_lowercase(), v.parse().unwrap_or(0));
            }
        }
    }
    s
}

fn decode_value(vm: &mut Vm, s: &str, by_name: &HashMap<String, ObjId>) -> Value {
    let (t, r) = s.split_at(1.min(s.len()));
    let f3 = |r: &str| {
        let v: Vec<f32> = r.split(',').filter_map(|x| x.parse().ok()).collect();
        [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0)]
    };
    match t {
        "b" => Value::Byte(r.parse().unwrap_or(0)),
        "i" => Value::Int(r.parse().unwrap_or(0)),
        "z" => Value::Bool(r == "1"),
        "f" => Value::Float(r.parse().unwrap_or(0.0)),
        "n" => Value::Name(r.into()),
        "s" => Value::Str(r.into()),
        "V" => Value::Vector(f3(r)),
        "R" => {
            let v = f3(r);
            Value::Rotator([v[0] as i32, v[1] as i32, v[2] as i32])
        }
        "o" => Value::Object(by_name.get(&r.to_ascii_lowercase()).copied()),
        "c" => Value::Class(r.split_once('.').and_then(|(p, c)| vm.find_class(p, c).ok())),
        "r" => Value::Resource(r.into()),
        _ => Value::Void,
    }
}

/// Instantiates a save package into `game`'s VM, replacing the current level. The map the save
/// was made in supplies the navigation network and static world (`map_pkg`).
pub fn load_into(game: &mut Game, save: std::sync::Arc<Package>, sidecar_path: Option<&Path>) -> LoadStats {
    load_into_with(game, save, sidecar_path, true)
}

/// [`load_into`] with control over the `PostLoadGame` events (off: the raw restored state, for
/// round-trip comparisons).
pub fn load_into_with(game: &mut Game, save: std::sync::Arc<Package>, sidecar_path: Option<&Path>, post_load: bool) -> LoadStats {
    let sc = sidecar_path.map(read_sidecar).unwrap_or_default();
    game.save_extensions = sc.extensions.clone();
    game.save_extension_errors.clear();
    let mut st = LoadStats::default();
    // The map: sidecar, else LevelInfo.Title / Level URL; geometry and nav come from it.
    let map_name = sc.map.clone().or_else(|| level_url_map(&save)).unwrap_or_default();
    st.map = map_name.clone();
    let vm = &mut game.world.vm;
    vm.truncate_objects(game.level.base_objects);
    vm.actors.clear();
    vm.level = None;
    vm.log.clear();
    // Reuse the regular level loader on the save package: same object creation, defaults and
    // property decoding (struct/array formats are identical).
    let lv = game.world.load_level(save.clone());
    st.exports = save.exports.len();
    st.objects = lv.objects.len();
    st.actors = lv.actors.len();
    st.props_applied = lv.stats.props_applied;
    st.props_unknown = lv.stats.props_unknown;
    st.props_undecoded = lv.stats.props_undecoded;
    st.classes_unresolved = lv.stats.classes_unresolved;
    // Names for sidecar lookups.
    let mut by_name: HashMap<String, ObjId> = HashMap::new();
    for (&e, &o) in &lv.objects {
        by_name.insert(save.name(save.exports[e].name).to_ascii_lowercase(), o);
    }
    // State frames.
    let mut ids: Vec<(usize, ObjId)> = lv.objects.iter().map(|(&k, &v)| (k, v)).collect();
    ids.sort();
    for (e, o) in &ids {
        let ex = &save.exports[*e];
        if ex.flags & RF_HAS_STACK == 0 {
            continue;
        }
        let Some((node, state, probe, _latent, offset)) = read_state_frame(&save, *e) else { continue };
        let vm = &mut game.world.vm;
        let class = vm.objects[*o].class;
        let state_name = save.ref_name(state).to_string();
        let Some(sf) = vm.find_state(class, &state_name) else { continue };
        st.states += 1;
        let name = save.name(ex.name).to_ascii_lowercase();
        let frame = &mut vm.objects[*o].exec;
        frame.state = Some(sf);
        frame.generation = frame.generation.wrapping_add(1);
        frame.disabled = sc.disabled.get(&name).copied().unwrap_or(!probe & disabled_mask());
        // Resume point: our sidecar's exact statement, else the stored code offset when it
        // starts a statement of that state's code, else the state's Begin label.
        let code = sc.code.get(&name).and_then(|(path, idx)| vm.find_path(path).map(|f| (f, *idx))).or_else(|| {
            let node_name = save.ref_name(node).to_string();
            let nf = if node == ObjRef::Null { None } else { vm.find_state(class, &node_name) }?;
            let idx = stmt_index(vm, nf, offset?)?;
            Some((nf, idx))
        });
        if code.is_some() {
            st.states_resumed_at_code += 1;
            vm.objects[*o].exec.code = code;
        } else {
            vm.goto_state(*o, &state_name, "Begin").ok();
            // goto_state on the same state only moves to the label (no BeginState).
        }
        if let Some((lname, native, elapsed, args)) = sc.latent.get(&name) {
            let args: Vec<Value> = args.iter().map(|a| decode_value(vm, a, &by_name)).collect();
            vm.objects[*o].exec.latent = Some(uscript::vm::Latent { name: lname.clone(), native: *native, args, elapsed: *elapsed });
            st.latents += 1;
        }
    }
    // Hand the level to the game: nav from the original map when available.
    let map_pkg = game.world.files.path(&map_name).and_then(|p| Package::open(p).ok()).map(std::sync::Arc::new);
    install_loaded_level(game, lv, map_pkg);
    if !post_load {
        return st;
    }
    // PostLoadGame on every actor.
    let vm = &mut game.world.vm;
    let actors: Vec<ObjId> = vm.actors.clone();
    for a in actors {
        if !vm.objects[a].deleted {
            vm.fire_event(a, "PostLoadGame", vec![]);
        }
    }
    st
}

fn disabled_mask() -> u64 {
    0
}

/// `(node, state, probe mask, latent action, code offset)` of an export's script frame.
pub fn read_state_frame(pkg: &Package, e: usize) -> Option<(ObjRef, ObjRef, u64, i32, Option<u32>)> {
    let ex = &pkg.exports[e];
    let mut r = upkg::Reader::at(&pkg.data[..ex.offset + ex.size], ex.offset);
    let node = ObjRef::from_raw(r.compact().ok()?);
    let state = ObjRef::from_raw(r.compact().ok()?);
    let probe = r.u64().ok()?;
    let latent = r.i32().ok()?;
    let off = if node != ObjRef::Null { Some(r.compact().ok()? as u32) } else { None };
    Some((node, state, probe, latent, off))
}

fn stmt_index(vm: &Vm, sf: uscript::vm::FieldId, offset: u32) -> Option<usize> {
    match vm.pkgs[sf.pkg as usize].objects.get(sf.export as usize)?.as_ref()? {
        uscript::ScriptObject::State(s) => s.base.script.stmt_at(offset),
        _ => None,
    }
}

/// Like `Game::setup` for a loaded save: actor list, host, hooks, GameInfo from LevelInfo.Game.
fn install_loaded_level(game: &mut Game, lv: Level, map_pkg: Option<std::sync::Arc<Package>>) {
    let world = &mut game.world;
    world.vm.level = lv.level_info;
    world.vm.actors = lv.actors.iter().map(|a| a.1).collect();
    let nav_pkg = map_pkg.clone().unwrap_or_else(|| lv.map.clone());
    let nav = p2nav::NavGraph::from_package(&nav_pkg);
    // Nav objects by name (the save's PathNodes keep their map names).
    let mut by_name: HashMap<String, ObjId> = HashMap::new();
    for (&e, &o) in &lv.objects {
        by_name.insert(lv.map.name(lv.map.exports[e].name).to_ascii_lowercase(), o);
    }
    let mut nav_objects: HashMap<usize, ObjId> = HashMap::new();
    for (i, e) in nav_pkg.exports.iter().enumerate() {
        if let Some(&o) = by_name.get(&nav_pkg.name(e.name).to_ascii_lowercase()) {
            nav_objects.insert(i, o);
        }
    }
    world.vm.set_nav(Box::new(p2nav::VmNav::new(nav, &nav_objects)));
    let host = Rc::new(GameHost::new(world.library()));
    if let Some(m) = &map_pkg {
        host.map_lib_insert(m);
    }
    host.map_lib_insert(&lv.map);
    let map_name = map_pkg.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| lv.map.name.clone());
    *host.map_name.borrow_mut() = map_name;
    *host.map.borrow_mut() = Some(map_pkg.clone().unwrap_or_else(|| lv.map.clone()));
    *host.level_objects.borrow_mut() = nav_objects;
    world.vm.set_host(host.clone());
    crate::host::run_level_hooks(&host, &mut world.vm);
    game.host = host;
    game.game_info = lv.level_info.and_then(|li| world.vm.get_prop(li, "Game").and_then(|v| v.as_obj()));
    game.game_class = game.game_info.map(|g| world.class_path(world.vm.objects[g].class)).unwrap_or_else(|| "(none)".into());
    game.player_controller = world.vm.actors.iter().copied().find(|&a| crate::cinema::is_a(&world.vm, a, "PlayerController"));
    game.time = 0.0;
    game.level = lv;
    game.level_serial += 1;
    game.loaded_from_save = true;
}

// ---------------------------------------------------------------------------------------------
// Slots, console commands

/// Saves the current level into `slot` of `dir` (package + sidecar + slot ini line).
pub fn save_slot(game: &Game, dir: &Path, slot: i32, slot_name: &str) -> std::io::Result<SaveStats> {
    if !game.save_extension_errors.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
            game.save_extension_errors.iter().map(|(name, error)| format!("{name}: {error}"))
                .collect::<Vec<_>>().join("; ")));
    }
    let t0 = std::time::Instant::now();
    std::fs::create_dir_all(dir)?;
    let (bytes, sidecar, st) = write_save(game, slot_name);
    let t1 = t0.elapsed();
    if std::env::var_os("P2GAME_SAVE_VERIFY").is_some() {
        // Rebuild without the lookup cache and compare (byte-identical check).
        let (b2, s2, _) = write_save_with(game, slot_name, false);
        eprintln!("save verify: package {} ({} vs {} bytes), sidecar {}", if b2 == bytes { "IDENTICAL" } else { "DIFFERENT" }, bytes.len(), b2.len(), if s2 == sidecar { "IDENTICAL" } else { "DIFFERENT" });
    }
    let path = slot_path(dir, slot);
    std::fs::write(&path, bytes)?;
    std::fs::write(path.with_extension("p2r"), sidecar)?;
    let t2 = t0.elapsed();
    write_slot_info(dir, slot, slot_name, game);
    if std::env::var_os("P2GAME_BOOT_TIMING").is_some() {
        eprintln!("save timing: build {:.1} ms, write {:.1} ms, slot info {:.1} ms", t1.as_secs_f64() * 1e3, (t2 - t1).as_secs_f64() * 1e3, (t0.elapsed() - t2).as_secs_f64() * 1e3);
    }
    Ok(st)
}

/// Loads `slot` (or an explicit file) into the game.
pub fn load_file(game: &mut Game, path: &Path) -> Result<LoadStats, String> {
    load_file_with(game, path, true)
}

/// [`load_file`] with control over the `PostLoadGame` events.
pub fn load_file_with(game: &mut Game, path: &Path, post_load: bool) -> Result<LoadStats, String> {
    let pkg = Package::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let side = path.with_extension("p2r");
    Ok(load_into_with(game, std::sync::Arc::new(pkg), side.is_file().then_some(side.as_path()), post_load))
}

fn write_slot_info(dir: &Path, slot: i32, name: &str, game: &Game) {
    let ini = dir.join("SavedGameInfo.ini");
    let mut infos = std::fs::read(&ini).map(|b| p2save::parse_slot_ini(&String::from_utf8_lossy(&b))).unwrap_or_default();
    if infos.len() < p2save::SLOT_COUNT {
        infos.resize(p2save::SLOT_COUNT, p2save::SlotInfo::default());
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let day = game_state_day(game);
    // `Year-MM-DD : HH:MM` from the LevelInfo clock (local time), as P2GameInfoSingle.SaveGame does.
    let vm = &game.world.vm;
    let li = game.level.level_info;
    let field = |n: &str| li.and_then(|l| vm.get_prop(l, n)).map(|v| v.as_int()).unwrap_or(0);
    let time = if field("Year") > 0 { format!("{}-{:02}-{:02} : {:02}:{:02}", field("Year"), field("Month"), field("Day"), field("Hour"), field("Minute")) } else { format_time(now) };
    if let Some(s) = infos.get_mut(slot.max(0) as usize) {
        *s = p2save::SlotInfo { name: name.to_string(), day, time, ltime: now, load_screen: day_load_tex(game) };
    }
    let mut text = String::from("[FPSGame.SlotInfoMgr]\r\n");
    for s in &infos {
        text += &p2save::format_slot(s);
        text += "\r\n";
    }
    std::fs::write(ini, text).ok();
}

fn game_state_day(game: &Game) -> i32 {
    let vm = &game.world.vm;
    (0..vm.objects.len()).filter(|&o| !vm.objects[o].deleted && vm.classes[vm.objects[o].class].name.to_ascii_lowercase().ends_with("gamestate")).find_map(|o| vm.get_prop(o, "CurrentDay").map(|v| v.as_int())).unwrap_or(0)
}

/// `YYYY-MM-DD : HH:MM` (UTC) like the game's slot list.
fn format_time(t: i64) -> String {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} : {:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

/// Pending save-game requests from script (`SaveGame N` console command); the front end executes
/// them after the frame (a save needs `&Game`).
#[derive(Default)]
pub struct SaveRequests {
    pub save: Option<i32>,
    pub load: Option<i32>,
}

thread_local! {
    static REQUESTS: std::cell::RefCell<SaveRequests> = std::cell::RefCell::new(SaveRequests::default());
    static SAVE_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Installs the console commands (`SaveGame N`, `LoadGame N`, `CHECKSAVEGAME N`) for every level.
pub fn install(dir: Option<PathBuf>) {
    SAVE_DIR.with(|d| *d.borrow_mut() = Some(dir.unwrap_or_else(user_save_dir)));
    crate::host::register_level_hook(|host, vm| {
        host.ext.borrow_mut().push(Rc::new(SaveHost));
        // Class defaults name what BeginPlay will spawn (pawn dialog packages); the first frame
        // adds what the live objects reference by then.
        if std::env::var_os("P2GAME_SAVE_NO_WARM").is_none() {
            warm_packages(&host.lib, vm, true);
            let lib = host.lib.clone();
            let mut done = false;
            host.add_frame_hook("save_warm", move |vm, _dt| {
                if !done {
                    done = true;
                    warm_packages(&lib, vm, false);
                }
            });
        }
    });
}

/// Opens (on a worker thread) every package the live actors' resource properties point into, so
/// the arrival autosave does not open them cold on the game thread (dialog packages, ~0.3 s in
/// total on suburbs-1). Only fills the library's package cache (`Library::package` is the same
/// call the save makes, same result). `P2GAME_SAVE_NO_WARM=1` turns it off.
fn warm_packages(lib: &std::sync::Arc<upkg::library::Library>, vm: &Vm, class_defaults: bool) {
    let mut pkgs: HashSet<String> = HashSet::new();
    fn walk(v: &Value, out: &mut HashSet<String>) {
        match v {
            Value::Resource(p) => {
                if let Some(pk) = p.split('.').next().filter(|s| !s.is_empty()) {
                    out.insert(pk.to_ascii_lowercase());
                }
            }
            Value::Struct(f) => f.iter().for_each(|(_, v)| walk(v, out)),
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    for o in &vm.objects {
        if !o.deleted {
            o.slots.iter().for_each(|v| walk(v, &mut pkgs));
        }
    }
    if class_defaults {
        for c in &vm.classes {
            c.defaults.iter().for_each(|v| walk(v, &mut pkgs));
        }
    }
    let lib = lib.clone();
    let mut pkgs: Vec<String> = pkgs.into_iter().collect();
    pkgs.sort();
    std::thread::Builder::new()
        .name("save-warm".into())
        .spawn(move || {
            let t = std::time::Instant::now();
            for p in &pkgs {
                lib.package(p);
            }
            if std::env::var_os("P2GAME_BOOT_TIMING").is_some() {
                eprintln!("save warm: {} packages in {:.0} ms", pkgs.len(), t.elapsed().as_secs_f64() * 1e3);
            }
        })
        .ok();
}

pub fn save_dir() -> PathBuf {
    SAVE_DIR.with(|d| d.borrow().clone()).unwrap_or_else(user_save_dir)
}

/// Queues a load of save slot `n` (performed by [`process_requests`] after the frame). Used for
/// the `?load=N` travel URL the Load Game menu sends.
pub fn request_load(n: i32) {
    REQUESTS.with(|r| r.borrow_mut().load = Some(n));
}

/// Takes pending requests (call after each frame) and performs them.
pub fn process_requests(game: &mut Game) -> Vec<String> {
    let (save, load) = REQUESTS.with(|r| {
        let mut r = r.borrow_mut();
        (r.save.take(), r.load.take())
    });
    let mut log = Vec::new();
    let dir = save_dir();
    if let Some(slot) = save {
        let name = slot_display_name(game);
        let game = &*game;
        match save_slot(game, &dir, slot, &name) {
            Ok(s) => log.push(format!("SaveGame {slot}: {} objects ({} actors), {} properties, {} states, {} bytes", s.objects, s.actors, s.properties, s.states, s.bytes)),
            Err(e) => log.push(format!("SaveGame {slot} failed: {e}")),
        }
    }
    if let Some(slot) = load {
        match load_file(game, &slot_path(&dir, slot)) {
            Ok(s) => log.push(format!("LoadGame {slot}: {} objects, {} actors, {} states", s.objects, s.actors, s.states)),
            Err(e) => log.push(format!("LoadGame {slot} failed: {e}")),
        }
    }
    log
}

/// The slot name P2GameInfoSingle.SaveGame writes: `Day - Level Title - Difficulty - Game`.
/// The difficulty name comes from script (`GetDiffName(TheGameState.GameDifficulty)`).
fn slot_display_name(game: &mut Game) -> String {
    let diff = game.game_info.and_then(|g| {
        let vm = &mut game.world.vm;
        let gs = vm.get_prop(g, "TheGameState").and_then(|v| v.as_obj());
        let d = gs.and_then(|s| vm.get_prop(s, "GameDifficulty")).map(|v| v.as_float()).unwrap_or(5.0);
        let cls = vm.objects[g].class;
        match vm.call(cls, Some(g), "GetDiffName", vec![Value::Float(d)]) {
            Ok(Value::Str(s)) if !s.is_empty() => Some(s),
            _ => None,
        }
    });
    let vm = &game.world.vm;
    let day = p2save::DAY_NAMES.get(game_state_day(game).max(0) as usize).copied().unwrap_or("Monday");
    let title = game.level.level_info.and_then(|li| vm.get_prop(li, "Title").map(|v| v.as_str())).unwrap_or_else(|| game.level.map.name.clone());
    let short = game.game_info.and_then(|g| vm.get_prop(g, "GameNameShort")).map(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or_else(|| "POSTAL 2".into());
    match diff {
        Some(d) => format!("{day} - {title} - {d} - {short}"),
        None => format!("{day} - {title} - {short}"),
    }
}

/// The current day's load screen texture (`DayBase.LoadTex`), as the slot info records it.
fn day_load_tex(game: &Game) -> String {
    let vm = &game.world.vm;
    let Some(g) = game.game_info else { return String::new() };
    let days: Vec<ObjId> = match vm.get_prop(g, "Days") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_obj()).collect(),
        _ => return String::new(),
    };
    let d = days.get(game_state_day(game).max(0) as usize).copied();
    match d.and_then(|d| vm.get_prop(d, "LoadTex")) {
        Some(Value::Name(n)) if !n.eq_ignore_ascii_case("None") => n.clone(),
        Some(Value::Resource(r)) => r.clone(),
        _ => String::new(),
    }
}

struct SaveHost;

impl Host for SaveHost {
    fn console_command(&self, _vm: &mut Vm, _from: Option<ObjId>, command: &str) -> String {
        let c = command.trim();
        let (verb, rest) = c.split_once(char::is_whitespace).unwrap_or((c, ""));
        let n = rest.trim().parse::<i32>().ok();
        match (verb.to_ascii_lowercase().as_str(), n) {
            ("savegame", Some(n)) => {
                REQUESTS.with(|r| r.borrow_mut().save = Some(n));
                "1".into()
            }
            ("loadgame", Some(n)) => {
                REQUESTS.with(|r| r.borrow_mut().load = Some(n));
                "1".into()
            }
            ("checksavegame", Some(n)) => if slot_path(&save_dir(), n).is_file() { "1" } else { "0" }.into(),
            _ => String::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Comparison (round-trip checks)

/// A compact, comparable snapshot of the live level: per actor (by name) its class, state and
/// every non-default property rendered as text.
pub fn snapshot(vm: &Vm) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for &a in &vm.actors {
        if vm.objects[a].deleted {
            continue;
        }
        let ci = &vm.classes[vm.objects[a].class];
        let mut m = BTreeMap::new();
        m.insert("#class".into(), ci.name.clone());
        m.insert("#state".into(), vm.state_name(a));
        for (s, slot) in ci.slots.iter().enumerate() {
            if slot.flags & (prop_flags::TRANSIENT | prop_flags::NATIVE) != 0 {
                continue;
            }
            let v = &vm.objects[a].slots[s];
            if ci.defaults.get(s) == Some(v) {
                continue;
            }
            m.insert(slot.name.clone(), render(vm, v));
        }
        out.insert(vm.object_name(a), m);
    }
    out
}

fn render(vm: &Vm, v: &Value) -> String {
    match v {
        // A reference to a destroyed object reads as None in UE2 (and is saved as None).
        Value::Object(Some(o)) if vm.objects.get(*o).is_none_or(|x| x.deleted) => "None".into(),
        Value::Object(Some(o)) => format!("@{}", vm.object_name(*o)),
        Value::Float(f) => format!("{f:.3}"),
        Value::Vector([x, y, z]) => format!("({x:.2},{y:.2},{z:.2})"),
        Value::Struct(f) => format!("{{{}}}", f.iter().map(|(n, v)| format!("{n}={}", render(vm, v))).collect::<Vec<_>>().join(",")),
        Value::Array(a) => format!("[{}]", a.iter().map(|v| render(vm, v)).collect::<Vec<_>>().join(",")),
        v => vm.value_string(v),
    }
}

/// Differences between two snapshots: (actor, property, before, after).
pub fn diff(a: &BTreeMap<String, BTreeMap<String, String>>, b: &BTreeMap<String, BTreeMap<String, String>>) -> Vec<(String, String, String, String)> {
    let mut d = Vec::new();
    for (k, pa) in a {
        match b.get(k) {
            None => d.push((k.clone(), "#exists".into(), "yes".into(), "no".into())),
            Some(pb) => {
                for (p, va) in pa {
                    let vb = pb.get(p).cloned().unwrap_or_else(|| "<default>".into());
                    if *va != vb {
                        d.push((k.clone(), p.clone(), va.clone(), vb));
                    }
                }
                for (p, vb) in pb {
                    if !pa.contains_key(p) {
                        d.push((k.clone(), p.clone(), "<default>".into(), vb.clone()));
                    }
                }
            }
        }
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            d.push((k.clone(), "#exists".into(), "no".into(), "yes".into()));
        }
    }
    d
}

pub fn empty_stats() -> LevelStats {
    LevelStats::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_extensions_keep_unicode_newlines_and_report_corrupt_payloads() {
        let path = std::env::temp_dir().join(format!("p2r-sidecar-{}.p2r", std::process::id()));
        let payload = "{\"robe\":\"Wizard\",\"note\":\"雪\"}\ncode not-an-actor\u{1f}";
        std::fs::write(&path, format!("map=suburbs-1\nextension {} {}\n",
            hex_text("magicka"), hex_text(payload))).unwrap();
        let sidecar = read_sidecar(&path);
        assert_eq!(sidecar.extensions.get("magicka").map(String::as_str), Some(payload));
        assert_eq!(sidecar.map.as_deref(), Some("suburbs-1"));
        assert!(sidecar.code.is_empty(), "extension newlines cannot inject sidecar statements");
        std::fs::write(&path, format!("extension {} invalid-hex\n", hex_text("magicka"))).unwrap();
        assert_eq!(read_sidecar(&path).extensions["magicka"], "invalid save extension encoding");
        std::fs::remove_file(path).unwrap();
        assert!(unhex_text("0").is_none());
        assert!(unhex_text("ff").is_none());
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(1755094169), "2025-08-13 : 14:09");
        assert_eq!(format_time(0), "1970-01-01 : 00:00");
    }
}
