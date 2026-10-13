//! Renders decoded scripts as readable pseudo-code.
//!
//! Output is a flat statement listing: each statement is prefixed with its memory offset, and
//! jumps refer to those offsets. Expressions are printed in infix form using the operator
//! symbols recovered from the native function declarations.

use crate::bytecode::Expr;
use crate::graph::{NativeTable, ScriptPackage};
use crate::objects::{FunctionData, PropKind, PropertyData, ScriptObject, func_flags, prop_flags};
use std::collections::BTreeSet;
use std::fmt::Write;
use upkg::ObjRef;

/// Names for the primitive conversion sub-tokens used by EX_PrimitiveCast.
pub fn cast_name(kind: u8) -> &'static str {
    match kind {
        0x39 => "vector",   // rotator -> vector
        0x3A => "int",      // byte -> int
        0x3B => "bool",     // byte -> bool
        0x3C => "float",    // byte -> float
        0x3D => "byte",     // int -> byte
        0x3E => "bool",     // int -> bool
        0x3F => "float",    // int -> float
        0x40 => "byte",     // bool -> byte
        0x41 => "int",      // bool -> int
        0x42 => "float",    // bool -> float
        0x43 => "byte",     // float -> byte
        0x44 => "int",      // float -> int
        0x45 => "bool",     // float -> bool
        0x47 => "bool",     // object -> bool
        0x48 => "bool",     // name -> bool
        0x49 => "byte",     // string -> byte
        0x4A => "int",      // string -> int
        0x4B => "bool",     // string -> bool
        0x4C => "float",    // string -> float
        0x4D => "vector",   // string -> vector
        0x4E => "rotator",  // string -> rotator
        0x4F => "bool",     // vector -> bool
        0x50 => "rotator",  // vector -> rotator
        0x51 => "bool",     // rotator -> bool
        0x52..=0x59 => "string",
        _ => "cast?",
    }
}

pub struct Disasm<'a> {
    pub sp: &'a ScriptPackage,
    pub natives: &'a NativeTable,
}

impl<'a> Disasm<'a> {
    fn n(&self, r: ObjRef) -> &str {
        self.sp.pkg.ref_name(r)
    }
    fn nm(&self, i: u32) -> &str {
        self.sp.pkg.name(i as usize)
    }

    fn args(&self, args: &[Expr]) -> String {
        args.iter().map(|a| self.expr(a)).collect::<Vec<_>>().join(", ")
    }

