//! Unreal Engine 2 particle systems as data: `Emitter` actors and their `ParticleEmitter`
//! sub-objects (SpriteEmitter, MeshEmitter, SparkEmitter, BeamEmitter, and POSTAL 2's BaseFX
//! SuperSpriteEmitter and StripEmitter).
//!
//! A ParticleEmitter is an ordinary UObject whose tagged properties are deltas against its class
//! defaults. Nested struct values (`Range`, `RangeVector`, `Plane`, scale keys) are tagged lists
//! whose missing fields are zero: the editor writes every non-zero field, even ones equal to the
//! class default (e.g. `StartSizeRange.Z=(100,100)` is stored although 100 is the default).
//!
//! Usage: start from [`ParticleEmitter::new`] (engine defaults for the subclass), optionally
//! [`ParticleEmitter::apply`] the class-default property lists from the script packages
//! (root class first), then apply the sub-object's own properties.

use crate::package::{ObjRef, Package};
use crate::props::{Properties, Value, read_properties};
use crate::reader::Reader;
use crate::{Result, begin_object};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Range {
    pub min: f32,
    pub max: f32,
}

impl Range {
    pub const fn new(min: f32, max: f32) -> Self {
        Self { min, max }
    }
    pub const fn splat(v: f32) -> Self {
        Self { min: v, max: v }
    }
    /// Value for a uniform random `t` in [0, 1).
    pub fn lerp(&self, t: f32) -> f32 {
        self.min + (self.max - self.min) * t
    }
    pub fn center(&self) -> f32 {
        (self.min + self.max) * 0.5
    }
    pub fn size(&self) -> f32 {
        self.max - self.min
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RangeVector {
    pub x: Range,
    pub y: Range,
    pub z: Range,
}

impl RangeVector {
    pub const fn splat(v: f32) -> Self {
        Self { x: Range::splat(v), y: Range::splat(v), z: Range::splat(v) }
    }
    pub fn axes(&self) -> [Range; 3] {
        [self.x, self.y, self.z]
    }
    pub fn max_abs(&self) -> f32 {
        self.axes().iter().map(|r| r.min.abs().max(r.max.abs())).fold(0.0, f32::max)
    }
}

/// `Plane` struct (X, Y, Z, W). Used for fade factors (as R, G, B, A multipliers) and collision planes.
pub type Plane = [f32; 4];

/// A colour key of `ColorScale`. `color` is stored in R, G, B, A order (the file stores B, G, R, A).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ColorKey {
    pub time: f32,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SizeKey {
    pub time: f32,
    pub size: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParticleSound {
    pub sound: ObjRef,
    pub radius: Range,
    pub pitch: Range,
    pub weight: i32,
    pub volume: Range,
    pub probability: Range,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeamEndPoint {
    pub actor_tag: String,
    pub offset: RangeVector,
    pub weight: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeamScale {
    pub frequency_scale: [f32; 3],
    pub relative_length: f32,
}

/// Which ParticleEmitter subclass an object is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EmitterKind {
    Sprite,
    /// BaseFX.SuperSpriteEmitter: a SpriteEmitter with line/circle spawn shapes.
    SuperSprite,
    Mesh,
    Spark,
    Beam,
    /// BaseFX.StripEmitter: particles joined into one camera-facing ribbon.
    Strip,
    Other(String),
}

impl EmitterKind {
    pub fn from_class(class: &str) -> Self {
        match class.to_ascii_lowercase().as_str() {
            "spriteemitter" => Self::Sprite,
            "superspriteemitter" => Self::SuperSprite,
            "meshemitter" => Self::Mesh,
            "sparkemitter" => Self::Spark,
            "beamemitter" => Self::Beam,
            "stripemitter" => Self::Strip,
            _ => Self::Other(class.to_string()),
        }
    }

    pub fn is_particle_emitter_class(class: &str) -> bool {
        !matches!(Self::from_class(class), Self::Other(_))
    }

    pub fn class_name(&self) -> &str {
        match self {
            Self::Sprite => "SpriteEmitter",
            Self::SuperSprite => "SuperSpriteEmitter",
            Self::Mesh => "MeshEmitter",
            Self::Spark => "SparkEmitter",
            Self::Beam => "BeamEmitter",
            Self::Strip => "StripEmitter",
            Self::Other(s) => s,
        }
    }

    /// Script class chain from root to leaf as (package, class), for applying class defaults.
    pub fn class_chain(&self) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![("Engine", "ParticleEmitter")];
        match self {
            Self::Sprite => v.push(("Engine", "SpriteEmitter")),
            Self::SuperSprite => v.extend([("Engine", "SpriteEmitter"), ("BaseFX", "SuperSpriteEmitter")]),
            Self::Mesh => v.push(("Engine", "MeshEmitter")),
            Self::Spark => v.push(("Engine", "SparkEmitter")),
            Self::Beam => v.push(("Engine", "BeamEmitter")),
            Self::Strip => v.push(("BaseFX", "StripEmitter")),
            Self::Other(_) => {}
        }
        v
    }
}

/// `EParticleDrawStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DrawStyle {
    Regular,
    AlphaBlend,
    Modulated,
    #[default]
    Translucent,
    AlphaModulate,
    Darken,
    Brighten,
}

impl DrawStyle {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Regular,
            1 => Self::AlphaBlend,
            2 => Self::Modulated,
            3 => Self::Translucent,
            4 => Self::AlphaModulate,
            5 => Self::Darken,
            _ => Self::Brighten,
        }
    }
}

