//! Actor collision flags from the original class defaults (via the uscript VM), cached per class.

use std::collections::HashMap;
use std::path::Path;

/// Collision-relevant defaults of an actor class.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClassFlags {
    pub collide_actors: bool,
    pub block_actors: bool,
    pub block_players: bool,
    pub block_zero_extent: bool,
    pub block_nonzero_extent: bool,
    pub use_cylinder: bool,
    pub collision_radius: f32,
    pub collision_height: f32,
    pub is_mover: bool,
    /// `bStatic`: false for actors script can move, hide or destroy (props, windows, KActors).
    pub is_static: bool,
    /// Class was resolved from script (false = fallback guess).
    pub from_script: bool,
}

impl ClassFlags {
    /// Engine defaults for `Actor` (used when no script is available) adjusted for static mesh
    /// actors, which every shipped map uses for blocking props.
    pub fn fallback(class: &str) -> ClassFlags {
        let sm = class.to_ascii_lowercase().contains("staticmeshactor");
        let mover = class.to_ascii_lowercase().contains("mover") || class.to_ascii_lowercase().contains("door");
        ClassFlags {
            collide_actors: sm || mover,
            block_actors: sm || mover,
            block_players: sm || mover,
            block_zero_extent: true,
            block_nonzero_extent: true,
            use_cylinder: false,
            collision_radius: 22.0,
            collision_height: 22.0,
            is_mover: mover,
            is_static: sm,
            from_script: false,
        }
    }
}

/// Size and movement defaults for the player pawn (Unreal units, seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PawnParams {
    pub collision_radius: f32,
    pub collision_height: f32,
    pub crouch_radius: f32,
    pub crouch_height: f32,
    pub base_eye_height: f32,
    pub ground_speed: f32,
    pub walking_pct: f32,
    pub crouched_pct: f32,
    pub accel_rate: f32,
    pub air_control: f32,
    pub jump_z: f32,
    pub gravity_z: f32,
    pub max_step_height: f32,
    /// Floors with normal Z below this are too steep to stand on.
    pub walkable_floor_z: f32,
    /// Where the values came from ("script PostalDude" or "built-in").
    pub source: &'static str,
}

impl Default for PawnParams {
    /// Values of the `PostalDude` class chain as shipped (FPSPawn / P2Pawn / Dude / Pawn defaults,
    /// PhysicsVolume gravity, Actor MAXSTEPHEIGHT), used when script packages are unavailable.
    fn default() -> Self {
        PawnParams {
            collision_radius: 28.0,
            collision_height: 72.0,
            crouch_radius: 25.0,
            crouch_height: 40.0,
            base_eye_height: 70.0,
            ground_speed: 450.0,
            walking_pct: 0.3,
            crouched_pct: 0.5,
            accel_rate: 2048.0,
            air_control: 0.05,
            jump_z: 550.0,
            gravity_z: -1500.0,
            max_step_height: 35.0,
            walkable_floor_z: 0.7,
            source: "built-in",
        }
    }
}

/// A class default property value (see [`ScriptDefaults::value`]).
#[derive(Clone, Debug, PartialEq)]
pub enum DefaultValue {
    Float(f32),
    Bool(bool),
    Byte(u8),
    Vector([f32; 3]),
    Rotator([i32; 3]),
    Name(String),
}

pub struct ScriptDefaults {
    #[cfg(feature = "script-defaults")]
    vm: Option<uscript::vm::Vm>,
    #[cfg(feature = "script-defaults")]
    class_ids: HashMap<String, Option<uscript::vm::ClassId>>,
    is_a_cache: HashMap<(String, String), bool>,
    cache: HashMap<String, ClassFlags>,
}

impl ScriptDefaults {
    pub fn new(game_dir: &Path) -> ScriptDefaults {
        #[cfg(feature = "script-defaults")]
        {
            let vm = uscript::vm::Vm::new(upkg::ci::join(game_dir, "System")).ok();
            ScriptDefaults { vm, class_ids: HashMap::new(), is_a_cache: HashMap::new(), cache: HashMap::new() }
        }
        #[cfg(not(feature = "script-defaults"))]
        {
            let _ = game_dir;
            ScriptDefaults { is_a_cache: HashMap::new(), cache: HashMap::new() }
        }
    }

    pub fn class_flags(&mut self, class: &str) -> ClassFlags {
        if let Some(f) = self.cache.get(class) {
            return *f;
        }
        let f = self.lookup(class).unwrap_or_else(|| ClassFlags::fallback(class));
        self.cache.insert(class.to_string(), f);
        f
    }