    pub fn expr(&self, e: &Expr) -> String {
        use Expr::*;
        match e {
            LocalVar(r) | InstanceVar(r) | NativeParm(r) => self.n(*r).to_string(),
            DefaultVar(r) => format!("default.{}", self.n(*r)),
            Return(x) => match **x {
                Nothing => "return".into(),
                _ => format!("return {}", self.expr(x)),
            },
            Switch { expr, .. } => format!("switch ({})", self.expr(expr)),
            Jump(t) => format!("goto L{t:04X}"),
            JumpIfNot { target, cond } => format!("if (!({})) goto L{target:04X}", self.expr(cond)),
            Stop => "stop".into(),
            Assert { line, cond } => format!("assert({}) /* line {line} */", self.expr(cond)),
            Case { next: 0xFFFF, .. } => "default:".into(),
            Case { next, value } => format!("case {}: /* next L{next:04X} */", value.as_ref().map(|v| self.expr(v)).unwrap_or_default()),
            Nothing => String::new(),
            LabelTable(l) => {
                let s: Vec<String> = l.iter().map(|(n, o)| format!("{}=L{o:04X}", self.nm(*n))).collect();
                format!("labels {{ {} }}", s.join(", "))
            }
            GotoLabel(x) => format!("goto {}", self.expr(x)),
            EatString(x) => self.expr(x),
            Let { dst, src } | LetBool { dst, src } | LetDelegate { dst, src } => format!("{} = {}", self.expr(dst), self.expr(src)),
            DynArrayElement { index, array } | ArrayElement { index, array } => format!("{}[{}]", self.expr(array), self.expr(index)),
            New { outer, name, flags, class } => {
                let mut parts = Vec::new();
                for x in [outer, name, flags] {
                    if !matches!(**x, Nothing) {
                        parts.push(self.expr(x));
                    }
                }
                if parts.is_empty() {
                    format!("new {}", self.expr(class))
                } else {
                    format!("new ({}) {}", parts.join(", "), self.expr(class))
                }
            }
            ClassContext { obj, expr, .. } => match **expr {
                DefaultVar(_) => format!("{}.{}", self.expr(obj), self.expr(expr)),
                _ => format!("{}.static.{}", self.expr(obj), self.expr(expr)),
            },
            Context { obj, expr, .. } => format!("{}.{}", self.expr(obj), self.expr(expr)),
            MetaCast { class, expr } => format!("class<{}>({})", self.n(*class), self.expr(expr)),
            SelfRef => "self".into(),
            Skip { expr, .. } => self.expr(expr),
            VirtualFunction { name, args } => format!("{}({})", self.nm(*name), self.args(args)),
            GlobalFunction { name, args } => format!("global.{}({})", self.nm(*name), self.args(args)),
            // A statically bound call to a function that is not itself final is a `super` call.
            FinalFunction { func, args } => {
                let is_super = match self.sp.get(*func) {
                    Some(ScriptObject::Function(f)) => !f.has(func_flags::FINAL),
                    _ => false,
                };
                if is_super {
                    format!("super({}).{}({})", self.n(self.sp.pkg.outer_of(*func)), self.n(*func), self.args(args))
                } else {
                    format!("{}({})", self.n(*func), self.args(args))
                }
            }
            DelegateFunction { name, args, .. } => format!("{}({})", self.nm(*name), self.args(args)),
            DelegateProperty(n) => self.nm(*n).to_string(),
            NativeCall { index, args } => self.native(*index, args),
            IntConst(i) => i.to_string(),
            FloatConst(f) => fmt_float(*f),
            StringConst(s) | UnicodeStringConst(s) => format!("{s:?}"),
            ObjectConst(r) => format!("{}'{}'", self.sp.pkg.class_of(*r), self.sp.pkg.full_path(*r)),
            NameConst(n) => format!("'{}'", self.nm(*n)),
            RotationConst([p, y, r]) => format!("rot({p}, {y}, {r})"),
            VectorConst([x, y, z]) => format!("vect({}, {}, {})", fmt_float(*x), fmt_float(*y), fmt_float(*z)),
            ByteConst(b) | IntConstByte(b) => b.to_string(),
            IntZero => "0".into(),
            IntOne => "1".into(),
            True => "true".into(),
            False => "false".into(),
            NoObject => "None".into(),
            BoolVariable(x) => self.expr(x),
            DynamicCast { class, expr } => format!("{}({})", self.n(*class), self.expr(expr)),
            Iterator { expr, end } => format!("foreach {} /* end L{end:04X} */", self.expr(expr)),
            IteratorPop => "iterator_pop".into(),
            IteratorNext => "iterator_next".into(),
            StructCmpEq { a, b, .. } => format!("({} == {})", self.expr(a), self.expr(b)),
            StructCmpNe { a, b, .. } => format!("({} != {})", self.expr(a), self.expr(b)),
            StructMember { prop, expr } => format!("{}.{}", self.expr(expr), self.n(*prop)),
            DynArrayLength(x) => format!("{}.Length", self.expr(x)),
            PrimitiveCast { kind, expr } => format!("{}({})", cast_name(*kind), self.expr(expr)),
            DynArrayInsert { array, index, count } => format!("{}.Insert({}, {})", self.expr(array), self.expr(index), self.expr(count)),
            DynArrayRemove { array, index, count } => format!("{}.Remove({}, {})", self.expr(array), self.expr(index), self.expr(count)),
        }
    }

    fn native(&self, index: u16, args: &[Expr]) -> String {
        let Some(info) = self.natives.get(index) else {
            return format!("native{index}({})", self.args(args));
        };
        if info.is_operator() {
            let op = &info.friendly;
            match args.len() {
                2 => return format!("({} {op} {})", self.expr(&args[0]), self.expr(&args[1])),
                1 if info.is_preop() => return format!("{op}{}", self.expr(&args[0])),
                1 => return format!("{}{op}", self.expr(&args[0])),
                _ => {}
            }
        }
        format!("{}({})", info.name, self.args(args))
    }

    pub fn type_name(&self, p: &PropertyData) -> String {
        let base = match &p.kind {
            PropKind::Byte { enum_ } if *enum_ != ObjRef::Null => self.n(*enum_).to_string(),
            PropKind::Byte { .. } => "byte".into(),
            PropKind::Int => "int".into(),
            PropKind::Bool => "bool".into(),
            PropKind::Float => "float".into(),
            PropKind::Object { class } => self.n(*class).to_string(),
            PropKind::Class { meta, .. } => format!("class<{}>", self.n(*meta)),
            PropKind::Name => "name".into(),
            PropKind::Str => "string".into(),
            PropKind::String { len } => format!("string[{len}]"),
            PropKind::FixedArray { inner, count } => {
                format!("{}[{count}]", self.sp.property(*inner).map(|i| self.type_name(i)).unwrap_or_else(|| "?".into()))
            }
            PropKind::Array { inner } => {
                format!("array<{}>", self.sp.property(*inner).map(|i| self.type_name(i)).unwrap_or_else(|| "?".into()))
            }
            PropKind::Map { .. } => "map".into(),
            PropKind::Struct { strukt } => self.n(*strukt).to_string(),
            PropKind::Delegate { func } => format!("delegate<{}>", self.n(*func)),
            PropKind::Pointer => "pointer".into(),
        };
        base
    }

