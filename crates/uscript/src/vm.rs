//! UnrealScript interpreter.
//!
//! - **Object model**: classes are loaded on demand from the `.u` packages in the System
//!   directory. Each class gets a slot layout (its super's slots followed by its own properties)
//!   and a defaults vector built from the zero value of every slot overlaid with the tagged
//!   default properties of each class in the chain (including struct and dynamic-array defaults).
//!   Objects are a class id, a slot vector and a [`StateFrame`].
//! - **Frames**: one per script call, holding parameters/locals plus the active `foreach`
//!   iterators of that call.
//! - **Execution**: statements are run directly from the decoded expression tree; jumps resolve
//!   memory offsets to statement indices.
//! - **States** ([`state`]): `GotoState`, `BeginState`/`EndState`, labels, `ignores`, probe
//!   masks, state code with latent functions (`Sleep`, `FinishAnim`, `MoveTo`, ...) resumed by
//!   [`Vm::tick_actor`] / [`Vm::tick_world`].
//! - **Natives**: Rust functions registered by function name; native indices used by the
//!   bytecode are mapped to names through the declarations in the loaded packages. Natives the VM
//!   does not implement are passed to the [`Host`] (`Host::native`), and otherwise reported in
//!   [`Vm::missing_natives`] (and fail or return a zero value, see [`MissingNativePolicy`]).
//! - **Host**: world/actor natives (Spawn, Destroy, SetLocation, Trace, PlaySound, PlayAnim,
//!   iterators, latent actions) go through the [`Host`] trait so an engine can implement them.
//!
//! Reading class defaults (stable API):
//!
//! ```no_run
//! # use uscript::vm::{Vm, Value};
//! let mut vm = Vm::new(r"C:\Game\System").unwrap();
//! // One call: package, class, property.
//! let mesh = vm.class_default("People", "Bystanders", "Mesh").unwrap(); // Some(Value::Resource(path))
//! // Or resolve the class once and read several properties.
//! let c = vm.find_class_by_name("People.Bystanders").unwrap();
//! let skins = vm.default_value(c, "Skins"); // Some(Value::Array(..)) of Value::Resource
//! ```
//!
//! Not supported: networking/replication, config files (SaveConfig is a no-op).

use crate::bytecode::{Expr, Script};
use crate::graph::ScriptPackage;
use crate::host::{DefaultHost, Host, NativeCall};
use crate::objects::{PropKind, ScriptObject, func_flags, prop_flags};
use crate::{Result, ScriptError};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use upkg::ObjRef;

mod state;
pub use state::{Latent, PROBE_NAMES, StateFrame, probe_bit};

/// A script field (class, function, property, ...) identified by loaded-package index and export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldId {
    pub pkg: u32,
    pub export: u32,
}

pub type ClassId = usize;
pub type ObjId = usize;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// No value (void return, unset optional, uninitialised dynamic array element).
    Void,
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    Name(String),
    Str(String),
    Object(Option<ObjId>),
    Class(Option<ClassId>),
    /// Reference to a non-script package object (texture, sound, mesh, ...) by full path,
    /// e.g. `"Engine.DefaultTexture"`.
    Resource(String),
    Vector([f32; 3]),
    Rotator([i32; 3]),
    Struct(Vec<(String, Value)>),
    Array(Vec<Value>),
    /// An unbound delegate (calls go to the delegate's own declared body).
    Delegate,
    /// A delegate bound to a function on an object.
    DelegateTo { obj: Option<ObjId>, func: String },
}

impl Value {
    pub fn as_int(&self) -> i32 {
        match self {
            Value::Int(i) => *i,
            Value::Byte(b) => *b as i32,
            Value::Bool(b) => *b as i32,
            Value::Float(f) => *f as i32,
            _ => 0,
        }
    }
    pub fn as_float(&self) -> f32 {
        match self {
            Value::Float(f) => *f,
            Value::Int(i) => *i as f32,
            Value::Byte(b) => *b as f32,
            _ => 0.0,
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Byte(b) => *b != 0,
            Value::Float(f) => *f != 0.0,
            Value::Object(o) => o.is_some(),
            Value::Class(c) => c.is_some(),
            Value::Name(n) => !n.eq_ignore_ascii_case("None"),
            Value::Resource(_) => true,
            _ => false,
        }
    }
    pub fn as_str(&self) -> String {
        match self {
            Value::Str(s) | Value::Name(s) => s.clone(),
            Value::Void => String::new(),
            v => v.to_ustring(),
        }
    }
    pub fn as_name(&self) -> String {
        match self {
            Value::Name(s) | Value::Str(s) => s.clone(),
            _ => "None".into(),
        }
    }
    pub fn as_vec(&self) -> [f32; 3] {
        match self {
            Value::Vector(v) => *v,
            _ => [0.0; 3],
        }
    }
    pub fn as_rot(&self) -> [i32; 3] {
        match self {
            Value::Rotator(r) => *r,
            _ => [0; 3],
        }
    }
    pub fn as_obj(&self) -> Option<ObjId> {
        match self {
            Value::Object(o) => *o,
            _ => None,
        }
    }
    pub fn as_class(&self) -> Option<ClassId> {
        match self {
            Value::Class(c) => *c,
            _ => None,
        }
    }
    /// Path of a non-script object reference (mesh, texture, sound, ...).
    pub fn as_resource(&self) -> Option<&str> {
        match self {
            Value::Resource(p) => Some(p),
            _ => None,
        }
    }
    /// True for `None` names and the empty name.
    pub fn is_none_name(&self) -> bool {
        match self {
            Value::Name(n) | Value::Str(n) => n.is_empty() || n.eq_ignore_ascii_case("None"),
            Value::Void => true,
            _ => false,
        }
    }

    /// The engine's string conversion of a value (used by `string(x)` casts).
    pub fn to_ustring(&self) -> String {
        match self {
            Value::Void => String::new(),
            Value::Byte(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => if *b { "True" } else { "False" }.into(),
            Value::Float(f) => format!("{f:.2}"),
            Value::Name(s) | Value::Str(s) => s.clone(),
            Value::Object(None) | Value::Class(None) => "None".into(),
            Value::Object(Some(o)) => format!("Object{o}"),
            Value::Class(Some(c)) => format!("Class{c}"),
            Value::Resource(p) => p.clone(),
            Value::Vector([x, y, z]) => format!("{x:.2},{y:.2},{z:.2}"),
            Value::Rotator([p, y, r]) => format!("{p},{y},{r}"),
            Value::Struct(_) | Value::Array(_) | Value::Delegate | Value::DelegateTo { .. } => String::new(),
        }
    }

    /// Script-level equality (names compare case-insensitively).
    pub fn script_eq(&self, o: &Value) -> bool {
        match (self, o) {
            (Value::Name(a), Value::Name(b)) => a.eq_ignore_ascii_case(b),
            (Value::Byte(_) | Value::Int(_), Value::Byte(_) | Value::Int(_)) => self.as_int() == o.as_int(),
            (Value::Float(_), _) | (_, Value::Float(_)) => self.as_float() == o.as_float(),
            (Value::Struct(a), Value::Struct(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.1.script_eq(&y.1)),
            _ => self == o,
        }
    }
}

/// Static type of a slot, used to build zero values.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Byte,
    Int,
    Bool,
    Float,
    Name,
    Str,
    Object,
    Class,
    Vector,
    Rotator,
    Struct(Vec<(String, Ty)>),
    Array(Box<Ty>),
    Fixed(Box<Ty>, usize),
    Delegate,
    Other,
}

impl Ty {
    pub fn zero(&self) -> Value {
        match self {
            Ty::Byte => Value::Byte(0),
            Ty::Int => Value::Int(0),
            Ty::Bool => Value::Bool(false),
            Ty::Float => Value::Float(0.0),
            Ty::Name => Value::Name("None".into()),
            Ty::Str => Value::Str(String::new()),
            Ty::Object => Value::Object(None),
            Ty::Class => Value::Class(None),
            Ty::Vector => Value::Vector([0.0; 3]),
            Ty::Rotator => Value::Rotator([0; 3]),
            Ty::Struct(f) => Value::Struct(f.iter().map(|(n, t)| (n.clone(), t.zero())).collect()),
            Ty::Array(_) => Value::Array(Vec::new()),
            Ty::Fixed(t, n) => Value::Array(vec![t.zero(); *n]),
            Ty::Delegate => Value::Delegate,
            Ty::Other => Value::Void,
        }
    }

    fn sub(&self, seg: &Seg) -> Option<&Ty> {
        match (self, seg) {
            (Ty::Array(i) | Ty::Fixed(i, _), Seg::Idx(_)) => Some(i),
            (Ty::Struct(f), Seg::Field(n)) => f.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, t)| t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Slot {
    pub name: String,
    pub field: FieldId,
    pub ty: Ty,
    pub flags: u32,
}

/// Class flag: the class cannot be spawned / instantiated.
pub const CLASS_ABSTRACT: u32 = 0x0000_0001;

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub field: FieldId,
    pub name: String,
    /// Name of the package the class is defined in.
    pub package: String,
    pub super_: Option<ClassId>,
    /// Class flags (see [`CLASS_ABSTRACT`]).
    pub flags: u32,
    pub slots: Vec<Slot>,
    pub slot_by_field: HashMap<FieldId, usize>,
    pub defaults: Vec<Value>,
    /// Lower-case property name to slot (the most derived declaration wins).
    pub slot_index: HashMap<Box<str>, usize>,
}

/// Runs `f` on an ASCII-lowercased copy of `s` without allocating for short names.
pub(crate) fn with_lower<R>(s: &str, f: impl FnOnce(&str) -> R) -> R {
    let b = s.as_bytes();
    if b.len() <= 64 {
        let mut buf = [0u8; 64];
        for (d, c) in buf.iter_mut().zip(b) {
            *d = c.to_ascii_lowercase();
        }
        // ASCII lowercasing keeps UTF-8 valid.
        f(std::str::from_utf8(&buf[..b.len()]).unwrap_or(s))
    } else {
        f(&s.to_ascii_lowercase())
    }
}

/// Case-insensitive 64-bit hash of a name (FNV-1a over lowercase bytes).
pub(crate) fn name_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in s.bytes() {
        h ^= c.to_ascii_lowercase() as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^ (s.len() as u64) << 56
}

impl ClassInfo {
    pub fn slot(&self, name: &str) -> Option<usize> {
        with_lower(name, |l| self.slot_index.get(l).copied())
    }
    /// A default property value by name (`None` if the class has no such property).
    pub fn default(&self, name: &str) -> Option<&Value> {
        self.defaults.get(self.slot(name)?)
    }
    pub fn is_abstract(&self) -> bool {
        self.flags & CLASS_ABSTRACT != 0
    }
}

#[derive(Debug, Clone)]
pub struct Object {
    pub class: ClassId,
    pub slots: Vec<Value>,
    /// Current state, state code position and latent action.
    pub exec: StateFrame,
    /// Destroyed (actors only). Destroyed objects stay addressable; references to them are
    /// cleared by [`Vm::cleanup_destroyed`].
    pub deleted: bool,
}

/// Parameter and local layout of a function.
#[derive(Debug)]
pub struct FuncInfo {
    pub field: FieldId,
    pub name: String,
    /// Name of the class (or state) that declares the function.
    pub owner: String,
    pub flags: u32,
    pub native: u16,
    /// (property export, flags, type) for every local including parameters, in declaration order.
    pub locals: Vec<(u32, u32, Ty)>,
    /// Indices into `locals` of the parameters (excluding the return value).
    pub params: Vec<usize>,
    pub ret: Option<usize>,
    pub local_index: HashMap<u32, usize>,
    /// Native implementation, resolved on first call (None inside = not registered).
    pub native_fn: std::cell::Cell<Option<Option<NativeFn>>>,
    /// Probe bit of the function name (for `ignores`), if it is a probe function.
    pub probe: Option<u32>,
    /// Script function whose body is only `return` (calling it has no effect beyond zeroing the
    /// return value, so events skip it).
    pub empty: bool,
    /// Host callback run on entry with the arguments (see [`Vm::watch_calls`]).
    pub watch: Option<CallWatch>,
}

impl FuncInfo {
    /// Zero value of the return type (Void for functions without a return value).
    pub fn ret_zero(&self) -> Value {
        self.ret.map(|i| self.locals[i].2.zero()).unwrap_or(Value::Void)
    }
}

pub type NativeFn = fn(&mut Vm, &mut Vec<Value>) -> Result<Value>;
/// Observer of a script function call: (vm, self, arguments, after). Called on entry with the
/// arguments as passed (`after` false) and on return with the final parameter values (`after` true).
/// On entry the watch may rewrite the arguments; the function then runs with the new values.
pub type CallWatch = fn(&mut Vm, Option<ObjId>, &mut [Value], bool);

/// What happens when script calls a native function that neither the VM nor the host implements.
/// Either way the call is counted in [`Vm::missing_natives`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingNativePolicy {
    /// Fail the call with `native function NAME (index N) is not implemented`.
    #[default]
    Strict,
    /// Return the zero value of the return type and leave out parameters unchanged.
    Lenient,
}

#[derive(Debug, Clone, Copy)]
enum Ctx {
    Frame,
    Obj(Option<ObjId>),
    Class(ClassId),
}

struct Frame {
    pkg: usize,
    this: Option<ObjId>,
    class: Option<ClassId>,
    locals: Vec<Value>,
    /// None for state code (which has no locals).
    fi: Option<Rc<FuncInfo>>,
    iters: Vec<IterState>,
}

/// An active `foreach`: rows of out-parameter values produced by the iterator function.
struct IterState {
    rows: Vec<Vec<Value>>,
    next: usize,
    places: Vec<Option<Place>>,
    /// Statement index of the loop body.
    body: usize,
}

enum Root {
    Local(usize),
    Inst(ObjId, usize),
    Default(ClassId, usize),
    Temp(Value),
}

enum Seg {
    Idx(usize),
    Field(String),
    Len,
}

struct Place {
    root: Root,
    path: Vec<Seg>,
}

/// How a statement list finished.
enum Flow {
    Return(Option<Value>),
    End,
    /// State code started a latent action; resume at this statement.
    Latent(usize),
    /// State code position changed (GotoState / Goto label); reload it.
    Switched,
}

#[derive(Clone, Copy)]
enum Mode {
    Func,
    State { obj: ObjId, epoch: u32 },
}

/// Path into a class default value (for class references resolved after loading).
#[derive(Debug, Clone)]
enum DefSeg {
    Idx(usize),
    Field(String),
}

type Pending = (Vec<DefSeg>, FieldId);

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(ScriptError::Bad(m.into()))
}