/// Every editable ParticleEmitter property plus the subclass-specific ones. Field names follow
/// the UnrealScript names in snake case; enums are kept as their byte values where the meaning is
/// documented on the field.
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleEmitter {
    pub kind: EmitterKind,
    /// Object name (e.g. "SpriteEmitter12"); `name` is the designer-set `Name` string property.
    pub object_name: String,

    // Acceleration
    pub acceleration: [f32; 3],
    // Collision
    pub use_collision: bool,
    pub extent_multiplier: [f32; 3],
    pub damping_factor_range: RangeVector,
    pub use_collision_planes: bool,
    pub collision_planes: Vec<Plane>,
    pub use_max_collisions: bool,
    pub max_collisions: Range,
    pub spawn_from_other_emitter: i32,
    pub spawn_amount: i32,
    pub spawned_velocity_scale_range: RangeVector,
    pub use_spawned_velocity_scale: bool,
    /// EParticleCollisionSound: 0 none, 1 linear global, 2 linear local, 3 random.
    pub collision_sound: u8,
    pub collision_sound_index: Range,
    pub collision_sound_probability: Range,
    // Sound
    pub sounds: Vec<ParticleSound>,
    // Color
    pub use_color_scale: bool,
    pub color_scale: Vec<ColorKey>,
    pub color_scale_repeats: f32,
    pub color_multiplier_range: RangeVector,
    // Fading
    pub fade_out_factor: Plane,
    pub fade_out_start_time: f32,
    pub fade_out: bool,
    pub fade_in_factor: Plane,
    pub fade_in_end_time: f32,
    pub fade_in: bool,
    // Force
    pub use_actor_forces: bool,
    // General
    /// EParticleCoordinateSystem: 0 independent (world), 1 relative (owner space), 2 absolute.
    pub coordinate_system: u8,
    pub max_particles: i32,
    pub reset_after_change: bool,
    /// EParticleEffectAxis: 0 negative X, 1 positive Z.
    pub effect_axis: u8,
    // Local
    pub respawn_dead_particles: bool,
    pub auto_destroy: bool,
    pub auto_reset: bool,
    pub disabled: bool,
    pub disable_fogging: bool,
    pub auto_reset_time_range: Range,
    pub name: String,
    // Location
    pub start_location_offset: [f32; 3],
    pub start_location_range: RangeVector,
    pub add_location_from_other_emitter: i32,
    /// EParticleStartLocationShape: 0 box, 1 sphere, 2 polar.
    pub start_location_shape: u8,
    pub sphere_radius_range: Range,
    pub start_location_polar_range: RangeVector,
    // Mass
    pub start_mass_range: Range,
    // Rendering
    pub alpha_ref: i32,
    pub alpha_test: bool,
    pub accepts_projectors: bool,
    pub z_test: bool,
    pub z_write: bool,
    // Rotation
    /// EParticleRotationSource: 0 none, 1 actor, 2 offset, 3 normal.
    pub use_rotation_from: u8,
    pub spin_particles: bool,
    pub rotation_offset: [i32; 3],
    pub spin_ccw_or_cw: [f32; 3],
    pub spins_per_second_range: RangeVector,
    pub start_spin_range: RangeVector,
    pub damp_rotation: bool,
    pub rotation_damping_factor_range: RangeVector,
    pub rotation_normal: [f32; 3],
    // Size
    pub use_size_scale: bool,
    pub use_regular_size_scale: bool,
    pub size_scale: Vec<SizeKey>,
    pub size_scale_repeats: f32,
    pub start_size_range: RangeVector,
    pub uniform_size: bool,
    // Spawning
    pub particles_per_second: f32,
    pub initial_particles_per_second: f32,
    pub automatic_initial_spawning: bool,
    // Texture
    pub draw_style: DrawStyle,
    pub texture: ObjRef,
    pub texture_u_subdivisions: i32,
    pub texture_v_subdivisions: i32,
    pub blend_between_subdivisions: bool,
    pub use_subdivision_scale: bool,
    pub subdivision_scale: Vec<f32>,
    pub subdivision_start: i32,
    pub subdivision_end: i32,
    pub use_random_subdivision: bool,
    // Tick
    pub seconds_before_inactive: f32,
    pub min_squared_velocity: f32,
    // Time
    pub initial_time_range: Range,
    pub lifetime_range: Range,
    pub initial_delay_range: Range,
    // Velocity
    pub start_velocity_range: RangeVector,
    pub start_velocity_radial_range: Range,
    pub max_abs_velocity: [f32; 3],
    pub velocity_loss_range: RangeVector,
    pub add_velocity_from_other_emitter: i32,
    pub add_velocity_multiplier_range: RangeVector,
    /// EParticleVelocityDirection: 0 none, 1 start position and owner, 2 owner and start position, 3 add radial.
    pub get_velocity_direction_from: u8,
    // Warmup
    pub warmup_ticks_per_second: f32,
    pub relative_warmup_time: f32,

    // SpriteEmitter / StripEmitter
    /// EParticleDirectionUsage (sprite): 0 none (camera facing), 1 up, 2 right, 3 forward,
    /// 4 normal, 5 up and normal, 6 right and normal. StripEmitter: 0 none, 1 normal.
    pub use_direction_as: u8,
    pub projection_normal: [f32; 3],
    // MeshEmitter
    pub static_mesh: ObjRef,
    pub use_mesh_blend_mode: bool,
    pub render_two_sided: bool,
    pub use_particle_color: bool,
    // SparkEmitter
    pub line_segments_range: Range,
    pub time_before_visible_range: Range,
    pub time_between_segments_range: Range,
    // BeamEmitter
    pub beam_distance_range: Range,
    pub beam_end_points: Vec<BeamEndPoint>,
    /// EBeamEndPointType: 0 velocity, 1 distance, 2 offset, 3 actor, 4 trace offset, 5 offset as absolute.
    pub determine_end_point_by: u8,
    pub beam_texture_u_scale: f32,
    pub beam_texture_v_scale: f32,
    pub rotating_sheets: i32,
    pub low_frequency_noise_range: RangeVector,
    pub low_frequency_points: i32,
    pub high_frequency_noise_range: RangeVector,
    pub high_frequency_points: i32,
    pub lf_scale_factors: Vec<BeamScale>,
    pub hf_scale_factors: Vec<BeamScale>,
    pub lf_scale_repeats: f32,
    pub hf_scale_repeats: f32,
    pub use_high_frequency_scale: bool,
    pub use_low_frequency_scale: bool,
    pub noise_determines_end_point: bool,
    pub use_branching: bool,
    pub branch_probability: Range,
    pub branch_emitter: i32,
    pub branch_spawn_amount_range: Range,
    pub linkup_lifetime: bool,
    // SuperSpriteEmitter (StripEmitter also has LineEnd and AllowParticleSpawn)
    pub line_start: [f32; 3],
    pub line_end: [f32; 3],
    /// EParticleStartLocationShapeExtend: 0 none, 1 line, 2 circle.
    pub location_shape_extend: u8,
    /// EParticleCoordinateSystemExtend: 0 none, 1 relative location.
    pub coordinate_system_extend: u8,
    pub allow_particle_spawn: bool,

    /// Property names that were present but are not modelled (for coverage reporting).
    pub unknown: Vec<String>,
}

