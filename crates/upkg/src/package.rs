//! Package container: header, name/import/export tables and object reference resolution.

use crate::reader::Reader;
use crate::{Error, Result};
use std::path::{Path, PathBuf};

pub const PACKAGE_TAG: u32 = 0x9E2A_83C1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjRef {
    Null,
    /// Index into `Package::exports`.
    Export(usize),
    /// Index into `Package::imports`.
    Import(usize),
}

impl ObjRef {
    pub fn from_raw(i: i32) -> Self {
        match i {
            0 => ObjRef::Null,
            i if i > 0 => ObjRef::Export(i as usize - 1),
            i => ObjRef::Import((-(i as i64) - 1) as usize),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Import {
    pub class_package: usize,
    pub class_name: usize,
    pub outer: ObjRef,
    pub name: usize,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub class: ObjRef,
    pub super_: ObjRef,
    pub outer: ObjRef,
    pub name: usize,
    pub flags: u32,
    pub size: usize,
    pub offset: usize,
}

pub struct Package {
    pub path: PathBuf,
    /// Lower-case file stem, e.g. "timb_mesh"; used as the package name for import resolution.
    pub name: String,
    pub version: u16,
    pub licensee: u16,
    pub flags: u32,
    pub names: Vec<String>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub data: Vec<u8>,
}

impl Package {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
        let name = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        Self::parse(path.to_path_buf(), name, data)
    }

    pub fn parse(path: PathBuf, name: String, data: Vec<u8>) -> Result<Self> {
        let mut r = Reader::new(&data);
        if data.len() < 36 {
            return Err(Error::NotAPackage(0));
        }
        let tag = r.u32()?;
        if tag != PACKAGE_TAG {
            return Err(Error::NotAPackage(tag));
        }
        let version = r.u16()?;
        let licensee = r.u16()?;
        let flags = r.u32()?;
        let name_count = r.i32()? as usize;
        let name_offset = r.i32()? as usize;
        let export_count = r.i32()? as usize;
        let export_offset = r.i32()? as usize;
        let import_count = r.i32()? as usize;
        let import_offset = r.i32()? as usize;

        let mut names = Vec::with_capacity(name_count);
        r.seek(name_offset);
        for _ in 0..name_count {
            names.push(r.fstring()?);
            let _flags = r.u32()?;
        }

        let mut imports = Vec::with_capacity(import_count);
        r.seek(import_offset);
        for _ in 0..import_count {
            imports.push(Import {
                class_package: r.compact()? as usize,
                class_name: r.compact()? as usize,
                outer: ObjRef::from_raw(r.i32()?),
                name: r.compact()? as usize,
            });
        }

        let mut exports = Vec::with_capacity(export_count);
        r.seek(export_offset);
        for _ in 0..export_count {
            let class = ObjRef::from_raw(r.compact()?);
            let super_ = ObjRef::from_raw(r.compact()?);
            let outer = ObjRef::from_raw(r.i32()?);
            let name = r.compact()? as usize;
            let flags = r.u32()?;
            let size = r.compact()? as usize;
            let offset = if size > 0 { r.compact()? as usize } else { 0 };
            exports.push(Export { class, super_, outer, name, flags, size, offset });
        }

        let pkg = Package { path, name, version, licensee, flags, names, imports, exports, data };
        pkg.validate()?;
        Ok(pkg)
    }

    fn validate(&self) -> Result<()> {
        let nn = self.names.len();
        for e in &self.exports {
            if e.name >= nn || e.offset + e.size > self.data.len() {
                return Err(Error::Corrupt("export out of range"));
            }
        }
        for i in &self.imports {
            if i.name >= nn || i.class_name >= nn || i.class_package >= nn {
                return Err(Error::Corrupt("import name out of range"));
            }
        }
        Ok(())
    }

    pub fn name(&self, i: usize) -> &str {
        self.names.get(i).map(|s| s.as_str()).unwrap_or("<bad-name>")
    }

    /// Short object name for a reference.
    pub fn ref_name(&self, r: ObjRef) -> &str {
        match r {
            ObjRef::Null => "None",
            ObjRef::Export(i) => self.name(self.exports[i].name),
            ObjRef::Import(i) => self.name(self.imports[i].name),
        }
    }

    pub fn outer_of(&self, r: ObjRef) -> ObjRef {
        match r {
            ObjRef::Null => ObjRef::Null,
            ObjRef::Export(i) => self.exports[i].outer,
            ObjRef::Import(i) => self.imports[i].outer,
        }
    }

    /// Dotted path including outers, e.g. "Timb_mesh.Fences.fence01". Exports are rooted at this package.
    pub fn full_path(&self, r: ObjRef) -> String {
        let mut parts = vec![self.ref_name(r).to_string()];
        let mut cur = self.outer_of(r);
        let mut guard = 0;
        while cur != ObjRef::Null && guard < 64 {
            parts.push(self.ref_name(cur).to_string());
            cur = self.outer_of(cur);
            guard += 1;
        }
        if matches!(r, ObjRef::Export(_)) {
            parts.push(self.name.clone());
        }
        parts.reverse();
        parts.join(".")
    }

    /// Class name of an export ("Class" for UClass objects whose class ref is null).
    pub fn export_class(&self, i: usize) -> &str {
        match self.exports[i].class {
            ObjRef::Null => "Class",
            c => self.ref_name(c),
        }
    }

    /// Class name of any reference (imports carry their class name directly).
    pub fn class_of(&self, r: ObjRef) -> &str {
        match r {
            ObjRef::Null => "None",
            ObjRef::Export(i) => self.export_class(i),
            ObjRef::Import(i) => self.name(self.imports[i].class_name),
        }
    }

    pub fn export_data(&self, i: usize) -> &[u8] {
        let e = &self.exports[i];
        &self.data[e.offset..e.offset + e.size]
    }

    pub fn find_export(&self, name: &str, class: Option<&str>) -> Option<usize> {
        self.exports.iter().enumerate().position(|(i, e)| {
            self.name(e.name).eq_ignore_ascii_case(name) && class.is_none_or(|c| self.export_class(i).eq_ignore_ascii_case(c))
        })
    }

    pub fn exports_of_class<'s>(&'s self, class: &'s str) -> impl Iterator<Item = usize> + 's {
        (0..self.exports.len()).filter(move |&i| self.export_class(i).eq_ignore_ascii_case(class))
    }
}
