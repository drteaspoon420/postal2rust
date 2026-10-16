//! Writing UE2 packages (version 118, licensee 2): name table, import and export tables, export
//! data, and the tagged property / compact index encodings that match [`crate::props`] and
//! [`crate::reader`]. Used for save games.
//!
//! Layout written: summary, names, export data, imports, exports (the order the engine uses when
//! saving), with the summary pointing at each table. A package written here reads back with
//! [`crate::Package::parse`].

use crate::package::{ObjRef, PACKAGE_TAG};
use std::collections::HashMap;

pub const VERSION: u16 = 118;
pub const LICENSEE: u16 = 2;

/// Object flags used for saved objects (RF_Public | RF_Standalone-ish load flags as found in the
/// shipped saves) and the flag announcing a script state frame before the properties.
pub const RF_LOAD: u32 = 0x0007_0001;
pub const RF_HAS_STACK: u32 = 0x0200_0000;

/// Tag type ids (see `props.rs`).
pub const T_BYTE: u8 = 1;
pub const T_INT: u8 = 2;
pub const T_BOOL: u8 = 3;
pub const T_FLOAT: u8 = 4;
pub const T_OBJECT: u8 = 5;
pub const T_NAME: u8 = 6;
pub const T_ARRAY: u8 = 9;
pub const T_STRUCT: u8 = 10;
pub const T_STR: u8 = 13;

/// Little-endian byte sink with the UE2 primitive encodings.
#[derive(Default, Clone)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn vec3(&mut self, v: [f32; 3]) {
        for c in v {
            self.f32(c);
        }
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }

    /// Compact index (inverse of `Reader::compact`).
    pub fn compact(&mut self, v: i32) {
        let neg = v < 0;
        let mut a = (v as i64).unsigned_abs();
        let mut b0 = (a & 0x3F) as u8;
        if neg {
            b0 |= 0x80;
        }
        a >>= 6;
        if a != 0 {
            b0 |= 0x40;
        }
        self.0.push(b0);
        while a != 0 {
            let mut b = (a & 0x7F) as u8;
            a >>= 7;
            if a != 0 {
                b |= 0x80;
            }
            self.0.push(b);
        }
    }

    /// FString: Latin-1 when possible, else UTF-16LE (negative length), both NUL-terminated.
    pub fn fstring(&mut self, s: &str) {
        if s.is_empty() {
            self.compact(0);
            return;
        }
        if s.chars().all(|c| (c as u32) < 256) {
            self.compact(s.chars().count() as i32 + 1);
            for c in s.chars() {
                self.0.push(c as u32 as u8);
            }
            self.0.push(0);
        } else {
            let u: Vec<u16> = s.encode_utf16().collect();
            self.compact(-(u.len() as i32 + 1));
            for c in u {
                self.u16(c);
            }
            self.u16(0);
        }
    }
}

pub fn raw_ref(r: ObjRef) -> i32 {
    match r {
        ObjRef::Null => 0,
        ObjRef::Export(i) => i as i32 + 1,
        ObjRef::Import(i) => -(i as i32) - 1,
    }
}

#[derive(Clone)]
struct ImportEntry {
    class_package: usize,
    class_name: usize,
    outer: ObjRef,
    name: usize,
}

#[derive(Clone)]
pub struct ExportEntry {
    pub class: ObjRef,
    pub super_: ObjRef,
    pub outer: ObjRef,
    pub name: usize,
    pub flags: u32,
    pub data: Vec<u8>,
}

/// Builds a package in memory.
#[derive(Default)]
pub struct PackageWriter {
    names: Vec<String>,
    name_index: HashMap<String, usize>,
    /// Exact spelling -> index (fast path of [`PackageWriter::name`]).
    name_exact: HashMap<String, usize>,
    imports: Vec<ImportEntry>,
    import_index: HashMap<(String, String, i32, String), usize>,
    pub exports: Vec<ExportEntry>,
    pub package_flags: u32,
}

impl PackageWriter {
    pub fn new() -> Self {
        let mut w = Self::default();
        w.name("None");
        w
    }