impl ParticleEmitter {
    /// Engine defaults for the given subclass (the `defaultproperties` of ParticleEmitter and the
    /// subclass as shipped in Engine.u / BaseFX.u).
    pub fn new(kind: EmitterKind) -> Self {
        let mut e = Self {
            kind: kind.clone(),
            object_name: String::new(),
            acceleration: [0.0; 3],
            use_collision: false,
            extent_multiplier: [1.0; 3],
            damping_factor_range: RangeVector::splat(1.0),
            use_collision_planes: false,
            collision_planes: vec![],
            use_max_collisions: false,
            max_collisions: Range::default(),
            spawn_from_other_emitter: -1,
            spawn_amount: 0,
            spawned_velocity_scale_range: RangeVector::default(),
            use_spawned_velocity_scale: false,
            collision_sound: 0,
            collision_sound_index: Range::default(),
            collision_sound_probability: Range::default(),
            sounds: vec![],
            use_color_scale: false,
            color_scale: vec![],
            color_scale_repeats: 0.0,
            color_multiplier_range: RangeVector::splat(1.0),
            fade_out_factor: [1.0; 4],
            fade_out_start_time: 0.0,
            fade_out: false,
            fade_in_factor: [1.0; 4],
            fade_in_end_time: 0.0,
            fade_in: false,
            use_actor_forces: false,
            coordinate_system: 0,
            max_particles: 10,
            reset_after_change: false,
            effect_axis: 0,
            respawn_dead_particles: true,
            auto_destroy: false,
            auto_reset: false,
            disabled: false,
            disable_fogging: false,
            auto_reset_time_range: Range::default(),
            name: String::new(),
            start_location_offset: [0.0; 3],
            start_location_range: RangeVector::default(),
            add_location_from_other_emitter: -1,
            start_location_shape: 0,
            sphere_radius_range: Range::default(),
            start_location_polar_range: RangeVector::default(),
            start_mass_range: Range::splat(1.0),
            alpha_ref: 0,
            alpha_test: true,
            accepts_projectors: false,
            z_test: true,
            z_write: false,
            use_rotation_from: 0,
            spin_particles: false,
            rotation_offset: [0; 3],
            spin_ccw_or_cw: [0.5; 3],
            spins_per_second_range: RangeVector::default(),
            start_spin_range: RangeVector::default(),
            damp_rotation: false,
            rotation_damping_factor_range: RangeVector::default(),
            rotation_normal: [0.0; 3],
            use_size_scale: false,
            use_regular_size_scale: true,
            size_scale: vec![],
            size_scale_repeats: 0.0,
            start_size_range: RangeVector::splat(100.0),
            uniform_size: false,
            particles_per_second: 0.0,
            initial_particles_per_second: 0.0,
            automatic_initial_spawning: true,
            draw_style: DrawStyle::Translucent,
            texture: ObjRef::Null,
            texture_u_subdivisions: 0,
            texture_v_subdivisions: 0,
            blend_between_subdivisions: false,
            use_subdivision_scale: false,
            subdivision_scale: vec![],
            subdivision_start: 0,
            subdivision_end: 0,
            use_random_subdivision: false,
            seconds_before_inactive: 1.0,
            min_squared_velocity: 0.0,
            initial_time_range: Range::default(),
            lifetime_range: Range::splat(4.0),
            initial_delay_range: Range::default(),
            start_velocity_range: RangeVector::default(),
            start_velocity_radial_range: Range::default(),
            max_abs_velocity: [0.0; 3],
            velocity_loss_range: RangeVector::default(),
            add_velocity_from_other_emitter: -1,
            add_velocity_multiplier_range: RangeVector::splat(1.0),
            get_velocity_direction_from: 0,
            warmup_ticks_per_second: 0.0,
            relative_warmup_time: 0.0,
            use_direction_as: 0,
            projection_normal: [0.0, 0.0, 1.0],
            static_mesh: ObjRef::Null,
            use_mesh_blend_mode: true,
            render_two_sided: false,
            use_particle_color: false,
            line_segments_range: Range::splat(5.0),
            time_before_visible_range: Range::default(),
            time_between_segments_range: Range::default(),
            beam_distance_range: Range::default(),
            beam_end_points: vec![],
            determine_end_point_by: 0,
            beam_texture_u_scale: 1.0,
            beam_texture_v_scale: 1.0,
            rotating_sheets: 0,
            low_frequency_noise_range: RangeVector::default(),
            low_frequency_points: 3,
            high_frequency_noise_range: RangeVector::default(),
            high_frequency_points: 10,
            lf_scale_factors: vec![],
            hf_scale_factors: vec![],
            lf_scale_repeats: 0.0,
            hf_scale_repeats: 0.0,
            use_high_frequency_scale: false,
            use_low_frequency_scale: false,
            noise_determines_end_point: false,
            use_branching: false,
            branch_probability: Range::default(),
            branch_emitter: -1,
            branch_spawn_amount_range: Range::default(),
            linkup_lifetime: false,
            line_start: [0.0; 3],
            line_end: [0.0; 3],
            location_shape_extend: 0,
            coordinate_system_extend: 0,
            allow_particle_spawn: true,
            unknown: vec![],
        };
        if kind == EmitterKind::Mesh {
            e.start_size_range = RangeVector::splat(1.0);
        }
        e
    }

