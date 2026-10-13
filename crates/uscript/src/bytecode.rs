//! UnrealScript bytecode decoder.
//!
//! A script is a sequence of expression tokens. `ScriptSize` in the struct header counts
//! *in-memory* bytes, where object references and names occupy 4 bytes each, while on disk they
//! are stored as compact indices. The decoder therefore tracks two cursors: the disk reader and
//! the memory offset. Jump targets inside scripts are memory offsets.
//!
//! Token numbering was established by decoding every function in the shipped packages and
//! checking that each token stream ends exactly at `ScriptSize`.

use crate::{Result, ScriptError};
use upkg::{ObjRef, Reader};

/// Native function call: tokens 0x70..=0xFF call the native with that index directly; tokens
/// 0x60..=0x6F carry the high nibble of a 12-bit index and are followed by the low byte.
pub const EX_EXTENDED_NATIVE: u8 = 0x60;
pub const EX_FIRST_NATIVE: u8 = 0x70;

/// A package-local name index (resolve with `Package::name`).
pub type NameIdx = u32;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LocalVar(ObjRef),
    InstanceVar(ObjRef),
    DefaultVar(ObjRef),
    Return(Box<Expr>),
    Switch { size: u8, expr: Box<Expr> },
    Jump(u16),
    JumpIfNot { target: u16, cond: Box<Expr> },
    Stop,
    Assert { line: u16, cond: Box<Expr> },
    /// `next` is the memory offset of the following case; 0xFFFF marks `default:`.
    Case { next: u16, value: Option<Box<Expr>> },
    Nothing,
    LabelTable(Vec<(NameIdx, u32)>),
    GotoLabel(Box<Expr>),
    EatString(Box<Expr>),
    Let { dst: Box<Expr>, src: Box<Expr> },
    DynArrayElement { index: Box<Expr>, array: Box<Expr> },
    New { outer: Box<Expr>, name: Box<Expr>, flags: Box<Expr>, class: Box<Expr> },
    ClassContext { obj: Box<Expr>, skip: u16, size: u8, expr: Box<Expr> },
    MetaCast { class: ObjRef, expr: Box<Expr> },
    LetBool { dst: Box<Expr>, src: Box<Expr> },
    SelfRef,
    Skip { len: u16, expr: Box<Expr> },
    Context { obj: Box<Expr>, skip: u16, size: u8, expr: Box<Expr> },
    ArrayElement { index: Box<Expr>, array: Box<Expr> },
    VirtualFunction { name: NameIdx, args: Vec<Expr> },
    FinalFunction { func: ObjRef, args: Vec<Expr> },
    GlobalFunction { name: NameIdx, args: Vec<Expr> },
    NativeCall { index: u16, args: Vec<Expr> },
    IntConst(i32),
    FloatConst(f32),
    StringConst(String),
    UnicodeStringConst(String),
    ObjectConst(ObjRef),
    NameConst(NameIdx),
    RotationConst([i32; 3]),
    VectorConst([f32; 3]),
    ByteConst(u8),
    IntZero,
    IntOne,
    True,
    False,
    NativeParm(ObjRef),
    NoObject,
    IntConstByte(u8),
    BoolVariable(Box<Expr>),
    DynamicCast { class: ObjRef, expr: Box<Expr> },
    Iterator { expr: Box<Expr>, end: u16 },
    IteratorPop,
    IteratorNext,
    StructCmpEq { strukt: ObjRef, a: Box<Expr>, b: Box<Expr> },
    StructCmpNe { strukt: ObjRef, a: Box<Expr>, b: Box<Expr> },
    StructMember { prop: ObjRef, expr: Box<Expr> },
    DynArrayLength(Box<Expr>),
    PrimitiveCast { kind: u8, expr: Box<Expr> },
    DynArrayInsert { array: Box<Expr>, index: Box<Expr>, count: Box<Expr> },
    DynArrayRemove { array: Box<Expr>, index: Box<Expr>, count: Box<Expr> },
    DelegateFunction { prop: ObjRef, name: NameIdx, args: Vec<Expr> },
    DelegateProperty(NameIdx),
    LetDelegate { dst: Box<Expr>, src: Box<Expr> },
}

/// A top-level token with the memory offset it starts at.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub offset: u32,
    pub expr: Expr,
}

/// Decoded script of one struct/function/state/class.
#[derive(Debug, Clone, Default)]
pub struct Script {
    pub stmts: Vec<Stmt>,
    /// In-memory size (equals the header's ScriptSize when decoding succeeded).
    pub mem_size: u32,
    /// Absolute disk byte range of the bytecode inside the package data.
    pub disk_start: usize,
    pub disk_end: usize,
}

impl Script {
    /// Index of the statement starting at a memory offset.
    pub fn stmt_at(&self, offset: u32) -> Option<usize> {
        self.stmts.binary_search_by_key(&offset, |s| s.offset).ok()
    }
}

pub struct Decoder<'a> {
    pub r: Reader<'a>,
    pub mem: u32,
    /// Index of the package's "None" name (used to terminate label tables).
    none_name: i32,
}