    /// Index of a name (case-insensitive, first spelling kept), adding it if needed.
    pub fn name(&mut self, s: &str) -> usize {
        // Fast path: a spelling already seen (no lowercase allocation). Same index as the
        // case-insensitive lookup below would return.
        if let Some(&i) = self.name_exact.get(s) {
            return i;
        }
        let k = s.to_ascii_lowercase();
        if let Some(&i) = self.name_index.get(&k) {
            self.name_exact.insert(s.to_string(), i);
            return i;
        }
        let i = self.names.len();
        self.names.push(s.to_string());
        self.name_index.insert(k, i);
        self.name_exact.insert(s.to_string(), i);
        i
    }

    /// An import (deduplicated).
    pub fn import(&mut self, class_package: &str, class_name: &str, outer: ObjRef, name: &str) -> ObjRef {
        let key = (class_package.to_ascii_lowercase(), class_name.to_ascii_lowercase(), raw_ref(outer), name.to_ascii_lowercase());
        if let Some(&i) = self.import_index.get(&key) {
            return ObjRef::Import(i);
        }
        let e = ImportEntry { class_package: self.name(class_package), class_name: self.name(class_name), outer, name: self.name(name) };
        let i = self.imports.len();
        self.imports.push(e);
        self.import_index.insert(key, i);
        ObjRef::Import(i)
    }

    /// Imports `Package.Group...Name` of class `class_package.class_name`; the package and groups
    /// become `Core.Package` imports.
    pub fn import_path(&mut self, path: &str, class_package: &str, class_name: &str) -> ObjRef {
        let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return ObjRef::Null;
        }
        let mut outer = ObjRef::Null;
        for (k, p) in parts.iter().enumerate() {
            outer = if k + 1 == parts.len() { self.import(class_package, class_name, outer, p) } else { self.import("Core", "Package", outer, p) };
        }
        outer
    }

    /// Adds an export (data filled later with [`PackageWriter::set_data`]).
    pub fn add_export(&mut self, class: ObjRef, outer: ObjRef, name: &str, flags: u32) -> ObjRef {
        let name = self.name(name);
        self.exports.push(ExportEntry { class, super_: ObjRef::Null, outer, name, flags, data: Vec::new() });
        ObjRef::Export(self.exports.len() - 1)
    }

    pub fn set_data(&mut self, export: usize, data: Vec<u8>, flags: u32) {
        self.exports[export].data = data;
        self.exports[export].flags = flags;
    }

    /// Serializes the package.
    pub fn finish(&self) -> Vec<u8> {
        let mut o = Bytes::default();
        // Summary (patched at the end).
        o.u32(PACKAGE_TAG);
        o.u16(VERSION);
        o.u16(LICENSEE);
        o.u32(self.package_flags);
        let counts_at = o.0.len();
        for _ in 0..6 {
            o.i32(0);
        }
        // GUID and one generation (export count, name count), as engine-written packages have.
        o.bytes(&[0u8; 16]);
        o.i32(1);
        o.i32(self.exports.len() as i32);
        o.i32(self.names.len() as i32);
        let name_offset = o.0.len();
        for n in &self.names {
            o.fstring(n);
            o.u32(0x0007_0010);
        }
        let mut offsets = Vec::with_capacity(self.exports.len());
        for e in &self.exports {
            offsets.push(o.0.len());
            o.bytes(&e.data);
        }
        let import_offset = o.0.len();
        for i in &self.imports {
            o.compact(i.class_package as i32);
            o.compact(i.class_name as i32);
            o.i32(raw_ref(i.outer));
            o.compact(i.name as i32);
        }
        let export_offset = o.0.len();
        for (e, off) in self.exports.iter().zip(&offsets) {
            o.compact(raw_ref(e.class));
            o.compact(raw_ref(e.super_));
            o.i32(raw_ref(e.outer));
            o.compact(e.name as i32);
            o.u32(e.flags);
            o.compact(e.data.len() as i32);
            if !e.data.is_empty() {
                o.compact(*off as i32);
            }
        }
        let fields = [self.names.len(), name_offset, self.exports.len(), export_offset, self.imports.len(), import_offset];
        for (k, v) in fields.iter().enumerate() {
            o.0[counts_at + k * 4..counts_at + k * 4 + 4].copy_from_slice(&(*v as i32).to_le_bytes());
        }
        o.0
    }
}

