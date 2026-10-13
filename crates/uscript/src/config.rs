//! Config properties: loading `config`/`globalconfig` values and `SaveConfig`.
//!
//! - Values come from two layers. The user layer holds everything saved by script and lives in a
//!   separate user directory: `P2_USER_DIR` if set, else `%APPDATA%\postal2rust` (or
//!   `$XDG_DATA_HOME`/`~/.local/share/postal2rust`). It is never inside the game install: a
//!   directory under the install root is refused. The base layer is a [`ConfigSource`] the host
//!   installs (p2game implements it over the shipped `System\*.ini` files).
//! - A class's config file is named by its `config(Name)` (inherited from the nearest super that
//!   names one): `System` is `Postal2.ini`, `User` is `User.ini`, anything else `Name.ini`.
//! - Sections: `Package.Class` of the class itself for `config` properties (falling back to the
//!   declaring class), the declaring class for `globalconfig`, and `ObjectName ClassName` for
//!   classes flagged `PerObjectConfig` (applied to each object at spawn).
//! - Dynamic arrays are written as repeated `Key=` lines, fixed arrays as `Key[i]=`, structs as
//!   `(Field=Value,...)`, names and strings unquoted unless they contain separators.
//! - `SaveConfig` writes an object's config values (and updates the class defaults so later
//!   spawns see them), `StaticSaveConfig` writes the class defaults, `ResetConfig` drops the
//!   user layer for the class and reloads the base values.

use crate::vm::{ClassId, ObjId, Ty, Value, Vm};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Class flag: config sections are per object (`ObjectName ClassName`).
pub const CLASS_PER_OBJECT_CONFIG: u32 = 0x0000_0400;
/// Class flag: the class has config properties.
pub const CLASS_CONFIG: u32 = 0x0000_0004;

/// Base config values (the shipped ini files). `file` is the ini file name (e.g. `Postal2.ini`,
/// `User.ini`); the result is every value of `key` in `section` as (index, text): `Key[N]=`
/// entries carry N, repeated plain keys are numbered in order.
pub trait ConfigSource {
    fn values(&self, file: &str, section: &str, key: &str) -> Vec<(usize, String)>;
}

/// One ini file of the user layer: section -> key -> indexed values, plus whether it changed.
#[derive(Debug, Default, Clone)]
pub struct UserIni {
    pub sections: BTreeMap<String, (String, BTreeMap<String, (String, Vec<(usize, String)>)>)>,
    dirty: bool,
}

impl UserIni {
    fn parse(text: &str) -> UserIni {
        let mut u = UserIni::default();
        let mut cur: Option<String> = None;
        for raw in text.lines() {
            let l = raw.trim();
            if l.is_empty() || l.starts_with(';') {
                continue;
            }
            if l.starts_with('[') && l.ends_with(']') {
                let name = l[1..l.len() - 1].to_string();
                u.sections.entry(name.to_ascii_lowercase()).or_insert_with(|| (name.clone(), BTreeMap::new()));
                cur = Some(name.to_ascii_lowercase());
                continue;
            }
            let (Some(s), Some((k, v))) = (&cur, l.split_once('=')) else { continue };
            let (key, idx) = match (k.find('['), k.ends_with(']')) {
                (Some(o), true) => (k[..o].trim(), k[o + 1..k.len() - 1].trim().parse().ok()),
                _ => (k.trim(), None),
            };
            let e = u.sections.get_mut(s).unwrap().1.entry(key.to_ascii_lowercase()).or_insert_with(|| (key.to_string(), Vec::new()));
            let i = idx.unwrap_or(e.1.len());
            e.1.retain(|(j, _)| *j != i);
            e.1.push((i, v.to_string()));
        }
        u
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for (name, keys) in self.sections.values() {
            out += &format!("[{name}]\n");
            for (key, vals) in keys.values() {
                let indexed = vals.iter().any(|(i, _)| *i > 0) && vals.windows(2).any(|w| w[1].0 != w[0].0 + 1) || vals.first().is_some_and(|v| v.0 != 0);
                for (i, v) in vals {
                    if indexed { out += &format!("{key}[{i}]={v}\n") } else { out += &format!("{key}={v}\n") }
                }
            }
            out += "\n";
        }
        out
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&Vec<(usize, String)>> {
        self.sections.get(&section.to_ascii_lowercase())?.1.get(&key.to_ascii_lowercase()).map(|e| &e.1)
    }

    fn set(&mut self, section: &str, key: &str, vals: Vec<(usize, String)>) {
        let s = self.sections.entry(section.to_ascii_lowercase()).or_insert_with(|| (section.to_string(), BTreeMap::new()));
        s.1.insert(key.to_ascii_lowercase(), (key.to_string(), vals));
        self.dirty = true;
    }

    fn remove_section(&mut self, section: &str) {
        if self.sections.remove(&section.to_ascii_lowercase()).is_some() {
            self.dirty = true;
        }
    }
}

/// Config state of a VM.
#[derive(Default)]
pub struct ConfigState {
    pub source: Option<Box<dyn ConfigSource>>,
    /// Where saved config goes (None: saving disabled, values kept in memory only).
    pub user_dir: Option<PathBuf>,
    user: HashMap<String, UserIni>,
    applied: HashSet<ClassId>,
    /// Lines written by the last flush (for tools).
    pub writes: u64,
}

/// The default user directory (see the module docs).
pub fn default_user_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("P2_USER_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(a) = std::env::var("APPDATA") {
        return Some(PathBuf::from(a).join("postal2rust"));
    }
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        return Some(PathBuf::from(x).join("postal2rust"));
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share/postal2rust"))
}

