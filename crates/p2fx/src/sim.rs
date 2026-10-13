//! CPU particle simulation following Unreal Engine 2's emitter rules, in Unreal space and units.
//!
//! Behaviour notes (observed from the shipped engine, re-expressed here):
//! - Each sub-emitter owns `MaxParticles` slots used as a ring. Spawning writes at `particle_index`
//!   and advances it modulo the slot count, so an emitter spawning faster than its particles die
//!   recycles the oldest slots.
//! - While fewer than `MaxParticles` slots have ever been used, the spawn rate is
//!   `MaxParticles / LifetimeRange.center` (AutomaticInitialSpawning) or InitialParticlesPerSecond;
//!   afterwards it is ParticlesPerSecond. Fractional spawns carry over between ticks, and each
//!   particle spawned within a tick is pre-aged by its share of the tick.
//! - A particle older than its lifetime is respawned in place when RespawnDeadParticles is set,
//!   otherwise it dies. An emitter whose particles are all dead (and that does not respawn) is
//!   finished: it can auto-reset, or be disabled when AutoDestroy is set.
//! - SizeScale/ColorScale keys are sampled at `frac((Repeats + 1) * age)`; before the first key the
//!   curve starts from size 1 / white, after the last key it holds the last key. Fading
//!   subtracts `Factor * fraction * 255` from the colour
//!   channels (alpha only for AlphaBlend, grey 127 with fading alpha for Modulated).
//! - Velocity: acceleration is added each tick, then the per-axis MaxAbsVelocity clamp, then the
//!   VelocityLossRange damping `v -= v * loss * dt`.

use bevy::math::{Mat3, Vec3};
use upkg::emitter::{DrawStyle, EmitterKind, ParticleEmitter, Range, RangeVector};

pub const F_ALIVE: u32 = 1;
/// Particle stopped after a collision (velocity below MinSquaredVelocity).
pub const F_STOPPED: u32 = 2;
/// Spawned during this tick: not moved again until the next tick.
pub const F_NEW: u32 = 4;

/// Small deterministic generator (SplitMix64).
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn frand(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, r: &Range) -> f32 {
        r.min + (r.max - r.min) * self.frand()
    }
    pub fn range_vector(&mut self, r: &RangeVector) -> Vec3 {
        Vec3::new(self.range(&r.x), self.range(&r.y), self.range(&r.z))
    }
    /// Random unit vector (uniform on the sphere).
    pub fn unit_vector(&mut self) -> Vec3 {
        loop {
            let v = Vec3::new(self.frand() * 2.0 - 1.0, self.frand() * 2.0 - 1.0, self.frand() * 2.0 - 1.0);
            let l = v.length_squared();
            if l > 1e-4 && l <= 1.0 {
                return v / l.sqrt();
            }
        }
    }
}

/// Unreal rotator (pitch, yaw, roll; 65536 = full turn) as a local-to-world rotation in Unreal
/// space: columns are the rotated X (forward), Y (right) and Z (up) axes.
pub fn rotator_matrix(r: [i32; 3]) -> Mat3 {
    let k = std::f32::consts::TAU / 65536.0;
    let (sp, cp) = (r[0] as f32 * k).sin_cos();
    let (sy, cy) = (r[1] as f32 * k).sin_cos();
    let (sr, cr) = (r[2] as f32 * k).sin_cos();
    let x = Vec3::new(cp * cy, cp * sy, sp);
    let y = Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp);
    let z = Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp);
    Mat3::from_cols(x, y, z)
}

/// Rotator pointing along a direction (roll 0).
pub fn direction_rotator(v: Vec3) -> [i32; 3] {
    let k = 32768.0 / std::f32::consts::PI;
    let yaw = (v.y.atan2(v.x) * k) as i32;
    let pitch = (v.z.atan2((v.x * v.x + v.y * v.y).sqrt()) * k) as i32;
    [pitch, yaw, 0]
}