impl<'a> Decoder<'a> {
    pub fn new(r: Reader<'a>, none_name: i32) -> Self {
        Self { r, mem: 0, none_name }
    }

    fn u8(&mut self) -> Result<u8> {
        self.mem += 1;
        Ok(self.r.u8()?)
    }
    fn u16(&mut self) -> Result<u16> {
        self.mem += 2;
        Ok(self.r.u16()?)
    }
    fn i32(&mut self) -> Result<i32> {
        self.mem += 4;
        Ok(self.r.i32()?)
    }
    fn f32(&mut self) -> Result<f32> {
        self.mem += 4;
        Ok(self.r.f32()?)
    }
    fn obj(&mut self) -> Result<ObjRef> {
        self.mem += 4;
        Ok(ObjRef::from_raw(self.r.compact()?))
    }
    fn name(&mut self) -> Result<NameIdx> {
        self.mem += 4;
        let n = self.r.compact()?;
        if n < 0 {
            return Err(ScriptError::Bad(format!("negative name index at {}", self.r.pos())));
        }
        Ok(n as u32)
    }
    fn b(&mut self) -> Result<Box<Expr>> {
        Ok(Box::new(self.expr()?))
    }

    /// Arguments up to and including EX_EndFunctionParms.
    fn args(&mut self) -> Result<Vec<Expr>> {
        let mut v = Vec::new();
        loop {
            if self.r.data().get(self.r.pos()) == Some(&0x16) {
                self.u8()?;
                return Ok(v);
            }
            v.push(self.expr()?);
            if v.len() > 64 {
                return Err(ScriptError::Bad("runaway argument list".into()));
            }
        }
    }

    pub fn expr(&mut self) -> Result<Expr> {
        let at = self.r.pos();
        let tok = self.u8()?;
        Ok(match tok {
            0x00 => Expr::LocalVar(self.obj()?),
            0x01 => Expr::InstanceVar(self.obj()?),
            0x02 => Expr::DefaultVar(self.obj()?),
            0x04 => Expr::Return(self.b()?),
            0x05 => {
                let size = self.u8()?;
                Expr::Switch { size, expr: self.b()? }
            }
            0x06 => Expr::Jump(self.u16()?),
            0x07 => {
                let target = self.u16()?;
                Expr::JumpIfNot { target, cond: self.b()? }
            }
            0x08 => Expr::Stop,
            0x09 => {
                let line = self.u16()?;
                Expr::Assert { line, cond: self.b()? }
            }
            0x0A => {
                let next = self.u16()?;
                let value = if next == 0xFFFF { None } else { Some(self.b()?) };
                Expr::Case { next, value }
            }
            0x0B => Expr::Nothing,
            0x0C => {
                let mut labels = Vec::new();
                loop {
                    self.mem += 4;
                    let n = self.r.compact()?;
                    let off = self.r.u32()?;
                    self.mem += 4;
                    if n == self.none_name {
                        break;
                    }
                    labels.push((n as u32, off));
                    if labels.len() > 4096 {
                        return Err(ScriptError::Bad("runaway label table".into()));
                    }
                }
                Expr::LabelTable(labels)
            }
            0x0D => Expr::GotoLabel(self.b()?),
            0x0E => Expr::EatString(self.b()?),
            0x0F => Expr::Let { dst: self.b()?, src: self.b()? },
            0x10 => Expr::DynArrayElement { index: self.b()?, array: self.b()? },
            0x11 => Expr::New { outer: self.b()?, name: self.b()?, flags: self.b()?, class: self.b()? },
            0x12 | 0x19 => {
                let obj = self.b()?;
                let skip = self.u16()?;
                let size = self.u8()?;
                let expr = self.b()?;
                if tok == 0x12 { Expr::ClassContext { obj, skip, size, expr } } else { Expr::Context { obj, skip, size, expr } }
            }
            0x13 => Expr::MetaCast { class: self.obj()?, expr: self.b()? },
            0x14 => Expr::LetBool { dst: self.b()?, src: self.b()? },
            0x17 => Expr::SelfRef,
            0x18 => {
                let len = self.u16()?;
                Expr::Skip { len, expr: self.b()? }
            }
            0x1A => Expr::ArrayElement { index: self.b()?, array: self.b()? },
            0x1B => {
                let name = self.name()?;
                Expr::VirtualFunction { name, args: self.args()? }
            }
            0x1C => {
                let func = self.obj()?;
                Expr::FinalFunction { func, args: self.args()? }
            }
            0x1D => Expr::IntConst(self.i32()?),
            0x1E => Expr::FloatConst(self.f32()?),
            0x1F => {
                let mut s = String::new();
                loop {
                    let c = self.u8()?;
                    if c == 0 {
                        break;
                    }
                    s.push(c as char);
                }
                Expr::StringConst(s)
            }
            0x20 => Expr::ObjectConst(self.obj()?),
            0x21 => Expr::NameConst(self.name()?),
            0x22 => Expr::RotationConst([self.i32()?, self.i32()?, self.i32()?]),
            0x23 => Expr::VectorConst([self.f32()?, self.f32()?, self.f32()?]),
            0x24 => Expr::ByteConst(self.u8()?),
            0x25 => Expr::IntZero,
            0x26 => Expr::IntOne,
            0x27 => Expr::True,
            0x28 => Expr::False,
            0x29 => Expr::NativeParm(self.obj()?),
            0x2A => Expr::NoObject,
            0x2C => Expr::IntConstByte(self.u8()?),
            0x2D => Expr::BoolVariable(self.b()?),
            0x2E => Expr::DynamicCast { class: self.obj()?, expr: self.b()? },
            0x2F => {
                let expr = self.b()?;
                Expr::Iterator { expr, end: self.u16()? }
            }
            0x30 => Expr::IteratorPop,
            0x31 => Expr::IteratorNext,
            0x32 | 0x33 => {
                let strukt = self.obj()?;
                let (a, b) = (self.b()?, self.b()?);
                if tok == 0x32 { Expr::StructCmpEq { strukt, a, b } } else { Expr::StructCmpNe { strukt, a, b } }
            }
            0x34 => {
                let mut units = Vec::new();
                loop {
                    let c = self.u16()?;
                    if c == 0 {
                        break;
                    }
                    units.push(c);
                }
                Expr::UnicodeStringConst(String::from_utf16_lossy(&units))
            }
            0x36 => Expr::StructMember { prop: self.obj()?, expr: self.b()? },
            0x37 => Expr::DynArrayLength(self.b()?),
            0x38 => {
                let name = self.name()?;
                Expr::GlobalFunction { name, args: self.args()? }
            }
            0x39 => {
                let kind = self.u8()?;
                Expr::PrimitiveCast { kind, expr: self.b()? }
            }
            0x40 => Expr::DynArrayInsert { array: self.b()?, index: self.b()?, count: self.b()? },
            0x41 => Expr::DynArrayRemove { array: self.b()?, index: self.b()?, count: self.b()? },
            0x42 => {
                let prop = self.obj()?;
                let name = self.name()?;
                Expr::DelegateFunction { prop, name, args: self.args()? }
            }
            0x43 => Expr::DelegateProperty(self.name()?),
            0x44 => Expr::LetDelegate { dst: self.b()?, src: self.b()? },
            EX_EXTENDED_NATIVE..=0x6F => {
                let lo = self.u8()?;
                let index = (((tok - EX_EXTENDED_NATIVE) as u16) << 8) | lo as u16;
                Expr::NativeCall { index, args: self.args()? }
            }
            EX_FIRST_NATIVE..=0xFF => Expr::NativeCall { index: tok as u16, args: self.args()? },
            t => return Err(ScriptError::Bad(format!("unknown token {t:#04x} at disk offset {at}"))),
        })
    }
}

