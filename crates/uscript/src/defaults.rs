//! End-aware reader for class default properties.
//!
//! A few shipped classes store tagged-property sizes that do not match their contents (arrays of
//! structs whose recorded size is too small, structs whose recorded size is too large). Because
//! the class layout tells us exactly where the defaults list must end, this reader tries the
//! recorded size first and, when the rest of the list cannot reach the terminator exactly at the
//! end of the export, falls back to sizes measured from the nested tagged data.

use upkg::{ObjRef, Package, Properties, Property, Reader, Value};

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

struct Tag {
    name: String,
    kind: u8,
    is_array: bool,
    struct_name: Option<String>,
    size: usize,
    index: u32,
}

fn name_at(pkg: &Package, i: i32) -> Option<&str> {
    pkg.names.get(usize::try_from(i).ok()?).map(|s| s.as_str())
}

/// Reads one tag header. Returns None at the "None" terminator.
fn read_tag(r: &mut Reader, pkg: &Package) -> Option<Option<Tag>> {
    let name = name_at(pkg, r.compact().ok()?)?;
    if name.eq_ignore_ascii_case("None") {
        return Some(None);
    }
    let info = r.u8().ok()?;
    let kind = info & 0x0F;
    let is_array = info & 0x80 != 0;
    let struct_name = if kind == T_STRUCT { Some(name_at(pkg, r.compact().ok()?)?.to_string()) } else { None };
    let size = match (info >> 4) & 7 {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 12,
        4 => 16,
        5 => r.u8().ok()? as usize,
        6 => r.u16().ok()? as usize,
        _ => usize::try_from(r.i32().ok()?).ok()?,
    };
    let mut index = 0;
    if is_array && kind != T_BOOL {
        let b = r.u8().ok()? as u32;
        index = if b & 0x80 == 0 {
            b
        } else if b & 0xC0 == 0x80 {
            ((b & 0x7F) << 8) | r.u8().ok()? as u32
        } else {
            let x = r.bytes(3).ok()?;
            ((b & 0x3F) << 24) | (x[0] as u32) << 16 | (x[1] as u32) << 8 | x[2] as u32
        };
    }
    Some(Some(Tag { name: name.to_string(), kind, is_array, struct_name, size, index }))
}

/// Nested tagged list length; returns the position after the terminator.
fn tagged_end(data: &[u8], pos: usize, pkg: &Package, depth: u32) -> Option<usize> {
    if depth > 8 {
        return None;
    }
    let mut r = Reader::at(data, pos);
    loop {
        match read_tag(&mut r, pkg)? {
            None => return Some(r.pos()),
            Some(tag) => {
                let start = r.pos();
                let len = *candidate_sizes(data, start, &tag, pkg, depth + 1).first()?;
                if start + len > data.len() {
                    return None;
                }
                r.seek(start + len);
            }
        }
    }
}

/// Possible body lengths for a tag, most trusted first.
fn candidate_sizes(data: &[u8], start: usize, tag: &Tag, pkg: &Package, depth: u32) -> Vec<usize> {
    let mut v = vec![tag.size];
    match tag.kind {
        T_STRUCT => {
            if let Some(end) = tagged_end(data, start, pkg, depth) {
                v.push(end - start);
            }
        }
        T_ARRAY => {
            // Array of tagged structs: count, then that many tagged lists.
            let mut r = Reader::at(data, start);
            if let Ok(n) = r.compact() {
                if (1..=4096).contains(&n) {
                    let mut p = r.pos();
                    let mut ok = true;
                    for _ in 0..n {
                        match tagged_end(data, p, pkg, depth) {
                            Some(e) => p = e,
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        v.push(p - start);
                    }
                }
            }
        }
        _ => {}
    }
    v.dedup();
    v
}

fn decode_value(pkg: &Package, tag: &Tag, body: &[u8]) -> Value {
    let mut b = Reader::new(body);
    let raw = || body.to_vec();
    match tag.kind {
        T_BYTE => Value::Byte(body.first().copied().unwrap_or(0)),
        T_INT => b.i32().map(Value::Int).unwrap_or(Value::Unknown { kind: tag.kind, raw: raw() }),
        T_BOOL => Value::Bool(tag.is_array),
        T_FLOAT => b.f32().map(Value::Float).unwrap_or(Value::Unknown { kind: tag.kind, raw: raw() }),
        T_OBJECT | T_CLASS => Value::Object(b.compact().map(ObjRef::from_raw).unwrap_or(ObjRef::Null)),
        T_NAME => Value::Name(b.compact().ok().and_then(|i| name_at(pkg, i)).unwrap_or("None").to_string()),
        T_STR | T_STRING => Value::Str(b.fstring().unwrap_or_default()),
        T_ARRAY => Value::Array { raw: raw() },
        T_STRUCT => {
            let sn = tag.struct_name.clone().unwrap_or_default();
            match (sn.to_ascii_lowercase().as_str(), body.len()) {
                ("vector", 12) => Value::Vector(b.vec3().unwrap_or_default()),
                ("rotator", 12) => Value::Rotator([b.i32().unwrap_or(0), b.i32().unwrap_or(0), b.i32().unwrap_or(0)]),
                ("color", 4) => Value::Color([body[0], body[1], body[2], body[3]]),
                _ => Value::Struct { name: sn, raw: raw() },
            }
        }
        k => Value::Unknown { kind: k, raw: raw() },
    }
}

/// Parses a tagged list that must end exactly at `end`. Returns the properties and the end
/// position, or None if no consistent interpretation exists.
pub fn read_defaults(pkg: &Package, data: &[u8], pos: usize, end: usize) -> Option<Properties> {
    let mut out = Vec::new();
    if parse_from(pkg, data, pos, end, &mut out, 0) { Some(Properties(out)) } else { None }
}

fn parse_from(pkg: &Package, data: &[u8], pos: usize, end: usize, out: &mut Vec<Property>, budget: u32) -> bool {
    let mut r = Reader::at(&data[..end], pos);
    let tag = match read_tag(&mut r, pkg) {
        Some(Some(t)) => t,
        Some(None) => return r.pos() == end,
        None => return false,
    };
    let start = r.pos();
    for (k, len) in candidate_sizes(&data[..end], start, &tag, pkg, 0).into_iter().enumerate() {
        if start + len > end || (k > 0 && budget > 16) {
            continue;
        }
        let mark = out.len();
        out.push(Property { name: tag.name.clone(), index: tag.index, value: decode_value(pkg, &tag, &data[start..start + len]) });
        if parse_from(pkg, data, start + len, end, out, budget + k as u32) {
            return true;
        }
        out.truncate(mark);
    }
    false
}