fn safe_normal(v: Vec3) -> Vec3 {
    let l = v.length_squared();
    if l < 1e-8 { Vec3::ZERO } else { v / l.sqrt() }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Particle {
    pub location: Vec3,
    pub old_location: Vec3,
    pub velocity: Vec3,
    pub start_size: Vec3,
    /// Spin rates and start angles in Unreal angle units (65535 = full turn).
    pub spins_per_second: Vec3,
    pub start_spin: Vec3,
    pub size: Vec3,
    pub start_location: Vec3,
    pub color_multiplier: Vec3,
    /// R, G, B, A.
    pub color: [u8; 4],
    pub time: f32,
    pub max_lifetime: f32,
    /// Inverse mass (0 = massless).
    pub mass: f32,
    pub hit_count: i32,
    pub flags: u32,
    /// Fixed sub-image (UseRandomSubdivision) or -1 for age-driven animation.
    pub subdivision: i32,
    /// Time value at spawn (used by spark trails).
    pub spawn_time: f32,
}

impl Particle {
    pub fn alive(&self) -> bool {
        self.flags & F_ALIVE != 0
    }
    /// Age in [0, 1].
    pub fn relative_time(&self) -> f32 {
        if self.max_lifetime != 0.0 { (self.time / self.max_lifetime).clamp(0.0, 1.0) } else { 0.0 }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SparkData {
    pub time_before_visible: f32,
    pub time_between_segments: f32,
    pub start_location: Vec3,
    pub start_velocity: Vec3,
}

/// The emitter's owner (the Emitter actor) in Unreal space.
#[derive(Clone, Copy, Debug)]
pub struct Owner {
    pub location: Vec3,
    /// Local-to-world rotation.
    pub rotation: Mat3,
    pub velocity: Vec3,
}

impl Default for Owner {
    fn default() -> Self {
        Self { location: Vec3::ZERO, rotation: Mat3::IDENTITY, velocity: Vec3::ZERO }
    }
}

/// Runtime state of one sub-emitter. Fields scripts can change at runtime (Disabled,
/// ParticlesPerSecond, ...) are copied here so the definition stays immutable and shared.
#[derive(Clone, Debug, Default)]
pub struct EmitterState {
    pub particles: Vec<Particle>,
    pub spark: Vec<SparkData>,
    /// StripEmitter: slot indices newest first.
    pub strip_order: Vec<usize>,
    pub initialized: bool,
    pub particle_index: usize,
    /// Number of slots ever used (high-water mark), as in UE2.
    pub active_particles: usize,
    pub pps_fraction: f32,
    pub all_dead: bool,
    pub warmed_up: bool,
    pub other_index: i32,
    pub initial_delay: f32,
    pub global_offset: Vec3,
    pub time_till_reset: f32,
    pub max_active: usize,
    pub kill_pending: bool,
    pub disabled: bool,
    pub respawn_dead_particles: bool,
    pub automatic_initial_spawning: bool,
    pub particles_per_second: f32,
    pub initial_particles_per_second: f32,
    pub allow_particle_spawn: bool,
    /// Live particles after the last update.
    pub live: usize,
}

impl EmitterState {
    pub fn new(def: &ParticleEmitter) -> Self {
        Self {
            disabled: def.disabled,
            respawn_dead_particles: def.respawn_dead_particles,
            automatic_initial_spawning: def.automatic_initial_spawning,
            particles_per_second: def.particles_per_second,
            initial_particles_per_second: def.initial_particles_per_second,
            allow_particle_spawn: def.allow_particle_spawn,
            ..Default::default()
        }
    }
}

/// Request to spawn particles into another emitter (collision spawning).
struct SpawnRequest {
    emitter: usize,
    location: Vec3,
    velocity: Vec3,
    scale_velocity: Option<Vec3>,
}

/// Read-only view of the other sub-emitters while one is updated.
pub struct Others<'a> {
    pub states: &'a [EmitterState],
}

impl Others<'_> {
    fn get(&self, i: i32) -> Option<&EmitterState> {
        if i < 0 { None } else { self.states.get(i as usize) }
    }
}

/// Level collision for UseCollision particles (Unreal space). Returns the contact point and normal.
pub trait Collider: Send + Sync {
    fn trace(&self, start: Vec3, end: Vec3, extent: Vec3) -> Option<(Vec3, Vec3)>;
}

pub struct Ctx<'a> {
    pub collider: Option<&'a dyn Collider>,
    pub def: &'a ParticleEmitter,
    pub owner: &'a Owner,
    pub rng: &'a mut Rng,
    pub num_emitters: usize,
}

const MAX_SPAWN_RECURSION: u32 = 8;

impl EmitterState {
    pub fn initialize(&mut self, def: &ParticleEmitter, rng: &mut Rng) {
        let n = def.max_particles.max(0) as usize;
        self.particles = vec![Particle::default(); n];
        if def.kind == EmitterKind::Spark {
            self.spark = vec![SparkData::default(); n];
        }
        self.strip_order.clear();
        self.max_active = n;
        self.initialized = true;
        self.reset(def, rng);
    }

    pub fn reset(&mut self, def: &ParticleEmitter, rng: &mut Rng) {
        self.active_particles = 0;
        self.particle_index = 0;
        self.other_index = 0;
        self.all_dead = false;
        self.warmed_up = false;
        self.strip_order.clear();
        for p in &mut self.particles {
            p.flags = 0;
        }
        self.initial_delay = rng.range(&def.initial_delay_range);
        self.time_till_reset = rng.range(&def.auto_reset_time_range);
    }

    /// Rotation applied to spawn offsets and velocities (UseRotationFrom).
    fn spawn_rotation(def: &ParticleEmitter, owner: &Owner) -> Option<Mat3> {
        match def.use_rotation_from {
            1 => Some(owner.rotation * rotator_matrix(def.rotation_offset)),
            2 => Some(rotator_matrix(def.rotation_offset)),
            3 => {
                let mut r = direction_rotator(Vec3::from(def.rotation_normal));
                if def.effect_axis == 1 {
                    r[0] -= 16384;
                }
                Some(rotator_matrix(r))
            }
            _ => None,
        }
    }

