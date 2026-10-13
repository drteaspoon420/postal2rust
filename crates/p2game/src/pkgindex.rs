//! Lightweight package index: only the name, import and export tables of a package, read with
//! seeks so resolving a cross-package reference never loads texture or sound payloads.
//!
//! Package files are found through the `[Core.System] Paths` globs of the boot ini (relative to
//! the System directory), exactly like the engine's package search.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use upkg::{ObjRef, Package, Reader};

pub struct TableEntry {
    pub name: usize,
    pub class: usize,
    pub outer: i32,
}

pub struct PackageIndex {
    pub names: Vec<String>,
    /// (class name index, outer raw ref, object name index) per import.
    pub imports: Vec<TableEntry>,
    /// (class raw ref, outer raw ref, object name index) per export. `class` is a raw ObjRef here.
    pub exports: Vec<(i32, i32, usize)>,
}

fn read_window(f: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity(len);
    f.take(len as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Parses a table at `offset` holding `count` entries, growing the read window until it fits.
fn read_table<T>(f: &mut File, file_len: u64, offset: u64, count: usize, mut parse: impl FnMut(&mut Reader) -> upkg::Result<T>) -> Option<Vec<T>> {
    let mut want = (count * 16).max(4096);
    loop {
        let buf = read_window(f, offset, want)?;
        let mut r = Reader::new(&buf);
        let mut out = Vec::with_capacity(count);
        let mut ok = true;
        for _ in 0..count {
            match parse(&mut r) {
                Ok(v) => out.push(v),
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some(out);
        }
        if offset + want as u64 >= file_len {
            return None;
        }
        want *= 4;
    }
}

impl PackageIndex {
    pub fn open(path: &Path) -> Option<Self> {
        let mut f = File::open(path).ok()?;
        let file_len = f.metadata().ok()?.len();
        let head = read_window(&mut f, 0, 36)?;
        let mut r = Reader::new(&head);
        if r.u32().ok()? != upkg::package::PACKAGE_TAG {
            return None;
        }
        let (_ver, _lic, _flags) = (r.u16().ok()?, r.u16().ok()?, r.u32().ok()?);
        let (nc, no, ec, eo, ic, io) = (r.i32().ok()?, r.i32().ok()?, r.i32().ok()?, r.i32().ok()?, r.i32().ok()?, r.i32().ok()?);
        let names = read_table(&mut f, file_len, no as u64, nc as usize, |r| {
            let s = r.fstring()?;
            r.u32()?;
            Ok(s)
        })?;
        let imports = read_table(&mut f, file_len, io as u64, ic as usize, |r| {
            let _cp = r.compact()?;
            let class = r.compact()? as usize;
            let outer = r.i32()?;
            let name = r.compact()? as usize;
            Ok(TableEntry { name, class, outer })
        })?;
        let exports = read_table(&mut f, file_len, eo as u64, ec as usize, |r| {
            let class = r.compact()?;
            let _super = r.compact()?;
            let outer = r.i32()?;
            let name = r.compact()? as usize;
            let _flags = r.u32()?;
            let size = r.compact()?;
            if size > 0 {
                r.compact()?;
            }
            Ok((class, outer, name))
        })?;
        Some(Self { names, imports, exports })
    }

    fn name(&self, i: usize) -> &str {
        self.names.get(i).map(|s| s.as_str()).unwrap_or("")
    }

    fn ref_name(&self, raw: i32) -> &str {
        match ObjRef::from_raw(raw) {
            ObjRef::Null => "None",
            ObjRef::Export(i) => self.exports.get(i).map(|e| self.name(e.2)).unwrap_or(""),
            ObjRef::Import(i) => self.imports.get(i).map(|e| self.name(e.name)).unwrap_or(""),
        }
    }

    /// Finds an export by leaf name and outer group chain (innermost first), optionally by class.
    pub fn find(&self, leaf: &str, groups: &[String], class: Option<&str>) -> Option<usize> {
        'next: for (i, &(cls, outer, name)) in self.exports.iter().enumerate() {
            if !self.name(name).eq_ignore_ascii_case(leaf) {
                continue;
            }
            if let Some(c) = class {
                let cn = if cls == 0 { "Class" } else { self.ref_name(cls) };
                if !cn.eq_ignore_ascii_case(c) {
                    continue;
                }
            }
            let mut o = outer;
            for g in groups {
                match ObjRef::from_raw(o) {
                    ObjRef::Export(k) if self.name(self.exports[k].2).eq_ignore_ascii_case(g) => o = self.exports[k].1,
                    _ => continue 'next,
                }
            }
            if o == 0 {
                return Some(i);
            }
        }
        None
    }
}

/// Package files reachable through the ini search paths, opened lazily as table-only indexes.
pub struct PackageFiles {
    pub files: HashMap<String, PathBuf>,
    cache: HashMap<String, Option<PackageIndex>>,
}

impl PackageFiles {
    /// `paths` are the `[Core.System] Paths=` globs, e.g. `..\Maps\*.fuk`, relative to `system_dir`.
    pub fn new(system_dir: &Path, paths: &[String]) -> Self {
        let mut files = HashMap::new();
        for p in paths {
            let p = p.replace('\\', "/");
            let (dir, pat) = p.rsplit_once('/').unwrap_or((".", p.as_str()));
            let ext = pat.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
            let Ok(rd) = std::fs::read_dir(system_dir.join(dir)) else { continue };
            for e in rd.flatten() {
                let path = e.path();
                let pe = path.extension().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                if ext == "*" || pe == ext {
                    let stem = path.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                    files.entry(stem).or_insert(path);
                }
            }
        }
        Self { files, cache: HashMap::new() }
    }

    pub fn path(&self, package: &str) -> Option<&PathBuf> {
        self.files.get(&package.to_ascii_lowercase())
    }

    pub fn index(&mut self, package: &str) -> Option<&PackageIndex> {
        let k = package.to_ascii_lowercase();
        if !self.cache.contains_key(&k) {
            let idx = self.files.get(&k).and_then(|p| PackageIndex::open(p));
            self.cache.insert(k.clone(), idx);
        }
        self.cache.get(&k).and_then(|o| o.as_ref())
    }

    /// Checks that an import made from `pkg` names an object that exists. Returns the canonical
    /// `Package.Group.Name` path on success.
    pub fn resolve_import(&mut self, pkg: &Package, r: ObjRef) -> Result<String, String> {
        let ObjRef::Import(i) = r else { return Err("not an import".into()) };
        let mut chain = Vec::new();
        let mut cur = r;
        while let ObjRef::Import(j) = cur {
            chain.push(pkg.name(pkg.imports[j].name).to_string());
            cur = pkg.imports[j].outer;
            if chain.len() > 16 {
                return Err("import chain too deep".into());
            }
        }
        let path = chain.iter().rev().cloned().collect::<Vec<_>>().join(".");
        let root = chain.pop().ok_or("empty import")?;
        if chain.is_empty() {
            // A package import itself.
            return if self.path(&root).is_some() { Ok(path) } else { Err(format!("missing package {root}")) };
        }
        let class = pkg.name(pkg.imports[i].class_name).to_string();
        let Some(idx) = self.index(&root) else { return Err(format!("missing package {root}")) };
        let leaf = chain[0].clone();
        let groups = chain[1..].to_vec();
        if idx.find(&leaf, &groups, Some(&class)).or_else(|| idx.find(&leaf, &groups, None)).is_some() {
            Ok(path)
        } else {
            Err(format!("{path} ({class}) not found"))
        }
    }
}