    /// Reads a ParticleEmitter export, starting from the built-in engine defaults.
    pub fn read(pkg: &Package, export: usize) -> Result<Self> {
        let kind = EmitterKind::from_class(pkg.export_class(export));
        let (_, props) = begin_object(pkg, export)?;
        let mut e = Self::new(kind);
        e.object_name = pkg.name(pkg.exports[export].name).to_string();
        e.apply(pkg, &props);
        Ok(e)
    }

    /// Overlays a tagged property list (class defaults or an object's own deltas). Name and object
    /// references resolve against `pkg`, the package the list was read from.
    pub fn apply(&mut self, pkg: &Package, props: &Properties) {
        for p in &props.0 {
            let v = &p.value;
            let known = self.apply_one(pkg, &p.name.to_ascii_lowercase(), p.index, v);
            if !known && !self.unknown.iter().any(|u| u.eq_ignore_ascii_case(&p.name)) {
                self.unknown.push(p.name.clone());
            }
        }
    }

    fn apply_one(&mut self, pkg: &Package, name: &str, index: u32, v: &Value) -> bool {
        macro_rules! set {
            ($field:expr, $conv:expr) => {{
                if let Some(x) = $conv {
                    $field = x;
                }
                true
            }};
        }
        let _ = index;
        match name {
            "acceleration" => set!(self.acceleration, vec3(v)),
            "usecollision" => set!(self.use_collision, boolean(v)),
            "extentmultiplier" => set!(self.extent_multiplier, vec3(v)),
            "dampingfactorrange" => set!(self.damping_factor_range, range_vector(pkg, v)),
            "usecollisionplanes" => set!(self.use_collision_planes, boolean(v)),
            "collisionplanes" => set!(self.collision_planes, plane_array(pkg, v)),
            "usemaxcollisions" => set!(self.use_max_collisions, boolean(v)),
            "maxcollisions" => set!(self.max_collisions, range(pkg, v)),
            "spawnfromotheremitter" => set!(self.spawn_from_other_emitter, int(v)),
            "spawnamount" => set!(self.spawn_amount, int(v)),
            "spawnedvelocityscalerange" => set!(self.spawned_velocity_scale_range, range_vector(pkg, v)),
            "usespawnedvelocityscale" => set!(self.use_spawned_velocity_scale, boolean(v)),
            "collisionsound" => set!(self.collision_sound, byte(v)),
            "collisionsoundindex" => set!(self.collision_sound_index, range(pkg, v)),
            "collisionsoundprobability" => set!(self.collision_sound_probability, range(pkg, v)),
            "sounds" => set!(self.sounds, struct_array(pkg, v, |p| particle_sound(pkg, p))),
            "usecolorscale" => set!(self.use_color_scale, boolean(v)),
            "colorscale" => set!(self.color_scale, struct_array(pkg, v, |p| ColorKey { time: pf(p, "RelativeTime"), color: pcolor(p, "Color") })),
            "colorscalerepeats" => set!(self.color_scale_repeats, float(v)),
            "colormultiplierrange" => set!(self.color_multiplier_range, range_vector(pkg, v)),
            "fadeoutfactor" => set!(self.fade_out_factor, plane(pkg, v)),
            "fadeoutstarttime" => set!(self.fade_out_start_time, float(v)),
            "fadeout" => set!(self.fade_out, boolean(v)),
            "fadeinfactor" => set!(self.fade_in_factor, plane(pkg, v)),
            "fadeinendtime" => set!(self.fade_in_end_time, float(v)),
            "fadein" => set!(self.fade_in, boolean(v)),
            "useactorforces" => set!(self.use_actor_forces, boolean(v)),
            "coordinatesystem" => set!(self.coordinate_system, byte(v)),
            "maxparticles" => set!(self.max_particles, int(v)),
            "resetafterchange" => set!(self.reset_after_change, boolean(v)),
            "effectaxis" => set!(self.effect_axis, byte(v)),
            "respawndeadparticles" => set!(self.respawn_dead_particles, boolean(v)),
            "autodestroy" => set!(self.auto_destroy, boolean(v)),
            "autoreset" => set!(self.auto_reset, boolean(v)),
            "disabled" => set!(self.disabled, boolean(v)),
            "disablefogging" => set!(self.disable_fogging, boolean(v)),
            "autoresettimerange" => set!(self.auto_reset_time_range, range(pkg, v)),
            "name" => set!(self.name, string(v)),
            "startlocationoffset" => set!(self.start_location_offset, vec3(v)),
            "startlocationrange" => set!(self.start_location_range, range_vector(pkg, v)),
            "addlocationfromotheremitter" => set!(self.add_location_from_other_emitter, int(v)),
            "startlocationshape" => set!(self.start_location_shape, byte(v)),
            "sphereradiusrange" => set!(self.sphere_radius_range, range(pkg, v)),
            "startlocationpolarrange" => set!(self.start_location_polar_range, range_vector(pkg, v)),
            "startmassrange" => set!(self.start_mass_range, range(pkg, v)),
            "alpharef" => set!(self.alpha_ref, int(v)),
            "alphatest" => set!(self.alpha_test, boolean(v)),
            "acceptsprojectors" => set!(self.accepts_projectors, boolean(v)),
            "ztest" => set!(self.z_test, boolean(v)),
            "zwrite" => set!(self.z_write, boolean(v)),
            "userotationfrom" => set!(self.use_rotation_from, byte(v)),
            "spinparticles" => set!(self.spin_particles, boolean(v)),
            "rotationoffset" => set!(self.rotation_offset, rotator(v)),
            "spinccworcw" => set!(self.spin_ccw_or_cw, vec3(v)),
            "spinspersecondrange" => set!(self.spins_per_second_range, range_vector(pkg, v)),
            "startspinrange" => set!(self.start_spin_range, range_vector(pkg, v)),
            "damprotation" => set!(self.damp_rotation, boolean(v)),
            "rotationdampingfactorrange" => set!(self.rotation_damping_factor_range, range_vector(pkg, v)),
            "rotationnormal" => set!(self.rotation_normal, vec3(v)),
            "usesizescale" => set!(self.use_size_scale, boolean(v)),
            "useregularsizescale" => set!(self.use_regular_size_scale, boolean(v)),
            "sizescale" => set!(self.size_scale, struct_array(pkg, v, |p| SizeKey { time: pf(p, "RelativeTime"), size: pf(p, "RelativeSize") })),
            "sizescalerepeats" => set!(self.size_scale_repeats, float(v)),
            "startsizerange" => set!(self.start_size_range, range_vector(pkg, v)),
            "uniformsize" => set!(self.uniform_size, boolean(v)),
            "particlespersecond" => set!(self.particles_per_second, float(v)),
            "initialparticlespersecond" => set!(self.initial_particles_per_second, float(v)),
            "automaticinitialspawning" => set!(self.automatic_initial_spawning, boolean(v)),
            "drawstyle" => set!(self.draw_style, byte(v).map(DrawStyle::from_byte)),
            "texture" => set!(self.texture, object(v)),
            "textureusubdivisions" => set!(self.texture_u_subdivisions, int(v)),
            "texturevsubdivisions" => set!(self.texture_v_subdivisions, int(v)),
            "blendbetweensubdivisions" => set!(self.blend_between_subdivisions, boolean(v)),
            "usesubdivisionscale" => set!(self.use_subdivision_scale, boolean(v)),
            "subdivisionscale" => set!(self.subdivision_scale, float_array(v)),
            "subdivisionstart" => set!(self.subdivision_start, int(v)),
            "subdivisionend" => set!(self.subdivision_end, int(v)),
            "userandomsubdivision" => set!(self.use_random_subdivision, boolean(v)),
            "secondsbeforeinactive" => set!(self.seconds_before_inactive, float(v)),
            "minsquaredvelocity" => set!(self.min_squared_velocity, float(v)),
            "initialtimerange" => set!(self.initial_time_range, range(pkg, v)),
            "lifetimerange" => set!(self.lifetime_range, range(pkg, v)),
            "initialdelayrange" => set!(self.initial_delay_range, range(pkg, v)),
            "startvelocityrange" => set!(self.start_velocity_range, range_vector(pkg, v)),
            "startvelocityradialrange" => set!(self.start_velocity_radial_range, range(pkg, v)),
            "maxabsvelocity" => set!(self.max_abs_velocity, vec3(v)),
            "velocitylossrange" => set!(self.velocity_loss_range, range_vector(pkg, v)),
            "addvelocityfromotheremitter" => set!(self.add_velocity_from_other_emitter, int(v)),
            "addvelocitymultiplierrange" => set!(self.add_velocity_multiplier_range, range_vector(pkg, v)),
            "getvelocitydirectionfrom" => set!(self.get_velocity_direction_from, byte(v)),
            "warmuptickspersecond" => set!(self.warmup_ticks_per_second, float(v)),
            "relativewarmuptime" => set!(self.relative_warmup_time, float(v)),
            "usedirectionas" => set!(self.use_direction_as, byte(v)),
            "projectionnormal" => set!(self.projection_normal, vec3(v)),
            "staticmesh" => set!(self.static_mesh, object(v)),
            "usemeshblendmode" => set!(self.use_mesh_blend_mode, boolean(v)),
            "rendertwosided" => set!(self.render_two_sided, boolean(v)),
            "useparticlecolor" => set!(self.use_particle_color, boolean(v)),
            "linesegmentsrange" => set!(self.line_segments_range, range(pkg, v)),
            "timebeforevisiblerange" => set!(self.time_before_visible_range, range(pkg, v)),
            "timebetweensegmentsrange" => set!(self.time_between_segments_range, range(pkg, v)),
            "beamdistancerange" => set!(self.beam_distance_range, range(pkg, v)),
            "beamendpoints" => set!(
                self.beam_end_points,
                struct_array(pkg, v, |p| BeamEndPoint {
                    actor_tag: match p.get("ActorTag") {
                        Some(Value::Name(n)) => n.clone(),
                        _ => String::new(),
                    },
                    offset: p.get("Offset").and_then(|x| range_vector(pkg, x)).unwrap_or_default(),
                    weight: pf(p, "Weight"),
                })
            ),
            "determineendpointby" => set!(self.determine_end_point_by, byte(v)),
            "beamtextureuscale" => set!(self.beam_texture_u_scale, float(v)),
            "beamtexturevscale" => set!(self.beam_texture_v_scale, float(v)),
            "rotatingsheets" => set!(self.rotating_sheets, int(v)),
            "lowfrequencynoiserange" => set!(self.low_frequency_noise_range, range_vector(pkg, v)),
            "lowfrequencypoints" => set!(self.low_frequency_points, int(v)),
            "highfrequencynoiserange" => set!(self.high_frequency_noise_range, range_vector(pkg, v)),
            "highfrequencypoints" => set!(self.high_frequency_points, int(v)),
            "lfscalefactors" => set!(self.lf_scale_factors, struct_array(pkg, v, beam_scale)),
            "hfscalefactors" => set!(self.hf_scale_factors, struct_array(pkg, v, beam_scale)),
            "lfscalerepeats" => set!(self.lf_scale_repeats, float(v)),
            "hfscalerepeats" => set!(self.hf_scale_repeats, float(v)),
            "usehighfrequencyscale" => set!(self.use_high_frequency_scale, boolean(v)),
            "uselowfrequencyscale" => set!(self.use_low_frequency_scale, boolean(v)),
            "noisedeterminesendpoint" => set!(self.noise_determines_end_point, boolean(v)),
            "usebranching" => set!(self.use_branching, boolean(v)),
            "branchprobability" => set!(self.branch_probability, range(pkg, v)),
            "branchemitter" => set!(self.branch_emitter, int(v)),
            "branchspawnamountrange" => set!(self.branch_spawn_amount_range, range(pkg, v)),
            "linkuplifetime" => set!(self.linkup_lifetime, boolean(v)),
            "linestart" => set!(self.line_start, vec3(v)),
            "lineend" => set!(self.line_end, vec3(v)),
            "locationshapeextend" => set!(self.location_shape_extend, byte(v)),
            "coordinatesystemextend" => set!(self.coordinate_system_extend, byte(v)),
            "allowparticlespawn" => set!(self.allow_particle_spawn, boolean(v)),
            _ => false,
        }
    }