    /// Function header such as `static final function float RandRange(float Min, float Max)`.
    pub fn signature(&self, export: usize, f: &FunctionData) -> String {
        let mut params = Vec::new();
        let mut ret = None;
        for c in self.sp.children(export) {
            let Some(ScriptObject::Property(p)) = &self.sp.objects[c] else { continue };
            if p.has(prop_flags::RETURN_PARM) {
                ret = Some(self.type_name(p));
            } else if p.has(prop_flags::PARM) {
                let mut s = String::new();
                if p.has(prop_flags::OPTIONAL_PARM) {
                    s.push_str("optional ");
                }
                if p.has(prop_flags::OUT_PARM) {
                    s.push_str("out ");
                }
                if p.has(prop_flags::COERCE_PARM) {
                    s.push_str("coerce ");
                }
                let _ = write!(s, "{} {}", self.type_name(p), self.sp.export_name(c));
                params.push(s);
            }
        }
        let mut mods = Vec::new();
        for (bit, word) in [
            (func_flags::NATIVE, "native"),
            (func_flags::STATIC, "static"),
            (func_flags::FINAL, "final"),
            (func_flags::SIMULATED, "simulated"),
            (func_flags::SINGULAR, "singular"),
            (func_flags::EXEC, "exec"),
            (func_flags::LATENT, "latent"),
            (func_flags::ITERATOR, "iterator"),
            (func_flags::PRIVATE, "private"),
            (func_flags::PROTECTED, "protected"),
            (func_flags::CONST, "const"),
        ] {
            if f.has(bit) {
                mods.push(if bit == func_flags::NATIVE && f.native != 0 { format!("native({})", f.native) } else { word.to_string() });
            }
        }
        let kind = if f.has(func_flags::DELEGATE) {
            "delegate"
        } else if f.has(func_flags::OPERATOR) {
            if f.has(func_flags::PRE_OPERATOR) { "preoperator" } else { "operator" }
        } else if f.has(func_flags::EVENT) {
            "event"
        } else {
            "function"
        };
        let name = if f.has(func_flags::OPERATOR) {
            self.sp.pkg.name(f.base.friendly_name as usize).to_string()
        } else {
            self.sp.export_name(export).to_string()
        };
        let mut s = mods.join(" ");
        if !s.is_empty() {
            s.push(' ');
        }
        let _ = write!(s, "{kind} {}{name}({})", ret.map(|r| r + " ").unwrap_or_default(), params.join(", "));
        s
    }

    /// Locals (non-parameter properties) of a function.
    pub fn locals(&self, export: usize) -> Vec<String> {
        let mut out = Vec::new();
        for c in self.sp.children(export) {
            if let Some(ScriptObject::Property(p)) = &self.sp.objects[c] {
                if !p.has(prop_flags::PARM) {
                    out.push(format!("local {} {};", self.type_name(p), self.sp.export_name(c)));
                }
            }
        }
        out
    }

    /// Statement listing of a struct's script.
    pub fn body(&self, export: usize, indent: &str) -> String {
        let mut s = String::new();
        let Some(Some(o)) = self.sp.objects.get(export) else { return s };
        let Some(sd) = o.struct_data() else { return s };
        // Offsets that are jump targets get a label line.
        let mut targets = BTreeSet::new();
        for st in &sd.script.stmts {
            crate::bytecode::walk(&st.expr, &mut |e| match e {
                Expr::Jump(t) | Expr::JumpIfNot { target: t, .. } => {
                    targets.insert(*t as u32);
                }
                Expr::LabelTable(l) => {
                    for (_, o) in l {
                        targets.insert(*o);
                    }
                }
                _ => {}
            });
        }
        for st in &sd.script.stmts {
            if targets.contains(&st.offset) {
                let _ = writeln!(s, "{indent}L{:04X}:", st.offset);
            }
            let text = self.expr(&st.expr);
            let _ = writeln!(s, "{indent}    /*{:04X}*/ {}", st.offset, if text.is_empty() { "nop" } else { &text });
        }
        s
    }

