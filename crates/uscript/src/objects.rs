//! Script object graph: the serialized forms of Class, State, Function, Struct, Enum, Const and
//! the Property subclasses as found in POSTAL 2 packages (version 118, licensee 2).
//!
//! Layouts were determined from the shipped `.u` files and every parse is checked to consume its
//! export exactly (see the `usscan` tool).

use crate::bytecode::{Script, decode_script};
use crate::{Result, ScriptError};
use upkg::{ObjRef, Package, Properties, Reader, read_properties};

/// Function flags.
pub mod func_flags {
    pub const FINAL: u32 = 0x0000_0001;
    pub const DEFINED: u32 = 0x0000_0002;
    pub const ITERATOR: u32 = 0x0000_0004;
    pub const LATENT: u32 = 0x0000_0008;
    pub const PRE_OPERATOR: u32 = 0x0000_0010;
    pub const SINGULAR: u32 = 0x0000_0020;
    pub const NET: u32 = 0x0000_0040;
    pub const NET_RELIABLE: u32 = 0x0000_0080;
    pub const SIMULATED: u32 = 0x0000_0100;
    pub const EXEC: u32 = 0x0000_0200;
    pub const NATIVE: u32 = 0x0000_0400;
    pub const EVENT: u32 = 0x0000_0800;
    pub const OPERATOR: u32 = 0x0000_1000;
    pub const STATIC: u32 = 0x0000_2000;
    pub const NO_EXPORT: u32 = 0x0000_4000;
    pub const CONST: u32 = 0x0000_8000;
    pub const INVARIANT: u32 = 0x0001_0000;
    pub const PUBLIC: u32 = 0x0002_0000;
    pub const PRIVATE: u32 = 0x0004_0000;
    pub const PROTECTED: u32 = 0x0008_0000;
    pub const DELEGATE: u32 = 0x0010_0000;
}

/// Property flags.
pub mod prop_flags {
    pub const EDIT: u32 = 0x0000_0001;
    pub const CONST: u32 = 0x0000_0002;
    pub const INPUT: u32 = 0x0000_0004;
    pub const EXPORT_OBJECT: u32 = 0x0000_0008;
    pub const OPTIONAL_PARM: u32 = 0x0000_0010;
    pub const NET: u32 = 0x0000_0020;
    pub const CONST_REF: u32 = 0x0000_0040;
    pub const PARM: u32 = 0x0000_0080;
    pub const OUT_PARM: u32 = 0x0000_0100;
    pub const SKIP_PARM: u32 = 0x0000_0200;
    pub const RETURN_PARM: u32 = 0x0000_0400;
    pub const COERCE_PARM: u32 = 0x0000_0800;
    pub const NATIVE: u32 = 0x0000_1000;
    pub const TRANSIENT: u32 = 0x0000_2000;
    pub const CONFIG: u32 = 0x0000_4000;
    pub const LOCALIZED: u32 = 0x0000_8000;
    pub const TRAVEL: u32 = 0x0001_0000;
    pub const EDIT_CONST: u32 = 0x0002_0000;
    pub const GLOBAL_CONFIG: u32 = 0x0004_0000;
    pub const NEED_CTOR_LINK: u32 = 0x0040_0000;
    /// POSTAL 2 extension: a doc comment FString follows the category / replication offset.
    pub const COMMENT: u32 = 0x0200_0000;
}

/// Common header of every UStruct-derived object.
#[derive(Debug, Clone)]
pub struct StructData {
    pub super_: ObjRef,
    pub next: ObjRef,
    pub script_text: ObjRef,
    /// First child field (linked through each field's `next`).
    pub children: ObjRef,
    pub friendly_name: u32,
    pub line: i32,
    pub text_pos: i32,
    pub script_size: u32,
    pub script: Script,
}

#[derive(Debug, Clone)]
pub struct FunctionData {
    pub base: StructData,
    pub native: u16,
    pub precedence: u8,
    pub flags: u32,
    pub rep_offset: Option<u16>,
}

impl FunctionData {
    pub fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }
}

#[derive(Debug, Clone)]
pub struct StateData {
    pub base: StructData,
    pub probe_mask: u64,
    pub ignore_mask: u64,
    pub label_table_offset: u16,
    pub state_flags: u32,
}

#[derive(Debug, Clone)]
pub struct Dependency {
    pub class: ObjRef,
    pub deep: u32,
    pub script_text_crc: u32,
}