/// `P2_CALL_TRACE=Func1,Func2`: script functions whose calls (args, result, self state) go to
/// stderr. Read once.
fn call_trace() -> Option<&'static [String]> {
    static T: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();
    T.get_or_init(|| std::env::var("P2_CALL_TRACE").ok().map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())).as_deref()
}

/// Latent functions the VM handles itself (Sleep) or through `Host::latent_tick`.
const BUILTIN_LATENT: &[&str] = &[
    "Sleep",
    "FinishAnim",
    "FinishInterpolation",
    "MoveTo",
    "MoveToward",
    "FinishRotation",
    "WaitForLanding",
    "WaitToSeeEnemy",
    "MoveToWithRadius",
    "MoveTowardWithRadius",
];

pub struct Vm {
    pub system_dir: PathBuf,
    pub pkgs: Vec<Rc<ScriptPackage>>,
    pkg_by_name: HashMap<String, Option<usize>>,
    pub classes: Vec<ClassInfo>,
    class_by_field: HashMap<FieldId, ClassId>,
    class_by_name: HashMap<u64, ClassId>,
    pub objects: Vec<Object>,
    natives: HashMap<String, NativeFn>,
    /// Script functions observed by the host, by lowercase name.
    watches: HashMap<String, CallWatch>,
    /// Names handled by the VM outside the native registry (latent functions, iterators).
    builtin: HashSet<String>,
    native_decl: HashMap<u16, FieldId>,
    funcs: HashMap<FieldId, Rc<FuncInfo>>,
    func_lookup: HashMap<(ClassId, u64), Option<FieldId>>,
    state_lookup: HashMap<(FieldId, u64), Option<FieldId>>,
    state_by_name: HashMap<(ClassId, u64), Option<FieldId>>,
    resolve_cache: HashMap<(usize, i32), Option<FieldId>>,
    /// Class-typed default values waiting for their class to be loaded.
    pending_class_defaults: Vec<(ClassId, usize, Vec<DefSeg>, FieldId)>,
    /// Subobject references found while converting class defaults, resolved after loading.
    pending_class_subobjects: Vec<(ClassId, usize, Vec<DefSeg>, FieldId)>,
    /// Subobject references found by `convert_ref` (relative path, export), drained by callers.
    pending_subobjects: Vec<Pending>,
    /// Instantiated package subobjects, by export.
    subobjects: HashMap<FieldId, ObjId>,
    /// Every object id ever created as a package subobject (including superseded instances), so
    /// a level reset can repoint any class default that still holds one.
    subobject_of: HashMap<ObjId, FieldId>,
    frames: Vec<Frame>,
    host: Rc<dyn Host>,
    native_this: Option<ObjId>,
    native_class: Option<ClassId>,
    /// Lines written by `Log`/`Warn` and VM warnings (capped at `log_cap`).
    pub log: Vec<String>,
    pub log_cap: usize,
    rng: u64,
    /// Maximum statements to execute per top-level call (runaway-loop guard).
    pub step_budget: u64,
    steps: u64,
    /// Statements executed since the VM was created (for benchmarks).
    pub steps_total: u64,
    /// Script function calls and native calls since the VM was created.
    pub calls_total: u64,
    pub native_calls_total: u64,
    /// When set, natives and events are timed into `profile_natives` / `profile_events`.
    pub profile: bool,
    /// name -> (calls, seconds including nested calls).
    pub profile_natives: HashMap<String, (u64, f64)>,
    pub profile_events: HashMap<String, (u64, f64)>,
    pub(crate) tick_slots: Rc<Vec<Option<[usize; 4]>>>,
    /// Per class: true when an actor outside any state has no Tick to run (see `tick_actor`).
    pub(crate) tick_noop: Vec<Option<bool>>,
    pub(crate) object_slot_cache: Rc<Vec<Vec<usize>>>,
    pub missing_native_policy: MissingNativePolicy,
    /// Unimplemented natives encountered, keyed `"Class.Function"`, with call counts.
    pub missing_natives: HashMap<String, u64>,
    /// Errors raised by events fired from [`Vm::fire_event`] / ticking, keyed by message.
    pub script_errors: HashMap<String, u64>,
    /// Live and destroyed actors in spawn order (destroyed ones are dropped by `cleanup_destroyed`).
    pub actors: Vec<ObjId>,
    /// The LevelInfo actor assigned to every spawned actor's `Level`.
    pub level: Option<ObjId>,
    actor_class: Option<Option<ClassId>>,
    pub(crate) destroyed_pending: bool,
    localized: HashMap<String, Option<HashMap<(String, String), String>>>,
    /// Navigation network and route-search state (see [`crate::nav`]).
    pub nav: crate::nav::NavState,
    /// Config persistence (see [`crate::config`]).
    pub config: crate::config::ConfigState,
}

impl Vm {
    /// Creates a VM over a System directory and loads Core (for natives and Object).
    pub fn new(system_dir: impl Into<PathBuf>) -> Result<Self> {
        let mut vm = Vm {
            system_dir: system_dir.into(),
            pkgs: Vec::new(),
            pkg_by_name: HashMap::new(),
            classes: Vec::new(),
            class_by_field: HashMap::new(),
            class_by_name: HashMap::new(),
            objects: Vec::new(),
            natives: HashMap::new(),
            watches: HashMap::new(),
            builtin: HashSet::new(),
            native_decl: HashMap::new(),
            funcs: HashMap::new(),
            func_lookup: HashMap::new(),
            state_lookup: HashMap::new(),
            state_by_name: HashMap::new(),
            resolve_cache: HashMap::new(),
            pending_class_defaults: Vec::new(),
            pending_class_subobjects: Vec::new(),
            pending_subobjects: Vec::new(),
            subobjects: HashMap::new(),
            subobject_of: HashMap::new(),
            frames: Vec::new(),
            host: Rc::new(DefaultHost),
            native_this: None,
            native_class: None,
            log: Vec::new(),
            log_cap: 200_000,
            rng: 0x2545_F491_4F6C_DD1D,
            step_budget: 10_000_000,
            steps: 0,
            steps_total: 0,
            calls_total: 0,
            native_calls_total: 0,
            profile: false,
            profile_natives: HashMap::new(),
            profile_events: HashMap::new(),
            tick_slots: Rc::new(Vec::new()),
            tick_noop: Vec::new(),
            object_slot_cache: Rc::new(Vec::new()),
            missing_native_policy: MissingNativePolicy::Strict,
            missing_natives: HashMap::new(),
            script_errors: HashMap::new(),
            actors: Vec::new(),
            level: None,
            actor_class: None,
            destroyed_pending: false,
            localized: HashMap::new(),
            nav: Default::default(),
            config: Default::default(),
        };
        crate::natives::register_core(&mut vm);
        crate::host::register_engine(&mut vm);
        crate::nav::register_nav(&mut vm);
        for n in BUILTIN_LATENT.iter().chain(crate::host::BUILTIN_ITERATORS) {
            vm.builtin.insert(n.to_ascii_lowercase());
        }
        vm.package("Core").ok_or_else(|| ScriptError::Bad("Core.u not found".into()))?;
        crate::config::register_config(&mut vm);
        Ok(vm)
    }

    pub fn register_native(&mut self, name: &str, f: NativeFn) {
        self.natives.insert(name.to_ascii_lowercase(), f);
        // Drop resolved native pointers so the new registration is seen.
        for fi in self.funcs.values() {
            fi.native_fn.set(None);
        }
    }

    /// Runs `f` on entry to and return from every script (non-native) function named `name`
    /// (host-side bookkeeping such as the last hit of a pawn; see [`CallWatch`]).
    pub fn watch_calls(&mut self, name: &str, f: CallWatch) {
        if self.watches.insert(name.to_ascii_lowercase(), f).is_none() {
            self.funcs.clear();
        }
    }

    /// True if the VM implements a native (registered function, latent action or iterator).
    pub fn has_native(&self, name: &str) -> bool {
        let k = name.to_ascii_lowercase();
        self.natives.contains_key(&k) || self.builtin.contains(&k)
    }

    /// Installs the engine host used for world/actor natives.
    /// Script call stack, innermost last ("Class.Func <- Class.Func"), for diagnostics.
    pub fn call_stack_summary(&self) -> String {
        self.frames.iter().rev().take(6).filter_map(|f| f.fi.as_ref().map(|fi| format!("{}.{}", fi.owner, fi.name))).collect::<Vec<_>>().join(" <- ")
    }

    pub fn set_host(&mut self, host: Rc<dyn Host>) {
        self.host = host;
    }

    pub fn host(&self) -> Rc<dyn Host> {
        self.host.clone()
    }

    /// The object a native function was called on (`self` in script), if any.
    pub fn native_self(&self) -> Option<ObjId> {
        self.native_this
    }

    /// The class context of a native call (the object's class, or the class of a static call).
    pub fn native_class(&self) -> Option<ClassId> {
        self.native_class
    }

    pub fn seed(&mut self, s: u64) {
        self.rng = s | 1;
    }

    /// Uniform random float in [0, 1).
    pub fn frand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Appends a line to the log (dropped once `log_cap` lines are stored).
    pub fn push_log(&mut self, line: impl Into<String>) {
        if self.log.len() < self.log_cap {
            self.log.push(line.into());
        }
    }

    /// Loads (or returns) a script package from the System directory.
    pub fn package(&mut self, name: &str) -> Option<usize> {
        let key = name.to_ascii_lowercase();
        if let Some(p) = self.pkg_by_name.get(&key) {
            return *p;
        }
        let path = self.system_dir.join(format!("{name}.u"));
        let loaded = ScriptPackage::open(&path).ok().map(|sp| {
            let idx = self.pkgs.len();
            for (i, o) in sp.objects.iter().enumerate() {
                if let Some(ScriptObject::Function(f)) = o {
                    if f.native != 0 {
                        self.native_decl.entry(f.native).or_insert(FieldId { pkg: idx as u32, export: i as u32 });
                    }
                }
            }
            self.pkgs.push(Rc::new(sp));
            idx
        });
        self.pkg_by_name.insert(key, loaded);
        loaded
    }