    /// Spawns (or respawns) the particle in slot `i`. `dt` is how long ago within the tick it
    /// was born; `flags` are extra particle flags; `no_owner`/`no_offset` skip adding the owner
    /// location / global offset; `offset` is added to the start location.
    #[allow(clippy::too_many_arguments)]
    fn spawn_particle(&mut self, ctx: &mut Ctx, others: &Others, i: usize, dt: f32, flags: u32, no_owner: bool, no_offset: bool, offset: Vec3, depth: u32) {
        let def = ctx.def;
        if self.particles.is_empty() || self.kill_pending || def.lifetime_range.max <= 0.0 || i >= self.particles.len() {
            return;
        }
        if matches!(def.kind, EmitterKind::Strip) && !self.allow_particle_spawn {
            return;
        }
        let owner = *ctx.owner;
        let rng = &mut *ctx.rng;
        let independent = def.coordinate_system == 0;
        let mut loc = Vec3::from(def.start_location_offset);
        let super_line = def.kind == EmitterKind::SuperSprite && def.location_shape_extend == 1;
        if def.kind == EmitterKind::SuperSprite && def.location_shape_extend != 0 {
            if super_line {
                loc += rng.range_vector(&def.start_location_range);
                loc += (Vec3::from(def.line_end) - Vec3::from(def.line_start)) * rng.frand();
            } else {
                // Circle in the XY plane, height from StartLocationRange.Z.
                let a = rng.frand() * 6.28;
                let r1 = rng.frand() * (def.sphere_radius_range.min - def.sphere_radius_range.max) + def.sphere_radius_range.max;
                let r2 = rng.frand() * (def.sphere_radius_range.min - def.sphere_radius_range.max) + def.sphere_radius_range.max;
                loc.x += a.cos() * r1;
                loc.y += a.sin() * r2;
                loc.z += rng.frand() * (def.start_location_range.z.min - def.start_location_range.z.max) + def.start_location_range.z.max;
            }
        } else {
            match def.start_location_shape {
                1 => loc += rng.unit_vector() * rng.range(&def.sphere_radius_range),
                2 if def.kind != EmitterKind::Strip => {
                    let r = rng.range_vector(&def.start_location_polar_range);
                    let k = 3.1415 / 180.0;
                    let (az, pol, rad) = (r.x * k, r.y * k, r.z);
                    loc += Vec3::new(az.cos() * rad * pol.sin(), az.sin() * rad * pol.sin(), pol.cos() * rad);
                }
                _ => loc += rng.range_vector(&def.start_location_range),
            }
        }
        self.other_index += 1;
        if let Some(o) = others.get(def.add_location_from_other_emitter) {
            if o.active_particles > 0 {
                let op = &o.particles[(self.other_index as usize) % o.active_particles];
                loc += op.location - owner.location;
            }
        }
        let rot = Self::spawn_rotation(def, &owner);
        if let Some(m) = rot {
            loc = m * loc;
        }
        if independent && !no_owner {
            if super_line {
                loc += Vec3::from(def.line_start);
            } else {
                loc += owner.location + owner.velocity * dt;
            }
        }
        if !no_offset {
            loc += self.global_offset;
        }
        loc += offset;

        let mut p = Particle { location: loc, old_location: loc, start_location: loc, ..Default::default() };
        p.color_multiplier = rng.range_vector(&def.color_multiplier_range);
        p.velocity = rng.range_vector(&def.start_velocity_range);
        p.max_lifetime = rng.range(&def.lifetime_range);
        p.time = rng.range(&def.initial_time_range) + dt;
        p.spawn_time = p.time - dt;
        p.hit_count = 0;
        p.flags = flags | F_ALIVE;
        p.mass = rng.range(&def.start_mass_range);
        p.start_size = rng.range_vector(&def.start_size_range);
        if def.uniform_size {
            p.start_size.y = p.start_size.x;
            p.start_size.z = p.start_size.x;
        }
        p.size = p.start_size;
        p.velocity += Vec3::from(def.acceleration) * dt;
        if let Some(m) = rot {
            p.velocity = m * p.velocity;
        }
        if def.get_velocity_direction_from != 0 {
            let dir = if def.kind == EmitterKind::Strip {
                safe_normal(if def.get_velocity_direction_from == 1 { p.location - owner.location } else { owner.location - p.location })
            } else if def.coordinate_system == 1 {
                safe_normal(p.location)
            } else {
                safe_normal(owner.location - p.location)
            };
            match (def.kind == EmitterKind::Strip, def.get_velocity_direction_from) {
                (true, 1 | 2) => p.velocity *= dir,
                (false, 1) => p.velocity = -p.velocity * dir,
                (false, 2) => p.velocity *= dir,
                (_, 3) => p.velocity += dir * rng.range(&def.start_velocity_radial_range),
                _ => {}
            }
        }
        if let Some(o) = others.get(def.add_velocity_from_other_emitter) {
            if o.active_particles > 0 {
                let ov = o.particles[(self.other_index as usize) % o.active_particles].velocity;
                if def.kind == EmitterKind::Strip {
                    p.velocity += ov;
                } else {
                    p.velocity += rng.range_vector(&def.add_velocity_multiplier_range) * ov;
                }
            }
        }
        p.location += p.velocity * dt;
        if p.mass != 0.0 {
            p.mass = 1.0 / p.mass;
        }
        p.start_spin = rng.range_vector(&def.start_spin_range) * 65535.0;
        p.spins_per_second = rng.range_vector(&def.spins_per_second_range);
        for axis in 0..3 {
            if rng.frand() < def.spin_ccw_or_cw[axis] {
                p.spins_per_second[axis] = -p.spins_per_second[axis];
            }
        }
        p.spins_per_second *= 65535.0;
        p.subdivision = if def.use_random_subdivision {
            if def.subdivision_end == 0 {
                (rng.frand() * def.texture_u_subdivisions as f32 * def.texture_v_subdivisions as f32) as i32
            } else {
                (rng.frand() * (def.subdivision_end - def.subdivision_start) as f32 + def.subdivision_start as f32) as i32
            }
        } else {
            -1
        };
        self.particles[i] = p;
        if def.kind == EmitterKind::Spark {
            let sd = SparkData {
                time_before_visible: rng.range(&def.time_before_visible_range),
                time_between_segments: rng.range(&def.time_between_segments_range),
                start_location: p.location,
                start_velocity: p.velocity,
            };
            if let Some(s) = self.spark.get_mut(i) {
                *s = sd;
            }
        }
        if def.kind == EmitterKind::Strip {
            self.strip_order.retain(|&x| x != i);
            self.strip_order.insert(0, i);
        }
        // Spawned already past its lifetime (large InitialTimeRange or dt): respawn wrapped.
        if p.max_lifetime < p.time && p.max_lifetime != 0.0 && depth < MAX_SPAWN_RECURSION {
            let wrapped = if def.kind == EmitterKind::Strip { dt - p.max_lifetime } else { p.time % p.max_lifetime };
            self.spawn_particle(ctx, others, i, wrapped, 0, false, false, Vec3::ZERO, depth + 1);
        }
        self.all_dead = false;
    }