#[derive(Debug, Clone)]
pub struct ClassData {
    pub state: StateData,
    pub class_flags: u32,
    pub guid: [u8; 16],
    pub dependencies: Vec<Dependency>,
    /// Name indices of packages this class imports from.
    pub package_imports: Vec<u32>,
    pub within: ObjRef,
    pub config_name: u32,
    pub hide_categories: Vec<u32>,
    pub defaults: Properties,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropKind {
    Byte { enum_: ObjRef },
    Int,
    Bool,
    Float,
    Object { class: ObjRef },
    Class { class: ObjRef, meta: ObjRef },
    Name,
    Str,
    String { len: i32 },
    FixedArray { inner: ObjRef, count: i32 },
    Array { inner: ObjRef },
    Map { key: ObjRef, value: ObjRef },
    Struct { strukt: ObjRef },
    Delegate { func: ObjRef },
    Pointer,
}

impl PropKind {
    pub fn class_name(&self) -> &'static str {
        match self {
            PropKind::Byte { .. } => "ByteProperty",
            PropKind::Int => "IntProperty",
            PropKind::Bool => "BoolProperty",
            PropKind::Float => "FloatProperty",
            PropKind::Object { .. } => "ObjectProperty",
            PropKind::Class { .. } => "ClassProperty",
            PropKind::Name => "NameProperty",
            PropKind::Str => "StrProperty",
            PropKind::String { .. } => "StringProperty",
            PropKind::FixedArray { .. } => "FixedArrayProperty",
            PropKind::Array { .. } => "ArrayProperty",
            PropKind::Map { .. } => "MapProperty",
            PropKind::Struct { .. } => "StructProperty",
            PropKind::Delegate { .. } => "DelegateProperty",
            PropKind::Pointer => "PointerProperty",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PropertyData {
    pub next: ObjRef,
    pub array_dim: i32,
    pub flags: u32,
    pub category: u32,
    pub rep_offset: Option<u16>,
    /// Editor tooltip / doc comment (licensee extension, present when `prop_flags::COMMENT` is set).
    pub comment: Option<String>,
    pub kind: PropKind,
}

impl PropertyData {
    pub fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }
}

#[derive(Debug, Clone)]
pub struct EnumData {
    pub next: ObjRef,
    pub names: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct ConstData {
    pub next: ObjRef,
    pub value: String,
}

#[derive(Debug, Clone)]
pub enum ScriptObject {
    Class(Box<ClassData>),
    State(StateData),
    Function(FunctionData),
    Struct(StructData),
    Property(PropertyData),
    Enum(EnumData),
    Const(ConstData),
}

impl ScriptObject {
    /// Next sibling in the owning struct's field list.
    pub fn next(&self) -> ObjRef {
        match self {
            ScriptObject::Class(c) => c.state.base.next,
            ScriptObject::State(s) => s.base.next,
            ScriptObject::Function(f) => f.base.next,
            ScriptObject::Struct(s) => s.next,
            ScriptObject::Property(p) => p.next,
            ScriptObject::Enum(e) => e.next,
            ScriptObject::Const(c) => c.next,
        }
    }