    /// Number of texture sub-images (`TextureUSubdivisions * TextureVSubdivisions`, at least 1).
    pub fn subdivisions(&self) -> i32 {
        (self.texture_u_subdivisions.max(0) * self.texture_v_subdivisions.max(0)).max(1)
    }
}

/// Emitter-actor level properties (from `Emitter` plus the relevant `Actor` fields).
#[derive(Debug, Clone, PartialEq)]
pub struct EmitterActor {
    pub class: String,
    pub object_name: String,
    /// References to the ParticleEmitter sub-objects, relative to the package the list came from.
    pub emitters: Vec<ObjRef>,
    pub auto_destroy: bool,
    pub auto_reset: bool,
    pub disable_fogging: bool,
    pub global_offset_range: RangeVector,
    pub time_till_reset_range: Range,
    pub location: [f32; 3],
    pub rotation: [i32; 3],
    pub draw_scale: f32,
    pub draw_scale3d: [f32; 3],
    pub life_span: f32,
    pub hidden: bool,
    pub tag: String,
}

impl Default for EmitterActor {
    fn default() -> Self {
        Self {
            class: "Emitter".into(),
            object_name: String::new(),
            emitters: vec![],
            auto_destroy: false,
            auto_reset: false,
            disable_fogging: false,
            global_offset_range: RangeVector::default(),
            time_till_reset_range: Range::default(),
            location: [0.0; 3],
            rotation: [0; 3],
            draw_scale: 1.0,
            draw_scale3d: [1.0; 3],
            life_span: 0.0,
            hidden: false,
            tag: String::new(),
        }
    }
}