    /// Spawns `pps * dt` particles (plus the carried fraction); returns the new fraction.
    fn spawn_particles(&mut self, ctx: &mut Ctx, others: &Others, fraction: f32, pps: f32, dt: f32) -> f32 {
        if pps <= 0.0 || self.max_active == 0 {
            return 0.0;
        }
        if ctx.def.kind == EmitterKind::SuperSprite && !self.allow_particle_spawn {
            return 0.0;
        }
        let total = pps * dt + fraction;
        let n = total.floor();
        let new_fraction = total - n;
        let inv = 1.0 / pps;
        let first = inv * fraction + dt - inv;
        let count = (n as i64).clamp(0, self.max_active as i64) as usize;
        for k in 0..count {
            let i = self.particle_index;
            self.spawn_particle(ctx, others, i, first - k as f32 * inv, F_NEW, false, false, Vec3::ZERO, 0);
            self.active_particles = self.active_particles.max(i + 1);
            self.particle_index = (i + 1) % self.max_active;
        }
        new_fraction
    }

    /// Spawns `amount` particles now (script `SpawnParticle`), optionally at an offset.
    pub fn spawn_now(&mut self, ctx: &mut Ctx, others: &Others, amount: usize, offset: Vec3) {
        if self.max_active == 0 {
            return;
        }
        for _ in 0..amount.min(self.max_active) {
            let i = self.particle_index;
            self.spawn_particle(ctx, others, i, 0.0, 0, false, false, offset, 0);
            self.active_particles = self.active_particles.max(i + 1);
            self.particle_index = (i + 1) % self.max_active;
        }
    }

    /// `SuperSpriteEmitter.SpawnParticleLength(StartP, EndP, UseSize)`: lays up to five particles
    /// along StartP..EndP one `UseSize` apart. Each spawn uses the accumulated offset as
    /// StartLocationOffset and advances LineStart by the same step; LineEnd becomes EndP once
    /// anything spawned. Mutates `def` the way the engine mutates the emitter object.
    pub fn spawn_length(&mut self, def: &mut ParticleEmitter, rng: &mut Rng, owner: &Owner, num_emitters: usize, start: Vec3, end: Vec3, size: f32) {
        if !self.initialized {
            self.initialize(def, rng);
        }
        if self.max_active == 0 {
            return;
        }
        let delta = end - start;
        let step = delta.normalize_or_zero() * size;
        let mut off = Vec3::ZERO;
        let mut spawned = false;
        let mut k = 0;
        while (off - delta).length() > size && k <= 4 {
            def.start_location_offset = off.to_array();
            let i = self.particle_index;
            {
                let others = Others { states: &[] };
                let mut ctx = Ctx { collider: None, def, owner, rng: &mut *rng, num_emitters };
                self.spawn_particle(&mut ctx, &others, i, 0.0, 0, false, false, Vec3::ZERO, 0);
            }
            off += step;
            def.line_start = (Vec3::from(def.line_start) + step).to_array();
            self.active_particles = self.active_particles.max(i + 1);
            self.particle_index = (i + 1) % self.max_active;
            spawned = true;
            k += 1;
        }
        self.all_dead = false;
        if spawned {
            def.line_end = end.to_array();
        }
    }

    /// `SuperSpriteEmitter.SetLiveParticleTimes(T)`: live particles older than T are set back to T.
    pub fn set_live_particle_times(&mut self, t: f32) {
        for p in self.particles.iter_mut().take(self.active_particles) {
            if p.alive() && t < p.time {
                p.time = t;
            }
        }
    }