fn ini_file(config_name: &str) -> String {
    match config_name.to_ascii_lowercase().as_str() {
        "system" | "" | "none" => "Postal2.ini".into(),
        "user" => "User.ini".into(),
        _ => format!("{config_name}.ini"),
    }
}

/// Text for a config value.
pub fn value_text(vm: &Vm, v: &Value) -> String {
    let quote = |s: &str| if s.contains([',', '(', ')', '"', '=']) || s.starts_with(' ') || s.ends_with(' ') { format!("\"{}\"", s.replace('"', "\\\"")) } else { s.to_string() };
    match v {
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Float(f) => format!("{f:.6}"),
        Value::Str(s) | Value::Name(s) => quote(s),
        Value::Class(None) | Value::Object(None) => "None".into(),
        Value::Class(Some(c)) => format!("{}.{}", vm.classes[*c].package, vm.classes[*c].name),
        Value::Vector([x, y, z]) => format!("(X={x:.6},Y={y:.6},Z={z:.6})"),
        Value::Rotator([p, y, r]) => format!("(Pitch={p},Yaw={y},Roll={r})"),
        Value::Struct(f) => format!("({})", f.iter().map(|(k, x)| format!("{k}={}", value_text(vm, x))).collect::<Vec<_>>().join(",")),
        v => vm.value_string(v),
    }
}