    /// Finds an export by its outer chain (innermost name first) inside a named package.
    fn lookup_chain(&mut self, root: &str, chain: &[String]) -> Option<FieldId> {
        let target = self.package(root)?;
        let tp = self.pkgs[target].clone();
        let (first, rest) = chain.split_first()?;
        'next: for (i, e) in tp.pkg.exports.iter().enumerate() {
            if !tp.pkg.name(e.name).eq_ignore_ascii_case(first) {
                continue;
            }
            let mut outer = e.outer;
            for g in rest {
                match outer {
                    ObjRef::Export(o) if tp.pkg.name(tp.pkg.exports[o].name).eq_ignore_ascii_case(g) => outer = tp.pkg.exports[o].outer,
                    _ => continue 'next,
                }
            }
            if outer == ObjRef::Null {
                return Some(FieldId { pkg: target as u32, export: i as u32 });
            }
        }
        None
    }

    /// Finds a script export by dotted path, e.g. `"Engine.Actor.EDrawType"`.
    pub fn find_path(&mut self, path: &str) -> Option<FieldId> {
        let mut parts: Vec<String> = path.split('.').map(str::to_string).collect();
        if parts.len() < 2 {
            return None;
        }
        let root = parts.remove(0);
        parts.reverse();
        self.lookup_chain(&root, &parts)
    }

    /// Resolves an object reference made from a loaded package to the field that defines it.
    pub fn resolve(&mut self, pkg: usize, r: ObjRef) -> Option<FieldId> {
        let raw = match r {
            ObjRef::Null => return None,
            ObjRef::Export(i) => return Some(FieldId { pkg: pkg as u32, export: i as u32 }),
            ObjRef::Import(i) => -(i as i32) - 1,
        };
        if let Some(c) = self.resolve_cache.get(&(pkg, raw)) {
            return *c;
        }
        let sp = self.pkgs[pkg].clone();
        let mut chain = Vec::new();
        let mut cur = r;
        while let ObjRef::Import(j) = cur {
            chain.push(sp.pkg.name(sp.pkg.imports[j].name).to_string());
            cur = sp.pkg.imports[j].outer;
            if chain.len() > 16 {
                break;
            }
        }
        let out = chain.pop().and_then(|root| self.lookup_chain(&root, &chain));
        self.resolve_cache.insert((pkg, raw), out);
        out
    }

    fn obj_of(&self, f: FieldId) -> Option<&ScriptObject> {
        self.pkgs[f.pkg as usize].objects.get(f.export as usize)?.as_ref()
    }

    fn field_name(&self, f: FieldId) -> String {
        self.pkgs[f.pkg as usize].export_name(f.export as usize).to_string()
    }

    /// Dotted path of a script export (`Package.Class.State`), the inverse of [`Vm::find_path`].
    pub fn field_path(&self, f: FieldId) -> String {
        let sp = &self.pkgs[f.pkg as usize];
        let mut parts = vec![sp.export_name(f.export as usize).to_string()];
        let mut outer = sp.pkg.exports[f.export as usize].outer;
        let mut guard = 0;
        while let ObjRef::Export(o) = outer {
            parts.push(sp.export_name(o).to_string());
            outer = sp.pkg.exports[o].outer;
            guard += 1;
            if guard > 16 {
                break;
            }
        }
        parts.push(sp.pkg.name.clone());
        parts.reverse();
        parts.join(".")
    }

    fn struct_ty(&mut self, f: FieldId, depth: u32) -> Ty {
        let name = self.field_name(f);
        if name.eq_ignore_ascii_case("Vector") {
            return Ty::Vector;
        }
        if name.eq_ignore_ascii_case("Rotator") {
            return Ty::Rotator;
        }
        if depth > 16 {
            return Ty::Other;
        }
        let sp = self.pkgs[f.pkg as usize].clone();
        let Some(ScriptObject::Struct(sd)) = self.obj_of(f).cloned() else { return Ty::Other };
        let mut fields = match self.resolve(f.pkg as usize, sd.super_) {
            Some(s) => match self.struct_ty(s, depth + 1) {
                Ty::Struct(v) => v,
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        for c in sp.children(f.export as usize) {
            if let Some(ScriptObject::Property(p)) = &sp.objects[c] {
                let ty = self.prop_ty(FieldId { pkg: f.pkg, export: c as u32 }, &p.kind, p.array_dim, depth + 1);
                fields.push((sp.export_name(c).to_string(), ty));
            }
        }
        Ty::Struct(fields)
    }

    fn prop_ty(&mut self, f: FieldId, kind: &PropKind, dim: i32, depth: u32) -> Ty {
        let pkg = f.pkg as usize;
        let base = match kind {
            PropKind::Byte { .. } => Ty::Byte,
            PropKind::Int => Ty::Int,
            PropKind::Bool => Ty::Bool,
            PropKind::Float => Ty::Float,
            PropKind::Name => Ty::Name,
            PropKind::Str | PropKind::String { .. } => Ty::Str,
            PropKind::Object { .. } => Ty::Object,
            PropKind::Class { .. } => Ty::Class,
            PropKind::Struct { strukt } => match self.resolve(pkg, *strukt) {
                Some(s) => self.struct_ty(s, depth),
                None => Ty::Other,
            },
            PropKind::Array { inner } | PropKind::FixedArray { inner, .. } => {
                let inner_ty = match self.resolve(pkg, *inner) {
                    Some(i) => match self.obj_of(i).cloned() {
                        Some(ScriptObject::Property(ip)) => self.prop_ty(i, &ip.kind, 1, depth + 1),
                        _ => Ty::Other,
                    },
                    None => Ty::Other,
                };
                match kind {
                    PropKind::FixedArray { count, .. } => Ty::Fixed(Box::new(inner_ty), (*count).max(0) as usize),
                    _ => Ty::Array(Box::new(inner_ty)),
                }
            }
            PropKind::Delegate { .. } => Ty::Delegate,
            PropKind::Map { .. } | PropKind::Pointer => Ty::Other,
        };
        if dim > 1 { Ty::Fixed(Box::new(base), dim as usize) } else { base }
    }

    /// Loads a class (and its supers), building its slot layout and default values.
    /// Classes referenced from default properties are loaded afterwards from a work queue, so
    /// mutually referencing classes do not recurse.
    pub fn load_class(&mut self, f: FieldId) -> Result<ClassId> {
        let id = self.load_class_inner(f)?;
        loop {
            while let Some((c, slot, path, target)) = self.pending_class_defaults.pop() {
                let v = Value::Class(self.load_class_inner(target).ok());
                // Subclasses copied this default before it was resolved; patch every class that still
                // holds the placeholder at this position.
                for k in 0..self.classes.len() {
                    if k != c && !self.is_child_of(k, c) {
                        continue;
                    }
                    if let Some(cell) = def_cell(&mut self.classes[k].defaults[slot], &path) {
                        if matches!(cell, Value::Class(None)) || k == c {
                            *cell = v.clone();
                        }
                    }
                }
            }
            // Subobjects referenced by class defaults (one shared instance per export, as the
            // engine's default objects all point at the same package object).
            let Some((c, slot, path, target)) = self.pending_class_subobjects.pop() else { break };
            let v = Value::Object(self.subobject(target));
            for k in 0..self.classes.len() {
                if k != c && !self.is_child_of(k, c) {
                    continue;
                }
                if let Some(cell) = def_cell(&mut self.classes[k].defaults[slot], &path) {
                    if matches!(cell, Value::Object(None)) || k == c {
                        *cell = v.clone();
                    }
                }
            }
        }
        Ok(id)
    }

    fn load_class_inner(&mut self, f: FieldId) -> Result<ClassId> {
        if let Some(&c) = self.class_by_field.get(&f) {
            return Ok(c);
        }
        let sp = self.pkgs[f.pkg as usize].clone();
        let Some(cd) = sp.class(f.export as usize) else { return err(format!("{} is not a class", self.field_name(f))) };
        let super_ = match self.resolve(f.pkg as usize, cd.state.base.super_) {
            Some(s) => Some(self.load_class_inner(s)?),
            None => None,
        };
        let (mut slots, mut slot_by_field, mut defaults) = match super_ {
            Some(s) => (self.classes[s].slots.clone(), self.classes[s].slot_by_field.clone(), self.classes[s].defaults.clone()),
            None => (Vec::new(), HashMap::new(), Vec::new()),
        };
        for c in sp.children(f.export as usize) {
            if let Some(ScriptObject::Property(p)) = &sp.objects[c] {
                let pf = FieldId { pkg: f.pkg, export: c as u32 };
                let ty = self.prop_ty(pf, &p.kind, p.array_dim, 0);
                slot_by_field.insert(pf, slots.len());
                defaults.push(ty.zero());
                slots.push(Slot { name: sp.export_name(c).to_string(), field: pf, ty, flags: p.flags });
            }
        }
        let slot_index = slots.iter().enumerate().map(|(i, s)| (s.name.to_ascii_lowercase().into_boxed_str(), i)).collect();
        // The package name with its declared capitalisation (the file stem may be lowercased).
        let package = sp.pkg.names.iter().find(|n| n.eq_ignore_ascii_case(&sp.pkg.name)).cloned().unwrap_or_else(|| sp.pkg.name.clone());
        let mut info = ClassInfo {
            field: f,
            name: sp.export_name(f.export as usize).to_string(),
            package,
            super_,
            flags: cd.class_flags,
            slots,
            slot_by_field,
            defaults,
            slot_index,
        };
        let id = self.classes.len();
        let mut pending: Vec<Pending> = Vec::new();
        let mut path = Vec::new();
        for p in &cd.defaults.0 {
            let Some(s) = info.slot(&p.name) else { continue };
            let slot_ty = info.slots[s].ty.clone();
            let (ty, idx) = match &slot_ty {
                Ty::Fixed(t, _) => (t.as_ref(), Some(p.index as usize)),
                t => (t, None),
            };
            path.clear();
            path.extend(idx.map(DefSeg::Idx));
            let mark = pending.len();
            let sub_mark = self.pending_subobjects.len();
            let Some(v) = self.convert_default(f.pkg as usize, &p.value, ty, &mut path, &mut pending, 0) else {
                self.pending_subobjects.truncate(sub_mark);
                continue;
            };
            for e in &pending[mark..] {
                self.pending_class_defaults.push((id, s, e.0.clone(), e.1));
            }
            for (rel, target) in self.pending_subobjects.drain(sub_mark..).collect::<Vec<_>>() {
                self.pending_class_subobjects.push((id, s, rel, target));
            }
            match (&mut info.defaults[s], idx) {
                (Value::Array(a), Some(i)) => {
                    if let Some(e) = a.get_mut(i) {
                        *e = v;
                    }
                }
                (d, _) => *d = v,
            }
        }
        // `Default.Class` (e.g. `String(Default.Class)` in static functions) is the class itself.
        if let Some(s) = info.slot("Class") {
            info.defaults[s] = Value::Class(Some(id));
        }
        self.classes.push(info);
        self.class_by_field.insert(f, id);
        Ok(id)
    }

    /// Converts a tagged default to a VM value. Class references are recorded in `pending`
    /// (with their path) and stored as `Class(None)` until the class is loaded.
    fn convert_default(&mut self, pkg: usize, v: &upkg::Value, ty: &Ty, path: &mut Vec<DefSeg>, pending: &mut Vec<Pending>, depth: u32) -> Option<Value> {
        if depth > 8 {
            return None;
        }
        Some(match v {
            upkg::Value::Byte(b) => match ty {
                Ty::Int => Value::Int(*b as i32),
                _ => Value::Byte(*b),
            },
            upkg::Value::Int(i) => Value::Int(*i),
            upkg::Value::Bool(b) => Value::Bool(*b),
            upkg::Value::Float(f) => Value::Float(*f),
            upkg::Value::Name(n) => Value::Name(n.clone()),
            upkg::Value::Str(s) => Value::Str(s.clone()),
            upkg::Value::Vector(v) => Value::Vector(*v),
            upkg::Value::Rotator(r) => Value::Rotator(*r),
            upkg::Value::Color(c) => match ty {
                Ty::Struct(fields) if fields.len() == 4 => {
                    Value::Struct(fields.iter().zip(c).map(|((n, _), b)| (n.clone(), Value::Byte(*b))).collect())
                }
                _ => return None,
            },
            upkg::Value::Object(r) => self.convert_ref(pkg, *r, ty, path, pending),
            upkg::Value::Struct { raw, .. } => {
                let mut r = upkg::Reader::new(raw);
                let v = self.read_struct_value(pkg, &mut r, ty, path, pending, depth)?;
                if r.pos() != raw.len() {
                    return None;
                }
                v
            }
            upkg::Value::Array { raw } => {
                let Ty::Array(inner) = ty else { return None };
                let mut r = upkg::Reader::new(raw);
                let n = r.compact().ok()?;
                if !(0..=65536).contains(&n) {
                    return None;
                }
                let mut out = Vec::with_capacity(n as usize);
                for k in 0..n as usize {
                    path.push(DefSeg::Idx(k));
                    let v = self.read_elem(pkg, &mut r, inner, path, pending, depth + 1);
                    path.pop();
                    out.push(v?);
                }
                if r.pos() != raw.len() {
                    return None;
                }
                Value::Array(out)
            }
            _ => return None,
        })
    }

    fn convert_ref(&mut self, pkg: usize, r: ObjRef, ty: &Ty, path: &[DefSeg], pending: &mut Vec<Pending>) -> Value {
        match ty {
            Ty::Class => match self.resolve(pkg, r) {
                Some(f) if self.pkgs[f.pkg as usize].class(f.export as usize).is_some() => {
                    pending.push((path.to_vec(), f));
                    Value::Class(None)
                }
                _ => Value::Class(None),
            },
            _ if r == ObjRef::Null => Value::Object(None),
            _ if matches!(r, ObjRef::Export(i) if self.subobject_class(pkg, i).is_some()) => {
                // An object defined inside the package with a script class (e.g. the DayBase /
                // ErrandBase objects of P2GameInfoSingle.Days): instantiated after the class
                // finishes loading, like class references.
                let ObjRef::Export(i) = r else { unreachable!() };
                self.pending_subobjects.push((path.to_vec(), FieldId { pkg: pkg as u32, export: i as u32 }));
                Value::Object(None)
            }
            _ if !matches!(r, ObjRef::Export(_)) && self.resolve(pkg, r).is_some_and(|f| self.subobject_class(f.pkg as usize, f.export as usize).is_some()) => {
                // A subobject defined in another script package (People pawns' RandomizedBoltons
                // point at Postal2Game.BoltonDef* objects): instantiate it the same way.
                let f = self.resolve(pkg, r).unwrap();
                self.pending_subobjects.push((path.to_vec(), f));
                Value::Object(None)
            }
            _ => Value::Resource(self.pkgs[pkg].pkg.full_path(r)),
        }
    }

    /// Drops every object from index `n` on (a level change). Package subobjects created after
    /// `n` (referenced from class defaults) are re-created and the class defaults repointed, so
    /// defaults never hold ids of dropped objects.
    pub fn truncate_objects(&mut self, n: usize) {
        self.objects.truncate(n);
        // Every dropped subobject instance, including ones a later instantiation of the same
        // export superseded in the cache (class defaults may still point at those).
        let stale: Vec<(FieldId, ObjId)> = self.subobject_of.iter().filter(|(o, _)| **o >= n).map(|(o, f)| (*f, *o)).collect();
        if stale.is_empty() {
            return;
        }
        for (_, o) in &stale {
            self.subobject_of.remove(o);
        }
        self.subobjects.retain(|_, o| *o < n);
        let mut remap: HashMap<ObjId, Option<ObjId>> = HashMap::new();
        for (f, old) in stale {
            let new = self.subobject(f);
            remap.insert(old, new);
        }
        fn fix(v: &mut Value, remap: &HashMap<ObjId, Option<ObjId>>) {
            match v {
                Value::Object(Some(o)) => {
                    if let Some(n) = remap.get(o) {
                        *v = Value::Object(*n);
                    }
                }
                Value::Array(a) => a.iter_mut().for_each(|e| fix(e, remap)),
                Value::Struct(s) => s.iter_mut().for_each(|(_, e)| fix(e, remap)),
                _ => {}
            }
        }
        for c in &mut self.classes {
            for d in &mut c.defaults {
                fix(d, &remap);
            }
        }
    }

    /// The script class of a package export that is a plain object (not a class, function,
    /// property, state or other script definition), if its class is a loadable script class.
    fn subobject_class(&mut self, pkg: usize, export: usize) -> Option<FieldId> {
        let sp = self.pkgs[pkg].clone();
        if sp.objects.get(export).is_some_and(|o| o.is_some()) {
            return None; // a script definition (class, function, property, ...)
        }
        let e = sp.pkg.exports.get(export)?;
        if matches!(e.class, ObjRef::Null) {
            return None;
        }
        let f = self.resolve(pkg, e.class)?;
        self.pkgs[f.pkg as usize].class(f.export as usize)?;
        Some(f)
    }

    /// Full package path (`Pkg.Group.Name`) of an object instantiated from a package export
    /// (e.g. a Texture embedded in a script package), so hosts can load the asset itself.
    pub fn subobject_path(&self, o: ObjId) -> Option<String> {
        let f = self.subobject_of.get(&o)?;
        Some(self.pkgs[f.pkg as usize].pkg.full_path(ObjRef::Export(f.export as usize)))
    }

    /// Instantiates a package subobject (cached per export): an object of its script class with
    /// its saved properties applied. References to other subobjects resolve recursively.
    pub fn subobject(&mut self, f: FieldId) -> Option<ObjId> {
        if let Some(&o) = self.subobjects.get(&f) {
            if o < self.objects.len() {
                return Some(o);
            }
            self.subobjects.remove(&f);
        }
        let class_field = self.subobject_class(f.pkg as usize, f.export as usize)?;
        let class = self.load_class(class_field).ok()?;
        let o = self.new_object(class);
        self.subobjects.insert(f, o);
        self.subobject_of.insert(o, f);
        let sp = self.pkgs[f.pkg as usize].clone();
        let name = sp.export_name(f.export as usize).to_string();
        self.set_prop(o, "Name", Value::Name(name));
        let Ok((_, props)) = upkg::begin_object(&sp.pkg, f.export as usize) else { return Some(o) };
        let mut pending: Vec<Pending> = Vec::new();
        let mut path = Vec::new();
        let mark_sub = self.pending_subobjects.len();
        for p in &props.0 {
            let Some(s) = self.classes[class].slot(&p.name) else { continue };
            let slot_ty = self.classes[class].slots[s].ty.clone();
            let (ty, idx) = match &slot_ty {
                Ty::Fixed(t, _) => (t.as_ref(), Some(p.index as usize)),
                t => (t, None),
            };
            path.clear();
            path.extend(idx.map(DefSeg::Idx));
            let mark = pending.len();
            let sub_mark = self.pending_subobjects.len();
            let Some(v) = self.convert_default(f.pkg as usize, &p.value, ty, &mut path, &mut pending, 0) else {
                self.pending_subobjects.truncate(sub_mark);
                pending.truncate(mark);
                continue;
            };
            let cell = &mut self.objects[o].slots[s];
            match (cell, idx) {
                (Value::Array(a), Some(i)) => {
                    if let Some(e) = a.get_mut(i) {
                        *e = v;
                    }
                }
                (d, _) => *d = v,
            }
            // Class references and nested subobjects inside this property.
            for (rel, target) in pending.drain(mark..).collect::<Vec<_>>() {
                let c = self.load_class(target).ok();
                if let Some(cell) = def_cell(&mut self.objects[o].slots[s], &rel) {
                    *cell = Value::Class(c);
                }
            }
            for (rel, target) in self.pending_subobjects.drain(sub_mark..).collect::<Vec<_>>() {
                let so = self.subobject(target);
                if let Some(cell) = def_cell(&mut self.objects[o].slots[s], &rel) {
                    *cell = Value::Object(so);
                }
            }
        }
        self.pending_subobjects.truncate(mark_sub);
        Some(o)
    }

    /// Reads a struct value: a tagged property list, or (for immutable structs) the binary fields.
    fn read_struct_value(&mut self, pkg: usize, r: &mut upkg::Reader, ty: &Ty, path: &mut Vec<DefSeg>, pending: &mut Vec<Pending>, depth: u32) -> Option<Value> {
        let Ty::Struct(fields) = ty else {
            return match ty {
                Ty::Vector => Some(Value::Vector(r.vec3().ok()?)),
                Ty::Rotator => Some(Value::Rotator([r.i32().ok()?, r.i32().ok()?, r.i32().ok()?])),
                _ => None,
            };
        };
        let save = r.clone();
        let sp = self.pkgs[pkg].clone();
        if let Ok(props) = upkg::read_properties(r, &sp.pkg) {
            let mut val = ty.zero();
            let mut ok = true;
            for p in &props.0 {
                let Some(k) = fields.iter().position(|(n, _)| n.eq_ignore_ascii_case(&p.name)) else { continue };
                let (fty, idx) = match &fields[k].1 {
                    Ty::Fixed(t, _) => (t.as_ref(), Some(p.index as usize)),
                    t => (t, None),
                };
                path.push(DefSeg::Field(fields[k].0.clone()));
                path.extend(idx.map(DefSeg::Idx));
                let cv = self.convert_default(pkg, &p.value, fty, path, pending, depth + 1);
                path.truncate(path.len() - 1 - idx.is_some() as usize);
                let Some(cv) = cv else {
                    ok = false;
                    continue;
                };
                if let Value::Struct(fv) = &mut val {
                    match (&mut fv[k].1, idx) {
                        (Value::Array(a), Some(i)) => {
                            if let Some(e) = a.get_mut(i) {
                                *e = cv;
                            }
                        }
                        (d, _) => *d = cv,
                    }
                }
            }
            if ok || !props.0.is_empty() {
                return Some(val);
            }
        }
        // Binary layout: fields in order.
        *r = save;
        let mut out = Vec::with_capacity(fields.len());
        for (n, t) in fields {
            path.push(DefSeg::Field(n.clone()));
            let v = self.read_elem(pkg, r, t, path, pending, depth + 1);
            path.pop();
            out.push((n.clone(), v?));
        }
        Some(Value::Struct(out))
    }

    /// Reads one binary-serialized array element / struct field.
    fn read_elem(&mut self, pkg: usize, r: &mut upkg::Reader, ty: &Ty, path: &mut Vec<DefSeg>, pending: &mut Vec<Pending>, depth: u32) -> Option<Value> {
        if depth > 8 {
            return None;
        }
        Some(match ty {
            Ty::Byte => Value::Byte(r.u8().ok()?),
            Ty::Bool => Value::Bool(r.u8().ok()? != 0),
            Ty::Int => Value::Int(r.i32().ok()?),
            Ty::Float => Value::Float(r.f32().ok()?),
            Ty::Name => {
                let i = r.compact().ok()?;
                Value::Name(self.pkgs[pkg].pkg.names.get(usize::try_from(i).ok()?)?.clone())
            }
            Ty::Str => Value::Str(r.fstring().ok()?),
            Ty::Object | Ty::Class => {
                let o = ObjRef::from_raw(r.compact().ok()?);
                self.convert_ref(pkg, o, ty, path, pending)
            }
            Ty::Vector | Ty::Rotator | Ty::Struct(_) => self.read_struct_value(pkg, r, ty, path, pending, depth)?,
            Ty::Fixed(t, n) => {
                let mut v = Vec::with_capacity(*n);
                for k in 0..*n {
                    path.push(DefSeg::Idx(k));
                    let e = self.read_elem(pkg, r, t, path, pending, depth + 1);
                    path.pop();
                    v.push(e?);
                }
                Value::Array(v)
            }
            _ => return None,
        })
    }

    /// Finds a loaded-or-loadable class by package and name.
    pub fn find_class(&mut self, package: &str, name: &str) -> Result<ClassId> {
        let key = name_hash(package) ^ name_hash(name).rotate_left(17);
        if let Some(&c) = self.class_by_name.get(&key) {
            return Ok(c);
        }
        let p = self.package(package).ok_or_else(|| ScriptError::Bad(format!("package {package} not found")))?;
        let i = self.pkgs[p].find_class(name).ok_or_else(|| ScriptError::Bad(format!("class {name} not in {package}")))?;
        let c = self.load_class(FieldId { pkg: p as u32, export: i as u32 })?;
        self.class_by_name.insert(key, c);
        Ok(c)
    }

    /// Finds a class by `"Package.Class"` or by bare class name. A bare name searches the
    /// loaded packages first, then every `.u` package in the System directory.
    pub fn find_class_by_name(&mut self, name: &str) -> Result<ClassId> {
        if let Some((p, c)) = name.split_once('.') {
            return self.find_class(p, c);
        }
        let hit = (0..self.pkgs.len()).find_map(|p| self.pkgs[p].find_class(name).map(|i| (p, i)));
        if let Some((p, i)) = hit {
            return self.load_class(FieldId { pkg: p as u32, export: i as u32 });
        }
        let mut files: Vec<_> = std::fs::read_dir(&self.system_dir)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("u"))).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            let Some(stem) = f.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
            if let Some(p) = self.package(&stem) {
                if let Some(i) = self.pkgs[p].find_class(name) {
                    return self.load_class(FieldId { pkg: p as u32, export: i as u32 });
                }
            }
        }
        err(format!("class {name} not found"))
    }

    /// A class default property by name (`None` if the class has no such property).
    pub fn default_value(&self, class: ClassId, prop: &str) -> Option<&Value> {
        self.classes[class].default(prop)
    }

    /// Convenience: loads `package.class` and returns a copy of one default property.
    /// `Ok(None)` when the class has no such property.
    pub fn class_default(&mut self, package: &str, class: &str, prop: &str) -> Result<Option<Value>> {
        let c = self.find_class(package, class)?;
        Ok(self.default_value(c, prop).cloned())
    }

    pub fn is_child_of(&self, mut c: ClassId, parent: ClassId) -> bool {
        loop {
            if c == parent {
                return true;
            }
            match self.classes[c].super_ {
                Some(s) => c = s,
                None => return false,
            }
        }
    }

    /// True if a class or one of its supers has this name.
    pub fn class_is_a(&self, mut c: ClassId, name: &str) -> bool {
        loop {
            if self.classes[c].name.eq_ignore_ascii_case(name) {
                return true;
            }
            match self.classes[c].super_ {
                Some(s) => c = s,
                None => return false,
            }
        }
    }

    /// Creates an object initialised from its class defaults (with `Name` and `Class` set).
    pub fn new_object(&mut self, class: ClassId) -> ObjId {
        let mut slots = self.classes[class].defaults.clone();
        let id = self.objects.len();
        let ci = &self.classes[class];
        if let Some(s) = ci.slot("Class") {
            slots[s] = Value::Class(Some(class));
        }
        if let Some(s) = ci.slot("Name") {
            slots[s] = Value::Name(format!("{}{id}", ci.name));
        }
        self.objects.push(Object { class, slots, exec: StateFrame::default(), deleted: false });
        id
    }

    /// The Engine.Actor class, if Engine is available.
    pub fn actor_class(&mut self) -> Option<ClassId> {
        if self.actor_class.is_none() {
            self.actor_class = Some(self.find_class("Engine", "Actor").ok());
        }
        self.actor_class.flatten()
    }

    pub fn is_actor(&mut self, obj: ObjId) -> bool {
        let c = self.objects[obj].class;
        self.actor_class().is_some_and(|a| self.is_child_of(c, a))
    }

    pub fn get_prop(&self, obj: ObjId, name: &str) -> Option<&Value> {
        // Hosts can hold ids from before a level change truncated the object table: treat those as
        // missing rather than panicking mid-frame.
        let o = self.objects.get(obj)?;
        o.slots.get(self.classes[o.class].slot(name)?)
    }

    pub fn set_prop(&mut self, obj: ObjId, name: &str, v: Value) -> bool {
        let class = self.objects[obj].class;
        match self.classes[class].slot(name) {
            Some(s) => {
                self.objects[obj].slots[s] = v;
                true
            }
            None => false,
        }
    }

    /// Display name of an object (its `Name` property).
    pub fn object_name(&self, obj: ObjId) -> String {
        match self.get_prop(obj, "Name") {
            Some(Value::Name(n)) if !n.eq_ignore_ascii_case("None") => n.clone(),
            _ => format!("{}{obj}", self.classes[self.objects[obj].class].name),
        }
    }

    /// String conversion with object and class names resolved.
    pub fn value_string(&self, v: &Value) -> String {
        match v {
            Value::Object(Some(o)) => self.object_name(*o),
            Value::Class(Some(c)) => format!("{}.{}", self.classes[*c].package, self.classes[*c].name),
            v => v.to_ustring(),
        }
    }

    pub(crate) fn func_info(&mut self, f: FieldId) -> Result<Rc<FuncInfo>> {
        if let Some(fi) = self.funcs.get(&f) {
            return Ok(fi.clone());
        }
        let sp = self.pkgs[f.pkg as usize].clone();
        let Some(fd) = sp.function(f.export as usize) else { return err(format!("{} is not a function", self.field_name(f))) };
        let owner = sp.pkg.ref_name(sp.pkg.exports[f.export as usize].outer).to_string();
        let name = self.field_name(f);
        let stmts = &fd.base.script.stmts;
        let empty = fd.flags & func_flags::NATIVE == 0
            && stmts.iter().all(|s| matches!(&s.expr, Expr::Nothing) || matches!(&s.expr, Expr::Return(x) if matches!(**x, Expr::Nothing)));
        let mut info = FuncInfo {
            field: f,
            probe: probe_bit(&name),
            empty,
            name,
            owner,
            flags: fd.flags,
            native: fd.native,
            locals: Vec::new(),
            params: Vec::new(),
            ret: None,
            local_index: HashMap::new(),
            native_fn: std::cell::Cell::new(None),
            watch: None,
        };
        if !self.watches.is_empty() {
            info.watch = self.watches.get(&info.name.to_ascii_lowercase()).copied();
        }
        for c in sp.children(f.export as usize) {
            if let Some(ScriptObject::Property(p)) = &sp.objects[c] {
                let ty = self.prop_ty(FieldId { pkg: f.pkg, export: c as u32 }, &p.kind, p.array_dim, 0);
                let idx = info.locals.len();
                if p.has(prop_flags::RETURN_PARM) {
                    info.ret = Some(idx);
                } else if p.has(prop_flags::PARM) {
                    info.params.push(idx);
                }
                info.local_index.insert(c as u32, idx);
                info.locals.push((c as u32, p.flags, ty));
            }
        }
        let rc = Rc::new(info);
        self.funcs.insert(f, rc.clone());
        Ok(rc)
    }

    /// Finds a function by name in a class or its supers (ignoring states).
    pub fn find_function(&mut self, class: ClassId, name: &str) -> Option<FieldId> {
        let key = (class, name_hash(name));
        if let Some(r) = self.func_lookup.get(&key) {
            return *r;
        }
        let mut c = Some(class);
        let mut found = None;
        while let Some(ci) = c {
            let f = self.classes[ci].field;
            let sp = &self.pkgs[f.pkg as usize];
            if let Some(k) = sp.children(f.export as usize).into_iter().find(|&k| {
                matches!(sp.objects[k], Some(ScriptObject::Function(_))) && sp.export_name(k).eq_ignore_ascii_case(name)
            }) {
                found = Some(FieldId { pkg: f.pkg, export: k as u32 });
                break;
            }
            c = self.classes[ci].super_;
        }
        self.func_lookup.insert(key, found);
        found
    }

    /// Calls a function by name on an object (or statically on a class when `this` is None).
    /// The lookup uses the class only; see [`Vm::call_event`] for state-aware calls.
    pub fn call(&mut self, class: ClassId, this: Option<ObjId>, func: &str, args: Vec<Value>) -> Result<Value> {
        let f = self.find_function(class, func).ok_or_else(|| ScriptError::Bad(format!("function {func} not found")))?;
        if self.frames.is_empty() {
            self.steps = 0;
        }
        Ok(self.invoke(f, this, Some(class), args)?.0)
    }

    /// Invokes a function with evaluated arguments. Returns the result and final parameter values
    /// (for out-parameter write-back).
    fn invoke(&mut self, f: FieldId, this: Option<ObjId>, class: Option<ClassId>, mut args: Vec<Value>) -> Result<(Value, Vec<Value>)> {
        let fi = self.func_info(f)?;
        self.calls_total += 1;
        if fi.flags & func_flags::NATIVE != 0 {
            self.native_calls_total += 1;
            // Natives see omitted optional parameters as Void.
            args.resize(args.len().max(fi.params.len()), Value::Void);
            return self.invoke_native(&fi, this, class, args);
        }
        for (k, &li) in fi.params.iter().enumerate() {
            match args.get_mut(k) {
                Some(a @ Value::Void) => *a = fi.locals[li].2.zero(),
                Some(_) => {}
                None => args.push(fi.locals[li].2.zero()),
            }
        }
        if self.frames.len() > 250 {
            return err("script call stack overflow");
        }
        let mut locals: Vec<Value> = fi.locals.iter().map(|l| l.2.zero()).collect();
        for (k, &li) in fi.params.iter().enumerate() {
            locals[li] = std::mem::replace(&mut args[k], Value::Void);
        }
        if let Some(w) = fi.watch {
            let mut a: Vec<Value> = fi.params.iter().map(|&li| locals[li].clone()).collect();
            w(self, this, &mut a, false);
            for (k, &li) in fi.params.iter().enumerate() {
                locals[li] = std::mem::replace(&mut a[k], Value::Void);
            }
        }
        let class = class.or_else(|| this.map(|o| self.objects[o].class));
        self.frames.push(Frame { pkg: f.pkg as usize, this, class, locals, fi: Some(fi.clone()), iters: Vec::new() });
        let sp = self.pkgs[f.pkg as usize].clone();
        let script = &sp.function(f.export as usize).unwrap().base.script;
        let r = self.exec(script, 0, Mode::Func, &fi.name);
        let frame = self.frames.pop().unwrap();
        let ret = match r? {
            Flow::Return(Some(v)) => v,
            _ => fi.ret.map(|i| frame.locals[i].clone()).unwrap_or(Value::Void),
        };
        if let Some(names) = call_trace() {
            if names.iter().any(|n| n.eq_ignore_ascii_case(&fi.name)) {
                let who = this.map(|o| format!("{} [{}]", self.object_name(o), self.state_name(o))).unwrap_or_default();
                let shown: Vec<String> = fi.params.iter().map(|&li| self.value_string(&frame.locals[li])).collect();
                eprintln!("CALL {}.{}({}) -> {} on {who}", fi.owner, fi.name, shown.join(", "), self.value_string(&ret));
            }
        }
        let mut params: Vec<Value> = fi.params.iter().map(|&li| frame.locals[li].clone()).collect();
        if let Some(w) = fi.watch {
            w(self, this, &mut params, true);
        }
        Ok((ret, params))
    }

    fn invoke_native(&mut self, fi: &FuncInfo, this: Option<ObjId>, class: Option<ClassId>, mut args: Vec<Value>) -> Result<(Value, Vec<Value>)> {
        let saved = (self.native_this, self.native_class);
        self.native_this = this;
        self.native_class = class.or_else(|| this.map(|o| self.objects[o].class));
        let t0 = self.profile.then(std::time::Instant::now);
        let nf = match fi.native_fn.get() {
            Some(nf) => nf,
            None => {
                let nf = self.natives.get(&fi.name.to_ascii_lowercase()).copied();
                fi.native_fn.set(Some(nf));
                nf
            }
        };
        let r = match nf {
            Some(nf) => Some(nf(self, &mut args)),
            None => {
                let h = self.host.clone();
                h.native(self, &NativeCall { this, class: &fi.owner, name: &fi.name }, &mut args)
            }
        };
        if let Some(t0) = t0 {
            let e = self.profile_natives.entry(fi.name.clone()).or_default();
            e.0 += 1;
            e.1 += t0.elapsed().as_secs_f64();
        }
        (self.native_this, self.native_class) = saved;
        match r {
            Some(r) => Ok((r?, args)),
            None if fi.flags & func_flags::LATENT != 0 => Ok((Value::Void, args)),
            None => {
                *self.missing_natives.entry(format!("{}.{}", fi.owner, fi.name)).or_default() += 1;
                match self.missing_native_policy {
                    MissingNativePolicy::Strict => err(format!("native function {} (index {}) is not implemented", fi.name, fi.native)),
                    MissingNativePolicy::Lenient => Ok((fi.ret_zero(), args)),
                }
            }
        }
    }

    /// Runs a statement list from `pc` until it returns, ends, or (state code) yields.
    fn exec(&mut self, script: &Script, mut pc: usize, mode: Mode, what: &str) -> Result<Flow> {
        let target = |off: u16| script.stmt_at(off as u32).ok_or_else(|| ScriptError::Bad(format!("jump to non-statement offset {off:#x} in {what}")));
        while pc < script.stmts.len() {
            self.steps += 1;
            self.steps_total += 1;
            if self.steps > self.step_budget {
                return err(format!("step budget exhausted in {what}"));
            }
            if let Mode::State { obj, epoch } = mode {
                if self.objects[obj].exec.generation != epoch {
                    return Ok(Flow::Switched);
                }
            }
            let e = &script.stmts[pc].expr;
            match e {
                Expr::Jump(t) => pc = target(*t)?,
                Expr::JumpIfNot { target: t, cond } => {
                    pc = if self.eval(cond, Ctx::Frame)?.as_bool() { pc + 1 } else { target(*t)? };
                }
                Expr::Return(x) => {
                    if let Mode::State { .. } = mode {
                        return Ok(Flow::End);
                    }
                    return Ok(Flow::Return(match **x {
                        Expr::Nothing => None,
                        _ => Some(self.eval(x, Ctx::Frame)?),
                    }));
                }
                Expr::Switch { expr, .. } => {
                    // Case headers follow the switch; a non-matching case jumps to the next one,
                    // the last case's link points past the switch when there is no default.
                    let v = self.eval(expr, Ctx::Frame)?;
                    let mut k = pc + 1;
                    pc = loop {
                        match script.stmts.get(k).map(|s| &s.expr) {
                            Some(Expr::Case { next: 0xFFFF, .. }) => break k + 1,
                            Some(Expr::Case { next, value: Some(cv) }) => {
                                if self.eval(cv, Ctx::Frame)?.script_eq(&v) {
                                    break k + 1;
                                }
                                k = target(*next)?;
                            }
                            _ => break k,
                        }
                    };
                }
                Expr::Case { .. } | Expr::Nothing | Expr::LabelTable(_) => pc += 1,
                Expr::Stop => return Ok(Flow::End),
                Expr::Iterator { expr, end } => {
                    let mut it = self.start_iterator(expr, pc + 1)?;
                    let has = self.iter_advance(&mut it)?;
                    self.frames.last_mut().unwrap().iters.push(it);
                    pc = if has { pc + 1 } else { target(*end)? };
                }
                Expr::IteratorNext => {
                    let Some(mut it) = self.frames.last_mut().unwrap().iters.pop() else { return err("iterator_next without an active iterator") };
                    let has = self.iter_advance(&mut it)?;
                    let body = it.body;
                    self.frames.last_mut().unwrap().iters.push(it);
                    pc = if has { body } else { pc + 1 };
                }
                Expr::IteratorPop => {
                    self.frames.last_mut().unwrap().iters.pop();
                    pc += 1;
                }
                Expr::GotoLabel(x) => {
                    let Mode::State { obj, .. } = mode else { return err(format!("goto label outside state code in {what}")) };
                    let label = self.eval(x, Ctx::Frame)?.as_name();
                    self.goto_label(obj, &label);
                    return Ok(Flow::Switched);
                }
                Expr::Assert { cond, line } => {
                    if !self.eval(cond, Ctx::Frame)?.as_bool() {
                        return err(format!("assertion failed in {what} line {line}"));
                    }
                    pc += 1;
                }
                other => {
                    if let Mode::State { obj, epoch } = mode {
                        if let Some(mut l) = self.latent_call(other)? {
                            if l.name.eq_ignore_ascii_case("Sleep") {
                                let t = l.args.first().map(|v| v.as_float()).unwrap_or(0.0);
                                self.set_prop(obj, "LatentFloat", Value::Float(t));
                            }
                            let h = self.host.clone();
                            h.latent_begin(self, obj, &mut l)?;
                            if self.objects[obj].exec.generation != epoch {
                                return Ok(Flow::Switched);
                            }
                            self.objects[obj].exec.latent = Some(l);
                            return Ok(Flow::Latent(pc + 1));
                        }
                    }
                    self.eval(other, Ctx::Frame)?;
                    pc += 1;
                }
            }
        }
        Ok(Flow::End)
    }

    /// If a state-code statement calls a latent function, evaluates its arguments.
    fn latent_call(&mut self, e: &Expr) -> Result<Option<Latent>> {
        let pkg = self.frame().pkg;
        let (f, args) = match e {
            Expr::NativeCall { index, args } => match self.native_decl.get(index) {
                Some(f) => (*f, args),
                None => return Ok(None),
            },
            Expr::FinalFunction { func, args } => match self.resolve(pkg, *func) {
                Some(f) => (f, args),
                None => return Ok(None),
            },
            Expr::VirtualFunction { name, args } => {
                let fname = self.pkgs[pkg].pkg.name(*name as usize).to_string();
                let (this, class) = (self.frame().this, self.frame().class);
                match class.and_then(|c| self.find_virtual(this, c, &fname)) {
                    Some(f) => (f, args),
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };
        let fi = self.func_info(f)?;
        if fi.flags & func_flags::LATENT == 0 {
            return Ok(None);
        }
        let mut vals = Vec::with_capacity(fi.params.len());
        for k in 0..fi.params.len() {
            vals.push(match args.get(k) {
                Some(Expr::Nothing) | None => Value::Void,
                Some(a) => self.eval(a, Ctx::Frame)?,
            });
        }
        Ok(Some(Latent { name: fi.name.clone(), native: fi.native, args: vals, elapsed: 0.0 }))
    }

    /// Evaluates a `foreach` header: calls the iterator function (through the host) and binds its
    /// out parameters to places in the current frame.
    fn start_iterator(&mut self, e: &Expr, body: usize) -> Result<IterState> {
        let pkg = self.frame().pkg;
        let (call, this) = match e {
            Expr::Context { obj, expr, .. } => (expr.as_ref(), self.eval(obj, Ctx::Frame)?.as_obj()),
            _ => (e, self.frame().this),
        };
        let class = this.map(|o| self.objects[o].class).or(self.frame().class);
        let (f, args) = match call {
            Expr::NativeCall { index, args } => (self.native_decl.get(index).copied(), args),
            Expr::FinalFunction { func, args } => (self.resolve(pkg, *func), args),
            Expr::VirtualFunction { name, args } | Expr::GlobalFunction { name, args } => {
                let fname = self.pkgs[pkg].pkg.name(*name as usize).to_string();
                (class.and_then(|c| self.find_virtual(this, c, &fname)), args)
            }
            other => return err(format!("unsupported iterator expression {other:?}")),
        };
        let f = f.ok_or_else(|| ScriptError::Bad("unresolved iterator function".into()))?;
        let fi = self.func_info(f)?;
        let mut vals = Vec::with_capacity(fi.params.len());
        let mut places = Vec::new();
        for (k, &li) in fi.params.iter().enumerate() {
            let out = fi.locals[li].1 & prop_flags::OUT_PARM != 0;
            match args.get(k) {
                Some(a) if !matches!(a, Expr::Nothing) => {
                    if out {
                        let p = self.place(a, Ctx::Frame)?;
                        vals.push(self.read(&p));
                        places.push(Some(p));
                    } else {
                        vals.push(self.eval(a, Ctx::Frame)?);
                    }
                }
                _ => {
                    vals.push(Value::Void);
                    if out {
                        places.push(None);
                    }
                }
            }
        }
        let rows = if fi.flags & func_flags::NATIVE != 0 {
            let h = self.host.clone();
            let r = h.iterate(self, this, &fi.name, &vals)?;
            match r {
                Some(r) => r,
                None => {
                    *self.missing_natives.entry(format!("{}.{}", fi.owner, fi.name)).or_default() += 1;
                    if self.missing_native_policy == MissingNativePolicy::Strict {
                        return err(format!("native function {} (index {}) is not implemented", fi.name, fi.native));
                    }
                    Vec::new()
                }
            }
        } else {
            return err(format!("iterator {} is not native", fi.name));
        };
        Ok(IterState { rows, next: 0, places, body })
    }

    /// Binds the next row of an iterator (skipping destroyed actors). False when exhausted.
    fn iter_advance(&mut self, it: &mut IterState) -> Result<bool> {
        while it.next < it.rows.len() {
            let row = std::mem::take(&mut it.rows[it.next]);
            it.next += 1;
            if let Some(Value::Object(Some(o))) = row.first() {
                if self.objects[*o].deleted {
                    continue;
                }
            }
            for (v, p) in row.into_iter().zip(&it.places) {
                if let Some(p) = p {
                    self.write(p, v)?;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn frame(&self) -> &Frame {
        self.frames.last().expect("no active frame")
    }

    fn ctx_obj(&self, ctx: Ctx) -> Option<ObjId> {
        match ctx {
            Ctx::Frame => self.frame().this,
            Ctx::Obj(o) => o,
            Ctx::Class(_) => None,
        }
    }

    fn ctx_class(&self, ctx: Ctx) -> Option<ClassId> {
        match ctx {
            Ctx::Frame => self.frame().class,
            Ctx::Obj(o) => o.map(|o| self.objects[o].class),
            Ctx::Class(c) => Some(c),
        }
    }

    fn place(&mut self, e: &Expr, ctx: Ctx) -> Result<Place> {
        let pkg = self.frame().pkg;
        Ok(match e {
            Expr::LocalVar(r) | Expr::NativeParm(r) => {
                let ObjRef::Export(i) = r else { return err("local variable is not an export") };
                let idx = self.frame().fi.as_ref().and_then(|fi| fi.local_index.get(&(*i as u32)).copied());
                let idx = idx.ok_or_else(|| ScriptError::Bad("unknown local".into()))?;
                Place { root: Root::Local(idx), path: Vec::new() }
            }
            Expr::InstanceVar(r) => {
                let f = self.resolve(pkg, *r).ok_or_else(|| ScriptError::Bad("unresolved instance variable".into()))?;
                let Some(o) = self.ctx_obj(ctx) else {
                    if let Ctx::Class(c) = ctx {
                        // Properties of a class object itself (Object.Name / Object.Class).
                        let n = self.field_name(f);
                        if n.eq_ignore_ascii_case("Name") {
                            return Ok(Place { root: Root::Temp(Value::Name(self.classes[c].name.clone())), path: Vec::new() });
                        }
                    }
                    let msg = format!("Warning: Accessed None ({})", self.field_name(f));
                    self.push_log(msg);
                    return Ok(Place { root: Root::Temp(Value::Void), path: Vec::new() });
                };
                let class = self.objects[o].class;
                let s = match self.classes[class].slot_by_field.get(&f) {
                    Some(&s) => s,
                    None => {
                        // `class X within Y`: Y's variables are accessed through the Outer object.
                        let outer = self.get_prop(o, "Outer").and_then(|v| v.as_obj());
                        match outer.and_then(|w| self.classes[self.objects[w].class].slot_by_field.get(&f).map(|&s| (w, s))) {
                            Some((w, s)) => return Ok(Place { root: Root::Inst(w, s), path: Vec::new() }),
                            None => return err(format!("{} not in object class", self.field_name(f))),
                        }
                    }
                };
                Place { root: Root::Inst(o, s), path: Vec::new() }
            }
            Expr::DefaultVar(r) => {
                let f = self.resolve(pkg, *r).ok_or_else(|| ScriptError::Bad("unresolved default variable".into()))?;
                let c = self.ctx_class(ctx).ok_or_else(|| ScriptError::Bad("default access without a class".into()))?;
                let s = *self.classes[c].slot_by_field.get(&f).ok_or_else(|| ScriptError::Bad("default variable not in class".into()))?;
                Place { root: Root::Default(c, s), path: Vec::new() }
            }
            Expr::ArrayElement { index, array } | Expr::DynArrayElement { index, array } => {
                let i = self.eval(index, Ctx::Frame)?.as_int().max(0) as usize;
                let mut p = self.place(array, ctx)?;
                p.path.push(Seg::Idx(i));
                p
            }
            Expr::StructMember { prop, expr } => {
                let name = self.pkgs[pkg].pkg.ref_name(*prop).to_string();
                let mut p = self.place(expr, ctx)?;
                p.path.push(Seg::Field(name));
                p
            }
            Expr::DynArrayLength(x) => {
                let mut p = self.place(x, ctx)?;
                p.path.push(Seg::Len);
                p
            }
            Expr::BoolVariable(x) => self.place(x, ctx)?,
            Expr::Context { obj, expr, .. } => {
                let o = self.eval(obj, Ctx::Frame)?;
                match o {
                    Value::Object(None) | Value::Void => {
                        let f = self.frames.last().and_then(|fr| fr.fi.as_ref()).map(|fi| format!("{}.{}", fi.owner, fi.name)).unwrap_or_default();
                        self.push_log(format!("Warning: Accessed None (in {f})"));
                        Place { root: Root::Temp(Value::Void), path: Vec::new() }
                    }
                    Value::Object(Some(o)) => self.place(expr, Ctx::Obj(Some(o)))?,
                    Value::Class(Some(c)) => self.place(expr, Ctx::Class(c))?,
                    Value::Resource(path) => {
                        // Property of a non-script object (Tex.USize): ask the host.
                        let prop = match expr.as_ref() {
                            Expr::InstanceVar(r) => self.resolve(pkg, *r).map(|f| self.field_name(f)),
                            _ => None,
                        };
                        let h = self.host.clone();
                        match prop.and_then(|p| h.resource_prop(self, &path, &p)) {
                            Some(v) => Place { root: Root::Temp(v), path: Vec::new() },
                            None => Place { root: Root::Temp(self.eval(expr, Ctx::Obj(None))?), path: Vec::new() },
                        }
                    }
                    _ => Place { root: Root::Temp(self.eval(expr, Ctx::Obj(None))?), path: Vec::new() },
                }
            }
            Expr::ClassContext { obj, expr, .. } => match self.eval(obj, Ctx::Frame)? {
                Value::Class(Some(c)) => self.place(expr, Ctx::Class(c))?,
                _ => {
                    self.push_log("Warning: Accessed None (class context)");
                    Place { root: Root::Temp(Value::Void), path: Vec::new() }
                }
            },
            other => Place { root: Root::Temp(self.eval_value(other, ctx)?), path: Vec::new() },
        })
    }

    fn read(&self, p: &Place) -> Value {
        let (v, ty): (&Value, Option<&Ty>) = match &p.root {
            Root::Local(i) => {
                let f = self.frame();
                (&f.locals[*i], f.fi.as_ref().map(|fi| &fi.locals[*i].2))
            }
            Root::Inst(o, s) => (&self.objects[*o].slots[*s], Some(&self.classes[self.objects[*o].class].slots[*s].ty)),
            Root::Default(c, s) => (&self.classes[*c].defaults[*s], Some(&self.classes[*c].slots[*s].ty)),
            Root::Temp(t) => (t, None),
        };
        match read_path(v, ty, &p.path) {
            // Destroyed actors read as None (the engine clears references at end of tick; this
            // also covers references nested in arrays, structs and locals).
            Value::Object(Some(o)) if self.objects[o].deleted => Value::Object(None),
            v => v,
        }
    }

    fn write(&mut self, p: &Place, val: Value) -> Result<()> {
        let fi_hold: Option<Rc<FuncInfo>>;
        let (cell, ty): (&mut Value, Option<&Ty>) = match &p.root {
            Root::Local(i) => {
                fi_hold = self.frames.last().unwrap().fi.clone();
                (&mut self.frames.last_mut().unwrap().locals[*i], fi_hold.as_deref().map(|fi| &fi.locals[*i].2))
            }
            Root::Inst(o, s) => {
                let c = self.objects[*o].class;
                (&mut self.objects[*o].slots[*s], Some(&self.classes[c].slots[*s].ty))
            }
            Root::Default(c, s) => {
                let ci = &mut self.classes[*c];
                (&mut ci.defaults[*s], Some(&ci.slots[*s].ty))
            }
            Root::Temp(_) => return Ok(()),
        };
        write_path(cell, ty, &p.path, val);
        Ok(())
    }

    /// Static type of a place (for zero-filling array growth).
    fn place_ty(&self, p: &Place) -> Option<Ty> {
        let mut ty = match &p.root {
            Root::Local(i) => self.frame().fi.as_ref().map(|fi| &fi.locals[*i].2),
            Root::Inst(o, s) => Some(&self.classes[self.objects[*o].class].slots[*s].ty),
            Root::Default(c, s) => Some(&self.classes[*c].slots[*s].ty),
            Root::Temp(_) => None,
        };
        for seg in &p.path {
            ty = ty.and_then(|t| t.sub(seg));
        }
        ty.cloned()
    }

    fn eval(&mut self, e: &Expr, ctx: Ctx) -> Result<Value> {
        match e {
            Expr::LocalVar(_)
            | Expr::NativeParm(_)
            | Expr::InstanceVar(_)
            | Expr::DefaultVar(_)
            | Expr::ArrayElement { .. }
            | Expr::DynArrayElement { .. }
            | Expr::StructMember { .. }
            | Expr::DynArrayLength(_)
            | Expr::BoolVariable(_)
            | Expr::Context { .. }
            | Expr::ClassContext { .. } => {
                let p = self.place(e, ctx)?;
                Ok(self.read(&p))
            }
            _ => self.eval_value(e, ctx),
        }
    }

    fn eval_value(&mut self, e: &Expr, ctx: Ctx) -> Result<Value> {
        let pkg = self.frame().pkg;
        Ok(match e {
            Expr::Let { dst, src } | Expr::LetBool { dst, src } | Expr::LetDelegate { dst, src } => {
                let p = self.place(dst, ctx)?;
                let v = self.eval(src, Ctx::Frame)?;
                self.write(&p, v)?;
                Value::Void
            }
            Expr::IntConst(i) => Value::Int(*i),
            Expr::FloatConst(f) => Value::Float(*f),
            Expr::StringConst(s) | Expr::UnicodeStringConst(s) => Value::Str(s.clone()),
            Expr::NameConst(n) => Value::Name(self.pkgs[pkg].pkg.name(*n as usize).to_string()),
            Expr::ByteConst(b) => Value::Byte(*b),
            Expr::IntConstByte(b) => Value::Int(*b as i32),
            Expr::IntZero => Value::Int(0),
            Expr::IntOne => Value::Int(1),
            Expr::True => Value::Bool(true),
            Expr::False => Value::Bool(false),
            Expr::NoObject => Value::Object(None),
            Expr::SelfRef => Value::Object(self.frame().this),
            Expr::VectorConst(v) => Value::Vector(*v),
            Expr::RotationConst(r) => Value::Rotator(*r),
            Expr::Nothing => Value::Void,
            Expr::ObjectConst(r) => self.object_const(pkg, *r)?,
            Expr::Skip { expr, .. } | Expr::EatString(expr) => self.eval(expr, ctx)?,
            Expr::PrimitiveCast { kind, expr } => {
                let v = self.eval(expr, ctx)?;
                match (kind, &v) {
                    (0x52..=0x59, Value::Object(Some(_)) | Value::Class(Some(_))) => Value::Str(self.value_string(&v)),
                    _ => cast(*kind, v),
                }
            }
            Expr::DynamicCast { class, expr } => {
                let v = self.eval(expr, ctx)?;
                let target = self.resolve(pkg, *class).map(|f| self.load_class(f)).transpose()?;
                // `Viewport` (and `NetConnection`) are native-only Player subclasses with no script
                // export. In a standalone game every Player is the local Viewport, so the cast
                // keeps Player objects.
                if target.is_none() && matches!(v, Value::Object(Some(_))) {
                    if let ObjRef::Import(i) = *class {
                        let sp = &self.pkgs[pkg];
                        if sp.pkg.name(sp.pkg.imports[i].name).eq_ignore_ascii_case("Viewport") {
                            let o = v.as_obj().unwrap();
                            return Ok(if self.class_is_a(self.objects[o].class, "Player") { Value::Object(Some(o)) } else { Value::Object(None) });
                        }
                    }
                }
                match (v, target) {
                    (Value::Object(Some(o)), Some(t)) if self.is_child_of(self.objects[o].class, t) => Value::Object(Some(o)),
                    (Value::Class(Some(c)), Some(t)) if self.is_child_of(c, t) => Value::Class(Some(c)),
                    (Value::Class(_), _) => Value::Class(None),
                    // Non-script objects (meshes, textures, ...) carry no class here; keep them.
                    (r @ Value::Resource(_), _) => r,
                    _ => Value::Object(None),
                }
            }
            Expr::MetaCast { class, expr } => {
                let v = self.eval(expr, ctx)?;
                let target = self.resolve(pkg, *class).map(|f| self.load_class(f)).transpose()?;
                match (v, target) {
                    (Value::Class(Some(c)), Some(t)) if self.is_child_of(c, t) => Value::Class(Some(c)),
                    _ => Value::Class(None),
                }
            }
            Expr::StructCmpEq { a, b, .. } => Value::Bool(self.eval(a, Ctx::Frame)?.script_eq(&self.eval(b, Ctx::Frame)?)),
            Expr::StructCmpNe { a, b, .. } => Value::Bool(!self.eval(a, Ctx::Frame)?.script_eq(&self.eval(b, Ctx::Frame)?)),
            Expr::New { outer, class, .. } => {
                let outer = match outer.as_ref() {
                    Expr::Nothing => None,
                    e => self.eval(e, Ctx::Frame)?.as_obj(),
                };
                match self.eval(class, Ctx::Frame)? {
                    Value::Class(Some(c)) => {
                        // UE2 StaticConstructObject loads config (and per-object config) for
                        // `new`, like Spawn does (ShellRootWindow's InstalledTitle etc.).
                        self.apply_class_config(c);
                        let o = self.new_object(c);
                        self.apply_object_config(o);
                        if outer.is_some() {
                            self.set_prop(o, "Outer", Value::Object(outer));
                        }
                        Value::Object(Some(o))
                    }
                    _ => Value::Object(None),
                }
            }
            Expr::DynArrayInsert { array, index, count } | Expr::DynArrayRemove { array, index, count } => {
                let p = self.place(array, ctx)?;
                let i = self.eval(index, Ctx::Frame)?.as_int().max(0) as usize;
                let n = self.eval(count, Ctx::Frame)?.as_int().max(0) as usize;
                let mut a = match self.read(&p) {
                    Value::Array(a) => a,
                    _ => Vec::new(),
                };
                if matches!(e, Expr::DynArrayInsert { .. }) {
                    let zero = match self.place_ty(&p) {
                        Some(Ty::Array(t)) => t.zero(),
                        _ => Value::Void,
                    };
                    let i = i.min(a.len());
                    a.splice(i..i, std::iter::repeat_n(zero, n));
                } else if i < a.len() {
                    a.drain(i..(i + n).min(a.len()));
                }
                self.write(&p, Value::Array(a))?;
                Value::Void
            }
            Expr::NativeCall { index, args } => {
                let f = *self.native_decl.get(index).ok_or_else(|| ScriptError::Bad(format!("unknown native index {index}")))?;
                // Short-circuit boolean operators: the right operand is wrapped in a skip token.
                if let [a, Expr::Skip { expr: b, .. }] = args.as_slice() {
                    let av = self.eval(a, Ctx::Frame)?.as_bool();
                    let sp = &self.pkgs[f.pkg as usize];
                    let name = sp.export_name(f.export as usize);
                    if name.eq_ignore_ascii_case("AndAnd_BoolBool") && !av {
                        return Ok(Value::Bool(false));
                    }
                    if name.eq_ignore_ascii_case("OrOr_BoolBool") && av {
                        return Ok(Value::Bool(true));
                    }
                    return Ok(Value::Bool(self.eval(b, Ctx::Frame)?.as_bool()));
                }
                // Indexed natives are called on the current object (e.g. GotoState, Spawn).
                let this = self.ctx_obj(ctx);
                self.call_expr(f, this, None, args)?
            }
            Expr::FinalFunction { func, args } => {
                let f = self.resolve(pkg, *func).ok_or_else(|| ScriptError::Bad("unresolved final function".into()))?;
                let (this, class) = (self.ctx_obj(ctx), self.ctx_class(ctx));
                self.call_expr(f, this, class, args)?
            }
            Expr::VirtualFunction { name, args } => {
                let sp = self.pkgs[pkg].clone();
                let fname = sp.pkg.name(*name as usize);
                let (this, class) = (self.ctx_obj(ctx), self.ctx_class(ctx));
                let Some(class) = class else {
                    self.push_log(format!("Warning: Accessed None (call {fname})"));
                    return Ok(Value::Void);
                };
                let f = match self.find_virtual(this, class, fname) {
                    Some(f) => f,
                    None => {
                        // `class X within Y`: functions of Y are callable on the Outer object.
                        let outer = this.and_then(|o| self.get_prop(o, "Outer")).and_then(|v| v.as_obj());
                        let fname = fname.to_string();
                        if let Some(w) = outer {
                            let wc = self.objects[w].class;
                            if let Some(f) = self.find_virtual(Some(w), wc, &fname) {
                                return self.call_expr(f, Some(w), Some(wc), args);
                            }
                        }
                        return err(format!("function {fname} not found"));
                    }
                };
                let fi = self.func_info(f)?;
                if let (Some(o), Some(bit)) = (this, fi.probe) {
                    if self.is_ignored_bit(o, bit) {
                        return Ok(fi.ret_zero());
                    }
                }
                self.call_expr(f, this, Some(class), args)?
            }
            Expr::GlobalFunction { name, args } => {
                let fname = self.pkgs[pkg].pkg.name(*name as usize).to_string();
                let (this, class) = (self.ctx_obj(ctx), self.ctx_class(ctx));
                let Some(class) = class else {
                    self.push_log(format!("Warning: Accessed None (call {fname})"));
                    return Ok(Value::Void);
                };
                let f = self.find_function(class, &fname).ok_or_else(|| ScriptError::Bad(format!("function {fname} not found")))?;
                self.call_expr(f, this, Some(class), args)?
            }
            Expr::DelegateProperty(n) => {
                let name = self.pkgs[pkg].pkg.name(*n as usize).to_string();
                if name.eq_ignore_ascii_case("None") { Value::Delegate } else { Value::DelegateTo { obj: self.ctx_obj(ctx), func: name } }
            }
            Expr::DelegateFunction { prop, name, args } => {
                let fname = self.pkgs[pkg].pkg.name(*name as usize).to_string();
                let (this, class) = (self.ctx_obj(ctx), self.ctx_class(ctx));
                let bound = match (self.resolve(pkg, *prop), this) {
                    (Some(pf), Some(o)) => {
                        let c = self.objects[o].class;
                        self.classes[c].slot_by_field.get(&pf).map(|&s| self.objects[o].slots[s].clone())
                    }
                    _ => None,
                };
                let (t_obj, t_class, t_name) = match bound {
                    Some(Value::DelegateTo { obj: Some(o), func }) if !self.objects[o].deleted => (Some(o), Some(self.objects[o].class), func),
                    _ => (this, class, fname),
                };
                let Some(tc) = t_class else { return Ok(Value::Void) };
                let f = self.find_virtual(t_obj, tc, &t_name).ok_or_else(|| ScriptError::Bad(format!("delegate target {t_name} not found")))?;
                self.call_expr(f, t_obj, Some(tc), args)?
            }
            other => {
                // Variable-like expressions evaluated as values (e.g. a struct member of a call result).
                if let Expr::StructMember { prop, expr } = other {
                    let name = self.pkgs[pkg].pkg.ref_name(*prop).to_string();
                    let v = self.eval(expr, ctx)?;
                    return Ok(self.read(&Place { root: Root::Temp(v), path: vec![Seg::Field(name)] }));
                }
                return err(format!("unsupported expression {other:?}"));
            }
        })
    }

    fn object_const(&mut self, pkg: usize, r: ObjRef) -> Result<Value> {
        if r == ObjRef::Null {
            return Ok(Value::Object(None));
        }
        match self.resolve(pkg, r) {
            Some(f) if self.pkgs[f.pkg as usize].class(f.export as usize).is_some() => Ok(Value::Class(Some(self.load_class(f)?))),
            _ => Ok(Value::Resource(self.pkgs[pkg].pkg.full_path(r))),
        }
    }

    /// Evaluates call arguments in the caller frame, invokes, and writes back out parameters.
    fn call_expr(&mut self, f: FieldId, this: Option<ObjId>, class: Option<ClassId>, args: &[Expr]) -> Result<Value> {
        let fi = self.func_info(f)?;
        let mut vals = Vec::with_capacity(args.len());
        let mut outs = Vec::new();
        for (k, a) in args.iter().enumerate() {
            let flags = fi.params.get(k).map(|&li| fi.locals[li].1).unwrap_or(0);
            if flags & prop_flags::OUT_PARM != 0 && !matches!(a, Expr::Nothing) {
                let p = self.place(a, Ctx::Frame)?;
                vals.push(self.read(&p));
                outs.push((k, p));
            } else {
                vals.push(self.eval(a, Ctx::Frame)?);
            }
        }
        let class = if fi.flags & func_flags::STATIC != 0 && class.is_none() { self.frame().class } else { class };
        let (ret, params) = self.invoke(f, this, class, vals)?;
        for (k, p) in outs {
            if let Some(v) = params.get(k) {
                self.write(&p, v.clone())?;
            }
        }
        Ok(ret)
    }

    /// Localized string from `System\<Package>.int` (`[Section] Key=Value`), or the engine's
    /// `<?int?Package.Section.Key?>` marker when missing.
    pub fn localize(&mut self, section: &str, key: &str, package: &str) -> String {
        let pk = package.to_ascii_lowercase();
        if !self.localized.contains_key(&pk) {
            let table = std::fs::read(self.system_dir.join(format!("{package}.int"))).ok().map(|b| parse_ini(&b));
            self.localized.insert(pk.clone(), table);
        }
        self.localized
            .get(&pk)
            .and_then(|t| t.as_ref())
            .and_then(|t| t.get(&(section.to_ascii_lowercase(), key.to_ascii_lowercase())))
            .cloned()
            .unwrap_or_else(|| format!("<?int?{package}.{section}.{key}?>"))
    }

    /// Names of the enum values of an `Enum` object reference (e.g. a `Resource` path).
    pub fn enum_names(&mut self, path: &str) -> Option<Vec<String>> {
        let f = self.find_path(path)?;
        let sp = self.pkgs[f.pkg as usize].clone();
        match sp.objects.get(f.export as usize)? {
            Some(ScriptObject::Enum(e)) => Some(e.names.iter().map(|&n| sp.pkg.name(n as usize).to_string()).collect()),
            _ => None,
        }
    }
}

/// Parses an `.int`/`.ini` file (UTF-8, Latin-1 or UTF-16LE with BOM).
fn parse_ini(b: &[u8]) -> HashMap<(String, String), String> {
    let text = if b.starts_with(&[0xFF, 0xFE]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        match std::str::from_utf8(b) {
            Ok(s) => s.trim_start_matches('\u{feff}').to_string(),
            Err(_) => b.iter().map(|&c| c as char).collect(),
        }
    };
    let mut out = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') {
            section = l[1..l.len() - 1].to_ascii_lowercase();
        } else if let Some((k, v)) = l.split_once('=') {
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
            out.entry((section.clone(), k.trim().to_ascii_lowercase())).or_insert_with(|| v.to_string());
        }
    }
    out
}

fn def_cell<'a>(mut v: &'a mut Value, path: &[DefSeg]) -> Option<&'a mut Value> {
    for seg in path {
        v = match (seg, v) {
            (DefSeg::Idx(i), Value::Array(a)) => a.get_mut(*i)?,
            (DefSeg::Field(n), Value::Struct(f)) => &mut f.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(n))?.1,
            _ => return None,
        };
    }
    Some(v)
}