    /// Advances the emitter by `dt` seconds. Returns collision spawn requests for other emitters.
    fn update(&mut self, ctx: &mut Ctx, others: &Others, dt: f32, requests: &mut Vec<SpawnRequest>) -> usize {
        let def = ctx.def;
        let n = self.max_active;
        let pps = if self.active_particles < n {
            if self.automatic_initial_spawning {
                let c = def.lifetime_range.center();
                if c != 0.0 { n as f32 / c } else { 0.0 }
            } else if self.initial_particles_per_second > 0.0 {
                self.initial_particles_per_second
            } else {
                // InitialParticlesPerSecond 0 means "no burst": fill at the normal rate. The
                // barrel/car FireSizeableEmitter smoke (pps 3, ipps 0) never spawned before,
                // so explosions lost their lingering smoke (FIDELITY #45).
                self.particles_per_second
            }
        } else {
            self.particles_per_second
        };
        if pps > 0.0 && !self.kill_pending {
            self.pps_fraction = self.spawn_particles(ctx, others, self.pps_fraction, pps, dt);
        }
        let accel = Vec3::from(def.acceleration);
        let relative = def.coordinate_system == 1;
        let mut dead = 0usize;
        let count = self.active_particles.min(n);
        for i in 0..count {
            let mut p = self.particles[i];
            if !p.alive() {
                dead += 1;
                continue;
            }
            let mut moving = p.flags & F_STOPPED == 0 || relative;
            if p.flags & F_NEW != 0 {
                p.flags &= !F_NEW;
                moving = false;
            } else {
                p.time += dt;
            }
            if p.time > p.max_lifetime {
                if self.respawn_dead_particles {
                    let old_max = p.max_lifetime;
                    let over = ctx.rng.range(&def.initial_time_range) + (p.time - old_max);
                    let wrapped = if old_max != 0.0 { over % old_max } else { 0.0 };
                    self.particles[i] = p;
                    self.spawn_particle(ctx, others, i, wrapped, 0, false, false, Vec3::ZERO, 0);
                    p = self.particles[i];
                    moving = false;
                    if !p.alive() {
                        dead += 1;
                        continue;
                    }
                } else {
                    p.flags &= !F_ALIVE;
                    self.particles[i] = p;
                    dead += 1;
                    continue;
                }
            } else if moving {
                p.velocity += accel * dt;
                p.old_location = p.location;
                p.location += p.velocity * dt;
            }

            // Collision: level geometry through the collider (UseCollision), else the emitter's
            // own planes (UseCollisionPlanes).
            let mut collided = false;
            let mut contact: Option<(Vec3, Vec3)> = None;
            if moving && !relative && def.use_collision {
                if let Some(c) = ctx.collider {
                    let ext = Vec3::ZERO;
                    contact = c.trace(p.old_location, p.location, ext);
                }
            }
            if contact.is_none() && moving && !relative && def.use_collision_planes {
                for pl in &def.collision_planes {
                    let n3 = Vec3::new(pl[0], pl[1], pl[2]);
                    let d0 = p.old_location.dot(n3) - pl[3];
                    let d1 = p.location.dot(n3) - pl[3];
                    if (d0 >= 0.0) != (d1 >= 0.0) {
                        let t = d0 / (d0 - d1);
                        contact = Some((p.old_location + (p.location - p.old_location) * t, n3));
                        break;
                    }
                }
            }
            if let Some((hit, n3)) = contact {
                {
                    {
                        collided = true;
                        if def.spawn_from_other_emitter >= 0 && (def.spawn_from_other_emitter as usize) < ctx.num_emitters && dt != 0.0 {
                            for _ in 0..def.spawn_amount.max(0) {
                                requests.push(SpawnRequest {
                                    emitter: def.spawn_from_other_emitter as usize,
                                    location: hit,
                                    velocity: n3,
                                    scale_velocity: def.use_spawned_velocity_scale.then(|| ctx.rng.range_vector(&def.spawned_velocity_scale_range)),
                                });
                            }
                        }
                        let max_hits = ctx.rng.range(&def.max_collisions).floor() as i32;
                        if def.use_max_collisions && max_hits <= p.hit_count + 1 {
                            if self.respawn_dead_particles {
                                self.particles[i] = p;
                                self.spawn_particle(ctx, others, i, dt * 0.5, 0, false, false, Vec3::ZERO, 0);
                                p = self.particles[i];
                            } else {
                                p.flags &= !F_ALIVE;
                            }
                        } else {
                            p.velocity -= accel * dt;
                            p.velocity = p.velocity - 2.0 * p.velocity.dot(n3) * n3;
                            p.velocity *= ctx.rng.range_vector(&def.damping_factor_range);
                            if def.damp_rotation {
                                p.spins_per_second *= ctx.rng.range_vector(&def.rotation_damping_factor_range);
                            }
                            p.hit_count += 1;
                            p.location = hit + n3 * 0.1;
                            p.old_location = p.location;
                        }
                    }
                }
                if !p.alive() {
                    self.particles[i] = p;
                    dead += 1;
                    continue;
                }
            }

            self.update_appearance(def, &mut p);

            let max_v = Vec3::from(def.max_abs_velocity);
            for a in 0..3 {
                if max_v[a] != 0.0 {
                    p.velocity[a] = p.velocity[a].clamp(-max_v[a], max_v[a]);
                }
            }
            let loss = ctx.rng.range_vector(&def.velocity_loss_range);
            p.velocity -= p.velocity * loss * dt;
            if collided && def.min_squared_velocity != 0.0 && p.velocity.length_squared() < def.min_squared_velocity {
                p.flags |= F_STOPPED;
            }
            self.particles[i] = p;
        }
        // An emitter that has never spawned (script-fed: SpawnParticleLength / SpawnParticle with
        // no automatic rate) is not "all dead" yet, otherwise an AutoDestroy actor would finish
        // before script gets to feed it.
        let never_spawned = self.active_particles == 0 && !self.kill_pending;
        if !never_spawned && (n <= dead || (self.active_particles == dead && pps == 0.0)) && !self.respawn_dead_particles {
            self.all_dead = true;
        } else {
            self.all_dead = false;
        }
        self.live = self.active_particles.saturating_sub(dead);
        self.live
    }