impl EmitterActor {
    /// Overlays a property list (a class's defaults or a placed actor's properties). Returns true
    /// if the list defined `Emitters`.
    pub fn apply(&mut self, pkg: &Package, props: &Properties) -> bool {
        let mut had_emitters = false;
        for p in &props.0 {
            let v = &p.value;
            match p.name.to_ascii_lowercase().as_str() {
                "emitters" => {
                    if let Some(list) = object_array(v) {
                        self.emitters = list;
                        had_emitters = true;
                    }
                }
                "autodestroy" => self.auto_destroy = boolean(v).unwrap_or(self.auto_destroy),
                "autoreset" => self.auto_reset = boolean(v).unwrap_or(self.auto_reset),
                "disablefogging" => self.disable_fogging = boolean(v).unwrap_or(self.disable_fogging),
                "globaloffsetrange" => self.global_offset_range = range_vector(pkg, v).unwrap_or(self.global_offset_range),
                "timetillresetrange" => self.time_till_reset_range = range(pkg, v).unwrap_or(self.time_till_reset_range),
                "location" => self.location = vec3(v).unwrap_or(self.location),
                "rotation" => self.rotation = rotator(v).unwrap_or(self.rotation),
                "drawscale" => self.draw_scale = float(v).unwrap_or(self.draw_scale),
                "drawscale3d" => self.draw_scale3d = vec3(v).unwrap_or(self.draw_scale3d),
                "lifespan" => self.life_span = float(v).unwrap_or(self.life_span),
                "bhidden" => self.hidden = boolean(v).unwrap_or(self.hidden),
                "tag" => {
                    if let Value::Name(n) = v {
                        self.tag = n.clone()
                    }
                }
                _ => {}
            }
        }
        had_emitters
    }