/// Decodes a whole script of `script_size` in-memory bytes starting at `start` in `data`.
pub fn decode_script(data: &[u8], start: usize, script_size: u32, none_name: i32) -> Result<Script> {
    let mut d = Decoder::new(Reader::at(data, start), none_name);
    let mut stmts = Vec::new();
    while d.mem < script_size {
        let offset = d.mem;
        let expr = d.expr().map_err(|e| ScriptError::Bad(format!("{e} (stmt at mem {offset:#x})")))?;
        stmts.push(Stmt { offset, expr });
    }
    if d.mem != script_size {
        return Err(ScriptError::Bad(format!("token stream overran ScriptSize: {} > {script_size}", d.mem)));
    }
    Ok(Script { stmts, mem_size: d.mem, disk_start: start, disk_end: d.r.pos() })
}

/// Visits an expression tree depth-first.
pub fn walk(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    use Expr::*;
    match e {
        Return(a) | Switch { expr: a, .. } | JumpIfNot { cond: a, .. } | Assert { cond: a, .. } | GotoLabel(a) | EatString(a)
        | MetaCast { expr: a, .. } | Skip { expr: a, .. } | BoolVariable(a) | DynamicCast { expr: a, .. } | Iterator { expr: a, .. }
        | StructMember { expr: a, .. } | DynArrayLength(a) | PrimitiveCast { expr: a, .. } => walk(a, f),
        Case { value: Some(a), .. } => walk(a, f),
        Let { dst: a, src: b } | LetBool { dst: a, src: b } | LetDelegate { dst: a, src: b } | DynArrayElement { index: a, array: b }
        | ArrayElement { index: a, array: b } | StructCmpEq { a, b, .. } | StructCmpNe { a, b, .. } => {
            walk(a, f);
            walk(b, f);
        }
        ClassContext { obj, expr, .. } | Context { obj, expr, .. } => {
            walk(obj, f);
            walk(expr, f);
        }
        New { outer, name, flags, class } => {
            for x in [outer, name, flags, class] {
                walk(x, f);
            }
        }
        DynArrayInsert { array, index, count } | DynArrayRemove { array, index, count } => {
            for x in [array, index, count] {
                walk(x, f);
            }
        }
        VirtualFunction { args, .. } | FinalFunction { args, .. } | GlobalFunction { args, .. } | NativeCall { args, .. }
        | DelegateFunction { args, .. } => {
            for a in args {
                walk(a, f);
            }
        }
        _ => {}
    }
}