    /// Size and colour for the particle's current age.
    pub fn update_appearance(&self, def: &ParticleEmitter, p: &mut Particle) {
        let rel = p.relative_time();
        let mut scale = 1.0;
        if def.use_size_scale {
            if !def.use_regular_size_scale {
                if p.max_lifetime != 0.0 {
                    let t = ((def.size_scale_repeats + 1.0) * rel).fract();
                    if let Some(k) = def.size_scale.iter().position(|key| t <= key.time) {
                        let key = def.size_scale[k];
                        let (pt, ps) = if k == 0 { (0.0, 1.0) } else { (def.size_scale[k - 1].time, def.size_scale[k - 1].size) };
                        scale = if key.time == 0.0 { key.size } else { ps + (key.size - ps) * ((t - pt) / (key.time - pt)) };
                    } else if let Some(last) = def.size_scale.last() {
                        // Past the last key the curve holds its final value.
                        scale = last.size;
                    }
                }
            } else {
                scale = 1.0 / (p.time + 1.0);
            }
        }
        p.size = p.start_size * scale;

        let (mut r, mut g, mut b, mut a) = (255.0f32, 255.0f32, 255.0f32, 255.0f32);
        if def.use_color_scale && p.max_lifetime != 0.0 {
            let t = ((def.color_scale_repeats + 1.0) * rel).fract();
            if let Some(k) = def.color_scale.iter().position(|key| t <= key.time) {
                let key = def.color_scale[k];
                let (pt, pc) = if k == 0 { (0.0, [255u8; 4]) } else { (def.color_scale[k - 1].time, def.color_scale[k - 1].color) };
                let w = if key.time == 0.0 { 1.0 } else { (t - pt) / (key.time - pt) };
                let lerp = |c0: u8, c1: u8| ((c1 as i32 - c0 as i32) as f32 * w + c0 as f32) as i32 as f32;
                r = lerp(pc[0], key.color[0]);
                g = lerp(pc[1], key.color[1]);
                b = lerp(pc[2], key.color[2]);
                a = if def.draw_style == DrawStyle::AlphaBlend { lerp(pc[3], key.color[3]) } else { 255.0 };
            } else if let Some(last) = def.color_scale.last() {
                // Past the last key the colour holds: BloodTrail splats darken from white to grey
                // 100 by 30 s of their hour-long life and stay dark ("drying"), as in the original
                // (sheet_orig_pool_c.png); falling back to white made them flash bright red again.
                r = last.color[0] as f32;
                g = last.color[1] as f32;
                b = last.color[2] as f32;
                a = if def.draw_style == DrawStyle::AlphaBlend { last.color[3] as f32 } else { 255.0 };
            }
        }
        // Colour multiplier: truncated to an integer, low byte kept (as the engine does).
        let mul = |c: f32, m: f32| ((c as i32 & 0xff) as f32 * m) as i32 as u8;
        let mut col = [mul(r, p.color_multiplier.x), mul(g, p.color_multiplier.y), mul(b, p.color_multiplier.z), (a as i32 & 0xff) as u8];

        let t = p.time;
        let fading_out = def.fade_out && def.fade_out_start_time < t && p.max_lifetime != def.fade_out_start_time;
        let fading_in = def.fade_in && t < def.fade_in_end_time && def.fade_in_end_time != 0.0;
        if fading_out || fading_in {
            let (f, factor) = if fading_out {
                ((t - def.fade_out_start_time) / (p.max_lifetime - def.fade_out_start_time), def.fade_out_factor)
            } else {
                ((def.fade_in_end_time - t) / def.fade_in_end_time, def.fade_in_factor)
            };
            let f = f * 255.0;
            match def.draw_style {
                DrawStyle::Modulated => {
                    let x = (factor[3] * f) as i32 as u8;
                    col = [0x7f, 0x7f, 0x7f, 255u8.wrapping_sub(x)];
                }
                DrawStyle::AlphaBlend => {
                    col[3] = (col[3] as f32 - factor[3] * f).clamp(0.0, 255.0) as u8;
                }
                _ => {
                    for c in 0..4 {
                        col[c] = (col[c] as f32 - factor[c] * f).clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        p.color = col;
    }

    /// Sub-image index range used for age-driven animation (count, start).
    pub fn subdivision_span(def: &ParticleEmitter) -> (i32, i32) {
        let total = def.texture_u_subdivisions * def.texture_v_subdivisions;
        if def.subdivision_end == 0 { (total, 0) } else { ((def.subdivision_end - def.subdivision_start).max(1), def.subdivision_start) }
    }
}

/// Actor-level state of an Emitter and its sub-emitters.
#[derive(Clone, Debug)]
pub struct SystemState {
    pub emitters: Vec<EmitterState>,
    pub rng: Rng,
    pub initialized: bool,
    pub global_offset: Vec3,
    pub time_till_reset: f32,
    /// Set when the actor has finished and should be destroyed (AutoDestroy).
    pub finished: bool,
    pub auto_destroy: bool,
    pub auto_reset: bool,
    /// Total simulated time.
    pub time: f32,
}

/// Actor-level emitter settings needed by the simulation.
#[derive(Clone, Debug, Default)]
pub struct ActorSettings {
    pub auto_destroy: bool,
    pub auto_reset: bool,
    pub global_offset_range: RangeVector,
    pub time_till_reset_range: Range,
}

impl SystemState {
    pub fn new(defs: &[ParticleEmitter], actor: &ActorSettings, seed: u64) -> Self {
        Self {
            emitters: defs.iter().map(EmitterState::new).collect(),
            rng: Rng(seed),
            initialized: false,
            global_offset: Vec3::ZERO,
            time_till_reset: 0.0,
            finished: false,
            auto_destroy: actor.auto_destroy,
            auto_reset: actor.auto_reset,
            time: 0.0,
        }
    }

    /// Stops spawning everywhere and lets live particles finish (`Emitter.Kill`).
    pub fn kill(&mut self) {
        for e in &mut self.emitters {
            e.respawn_dead_particles = false;
            e.particles_per_second = 0.0;
            e.initial_particles_per_second = 0.0;
            e.automatic_initial_spawning = false;
            e.kill_pending = true;
        }
        self.auto_destroy = true;
        self.auto_reset = false;
    }

    /// Toggles every sub-emitter (`Emitter.Trigger`).
    pub fn trigger(&mut self) {
        for e in &mut self.emitters {
            e.disabled = !e.disabled;
        }
    }

    /// One actor tick.
    pub fn tick(&mut self, defs: &[ParticleEmitter], actor: &ActorSettings, owner: &Owner, dt: f32) {
        self.tick_with(defs, actor, owner, dt, None);
    }

    /// One actor tick with optional level collision.
    pub fn tick_with(&mut self, defs: &[ParticleEmitter], actor: &ActorSettings, owner: &Owner, dt: f32, collider: Option<&dyn Collider>) {
        if !self.initialized {
            self.global_offset = self.rng.range_vector(&actor.global_offset_range);
            self.time_till_reset = self.rng.range(&actor.time_till_reset_range);
            self.initialized = true;
        }
        self.time += dt;
        let mut none_updated = true;
        let mut none_enabled = true;
        let num = defs.len();
        for i in 0..num {
            let def = &defs[i];
            if self.emitters[i].disabled {
                continue;
            }
            none_enabled = false;
            let mut st = std::mem::take(&mut self.emitters[i]);
            if !st.initialized {
                st.initialize(def, &mut self.rng);
            }
            if st.initial_delay != 0.0 {
                st.initial_delay -= dt;
                if st.initial_delay > 0.0 {
                    self.emitters[i] = st;
                    continue;
                }
                st.initial_delay = 0.0;
            }
            st.global_offset = self.global_offset;
            let mut requests = Vec::new();
            if !st.warmed_up && def.warmup_ticks_per_second != 0.0 && def.relative_warmup_time != 0.0 {
                let ticks = (def.lifetime_range.center() * def.relative_warmup_time * def.warmup_ticks_per_second) as i32;
                let step = 1.0 / def.warmup_ticks_per_second;
                for _ in 0..ticks.clamp(0, 100_000) {
                    let others = Others { states: &self.emitters };
                    let mut ctx = Ctx { collider, def, owner, rng: &mut self.rng, num_emitters: num };
                    st.update(&mut ctx, &others, step, &mut requests);
                }
                st.warmed_up = true;
            }
            if !st.all_dead {
                let others = Others { states: &self.emitters };
                let mut ctx = Ctx { collider, def, owner, rng: &mut self.rng, num_emitters: num };
                st.update(&mut ctx, &others, dt, &mut requests);
                none_updated = false;
            } else if !def.auto_reset || self.auto_reset {
                if def.auto_destroy && !self.auto_destroy {
                    st.disabled = true;
                }
            } else if st.time_till_reset - dt <= 0.0 {
                st.time_till_reset = self.rng.range(&def.auto_reset_time_range);
                st.reset(def, &mut self.rng);
            } else {
                st.time_till_reset -= dt;
            }
            self.emitters[i] = st;
            for r in requests {
                let target_def = &defs[r.emitter];
                let mut target = std::mem::take(&mut self.emitters[r.emitter]);
                if !target.initialized {
                    target.initialize(target_def, &mut self.rng);
                }
                if target.max_active > 0 {
                    let others = Others { states: &self.emitters };
                    let mut ctx = Ctx { collider, def: target_def, owner, rng: &mut self.rng, num_emitters: num };
                    let slot = target.particle_index;
                    let offset = if target_def.coordinate_system == 0 { r.location - owner.location } else { r.location };
                    target.spawn_particle(&mut ctx, &others, slot, dt, F_NEW, true, true, offset + if target_def.coordinate_system == 0 { owner.location } else { Vec3::ZERO }, 0);
                    if let Some(s) = r.scale_velocity {
                        target.particles[slot].velocity *= s;
                    }
                    let _ = r.velocity;
                    target.active_particles = target.active_particles.max(slot + 1);
                    target.particle_index = (slot + 1) % target.max_active;
                }
                self.emitters[r.emitter] = target;
            }
        }
        if none_updated {
            if !none_enabled && num != 0 && self.auto_destroy {
                self.finished = true;
            } else if self.auto_reset && !self.auto_destroy {
                if self.time_till_reset - dt <= 0.0 {
                    self.global_offset = self.rng.range_vector(&actor.global_offset_range);
                    self.time_till_reset = self.rng.range(&actor.time_till_reset_range);
                    for (i, e) in self.emitters.iter_mut().enumerate() {
                        e.reset(&defs[i], &mut self.rng);
                    }
                } else {
                    self.time_till_reset -= dt;
                }
            }
        }
    }

    pub fn live_particles(&self) -> usize {
        self.emitters.iter().map(|e| e.live).sum()
    }

    /// Script `ParticleEmitter.SpawnParticle(Amount)` on sub-emitter `i`.
    pub fn spawn_now(&mut self, defs: &[ParticleEmitter], i: usize, owner: &Owner, amount: usize) {
        let Some(def) = defs.get(i) else { return };
        let mut st = std::mem::take(&mut self.emitters[i]);
        if !st.initialized {
            st.initialize(def, &mut self.rng);
        }
        {
            let others = Others { states: &self.emitters };
            let mut ctx = Ctx { collider: None, def, owner, rng: &mut self.rng, num_emitters: defs.len() };
            st.spawn_now(&mut ctx, &others, amount, Vec3::ZERO);
        }
        st.all_dead = false;
        self.emitters[i] = st;
    }

    /// `SetMaxParticles(N)`: resizes sub-emitter `i` and restarts it.
    pub fn set_max_particles(&mut self, defs: &mut [ParticleEmitter], i: usize, n: i32) {
        let Some(def) = defs.get_mut(i) else { return };
        def.max_particles = n.max(0);
        self.emitters[i].initialize(def, &mut self.rng);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use upkg::emitter::{ColorKey, SizeKey};

    fn sprite() -> ParticleEmitter {
        let mut d = ParticleEmitter::new(EmitterKind::Sprite);
        d.max_particles = 10;
        d.lifetime_range = Range::splat(2.0);
        d
    }

    #[test]
    fn automatic_initial_spawning_fills_slots_over_one_lifetime() {
        let defs = vec![sprite()];
        let actor = ActorSettings::default();
        let mut s = SystemState::new(&defs, &actor, 1);
        let owner = Owner::default();
        for _ in 0..10 {
            s.tick(&defs, &actor, &owner, 0.1);
        }
        // 10 particles over a 2 s lifetime = 5/s; after 1 s about 5 have spawned.
        assert_eq!(s.emitters[0].active_particles, 5);
        for _ in 0..30 {
            s.tick(&defs, &actor, &owner, 0.1);
        }
        assert_eq!(s.emitters[0].active_particles, 10);
        assert_eq!(s.live_particles(), 10, "respawning keeps every slot alive");
    }

    #[test]
    fn non_respawning_emitter_finishes_and_auto_destroys() {
        let mut d = sprite();
        d.respawn_dead_particles = false;
        d.initial_particles_per_second = 1000.0;
        d.automatic_initial_spawning = false;
        let defs = vec![d];
        let actor = ActorSettings { auto_destroy: true, ..Default::default() };
        let mut s = SystemState::new(&defs, &actor, 7);
        let owner = Owner::default();
        s.tick(&defs, &actor, &owner, 0.05);
        assert_eq!(s.live_particles(), 10);
        for _ in 0..60 {
            s.tick(&defs, &actor, &owner, 0.05);
        }
        assert!(s.emitters[0].all_dead);
        assert!(s.finished);
    }

    #[test]
    fn size_and_color_scale_curves() {
        let mut d = sprite();
        d.use_size_scale = true;
        d.use_regular_size_scale = false;
        d.size_scale = vec![SizeKey { time: 0.5, size: 3.0 }];
        d.use_color_scale = true;
        d.color_scale = vec![ColorKey { time: 0.0, color: [255, 0, 0, 255] }, ColorKey { time: 1.0, color: [0, 0, 255, 255] }];
        let st = EmitterState::new(&d);
        let mut p = Particle { start_size: Vec3::splat(10.0), max_lifetime: 2.0, time: 0.5, color_multiplier: Vec3::ONE, flags: F_ALIVE, ..Default::default() };
        st.update_appearance(&d, &mut p);
        // Age 0.25: halfway from the implicit size 1 at t=0 to 3 at t=0.5.
        assert!((p.size.x - 20.0).abs() < 1e-3);
        assert_eq!(p.color, [191, 0, 63, 255]);
    }

    #[test]
    fn color_and_size_hold_after_last_key() {
        // BloodTrail: white -> grey 100 by relative time 1/120, then held for the rest of its life.
        let mut d = sprite();
        d.use_color_scale = true;
        d.color_scale = vec![ColorKey { time: 0.0, color: [255; 4] }, ColorKey { time: 0.008333334, color: [100, 100, 100, 255] }];
        d.use_size_scale = true;
        d.use_regular_size_scale = false;
        d.size_scale = vec![SizeKey { time: 0.0, size: 0.0 }, SizeKey { time: 0.008333334, size: 1.0 }];
        let st = EmitterState::new(&d);
        let mut p = Particle { start_size: Vec3::splat(8.0), max_lifetime: 3599.0, time: 600.0, color_multiplier: Vec3::ONE, flags: F_ALIVE, ..Default::default() };
        st.update_appearance(&d, &mut p);
        assert_eq!(&p.color[..3], &[100, 100, 100]);
        assert!((p.size.x - 8.0).abs() < 1e-3);
    }

    #[test]
    fn fade_out_reduces_additive_colour() {
        let mut d = sprite();
        d.fade_out = true;
        d.fade_out_start_time = 1.0;
        let st = EmitterState::new(&d);
        let mut p = Particle { start_size: Vec3::ONE, max_lifetime: 2.0, time: 1.5, color_multiplier: Vec3::ONE, flags: F_ALIVE, ..Default::default() };
        st.update_appearance(&d, &mut p);
        assert_eq!(p.color, [127, 127, 127, 127]);
    }

    #[test]
    fn rotator_axes() {
        let m = rotator_matrix([0, 16384, 0]);
        assert!((m.x_axis - Vec3::Y).length() < 1e-4);
        let r = direction_rotator(Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(r[0], 16384);
    }
}