fn read_path(mut v: &Value, mut ty: Option<&Ty>, path: &[Seg]) -> Value {
    for seg in path {
        let next_ty = ty.and_then(|t| t.sub(seg));
        let zero = || next_ty.map(Ty::zero).unwrap_or(Value::Void);
        v = match (seg, v) {
            (Seg::Idx(i), Value::Array(a)) => match a.get(*i) {
                Some(x) => x,
                None => return zero(),
            },
            (Seg::Len, Value::Array(a)) => return Value::Int(a.len() as i32),
            (Seg::Len, _) => return Value::Int(0),
            (Seg::Field(n), Value::Vector(xyz)) => return Value::Float(vec_field(xyz, n)),
            (Seg::Field(n), Value::Rotator(r)) => return Value::Int(rot_field(r, n)),
            (Seg::Field(n), Value::Struct(f)) => match f.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
                Some((_, x)) => x,
                None => return Value::Void,
            },
            _ => return zero(),
        };
        ty = next_ty;
    }
    match (v, ty) {
        (Value::Void, Some(t)) => t.zero(),
        _ => v.clone(),
    }
}

fn write_path(mut v: &mut Value, mut ty: Option<&Ty>, path: &[Seg], val: Value) {
    for seg in path {
        if let (Value::Void, Some(t)) = (&*v, ty) {
            *v = t.zero();
        }
        let next_ty = ty.and_then(|t| t.sub(seg));
        v = match (seg, v) {
            (Seg::Idx(i), Value::Array(a)) => {
                if *i >= a.len() {
                    a.resize(*i + 1, next_ty.map(Ty::zero).unwrap_or(Value::Void));
                }
                &mut a[*i]
            }
            (Seg::Len, Value::Array(a)) => {
                let elem = ty.and_then(|t| t.sub(&Seg::Idx(0)));
                a.resize(val.as_int().max(0) as usize, elem.map(Ty::zero).unwrap_or(Value::Void));
                return;
            }
            (Seg::Field(n), Value::Vector(xyz)) => {
                if let Some(c) = vec_index(n) {
                    xyz[c] = val.as_float();
                }
                return;
            }
            (Seg::Field(n), Value::Rotator(r)) => {
                if let Some(c) = rot_index(n) {
                    r[c] = val.as_int();
                }
                return;
            }
            (Seg::Field(n), Value::Struct(f)) => match f.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
                Some((_, x)) => x,
                None => return,
            },
            _ => return,
        };
        ty = next_ty;
    }
    *v = val;
}