    /// Reads a placed emitter actor's own properties (class defaults are not applied).
    pub fn read(pkg: &Package, export: usize) -> Result<Self> {
        let (_, props) = begin_object(pkg, export)?;
        let mut a = Self { class: pkg.export_class(export).to_string(), object_name: pkg.name(pkg.exports[export].name).to_string(), ..Default::default() };
        a.apply(pkg, &props);
        Ok(a)
    }
}

// ---- value helpers ----

fn nested(pkg: &Package, raw: &[u8]) -> Option<Properties> {
    let mut r = Reader::new(raw);
    read_properties(&mut r, pkg).ok()
}

fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Byte(b) => Some(*b != 0),
        _ => None,
    }
}

fn byte(v: &Value) -> Option<u8> {
    match v {
        Value::Byte(b) => Some(*b),
        Value::Int(i) => Some(*i as u8),
        _ => None,
    }
}

fn int(v: &Value) -> Option<i32> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Byte(b) => Some(*b as i32),
        _ => None,
    }
}

fn float(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Int(i) => Some(*i as f32),
        _ => None,
    }
}

fn string(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) | Value::Name(s) => Some(s.clone()),
        _ => None,
    }
}

fn object(v: &Value) -> Option<ObjRef> {
    match v {
        Value::Object(o) => Some(*o),
        _ => None,
    }
}

fn vec3(v: &Value) -> Option<[f32; 3]> {
    match v {
        Value::Vector(x) => Some(*x),
        Value::Struct { raw, .. } if raw.len() == 12 => {
            let mut r = Reader::new(raw);
            r.vec3().ok()
        }
        _ => None,
    }
}

fn rotator(v: &Value) -> Option<[i32; 3]> {
    match v {
        Value::Rotator(x) => Some(*x),
        _ => None,
    }
}

/// Float field of a nested tagged list (missing = 0).
fn pf(p: &Properties, name: &str) -> f32 {
    p.get(name).and_then(float).unwrap_or(0.0)
}