    pub fn struct_data(&self) -> Option<&StructData> {
        match self {
            ScriptObject::Class(c) => Some(&c.state.base),
            ScriptObject::State(s) => Some(&s.base),
            ScriptObject::Function(f) => Some(&f.base),
            ScriptObject::Struct(s) => Some(s),
            _ => None,
        }
    }
}

/// True for export classes this module understands.
pub fn is_script_class(class: &str) -> bool {
    matches!(class, "Class" | "State" | "Function" | "Struct" | "Enum" | "Const") || class.ends_with("Property")
}

/// Index of the "None" name in a package (terminates tagged lists and label tables).
pub fn none_name(pkg: &Package) -> i32 {
    pkg.names.iter().position(|n| n.eq_ignore_ascii_case("None")).map(|i| i as i32).unwrap_or(-1)
}

fn obj(r: &mut Reader) -> Result<ObjRef> {
    Ok(ObjRef::from_raw(r.compact()?))
}

fn name(r: &mut Reader) -> Result<u32> {
    let n = r.compact()?;
    if n < 0 {
        return Err(ScriptError::Bad("negative name index".into()));
    }
    Ok(n as u32)
}

fn read_struct(r: &mut Reader, pkg: &Package, with_props: bool) -> Result<StructData> {
    if with_props {
        // Tagged properties of the object itself (always empty for script fields).
        read_properties(r, pkg)?;
    }
    let super_ = obj(r)?;
    let next = obj(r)?;
    let script_text = obj(r)?;
    let children = obj(r)?;
    let friendly_name = name(r)?;
    let line = r.i32()?;
    let text_pos = r.i32()?;
    let script_size = r.i32()?;
    if !(0..=1 << 20).contains(&script_size) {
        return Err(ScriptError::Bad(format!("implausible ScriptSize {script_size}")));
    }
    let script = decode_script(r.data(), r.pos(), script_size as u32, none_name(pkg))?;
    r.seek(script.disk_end);
    Ok(StructData { super_, next, script_text, children, friendly_name, line, text_pos, script_size: script_size as u32, script })
}

fn read_state(r: &mut Reader, pkg: &Package, with_props: bool) -> Result<StateData> {
    let base = read_struct(r, pkg, with_props)?;
    Ok(StateData { base, probe_mask: r.u64()?, ignore_mask: r.u64()?, label_table_offset: r.u16()?, state_flags: r.u32()? })
}

fn names_array(r: &mut Reader) -> Result<Vec<u32>> {
    Ok(r.array(|r| r.compact().map(|n| n.max(0) as u32))?)
}

/// Parses one script export. Returns `Ok(None)` for exports that are not script fields.
/// Fails if the parse does not end exactly at the end of the export's data.
pub fn parse_export(pkg: &Package, export: usize) -> Result<Option<ScriptObject>> {
    let class = pkg.export_class(export).to_string();
    if !is_script_class(&class) {
        return Ok(None);
    }
    let e = &pkg.exports[export];
    let end = e.offset + e.size;
    let mut r = Reader::at(&pkg.data[..end], e.offset);
    let out = match class.as_str() {
        "Class" => {
            // Class objects carry no tagged property block of their own.
            let state = read_state(&mut r, pkg, false)?;
            let class_flags = r.u32()?;
            let guid: [u8; 16] = r.bytes(16)?.try_into().unwrap();
            let dependencies =
                r.array(|r| Ok(Dependency { class: ObjRef::from_raw(r.compact()?), deep: r.u32()?, script_text_crc: r.u32()? }))?;
            let package_imports = names_array(&mut r)?;
            let within = obj(&mut r)?;
            let config_name = name(&mut r)?;
            let hide_categories = names_array(&mut r)?;
            let defaults = crate::defaults::read_defaults(pkg, r.data(), r.pos(), end)
                .ok_or_else(|| ScriptError::Bad("class defaults do not parse to the end of the export".into()))?;
            r.seek(end);
            ScriptObject::Class(Box::new(ClassData {
                state,
                class_flags,
                guid,
                dependencies,
                package_imports,
                within,
                config_name,
                hide_categories,
                defaults,
            }))
        }
        "State" => ScriptObject::State(read_state(&mut r, pkg, true)?),
        "Function" => {
            let base = read_struct(&mut r, pkg, true)?;
            let native = r.u16()?;
            let precedence = r.u8()?;
            let flags = r.u32()?;
            let rep_offset = if flags & func_flags::NET != 0 { Some(r.u16()?) } else { None };
            ScriptObject::Function(FunctionData { base, native, precedence, flags, rep_offset })
        }
        "Struct" => ScriptObject::Struct(read_struct(&mut r, pkg, true)?),
        "Enum" => {
            read_properties(&mut r, pkg)?;
            let _super = obj(&mut r)?;
            let next = obj(&mut r)?;
            ScriptObject::Enum(EnumData { next, names: names_array(&mut r)? })
        }
        "Const" => {
            read_properties(&mut r, pkg)?;
            let _super = obj(&mut r)?;
            let next = obj(&mut r)?;
            ScriptObject::Const(ConstData { next, value: r.fstring()? })
        }
        _ => {
            read_properties(&mut r, pkg)?;
            let _super = obj(&mut r)?;
            let next = obj(&mut r)?;
            let array_dim = r.i32()?;
            let flags = r.u32()?;
            let category = name(&mut r)?;
            let rep_offset = if flags & prop_flags::NET != 0 { Some(r.u16()?) } else { None };
            let comment = if flags & prop_flags::COMMENT != 0 { Some(r.fstring()?) } else { None };
            let kind = match class.as_str() {
                "ByteProperty" => PropKind::Byte { enum_: obj(&mut r)? },
                "IntProperty" => PropKind::Int,
                "BoolProperty" => PropKind::Bool,
                "FloatProperty" => PropKind::Float,
                "ObjectProperty" => PropKind::Object { class: obj(&mut r)? },
                "ClassProperty" => PropKind::Class { class: obj(&mut r)?, meta: obj(&mut r)? },
                "NameProperty" => PropKind::Name,
                "StrProperty" => PropKind::Str,
                "StringProperty" => PropKind::String { len: r.i32()? },
                "FixedArrayProperty" => PropKind::FixedArray { inner: obj(&mut r)?, count: r.i32()? },
                "ArrayProperty" => PropKind::Array { inner: obj(&mut r)? },
                "MapProperty" => PropKind::Map { key: obj(&mut r)?, value: obj(&mut r)? },
                "StructProperty" => PropKind::Struct { strukt: obj(&mut r)? },
                "DelegateProperty" => PropKind::Delegate { func: obj(&mut r)? },
                "PointerProperty" => PropKind::Pointer,
                other => return Err(ScriptError::Bad(format!("unknown property class {other}"))),
            };
            ScriptObject::Property(PropertyData { next, array_dim, flags, category, rep_offset, comment, kind })
        }
    };
    if r.pos() != end {
        return Err(ScriptError::Bad(format!("{class} parse ended at {} of {} bytes", r.pos() - e.offset, e.size)));
    }
    Ok(Some(out))
}