fn vec_index(n: &str) -> Option<usize> {
    match n.to_ascii_lowercase().as_str() {
        "x" => Some(0),
        "y" => Some(1),
        "z" => Some(2),
        _ => None,
    }
}

fn vec_field(v: &[f32; 3], n: &str) -> f32 {
    vec_index(n).map(|i| v[i]).unwrap_or(0.0)
}

fn rot_index(n: &str) -> Option<usize> {
    match n.to_ascii_lowercase().as_str() {
        "pitch" => Some(0),
        "yaw" => Some(1),
        "roll" => Some(2),
        _ => None,
    }
}

fn rot_field(r: &[i32; 3], n: &str) -> i32 {
    rot_index(n).map(|i| r[i]).unwrap_or(0)
}

/// Applies an EX_PrimitiveCast conversion.
pub fn cast(kind: u8, v: Value) -> Value {
    match kind {
        0x39 => {
            let r = v.as_rot();
            let (p, y) = (r[0] as f32 * std::f32::consts::PI / 32768.0, r[1] as f32 * std::f32::consts::PI / 32768.0);
            Value::Vector([p.cos() * y.cos(), p.cos() * y.sin(), p.sin()])
        }
        0x3A | 0x41 | 0x44 | 0x4A => Value::Int(match v {
            Value::Str(s) => parse_int(&s),
            v => v.as_int(),
        }),
        0x3B | 0x3E | 0x45 | 0x47 | 0x48 | 0x51 => Value::Bool(v.as_bool()),
        0x4F => Value::Bool(v.as_vec() != [0.0; 3]),
        0x4B => Value::Bool(match v {
            Value::Str(s) => s.eq_ignore_ascii_case("true") || parse_int(&s) != 0,
            _ => false,
        }),
        0x3C | 0x3F | 0x42 | 0x4C => Value::Float(match v {
            Value::Str(s) => s.trim().parse().unwrap_or(0.0),
            v => v.as_float(),
        }),
        0x3D | 0x40 | 0x43 | 0x49 => Value::Byte(match v {
            Value::Str(s) => parse_int(&s) as u8,
            v => v.as_int() as u8,
        }),
        0x4D => {
            let s = v.as_str();
            let mut it = s.split(',').map(|x| x.trim().parse::<f32>().unwrap_or(0.0));
            Value::Vector([it.next().unwrap_or(0.0), it.next().unwrap_or(0.0), it.next().unwrap_or(0.0)])
        }
        0x4E => {
            let s = v.as_str();
            let mut it = s.split(',').map(parse_int);
            Value::Rotator([it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0)])
        }
        0x50 => {
            let [x, y, z] = v.as_vec();
            let k = 32768.0 / std::f32::consts::PI;
            Value::Rotator([((z.atan2((x * x + y * y).sqrt())) * k) as i32 & 0xFFFF, (y.atan2(x) * k) as i32 & 0xFFFF, 0])
        }
        0x52..=0x59 => Value::Str(v.to_ustring()),
        _ => v,
    }
}

pub(crate) fn parse_int(s: &str) -> i32 {
    let s = s.trim();
    let end = s.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')))).map(|(i, _)| i).unwrap_or(s.len());
    s[..end].parse().unwrap_or(0)
}
