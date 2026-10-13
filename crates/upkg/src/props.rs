//! Tagged property lists (the default serialization for every UObject's script-visible fields).

use crate::package::{ObjRef, Package};
use crate::reader::Reader;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    Object(ObjRef),
    Name(String),
    Str(String),
    Vector([f32; 3]),
    /// Pitch, Yaw, Roll in Unreal angle units (65536 = full turn).
    Rotator([i32; 3]),
    Color([u8; 4]),
    /// A struct this crate does not decode into fields; raw bytes kept for later interpretation.
    Struct { name: String, raw: Vec<u8> },
    /// Dynamic array; elements left raw because element type is not stored in the tag.
    Array { raw: Vec<u8> },
    Unknown { kind: u8, raw: Vec<u8> },
}

#[derive(Debug, Clone)]
pub struct Property {
    pub name: String,
    pub index: u32,
    pub value: Value,
}

#[derive(Debug, Default, Clone)]
pub struct Properties(pub Vec<Property>);

impl Properties {
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.get_at(name, 0)
    }

    pub fn get_at(&self, name: &str, index: u32) -> Option<&Value> {
        self.0.iter().find(|p| p.index == index && p.name.eq_ignore_ascii_case(name)).map(|p| &p.value)
    }

    pub fn float(&self, name: &str) -> Option<f32> {
        match self.get(name)? {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn byte(&self, name: &str) -> Option<u8> {
        match self.get(name)? {
            Value::Byte(b) => Some(*b),
            _ => None,
        }
    }

    pub fn int(&self, name: &str) -> Option<i32> {
        match self.get(name)? {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn object(&self, name: &str) -> Option<ObjRef> {
        self.object_at(name, 0)
    }

    pub fn object_at(&self, name: &str, index: u32) -> Option<ObjRef> {
        match self.get_at(name, index)? {
            Value::Object(o) => Some(*o),
            _ => None,
        }
    }

    pub fn vector(&self, name: &str) -> Option<[f32; 3]> {
        match self.get(name)? {
            Value::Vector(v) => Some(*v),
            _ => None,
        }
    }

    pub fn rotator(&self, name: &str) -> Option<[i32; 3]> {
        match self.get(name)? {
            Value::Rotator(r) => Some(*r),
            _ => None,
        }
    }
}

// Tag type ids.
const T_BYTE: u8 = 1;
const T_INT: u8 = 2;
const T_BOOL: u8 = 3;
const T_FLOAT: u8 = 4;
const T_OBJECT: u8 = 5;
const T_NAME: u8 = 6;
const T_STRING: u8 = 7;
const T_CLASS: u8 = 8;
const T_ARRAY: u8 = 9;
const T_STRUCT: u8 = 10;
const T_STR: u8 = 13;

fn name_at<'p>(pkg: &'p Package, idx: i32) -> Result<&'p str> {
    pkg.names.get(idx as usize).map(|s| s.as_str()).ok_or(Error::Corrupt("property name index"))
}

/// Reads properties until the "None" terminator. Leaves the reader just past the terminator.
pub fn read_properties(r: &mut Reader, pkg: &Package) -> Result<Properties> {
    let mut out = Vec::new();
    loop {
        let name = name_at(pkg, r.compact()?)?;
        if name.eq_ignore_ascii_case("None") {
            break;
        }
        let info = r.u8()?;
        let kind = info & 0x0F;
        let size_code = (info >> 4) & 0x07;
        let is_array = info & 0x80 != 0;
        let struct_name = if kind == T_STRUCT { Some(name_at(pkg, r.compact()?)?.to_string()) } else { None };
        let size = match size_code {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 12,
            4 => 16,
            5 => r.u8()? as usize,
            6 => r.u16()? as usize,
            _ => r.i32()? as usize,
        };
        let mut index = 0u32;
        if is_array && kind != T_BOOL {
            let b = r.u8()? as u32;
            index = if b & 0x80 == 0 {
                b
            } else if b & 0xC0 == 0x80 {
                ((b & 0x7F) << 8) | r.u8()? as u32
            } else {
                let rest = r.bytes(3)?;
                ((b & 0x3F) << 24) | (rest[0] as u32) << 16 | (rest[1] as u32) << 8 | rest[2] as u32
            };
        }
        // Some level-saved structs (seen on SoundThing actors) store a size that is larger than
        // their actual tagged content. Prefer the stored size, but if the remainder of the list
        // cannot be parsed from there, fall back to the measured length of the nested tag list.
        let size = if kind == T_STRUCT {
            match tagged_struct_len(r, pkg) {
                Some(alt) if alt != size && !rest_parses(r, pkg, size) && rest_parses(r, pkg, alt) => alt,
                _ => size,
            }
        } else {
            size
        };
        let body = r.bytes(size)?;
        let mut b = Reader::new(body);
        let value = match kind {
            T_BYTE => Value::Byte(body.first().copied().unwrap_or(0)),
            T_INT => Value::Int(b.i32()?),
            T_BOOL => Value::Bool(is_array),
            T_FLOAT => Value::Float(b.f32()?),
            T_OBJECT | T_CLASS => Value::Object(ObjRef::from_raw(b.compact()?)),
            T_NAME => Value::Name(name_at(pkg, b.compact()?)?.to_string()),
            T_STR | T_STRING => Value::Str(b.fstring()?),
            T_ARRAY => Value::Array { raw: body.to_vec() },
            T_STRUCT => {
                let sn = struct_name.unwrap_or_default();
                match (sn.to_ascii_lowercase().as_str(), size) {
                    ("vector", 12) => Value::Vector(b.vec3()?),
                    ("rotator", 12) => Value::Rotator([b.i32()?, b.i32()?, b.i32()?]),
                    ("color", 4) => Value::Color([body[0], body[1], body[2], body[3]]),
                    _ => Value::Struct { name: sn, raw: body.to_vec() },
                }
            }
            k => Value::Unknown { kind: k, raw: body.to_vec() },
        };
        out.push(Property { name: name.to_string(), index, value });
    }
    Ok(Properties(out))
}

/// True if, after skipping `size` bytes, the rest of the property list parses to its terminator.
fn rest_parses(r: &Reader, pkg: &Package, size: usize) -> bool {
    let mut p = r.clone();
    p.skip(size).is_ok() && read_properties(&mut p, pkg).is_ok()
}

/// Length in bytes of a tagged property list starting at the reader (through the None terminator).
fn tagged_struct_len(r: &Reader, pkg: &Package) -> Option<usize> {
    let mut p = r.clone();
    let start = p.pos();
    read_properties(&mut p, pkg).ok()?;
    Some(p.pos() - start)
}

/// Object flag: object carries a script state frame before its properties.
pub const RF_HAS_STACK: u32 = 0x0200_0000;

/// Positions a reader at the start of an export's data, skips the state frame if present,
/// and parses the property list. Returns the reader positioned at the native (class-specific) data.
pub fn begin_object<'a>(pkg: &'a Package, export: usize) -> Result<(Reader<'a>, Properties)> {
    let e = &pkg.exports[export];
    let mut r = Reader::at(&pkg.data[..e.offset + e.size], e.offset);
    if e.flags & RF_HAS_STACK != 0 {
        let node = r.compact()?;
        let _state_node = r.compact()?;
        let _probe_mask = r.u64()?;
        let _latent = r.i32()?;
        if node != 0 {
            let _offset = r.compact()?;
        }
    }
    let props = read_properties(&mut r, pkg)?;
    Ok((r, props))
}