/// Colour field of a nested tagged list, converted from stored B, G, R, A to R, G, B, A.
fn pcolor(p: &Properties, name: &str) -> [u8; 4] {
    match p.get(name) {
        Some(Value::Color(c)) => [c[2], c[1], c[0], c[3]],
        _ => [0; 4],
    }
}

fn range_from(p: &Properties) -> Range {
    Range { min: pf(p, "Min"), max: pf(p, "Max") }
}

fn range(pkg: &Package, v: &Value) -> Option<Range> {
    match v {
        Value::Struct { raw, .. } => {
            // Binary form (two floats) or a tagged list.
            if raw.len() == 8 {
                let mut r = Reader::new(raw);
                return Some(Range { min: r.f32().ok()?, max: r.f32().ok()? });
            }
            nested(pkg, raw).map(|p| range_from(&p))
        }
        _ => None,
    }
}

fn range_vector(pkg: &Package, v: &Value) -> Option<RangeVector> {
    match v {
        Value::Struct { raw, .. } => {
            if raw.len() == 24 {
                let mut r = Reader::new(raw);
                let mut f = [0.0f32; 6];
                for x in &mut f {
                    *x = r.f32().ok()?;
                }
                return Some(RangeVector { x: Range::new(f[0], f[1]), y: Range::new(f[2], f[3]), z: Range::new(f[4], f[5]) });
            }
            let p = nested(pkg, raw)?;
            let axis = |n: &str| p.get(n).and_then(|x| range(pkg, x)).unwrap_or_default();
            Some(RangeVector { x: axis("X"), y: axis("Y"), z: axis("Z") })
        }
        _ => None,
    }
}

fn plane(pkg: &Package, v: &Value) -> Option<Plane> {
    match v {
        Value::Struct { raw, .. } => {
            if raw.len() == 16 {
                let mut r = Reader::new(raw);
                return Some([r.f32().ok()?, r.f32().ok()?, r.f32().ok()?, r.f32().ok()?]);
            }
            let p = nested(pkg, raw)?;
            Some([pf(&p, "X"), pf(&p, "Y"), pf(&p, "Z"), pf(&p, "W")])
        }
        _ => None,
    }
}

/// Dynamic array of tagged structs: element count then one tagged list per element.
fn struct_array<T>(pkg: &Package, v: &Value, mut f: impl FnMut(&Properties) -> T) -> Option<Vec<T>> {
    let Value::Array { raw } = v else { return None };
    let mut r = Reader::new(raw);
    let n = r.count().ok()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let p = read_properties(&mut r, pkg).ok()?;
        out.push(f(&p));
    }
    Some(out)
}

fn plane_array(pkg: &Package, v: &Value) -> Option<Vec<Plane>> {
    let Value::Array { raw } = v else { return None };
    let mut r = Reader::new(raw);
    let n = r.count().ok()?;
    if r.remaining() == n * 16 {
        return (0..n).map(|_| Some([r.f32().ok()?, r.f32().ok()?, r.f32().ok()?, r.f32().ok()?])).collect();
    }
    struct_array(pkg, v, |p| [pf(p, "X"), pf(p, "Y"), pf(p, "Z"), pf(p, "W")])
}

fn float_array(v: &Value) -> Option<Vec<f32>> {
    let Value::Array { raw } = v else { return None };
    let mut r = Reader::new(raw);
    let n = r.count().ok()?;
    (0..n).map(|_| r.f32().ok()).collect()
}

/// Dynamic array of object references.
pub fn object_array(v: &Value) -> Option<Vec<ObjRef>> {
    let Value::Array { raw } = v else { return None };
    let mut r = Reader::new(raw);
    let n = r.count().ok()?;
    (0..n).map(|_| r.compact().ok().map(ObjRef::from_raw)).collect()
}

fn particle_sound(pkg: &Package, p: &Properties) -> ParticleSound {
    let rg = |n: &str| p.get(n).and_then(|x| range(pkg, x)).unwrap_or_default();
    ParticleSound {
        sound: p.get("Sound").and_then(object).unwrap_or(ObjRef::Null),
        radius: rg("Radius"),
        pitch: rg("Pitch"),
        weight: p.get("Weight").and_then(int).unwrap_or(0),
        volume: rg("Volume"),
        probability: rg("Probability"),
    }
}

fn beam_scale(p: &Properties) -> BeamScale {
    BeamScale { frequency_scale: p.get("FrequencyScale").and_then(vec3).unwrap_or_default(), relative_length: pf(p, "RelativeLength") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_helpers() {
        let r = Range::new(2.0, 6.0);
        assert_eq!(r.lerp(0.5), 4.0);
        assert_eq!(r.center(), 4.0);
        assert_eq!(RangeVector::splat(-3.0).max_abs(), 3.0);
    }

    #[test]
    fn kinds() {
        assert_eq!(EmitterKind::from_class("SuperSpriteEmitter"), EmitterKind::SuperSprite);
        assert!(EmitterKind::is_particle_emitter_class("StripEmitter"));
        assert!(!EmitterKind::is_particle_emitter_class("Emitter"));
        assert_eq!(ParticleEmitter::new(EmitterKind::Mesh).start_size_range, RangeVector::splat(1.0));
    }
}