    /// Full pseudo-code of a class: variables, functions and states.
    pub fn class(&self, export: usize) -> String {
        let mut s = String::new();
        let Some(c) = self.sp.class(export) else { return s };
        let pkg = &self.sp.pkg;
        let _ = writeln!(s, "class {} extends {};", self.sp.export_name(export), self.n(c.state.base.super_));
        if c.config_name != 0 && !pkg.name(c.config_name as usize).eq_ignore_ascii_case("None") {
            let _ = writeln!(s, "// config({})", pkg.name(c.config_name as usize));
        }
        let _ = writeln!(s, "// class flags {:#010x}", c.class_flags);
        let kids = self.sp.children(export);
        for &k in kids.iter().rev() {
            match &self.sp.objects[k] {
                Some(ScriptObject::Const(cd)) => {
                    let _ = writeln!(s, "const {} = {};", self.sp.export_name(k), cd.value);
                }
                Some(ScriptObject::Enum(e)) => {
                    let names: Vec<&str> = e.names.iter().map(|&n| pkg.name(n as usize)).collect();
                    let _ = writeln!(s, "enum {} {{ {} }};", self.sp.export_name(k), names.join(", "));
                }
                Some(ScriptObject::Struct(_)) => {
                    let _ = writeln!(s, "struct {} {{", self.sp.export_name(k));
                    for &m in self.sp.children(k).iter().rev() {
                        if let Some(ScriptObject::Property(p)) = &self.sp.objects[m] {
                            let _ = writeln!(s, "    var {} {};", self.type_name(p), self.sp.export_name(m));
                        }
                    }
                    let _ = writeln!(s, "}};");
                }
                Some(ScriptObject::Property(p)) => {
                    let dim = if p.array_dim > 1 { format!("[{}]", p.array_dim) } else { String::new() };
                    let cfg = if p.has(prop_flags::CONFIG) { "config " } else { "" };
                    let _ = writeln!(s, "var {cfg}{} {}{dim};", self.type_name(p), self.sp.export_name(k));
                }
                _ => {}
            }
        }
        if !c.state.base.script.stmts.is_empty() {
            let _ = writeln!(s, "\n// replication / class script\n{}", self.body(export, ""));
        }
        for &k in kids.iter().rev() {
            match &self.sp.objects[k] {
                Some(ScriptObject::Function(f)) => s.push_str(&self.function(k, f, "")),
                Some(ScriptObject::State(st)) => {
                    let sup = if st.base.super_ != ObjRef::Null { format!(" extends {}", self.n(st.base.super_)) } else { String::new() };
                    let _ = writeln!(s, "\nstate {}{sup}\n{{", self.sp.export_name(k));
                    for &m in self.sp.children(k).iter().rev() {
                        if let Some(ScriptObject::Function(f)) = &self.sp.objects[m] {
                            s.push_str(&self.function(m, f, "    "));
                        }
                    }
                    if !st.base.script.stmts.is_empty() {
                        let _ = writeln!(s, "    // state code");
                        s.push_str(&self.body(k, "    "));
                    }
                    let _ = writeln!(s, "}}");
                }
                _ => {}
            }
        }
        if !c.defaults.0.is_empty() {
            let _ = writeln!(s, "\ndefaultproperties\n{{");
            for p in &c.defaults.0 {
                let v = match &p.value {
                    upkg::Value::Object(o) => pkg.full_path(*o),
                    upkg::Value::Struct { name, raw } => format!("<{name}: {} bytes>", raw.len()),
                    upkg::Value::Array { raw } => format!("<array: {} bytes>", raw.len()),
                    v => format!("{v:?}"),
                };
                let idx = if p.index > 0 { format!("({})", p.index) } else { String::new() };
                let _ = writeln!(s, "    {}{idx}={v}", p.name);
            }
            let _ = writeln!(s, "}}");
        }
        s
    }

    pub fn function(&self, export: usize, f: &FunctionData, indent: &str) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "\n{indent}{}", self.signature(export, f));
        if f.base.script.stmts.is_empty() || f.has(func_flags::NATIVE) {
            let _ = writeln!(s, "{indent};");
            return s;
        }
        let _ = writeln!(s, "{indent}{{");
        for l in self.locals(export) {
            let _ = writeln!(s, "{indent}    {l}");
        }
        s.push_str(&self.body(export, indent));
        let _ = writeln!(s, "{indent}}}");
        s
    }
}

fn fmt_float(f: f32) -> String {
    if f.fract() == 0.0 && f.abs() < 1e9 { format!("{f:.1}") } else { format!("{f}") }
}