    #[cfg(feature = "script-defaults")]
    fn lookup(&mut self, class: &str) -> Option<ClassFlags> {
        let vm = self.vm.as_mut()?;
        let c = vm.find_class_by_name(class).ok()?;
        let b = |vm: &uscript::vm::Vm, n: &str| vm.default_value(c, n).map(|v| v.as_bool()).unwrap_or(false);
        let f = |vm: &uscript::vm::Vm, n: &str| vm.default_value(c, n).map(|v| v.as_float()).unwrap_or(0.0);
        Some(ClassFlags {
            collide_actors: b(vm, "bCollideActors"),
            block_actors: b(vm, "bBlockActors"),
            block_players: b(vm, "bBlockPlayers"),
            block_zero_extent: b(vm, "bBlockZeroExtentTraces"),
            block_nonzero_extent: b(vm, "bBlockNonZeroExtentTraces"),
            use_cylinder: b(vm, "bUseCylinderCollision"),
            collision_radius: f(vm, "CollisionRadius"),
            collision_height: f(vm, "CollisionHeight"),
            is_mover: vm.class_is_a(c, "Mover"),
            is_static: b(vm, "bStatic"),
            from_script: true,
        })
    }

    #[cfg(not(feature = "script-defaults"))]
    fn lookup(&mut self, _class: &str) -> Option<ClassFlags> {
        None
    }

    /// A class default as a plain value: `Some(Float/Bool/Byte/Vector/Name)` when the script
    /// packages are available and the class declares the property.
    pub fn value(&mut self, class: &str, prop: &str) -> Option<DefaultValue> {
        #[cfg(feature = "script-defaults")]
        {
            let vm = self.vm.as_mut()?;
            let c = (*self.class_ids.entry(class.to_ascii_lowercase()).or_insert_with(|| vm.find_class_by_name(class).ok()))?;
            use uscript::vm::Value as V;
            return match vm.default_value(c, prop)? {
                V::Float(f) => Some(DefaultValue::Float(*f)),
                V::Int(i) => Some(DefaultValue::Float(*i as f32)),
                V::Bool(b) => Some(DefaultValue::Bool(*b)),
                V::Byte(b) => Some(DefaultValue::Byte(*b)),
                V::Vector(v) => Some(DefaultValue::Vector(*v)),
                V::Rotator(r) => Some(DefaultValue::Rotator(*r)),
                V::Name(n) => Some(DefaultValue::Name(n.clone())),
                _ => None,
            };
        }
        #[cfg(not(feature = "script-defaults"))]
        {
            let _ = (class, prop);
            None
        }
    }

    /// True if `class` is `parent` or derives from it (script packages required; falls back to
    /// a case-insensitive name comparison). Cached per (class, parent).
    pub fn is_a(&mut self, class: &str, parent: &str) -> bool {
        if class.eq_ignore_ascii_case(parent) {
            return true;
        }
        let key = (class.to_ascii_lowercase(), parent.to_ascii_lowercase());
        if let Some(&b) = self.is_a_cache.get(&key) {
            return b;
        }
        #[allow(unused_mut)]
        let mut r = false;
        #[cfg(feature = "script-defaults")]
        if let Some(vm) = self.vm.as_mut() {
            if let Some(c) = self.class_ids.entry(key.0.clone()).or_insert_with(|| vm.find_class_by_name(class).ok()).as_ref() {
                r = vm.class_is_a(*c, parent);
            }
        }
        self.is_a_cache.insert(key, r);
        r
    }

    /// Player pawn parameters from the `PostalDude` class defaults.
    pub fn pawn_params(&mut self) -> PawnParams {
        #[cfg(feature = "script-defaults")]
        if let Some(vm) = self.vm.as_mut() {
            if let Ok(c) = vm.find_class("People", "PostalDude") {
                let d = PawnParams::default();
                let f = |n: &str, dv: f32| vm.default_value(c, n).map(|v| v.as_float()).filter(|v| *v != 0.0).unwrap_or(dv);
                let mut p = PawnParams {
                    collision_radius: f("CollisionRadius", d.collision_radius),
                    collision_height: f("CollisionHeight", d.collision_height),
                    crouch_radius: f("CrouchRadius", d.crouch_radius),
                    crouch_height: f("CrouchHeight", d.crouch_height),
                    base_eye_height: f("BaseEyeHeight", d.base_eye_height),
                    ground_speed: f("GroundSpeed", d.ground_speed),
                    walking_pct: f("WalkingPct", d.walking_pct),
                    crouched_pct: f("CrouchedPct", d.crouched_pct),
                    accel_rate: f("AccelRate", d.accel_rate),
                    air_control: f("AirControl", d.air_control),
                    jump_z: f("JumpZ", d.jump_z),
                    source: "script PostalDude",
                    ..d
                };
                if let Ok(pv) = vm.find_class("Engine", "PhysicsVolume") {
                    if let Some(g) = vm.default_value(pv, "Gravity").map(|v| v.as_vec()[2]).filter(|g| *g < 0.0) {
                        p.gravity_z = g;
                    }
                }
                return p;
            }
        }
        PawnParams::default()
    }
}