/// Writes one property tag header plus body. `struct_name` for struct tags; `index` is the
/// static-array index. Bool values go in the tag (body must be empty).
pub fn write_tag(w: &mut PackageWriter, out: &mut Bytes, name: &str, kind: u8, struct_name: Option<&str>, index: u32, body: &[u8], bool_value: bool) {
    out.compact(w.name(name) as i32);
    let size = body.len();
    let code: u8 = match size {
        1 => 0,
        2 => 1,
        4 => 2,
        12 => 3,
        16 => 4,
        n if n <= 255 => 5,
        n if n <= 65535 => 6,
        _ => 7,
    };
    let is_array = if kind == T_BOOL { bool_value } else { index > 0 };
    out.u8(kind | (code << 4) | if is_array { 0x80 } else { 0 });
    if kind == T_STRUCT {
        out.compact(w.name(struct_name.unwrap_or("Struct")) as i32);
    }
    match code {
        5 => out.u8(size as u8),
        6 => out.u16(size as u16),
        7 => out.i32(size as i32),
        _ => {}
    }
    if is_array && kind != T_BOOL {
        if index < 128 {
            out.u8(index as u8);
        } else if index < 16384 {
            out.u8(0x80 | (index >> 8) as u8);
            out.u8(index as u8);
        } else {
            out.u8(0xC0 | (index >> 24) as u8);
            out.u8((index >> 16) as u8);
            out.u8((index >> 8) as u8);
            out.u8(index as u8);
        }
    }
    out.bytes(body);
}

/// Ends a property list.
pub fn write_none(w: &mut PackageWriter, out: &mut Bytes) {
    out.compact(w.name("None") as i32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Package, Reader, Value, read_properties};

    #[test]
    fn compact_roundtrip() {
        for v in [0, 1, -1, 63, 64, -64, 127, 128, 8191, 8192, 1 << 20, -(1 << 27), i32::MAX / 2] {
            let mut b = Bytes::default();
            b.compact(v);
            assert_eq!(Reader::new(&b.0).compact().unwrap(), v, "{v}");
        }
        let mut b = Bytes::default();
        b.fstring("Hällo");
        b.fstring("Привет");
        let mut r = Reader::new(&b.0);
        assert_eq!(r.fstring().unwrap(), "Hällo");
        assert_eq!(r.fstring().unwrap(), "Привет");
    }

    #[test]
    fn package_roundtrip() {
        let mut w = PackageWriter::new();
        let cls = w.import_path("Engine.Light", "Core", "Class");
        let tex = w.import_path("Engine.DefaultTexture", "Engine", "Texture");
        let e = w.add_export(cls, ObjRef::Null, "Light7", RF_LOAD);
        let mut body = Bytes::default();
        let mut f = Bytes::default();
        f.f32(1.5);
        write_tag(&mut w, &mut body, "LightBrightness", T_FLOAT, None, 0, &f.0, false);
        write_tag(&mut w, &mut body, "bHidden", T_BOOL, None, 0, &[], true);
        let mut v = Bytes::default();
        v.vec3([1.0, 2.0, 3.0]);
        write_tag(&mut w, &mut body, "Location", T_STRUCT, Some("Vector"), 0, &v.0, false);
        let mut o = Bytes::default();
        o.compact(raw_ref(tex));
        write_tag(&mut w, &mut body, "Skins", T_OBJECT, None, 2, &o.0, false);
        let mut s = Bytes::default();
        s.fstring("hello");
        write_tag(&mut w, &mut body, "Title", T_STR, None, 0, &s.0, false);
        write_none(&mut w, &mut body);
        let ObjRef::Export(i) = e else { panic!() };
        w.set_data(i, body.0, RF_LOAD);
        let bytes = w.finish();
        let p = Package::parse("x.usa".into(), "x".into(), bytes).unwrap();
        assert_eq!(p.exports.len(), 1);
        assert_eq!(p.export_class(0), "Light");
        assert_eq!(p.name(p.exports[0].name), "Light7");
        let (_, props) = crate::begin_object(&p, 0).unwrap();
        assert_eq!(props.float("LightBrightness"), Some(1.5));
        assert_eq!(props.bool("bHidden"), Some(true));
        assert_eq!(props.vector("Location"), Some([1.0, 2.0, 3.0]));
        assert_eq!(props.get_at("Skins", 2), Some(&Value::Object(tex)));
        assert_eq!(p.full_path(tex), "Engine.DefaultTexture");
        assert!(matches!(props.get("Title"), Some(Value::Str(s)) if s == "hello"));
        let _ = read_properties;
    }
}
