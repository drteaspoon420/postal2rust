//! A parsed script package: every script export decoded once, with helpers to walk the field
//! lists of classes, states, functions and structs, and a table of native functions.

use crate::Result;
use crate::objects::{ClassData, FunctionData, PropertyData, ScriptObject, func_flags, parse_export};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use upkg::{ObjRef, Package};

pub struct ScriptPackage {
    pub pkg: Arc<Package>,
    /// Parsed script object per export index (None for non-script exports).
    pub objects: Vec<Option<ScriptObject>>,
}

impl ScriptPackage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(Arc::new(Package::open(path)?))
    }

    pub fn new(pkg: Arc<Package>) -> Result<Self> {
        let mut objects = Vec::with_capacity(pkg.exports.len());
        for i in 0..pkg.exports.len() {
            objects.push(parse_export(&pkg, i)?);
        }
        Ok(Self { pkg, objects })
    }

    pub fn get(&self, r: ObjRef) -> Option<&ScriptObject> {
        match r {
            ObjRef::Export(i) => self.objects.get(i)?.as_ref(),
            _ => None,
        }
    }

    pub fn name(&self, r: ObjRef) -> &str {
        self.pkg.ref_name(r)
    }

    pub fn export_name(&self, i: usize) -> &str {
        self.pkg.name(self.pkg.exports[i].name)
    }

    pub fn class(&self, i: usize) -> Option<&ClassData> {
        match self.objects.get(i)?.as_ref()? {
            ScriptObject::Class(c) => Some(c),
            _ => None,
        }
    }

    pub fn function(&self, i: usize) -> Option<&FunctionData> {
        match self.objects.get(i)?.as_ref()? {
            ScriptObject::Function(f) => Some(f),
            _ => None,
        }
    }

    pub fn property(&self, r: ObjRef) -> Option<&PropertyData> {
        match self.get(r)? {
            ScriptObject::Property(p) => Some(p),
            _ => None,
        }
    }

    pub fn find_class(&self, name: &str) -> Option<usize> {
        (0..self.objects.len()).find(|&i| self.class(i).is_some() && self.export_name(i).eq_ignore_ascii_case(name))
    }

    /// Export indices of a struct's direct children, in declaration-list order.
    pub fn children(&self, owner: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let Some(Some(o)) = self.objects.get(owner) else { return out };
        let Some(s) = o.struct_data() else { return out };
        let mut cur = s.children;
        while let ObjRef::Export(i) = cur {
            if out.len() > 100_000 || out.contains(&i) {
                break;
            }
            out.push(i);
            cur = match &self.objects[i] {
                Some(o) => o.next(),
                None => ObjRef::Null,
            };
        }
        out
    }

    /// Finds a function by name among a struct's children.
    pub fn find_child(&self, owner: usize, name: &str) -> Option<usize> {
        self.children(owner).into_iter().find(|&i| self.export_name(i).eq_ignore_ascii_case(name))
    }
}

/// Information about a native function, gathered from declarations with a native index.
#[derive(Debug, Clone)]
pub struct NativeInfo {
    pub name: String,
    /// Operator symbol for operators ("+", "==", ...), else the same as `name`.
    pub friendly: String,
    pub flags: u32,
    pub precedence: u8,
    pub owner: String,
}

impl NativeInfo {
    pub fn is_operator(&self) -> bool {
        self.flags & func_flags::OPERATOR != 0
    }
    pub fn is_preop(&self) -> bool {
        self.flags & func_flags::PRE_OPERATOR != 0
    }
}

#[derive(Default, Clone)]
pub struct NativeTable {
    pub by_index: HashMap<u16, NativeInfo>,
}

impl NativeTable {
    /// Adds all functions with a native index from a package.
    pub fn add_package(&mut self, sp: &ScriptPackage) {
        for (i, o) in sp.objects.iter().enumerate() {
            let Some(ScriptObject::Function(f)) = o else { continue };
            if f.native == 0 {
                continue;
            }
            let pkg = &sp.pkg;
            let name = sp.export_name(i).to_string();
            let friendly = pkg.name(f.base.friendly_name as usize).to_string();
            let owner = pkg.ref_name(pkg.exports[i].outer).to_string();
            self.by_index.entry(f.native).or_insert(NativeInfo { name, friendly, flags: f.flags, precedence: f.precedence, owner });
        }
    }

    pub fn get(&self, i: u16) -> Option<&NativeInfo> {
        self.by_index.get(&i)
    }
}