fn split_top(s: &str) -> Vec<&str> {
    let (mut depth, mut q, mut start, mut out) = (0, false, 0, Vec::new());
    for (i, c) in s.char_indices() {
        match c {
            '"' => q = !q,
            '(' if !q => depth += 1,
            ')' if !q => depth -= 1,
            ',' if !q && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    match s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        Some(x) => x.replace("\\\"", "\""),
        None => s.to_string(),
    }
}

/// Parses config text into a value of a slot type (None when it does not fit).
pub fn parse_value(vm: &mut Vm, ty: &Ty, s: &str) -> Option<Value> {
    let s = s.trim();
    Some(match ty {
        Ty::Bool => Value::Bool(s.eq_ignore_ascii_case("true") || s == "1" || s.eq_ignore_ascii_case("yes")),
        Ty::Int => Value::Int(s.parse::<f64>().ok()? as i32),
        Ty::Byte => Value::Byte(s.parse::<i64>().ok()? as u8),
        Ty::Float => Value::Float(s.trim_end_matches(['f', 'F']).parse().ok()?),
        Ty::Name => Value::Name(unquote(s)),
        Ty::Str => Value::Str(unquote(s)),
        Ty::Class => {
            let n = unquote(s);
            let n = n.strip_prefix("Class'").and_then(|x| x.strip_suffix('\'')).unwrap_or(&n).to_string();
            if n.eq_ignore_ascii_case("none") || n.is_empty() { Value::Class(None) } else { Value::Class(vm.find_class_by_name(&n).ok()) }
        }
        Ty::Object => {
            let n = unquote(s);
            // `Sound'Pkg.Name'` style references: keep the object path.
            let n = match n.split_once('\'') {
                Some((cls, rest)) if !cls.is_empty() && cls.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => rest.strip_suffix('\'').unwrap_or(rest).to_string(),
                _ => n,
            };
            if n.eq_ignore_ascii_case("none") || n.is_empty() { Value::Object(None) } else { Value::Resource(n) }
        }
        Ty::Vector | Ty::Rotator | Ty::Struct(_) => {
            let inner = s.strip_prefix('(')?.strip_suffix(')')?;
            let mut val = ty.zero();
            for part in split_top(inner) {
                let Some((k, v)) = part.split_once('=') else { continue };
                let k = k.trim();
                match &mut val {
                    Value::Vector(xyz) => {
                        let i = ["x", "y", "z"].iter().position(|n| n.eq_ignore_ascii_case(k))?;
                        xyz[i] = v.trim().parse().ok()?;
                    }
                    Value::Rotator(r) => {
                        let i = ["pitch", "yaw", "roll"].iter().position(|n| n.eq_ignore_ascii_case(k))?;
                        r[i] = v.trim().parse::<f64>().ok()? as i32;
                    }
                    Value::Struct(fields) => {
                        let Ty::Struct(tys) = ty else { return None };
                        let Some(fi) = tys.iter().position(|(n, _)| n.eq_ignore_ascii_case(k)) else { continue };
                        if let Some(x) = parse_value(vm, &tys[fi].1, v) {
                            fields[fi].1 = x;
                        }
                    }
                    _ => {}
                }
            }
            val
        }
        // Dynamic array written inline (struct members: `Text=("a","b")`, `Time=(3,,,)`); empty
        // elements keep the element's zero value.
        Ty::Array(t) => {
            let inner = s.strip_prefix('(').and_then(|x| x.strip_suffix(')')).unwrap_or(s);
            if inner.trim().is_empty() {
                return Some(Value::Array(Vec::new()));
            }
            let mut out = Vec::new();
            for part in split_top(inner) {
                out.push(if part.trim().is_empty() { t.zero() } else { parse_value(vm, t, part).unwrap_or_else(|| t.zero()) });
            }
            Value::Array(out)
        }
        _ => return None,
    })
}

impl Vm {
    /// Installs the base config layer (shipped ini files).
    pub fn set_config_source(&mut self, src: Box<dyn ConfigSource>) {
        self.config.source = Some(src);
        self.config.applied.clear();
    }

    /// Sets the user config directory. Refuses directories inside the game install.
    pub fn set_user_config_dir(&mut self, dir: impl Into<PathBuf>) -> crate::Result<()> {
        let dir = dir.into();
        if let Some(root) = self.system_dir.parent() {
            // Canonicalize the nearest existing ancestor so `..` and links resolve even when the
            // directory does not exist yet.
            fn canon(p: &Path) -> String {
                let mut cur = p.to_path_buf();
                let mut tail = Vec::new();
                loop {
                    if let Ok(c) = std::fs::canonicalize(&cur) {
                        let mut c = c;
                        for t in tail.iter().rev() {
                            c.push(t);
                        }
                        return c.to_string_lossy().to_ascii_lowercase();
                    }
                    match (cur.file_name().map(|f| f.to_os_string()), cur.parent()) {
                        (Some(f), Some(parent)) => {
                            tail.push(f);
                            cur = parent.to_path_buf();
                        }
                        _ => return p.to_string_lossy().to_ascii_lowercase(),
                    }
                }
            }
            let (d, r) = (canon(&dir), canon(root));
            if d == r || d.starts_with(&format!("{}{}", r.trim_end_matches(std::path::MAIN_SEPARATOR), std::path::MAIN_SEPARATOR)) {
                return Err(crate::ScriptError::Bad(format!("user config dir {} is inside the game install", dir.display())));
            }
        }
        self.config.user_dir = Some(dir);
        self.config.user.clear();
        self.config.applied.clear();
        Ok(())
    }

    /// The config file name of a class (`Postal2.ini`, `User.ini`, ...).
    pub fn class_config_file(&self, c: ClassId) -> String {
        let mut cur = Some(c);
        while let Some(ci) = cur {
            let f = self.classes[ci].field;
            let sp = &self.pkgs[f.pkg as usize];
            if let Some(cd) = sp.class(f.export as usize) {
                let n = sp.pkg.name(cd.config_name as usize);
                if !n.is_empty() && !n.eq_ignore_ascii_case("None") {
                    return ini_file(n);
                }
            }
            cur = self.classes[ci].super_;
        }
        ini_file("System")
    }

    fn user_ini(&mut self, file: &str) -> &mut UserIni {
        let key = file.to_ascii_lowercase();
        if !self.config.user.contains_key(&key) {
            let loaded = self.config.user_dir.as_ref().and_then(|d| std::fs::read_to_string(d.join(file)).ok()).map(|t| UserIni::parse(&t)).unwrap_or_default();
            self.config.user.insert(key.clone(), loaded);
        }
        self.config.user.get_mut(&key).unwrap()
    }

    /// `(key, value)` lines of a section of a user ini file (for host-owned sections such as
    /// `[Engine.Input]`, which no script class owns).
    pub fn user_ini_section(&mut self, file: &str, section: &str) -> Vec<(String, String)> {
        let Some((_, keys)) = self.user_ini(file).sections.get(&section.to_ascii_lowercase()) else { return vec![] };
        let mut out = Vec::new();
        for (key, vals) in keys.values() {
            for (i, v) in vals {
                let k = if vals.len() > 1 || *i > 0 { format!("{key}[{i}]") } else { key.clone() };
                out.push((k, v.clone()));
            }
        }
        out
    }

    /// Sets one value of a host-owned user ini section and writes the file.
    pub fn user_ini_set(&mut self, file: &str, section: &str, key: &str, value: &str) -> crate::Result<()> {
        self.user_ini(file).set(section, key, vec![(0, value.to_string())]);
        self.flush_config()
    }

    /// Writes changed user ini files.
    pub fn flush_config(&mut self) -> crate::Result<()> {
        let Some(dir) = self.config.user_dir.clone() else { return Ok(()) };
        for (file, u) in self.config.user.iter_mut() {
            if !u.dirty {
                continue;
            }
            std::fs::create_dir_all(&dir).map_err(|e| crate::ScriptError::Bad(format!("config dir {}: {e}", dir.display())))?;
            let name = match file.as_str() {
                "postal2.ini" => "Postal2.ini".to_string(),
                "user.ini" => "User.ini".to_string(),
                f => f.to_string(),
            };
            std::fs::write(dir.join(&name), u.render()).map_err(|e| crate::ScriptError::Bad(format!("writing {name}: {e}")))?;
            u.dirty = false;
            self.config.writes += 1;
        }
        Ok(())
    }

    /// (slot index, section, key) for every config property of a class. `object` selects the
    /// per-object section for PerObjectConfig classes.
    fn config_slots(&self, c: ClassId, object: Option<&str>) -> Vec<(usize, Vec<String>)> {
        use crate::objects::prop_flags::{CONFIG, GLOBAL_CONFIG};
        let ci = &self.classes[c];
        let own = format!("{}.{}", ci.package, ci.name);
        let per_object = ci.flags & CLASS_PER_OBJECT_CONFIG != 0;
        let mut out = Vec::new();
        for (si, slot) in ci.slots.iter().enumerate() {
            if slot.flags & (CONFIG | GLOBAL_CONFIG) == 0 {
                continue;
            }
            let dsp = &self.pkgs[slot.field.pkg as usize];
            let owner = dsp.pkg.ref_name(dsp.pkg.exports[slot.field.export as usize].outer);
            let dpkg = dsp.pkg.names.iter().find(|n| n.eq_ignore_ascii_case(&dsp.pkg.name)).map(String::as_str).unwrap_or(&dsp.pkg.name);
            let decl = format!("{dpkg}.{owner}");
            let sections = if slot.flags & GLOBAL_CONFIG != 0 {
                vec![decl]
            } else if let (true, Some(o)) = (per_object, object) {
                vec![format!("{o} {}", ci.name)]
            } else if own.eq_ignore_ascii_case(&decl) {
                vec![own.clone()]
            } else {
                vec![own.clone(), decl]
            };
            out.push((si, sections));
        }
        out
    }

    fn config_values(&mut self, file: &str, sections: &[String], key: &str) -> Vec<(usize, String)> {
        for s in sections {
            if let Some(v) = self.user_ini(file).get(s, key) {
                return v.clone();
            }
            if let Some(src) = &self.config.source {
                let v = src.values(file, s, key);
                if !v.is_empty() {
                    return v;
                }
            }
        }
        Vec::new()
    }

    fn apply_values(&mut self, ty: &Ty, cur: Value, vals: Vec<(usize, String)>) -> Option<Value> {
        match (ty, cur) {
            (Ty::Fixed(t, _), Value::Array(mut a)) => {
                for (i, s) in vals {
                    if let (Some(v), true) = (parse_value(self, t, &s), i < a.len()) {
                        a[i] = v;
                    }
                }
                Some(Value::Array(a))
            }
            (Ty::Array(t), _) => {
                let mut out = Vec::new();
                for (i, s) in vals {
                    if let Some(v) = parse_value(self, t, &s) {
                        if out.len() <= i {
                            out.resize(i + 1, t.zero());
                        }
                        out[i] = v;
                    }
                }
                Some(Value::Array(out))
            }
            (ty, _) => vals.last().and_then(|(_, s)| parse_value(self, ty, s)),
        }
    }

    /// Loads config values into a class's defaults (supers first). Done once per class; called
    /// automatically when a class with config properties is spawned.
    pub fn apply_class_config(&mut self, c: ClassId) {
        if !self.config.applied.insert(c) {
            return;
        }
        if let Some(s) = self.classes[c].super_ {
            self.apply_class_config(s);
        }
        if self.config.source.is_none() && self.config.user_dir.is_none() {
            return;
        }
        let file = self.class_config_file(c);
        for (si, sections) in self.config_slots(c, None) {
            let key = self.classes[c].slots[si].name.clone();
            let vals = self.config_values(&file, &sections, &key);
            if vals.is_empty() {
                continue;
            }
            let ty = self.classes[c].slots[si].ty.clone();
            let cur = self.classes[c].defaults[si].clone();
            if let Some(v) = self.apply_values(&ty, cur, vals) {
                self.classes[c].defaults[si] = v;
            }
        }
    }

    /// Applies config to a newly created object: class config, plus per-object sections.
    pub fn apply_object_config(&mut self, o: ObjId) {
        let c = self.objects[o].class;
        if self.classes[c].flags & CLASS_PER_OBJECT_CONFIG == 0 {
            return;
        }
        let name = self.object_name(o);
        let file = self.class_config_file(c);
        for (si, sections) in self.config_slots(c, Some(&name)) {
            let key = self.classes[c].slots[si].name.clone();
            let vals = self.config_values(&file, &sections, &key);
            if vals.is_empty() {
                continue;
            }
            let ty = self.classes[c].slots[si].ty.clone();
            let cur = self.objects[o].slots[si].clone();
            if let Some(v) = self.apply_values(&ty, cur, vals) {
                self.objects[o].slots[si] = v;
            }
        }
    }

    fn save_config_values(&mut self, c: ClassId, object: Option<ObjId>) -> crate::Result<()> {
        let file = self.class_config_file(c);
        let name = object.map(|o| self.object_name(o));
        let per_object = self.classes[c].flags & CLASS_PER_OBJECT_CONFIG != 0;
        for (si, sections) in self.config_slots(c, name.as_deref()) {
            let v = match object {
                Some(o) => self.objects[o].slots[si].clone(),
                None => self.classes[c].defaults[si].clone(),
            };
            if object.is_some() && !per_object {
                // Instance values become the class's config defaults.
                self.classes[c].defaults[si] = v.clone();
            }
            let vals: Vec<(usize, String)> = match (&self.classes[c].slots[si].ty, &v) {
                (Ty::Fixed(..) | Ty::Array(_), Value::Array(a)) => a.iter().enumerate().map(|(i, x)| (i, value_text(self, x))).collect(),
                _ => vec![(0, value_text(self, &v))],
            };
            let key = self.classes[c].slots[si].name.clone();
            if std::env::var_os("P2_CONFIG_TRACE").is_some() {
                eprintln!("SaveConfig {} [{}] {key}={vals:?} (by {})", file, sections[0], object.map(|o| self.object_name(o)).unwrap_or_else(|| self.classes[c].name.clone()));
            }
            self.user_ini(&file).set(&sections[0], &key, vals);
        }
        self.flush_config()
    }
}

impl Vm {
    /// `GET Class Property`: the class default of a property as config text (UE2's console GET).
    /// `Class` is `Package.Class` or a bare class name. None if either is unknown.
    pub fn console_get(&mut self, class: &str, prop: &str) -> Option<String> {
        let c = self.class_for_console(class)?;
        self.apply_class_config(c);
        let si = self.classes[c].slot(prop)?;
        let v = self.classes[c].defaults[si].clone();
        Some(match (&self.classes[c].slots[si].ty, &v) {
            (Ty::Fixed(..) | Ty::Array(_), Value::Array(a)) => a.first().map(|x| value_text(self, x)).unwrap_or_default(),
            _ => value_text(self, &v),
        })
    }

    /// `SET Class Property Value`: sets the class default and the property on every live object
    /// of that class (or a subclass), and saves it to the user ini when the property is config, as
    /// UE2's console SET does. Returns false if the class or property is unknown or the value does
    /// not parse.
    pub fn console_set(&mut self, class: &str, prop: &str, text: &str) -> bool {
        use crate::objects::prop_flags::{CONFIG, GLOBAL_CONFIG};
        let Some(c) = self.class_for_console(class) else { return false };
        self.apply_class_config(c);
        let Some(si) = self.classes[c].slot(prop) else { return false };
        let ty = self.classes[c].slots[si].ty.clone();
        let Some(v) = parse_value(self, &ty, text) else { return false };
        let field = self.classes[c].slots[si].field;
        // The class, its subclasses (defaults) and live objects of any of them.
        for k in 0..self.classes.len() {
            if !self.is_child_of(k, c) {
                continue;
            }
            if let Some(&s) = self.classes[k].slot_by_field.get(&field) {
                self.classes[k].defaults[s] = v.clone();
            }
        }
        for o in 0..self.objects.len() {
            if self.objects[o].deleted || !self.is_child_of(self.objects[o].class, c) {
                continue;
            }
            let k = self.objects[o].class;
            if let Some(&s) = self.classes[k].slot_by_field.get(&field) {
                self.objects[o].slots[s] = v.clone();
            }
        }
        if self.classes[c].slots[si].flags & (CONFIG | GLOBAL_CONFIG) != 0 {
            let file = self.class_config_file(c);
            if let Some((_, sections)) = self.config_slots(c, None).into_iter().find(|(s, _)| *s == si) {
                let vals = match (&ty, &v) {
                    (Ty::Fixed(..) | Ty::Array(_), Value::Array(a)) => a.iter().enumerate().map(|(i, x)| (i, value_text(self, x))).collect(),
                    _ => vec![(0, value_text(self, &v))],
                };
                if std::env::var_os("P2_CONFIG_TRACE").is_some() {
                    eprintln!("SET {file} [{}] {prop}={vals:?}", sections[0]);
                }
                self.user_ini(&file).set(&sections[0], prop, vals);
                let _ = self.flush_config();
            }
        }
        true
    }

    fn class_for_console(&mut self, class: &str) -> Option<ClassId> {
        match class.split_once('.') {
            Some((p, n)) => self.find_class(p, n).ok().or_else(|| self.find_class_by_name(n).ok()),
            None => self.find_class_by_name(class).ok(),
        }
    }

    /// `ini:Pkg.Class.Prop`: the system ini value `[Pkg.Class] Prop=` names a class (e.g.
    /// `Engine.Engine.ViewportManager` is `WinDrv.WindowsClient`) whose section holds the key.
    /// Without such an indirection the text before the key is used as the section itself.
    fn ini_section(&mut self, path: &str) -> String {
        if let Some((section, prop)) = path.rsplit_once('.') {
            if let Some((_, v)) = self.config_values("Postal2.ini", &[section.to_string()], prop).pop() {
                let v = unquote(&v);
                if !v.is_empty() {
                    return v;
                }
            }
        }
        path.to_string()
    }

    /// The engine's console GET/SET commands (`get Class Prop`, `set Class Prop Value`, and
    /// `get/set ini:Pkg.Class.Prop Key` on the system ini); None for any other command.
    pub fn console_get_set(&mut self, command: &str) -> Option<String> {
        let mut it = command.split_whitespace();
        let verb = it.next()?;
        let class = it.next()?;
        let prop = it.next()?;
        if let Some(path) = class.get(..4).filter(|p| p.eq_ignore_ascii_case("ini:")).map(|_| &class[4..]) {
            let section = self.ini_section(path);
            if verb.eq_ignore_ascii_case("get") {
                return Some(self.config_values("Postal2.ini", &[section], prop).pop().map(|(_, v)| unquote(&v)).unwrap_or_default());
            }
            if verb.eq_ignore_ascii_case("set") {
                let value = it.collect::<Vec<_>>().join(" ");
                self.user_ini("Postal2.ini").set(&section, prop, vec![(0, value)]);
                let _ = self.flush_config();
                return Some("1".into());
            }
            return None;
        }
        if verb.eq_ignore_ascii_case("get") {
            return self.console_get(class, prop);
        }
        if verb.eq_ignore_ascii_case("set") {
            let value = it.collect::<Vec<_>>().join(" ");
            return Some(if self.console_set(class, prop, &value) { "1".into() } else { String::new() });
        }
        None
    }
}

macro_rules! reg {
    ($vm:expr, $($name:literal => $f:expr),* $(,)?) => {
        $( $vm.register_native($name, $f); )*
    };
}

/// Registers SaveConfig / StaticSaveConfig / ResetConfig. The user directory starts as
/// [`default_user_dir`] unless that would be inside the game install.
pub fn register_config(vm: &mut Vm) {
    if vm.config.user_dir.is_none() {
        if let Some(d) = default_user_dir() {
            let _ = vm.set_user_config_dir(d);
        }
    }
    reg!(vm,
        "SaveConfig" => |vm, _| {
            let Some(o) = vm.native_self() else { return Ok(Value::Void) };
            let c = vm.objects[o].class;
            vm.save_config_values(c, Some(o))?;
            Ok(Value::Void)
        },
        "StaticSaveConfig" => |vm, _| {
            let c = match vm.native_self() { Some(o) => Some(vm.objects[o].class), None => vm.native_class() };
            if let Some(c) = c { vm.save_config_values(c, None)?; }
            Ok(Value::Void)
        },
        "ResetConfig" => |vm, _| {
            let c = match vm.native_self() { Some(o) => Some(vm.objects[o].class), None => vm.native_class() };
            let Some(c) = c else { return Ok(Value::Void) };
            let file = vm.class_config_file(c);
            for (_, sections) in vm.config_slots(c, None) {
                for s in sections {
                    vm.user_ini(&file).remove_section(&s);
                }
            }
            vm.flush_config()?;
            // Reload from the base layer on top of the compiled defaults.
            vm.config.applied.remove(&c);
            vm.apply_class_config(c);
            Ok(Value::Void)
        },
    );
}
