//! Per-tick physics modes (UE2 `performPhysics`): walking, falling, swimming, flying, ladder,
//! projectile, rotating, interpolating, moving brush (movers) and a simple Karma stand-in.
//!
//! The controller's desired movement is `Actor::acceleration` (as in UE2, where the controller
//! sets `Pawn.Acceleration` and script handles jumping by setting `Velocity.Z` and
//! `SetPhysics(PHYS_Falling)`).

use crate::actor::{Collider, EncroachType};
use crate::types::{ActorId, Event, HitActor, MoveHit, Physics, rot};
use crate::world::{PhysWorld, SKIN};
use glam::Vec3;

/// Gravity multiplier for PHYS_Karma bodies relative to the zone gravity (see `phys_karma`).
/// A kicked chair leaves at ~1700 uu/s up and is back on the ground at ~0.8 s in the original:
/// 2 * 1725 / 0.8 = ~4300 uu/s^2, about 4.5 x the default zone gravity of 950.
pub const KARMA_GRAVITY_SCALE: f32 = 4.5;

/// Extra distance probed below a walking pawn for floor contact.
const FLOOR_PROBE: f32 = 4.0;
/// UE2 walking keeps a pawn's cylinder floating MIN..MAX above its floor (measured along Z) and
/// re-centres it on FLOOR_DIST when it leaves that band. Sitting on the sweep skin (0.5) instead
/// drew every walking pawn ~1.7 uu low: skeletal meshes are authored for the original gap, so
/// shoes sank into the ground (and the eye was that much lower).
const MIN_FLOOR_DIST: f32 = 1.9;
const MAX_FLOOR_DIST: f32 = 2.4;
const FLOOR_DIST: f32 = 0.5 * (MIN_FLOOR_DIST + MAX_FLOOR_DIST);
/// Fluid friction applied while swimming when the volume does not set one.
const MIN_FLUID_FRICTION: f32 = 0.3;

impl PhysWorld {
    /// Advances every actor's physics by `dt` seconds. Events are queued (see `drain_events`).
    pub fn tick(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.25);
        if dt <= 0.0 {
            return;
        }
        // Movers first so pawns react to their new positions in the same frame.
        let ids = self.ids();
        let (movers, rest): (Vec<ActorId>, Vec<ActorId>) = ids.into_iter().partition(|&i| self.get(i).is_some_and(|a| a.physics == Physics::MovingBrush));
        for id in movers.into_iter().chain(rest) {
            if self.get(id).is_some() {
                self.tick_actor(id, dt);
            }
        }
    }

    /// Advances one actor.
    pub fn tick_actor(&mut self, id: ActorId, dt: f32) {
        let a = self.actor(id);
        let physics = a.physics;
        if a.is_pawn() {
            self.update_crouch(id);
        }
        match physics {
            Physics::None | Physics::Trailer | Physics::RootMotion | Physics::Spider => {}
            Physics::Walking => self.phys_walking(id, dt),
            Physics::Falling => self.phys_falling(id, dt),
            Physics::Swimming => self.phys_swimming(id, dt),
            Physics::Flying => self.phys_flying(id, dt),
            Physics::Ladder => self.phys_ladder(id, dt),
            Physics::Projectile => self.phys_projectile(id, dt),
            Physics::Rotating => {}
            Physics::Interpolating => self.phys_interpolating(id, dt),
            Physics::MovingBrush => self.phys_moving_brush(id, dt),
            Physics::Karma => self.phys_karma(id, dt),
            // Ragdolls are driven by the host's ragdoll solver (Location written each frame).
            Physics::KarmaRagDoll => {}
        }
        if self.get(id).is_some() {
            self.phys_rotation(id, dt);
        }
    }

    // ------------------------------------------------------------------
    // Velocity helpers
    // ------------------------------------------------------------------

    /// Pawn ground speed limit for the current state.
    fn max_speed(&self, id: ActorId) -> f32 {
        let a = self.actor(id);
        let Some(p) = &a.pawn else { return a.velocity.length().max(1.0) };
        let mut s = match a.physics {
            Physics::Swimming => p.water_speed,
            Physics::Flying => p.air_speed,
            Physics::Ladder => p.ladder_speed,
            _ => p.ground_speed,
        };
        if p.is_crouched {
            s *= p.crouched_pct;
        } else if p.is_walking {
            s *= p.walking_pct;
        }
        s
    }

    /// UE2-style CalcVelocity: friction steers toward the acceleration direction; without
    /// acceleration velocity brakes to a stop. Clamped to `max_speed`.
    fn calc_velocity(v: Vec3, accel: Vec3, dt: f32, max_speed: f32, friction: f32) -> Vec3 {
        let mut v = v;
        if accel.length_squared() > 1e-6 {
            let dir = accel.normalize();
            let speed = v.length();
            v -= (v - dir * speed) * (dt * friction).min(1.0);
            v += accel * dt;
        } else {
            let speed = v.length();
            if speed > 0.0 {
                let drop = speed * friction * dt;
                v *= (speed - drop).max(0.0) / speed;
            }
        }
        if v.length() > max_speed {
            v = v.normalize() * max_speed;
        }
        v
    }

    // ------------------------------------------------------------------
    // Walking
    // ------------------------------------------------------------------

    fn phys_walking(&mut self, id: ActorId, dt: f32) {
        // A pawn standing still on its floor stays put until it accelerates, gets velocity or
        // is moved (relink wakes it; carried pawns are moved with their base).
        {
            let a = self.actor(id);
            if a.sleeping && a.velocity == Vec3::ZERO && a.acceleration.truncate() == glam::Vec2::ZERO {
                return;
            }
        }
        let (accel_rate, walkable, step, can_walk_off, avoid, start) = {
            let a = self.actor(id);
            let p = a.pawn.clone().unwrap_or_default();
            (p.accel_rate, p.walkable_floor_z, p.max_step_height, p.can_walk_off_ledges || p.is_player, p.avoid_ledges || p.stop_at_ledges, a.location)
        };
        let info = self.volume_info(id).clone();
        if info.water && self.can_swim(id) {
            self.set_physics(id, Physics::Swimming);
            return self.phys_swimming(id, dt);
        }
        let mut accel = self.actor(id).acceleration;
        accel.z = 0.0;
        if accel.length() > accel_rate {
            accel = accel.normalize() * accel_rate;
        }
        let max_speed = self.max_speed(id);
        let mut v = self.actor(id).velocity;
        v.z = 0.0;
        v = Self::calc_velocity(v, accel, dt, max_speed, info.ground_friction);
        self.actor_mut(id).velocity = v;
        let delta = (v + info.zone_velocity * Vec3::new(1.0, 1.0, 0.0)) * dt;

        // Horizontal move with sliding and step-up.
        let mut pos = start;
        let mut hits = Vec::new();
        if delta.length_squared() > 0.0 {
            self.slide(id, &mut pos, delta, true, walkable, &mut hits, &[]);
            let moved = (pos - start).truncate().length();
            if !hits.is_empty() && moved < delta.truncate().length() * 0.9 {
                if let Some(p) = self.try_step_up(id, start, delta) {
                    if (p - start).truncate().length() > moved {
                        pos = p;
                    }
                }
            }
        }
        // Floor check.
        let probe = step + FLOOR_PROBE;
        let probe_from = pos;
        let (down, floor) = self.sweep_pos(id, pos, Vec3::Z * -probe, &[]);
        let floor_ok = floor.filter(|h| h.start_solid || h.normal.z >= walkable);
        match floor_ok {
            Some(h) => {
                if !h.start_solid {
                    // Height at which the cylinder touches the floor; keep the pawn in UE2's
                    // floor band above it (no move while it already is).
                    let contact = probe_from.z - h.t * probe;
                    let gap = probe_from.z - contact;
                    pos = probe_from;
                    if !(MIN_FLOOR_DIST..=MAX_FLOOR_DIST).contains(&gap) {
                        let target = contact + FLOOR_DIST;
                        if target > pos.z {
                            pos = self.sweep_pos(id, pos, Vec3::Z * (target - pos.z), &[]).0;
                        } else {
                            pos.z = target.max(down.z);
                        }
                    }
                }
                if let Some(p) = self.actor_mut(id).pawn.as_mut() {
                    p.floor = if h.start_solid { Vec3::Z } else { h.normal };
                }
                let base = h.actor.actor().filter(|&b| self.get(b).is_some_and(|o| o.can_be_base));
                if self.actor(id).base != base {
                    self.set_base(id, base);
                }
            }
            None => {
                // Off a ledge.
                if !can_walk_off || avoid {
                    self.events.push(Event::MayFall { actor: id });
                    // Stay on the ledge: keep the start position and stop.
                    self.actor_mut(id).velocity = Vec3::ZERO;
                    pos = start;
                    self.commit_walk(id, pos, &hits, dt, start);
                    return;
                }
                self.commit_walk(id, pos, &hits, dt, start);
                self.start_falling(id);
                return;
            }
        }
        self.commit_walk(id, pos, &hits, dt, start);
        // Standing still on a floor: sleep until something changes.
        {
            let a = self.actor_mut(id);
            if a.physics == Physics::Walking && a.location == start && a.velocity == Vec3::ZERO && a.acceleration.truncate() == glam::Vec2::ZERO {
                a.sleeping = true;
            }
        }
        // Entered water this move?
        if self.volume_info(id).water && self.can_swim(id) {
            self.set_physics(id, Physics::Swimming);
        }
    }

    fn commit_walk(&mut self, id: ActorId, pos: Vec3, hits: &[MoveHit], dt: f32, start: Vec3) {
        for h in hits.iter().filter(|h| h.normal.z.abs() < 0.7) {
            self.hit_wall(id, h);
            break;
        }
        self.commit(id, pos, None, hits);
        // Remove velocity into walls.
        let actual = (self.actor(id).location - start).truncate() / dt;
        let v = self.actor(id).velocity;
        if actual.length() < v.truncate().length() {
            self.actor_mut(id).velocity = Vec3::new(actual.x, actual.y, 0.0);
        }
    }

    fn start_falling(&mut self, id: ActorId) {
        self.set_physics(id, Physics::Falling);
        if self.actor(id).base.is_some() {
            self.set_base(id, None);
        }
        self.events.push(Event::Falling { actor: id });
    }

    /// Up by the step height, forward, back down onto a walkable floor. Returns the new position.
    pub(crate) fn try_step_up(&mut self, id: ActorId, start: Vec3, delta: Vec3) -> Option<Vec3> {
        let a = self.actor(id);
        let (step, walkable) = a.pawn.as_ref().map(|p| (p.max_step_height, p.walkable_floor_z)).unwrap_or((0.0, 0.7));
        if step <= 0.0 {
            return None;
        }
        let (up, hit) = self.sweep_pos(id, start, Vec3::Z * step, &[]);
        if hit.is_some() && up.z - start.z < 1.0 {
            return None;
        }
        let lifted = up.z - start.z;
        let mut pos = up;
        let mut hits = Vec::new();
        self.slide(id, &mut pos, Vec3::new(delta.x, delta.y, 0.0), true, walkable, &mut hits, &[]);
        let (down, h) = self.sweep_pos(id, pos, Vec3::Z * -(lifted + FLOOR_PROBE), &[]);
        match h {
            Some(h) if !h.start_solid && h.normal.z >= walkable && (down - start).truncate().length() > 0.1 => Some(down),
            _ => None,
        }
    }

    fn can_swim(&self, id: ActorId) -> bool {
        self.actor(id).pawn.as_ref().is_some_and(|p| p.can_swim)
    }

    // ------------------------------------------------------------------
    // Falling
    // ------------------------------------------------------------------

    fn phys_falling(&mut self, id: ActorId, dt: f32) {
        let info = self.volume_info(id).clone();
        if info.water && (self.can_swim(id) || !self.actor(id).is_pawn()) {
            if self.actor(id).is_pawn() {
                self.set_physics(id, Physics::Swimming);
                return self.phys_swimming(id, dt);
            }
        }
        let (accel, air_control, accel_rate, walkable, is_pawn, bounce) = {
            let a = self.actor(id);
            let p = a.pawn.clone();
            (a.acceleration, p.as_ref().map(|p| p.air_control).unwrap_or(0.0), p.as_ref().map(|p| p.accel_rate).unwrap_or(0.0), p.as_ref().map(|p| p.walkable_floor_z).unwrap_or(0.7), p.is_some(), a.bounce)
        };
        let max_speed = self.max_speed(id);
        let v0 = self.actor(id).velocity;
        let mut v = v0;
        if is_pawn {
            // Air control: a fraction of the walking acceleration, limited to ground speed.
            let mut a = Vec3::new(accel.x, accel.y, 0.0);
            if a.length() > accel_rate {
                a = a.normalize() * accel_rate;
            }
            let mut hv = v.truncate() + a.truncate() * air_control * dt;
            let lim = max_speed.max(v0.truncate().length());
            if hv.length() > lim {
                hv = hv.normalize() * lim;
            }
            v.x = hv.x;
            v.y = hv.y;
        }
        v += info.gravity * dt;
        if v.length() > info.terminal_velocity {
            v = v.normalize() * info.terminal_velocity;
        }
        self.actor_mut(id).velocity = v;
        let start = self.actor(id).location;
        let delta = (v0 + v) * 0.5 * dt + info.zone_velocity * dt;
        let mut pos = start;
        let mut hits = Vec::new();
        self.slide(id, &mut pos, delta, false, walkable, &mut hits, &[]);
        let landed = hits.iter().find(|h| h.normal.z >= walkable && self.actor(id).velocity.z <= 0.0).copied();
        if std::env::var_os("P2GAME_JUMP_TRACE").is_some() && self.actor(id).is_player() && (landed.is_some() || v0.z > 0.0 && v.z > 0.0 && (pos.z - start.z) < 1.0) {
            let hs: Vec<String> = hits.iter().map(|h| format!("{:?} t {:.2} n {:.2?} solid {} depth {:.1}", h.actor, h.t, h.normal, h.start_solid, h.depth)).collect();
            println!("FALLSTEP v0z {:.0} vz {:.0} dz {:.1} delta {:.1?} landed {} hits {hs:?}", v0.z, self.actor(id).velocity.z, pos.z - start.z, delta, landed.is_some());
        }
        if !bounce {
            if let Some(h) = landed {
                self.commit(id, pos, None, &hits);
                self.process_landed(id, &h);
                return;
            }
        }
        for h in &hits {
            self.hit_wall(id, h);
        }
        // Commit (and queue Bump) with the velocity the actor hit with; UE2 calls Bump inside
        // the move, before the slide removes the speed into the surface (Window.Bump reads it).
        self.commit(id, pos, None, &hits);
        if !bounce {
            for h in &hits {
                let into = self.actor(id).velocity.dot(h.normal);
                if into < 0.0 {
                    self.actor_mut(id).velocity -= h.normal * into;
                }
            }
        }
        if self.volume_info(id).water && self.can_swim(id) {
            self.set_physics(id, Physics::Swimming);
        }
    }

    /// Lands on a floor: `Landed(HitNormal)`; pawns walk, other actors stop (PHYS_None) and are
    /// based on what they landed on.
    fn process_landed(&mut self, id: ActorId, h: &MoveHit) {
        self.events.push(Event::Landed { actor: id, normal: h.normal });
        let is_pawn = self.actor(id).is_pawn();
        if is_pawn {
            self.set_physics(id, Physics::Walking);
            if let Some(p) = self.actor_mut(id).pawn.as_mut() {
                p.floor = h.normal;
            }
            let v = self.actor(id).velocity;
            self.actor_mut(id).velocity = Vec3::new(v.x, v.y, 0.0);
        } else {
            self.set_physics(id, Physics::None);
            self.actor_mut(id).velocity = Vec3::ZERO;
        }
        let base = h.actor.actor().filter(|&b| self.get(b).is_some_and(|o| o.can_be_base));
        if self.actor(id).base != base {
            self.set_base(id, base);
        }
    }

    // ------------------------------------------------------------------
    // Swimming, flying, ladder
    // ------------------------------------------------------------------

    fn phys_swimming(&mut self, id: ActorId, dt: f32) {
        let info = self.volume_info(id).clone();
        if !info.water {
            // Left the water: fall (script handles jumping out of water).
            self.start_falling(id);
            return self.phys_falling(id, dt);
        }
        let (accel, mass, buoyancy, walkable) = {
            let a = self.actor(id);
            (a.acceleration, a.mass.max(1.0), a.buoyancy, a.pawn.as_ref().map(|p| p.walkable_floor_z).unwrap_or(0.7))
        };
        let max_speed = self.max_speed(id);
        let friction = info.fluid_friction.max(MIN_FLUID_FRICTION);
        let mut v = self.actor(id).velocity;
        // Net gravity: weight less buoyancy.
        v += info.gravity * (1.0 - (buoyancy / mass).min(1.0)) * dt * 0.1;
        v = Self::calc_velocity(v, accel, dt, max_speed, friction * 4.0);
        self.actor_mut(id).velocity = v;
        let start = self.actor(id).location;
        let mut pos = start;
        let mut hits = Vec::new();
        self.slide(id, &mut pos, (v + info.zone_velocity) * dt, false, walkable, &mut hits, &[]);
        if let Some(h) = hits.first() {
            self.hit_wall(id, h);
        }
        self.commit(id, pos, None, &hits);
    }

    fn phys_flying(&mut self, id: ActorId, dt: f32) {
        let info = self.volume_info(id).clone();
        let accel = self.actor(id).acceleration;
        let max_speed = self.max_speed(id);
        let v = Self::calc_velocity(self.actor(id).velocity, accel, dt, max_speed, info.fluid_friction.max(0.1) * 4.0);
        self.actor_mut(id).velocity = v;
        let start = self.actor(id).location;
        let mut pos = start;
        let mut hits = Vec::new();
        self.slide(id, &mut pos, (v + info.zone_velocity) * dt, false, 0.7, &mut hits, &[]);
        if let Some(h) = hits.first() {
            self.hit_wall(id, h);
        }
        self.commit(id, pos, None, &hits);
    }

    fn phys_ladder(&mut self, id: ActorId, dt: f32) {
        let ladder = self.actor(id).pawn.as_ref().and_then(|p| p.on_ladder).or_else(|| {
            let v = self.actor(id).physics_volume?;
            self.get(v)?.volume.as_ref()?.ladder.map(|_| v)
        });
        let Some(info) = ladder.and_then(|l| self.get(l)).and_then(|l| l.volume.as_ref()).and_then(|v| v.ladder) else {
            self.start_falling(id);
            return;
        };
        // Still inside the ladder volume?
        let loc = self.actor(id).location;
        if let Some(l) = ladder {
            if let Collider::Brush { placed, .. } = &self.actor(l).collider {
                let (r, h) = (self.actor(id).collision_radius, self.actor(id).collision_height);
                if !placed.overlaps_cylinder(loc, r, h) {
                    self.start_falling(id);
                    return;
                }
            }
        }
        let accel = self.actor(id).acceleration;
        let speed = self.max_speed(id);
        // Moving along the look direction climbs; acceleration perpendicular to the wall slides.
        let climb = info.climb_dir.normalize_or(Vec3::Z);
        let look = info.look_dir.normalize_or(Vec3::X);
        let into_wall = accel.dot(look);
        let along = into_wall + accel.dot(climb);
        let lateral = accel - climb * accel.dot(climb) - look * into_wall;
        let mut v = climb * along.signum() * if along.abs() > 1e-3 { speed } else { 0.0 } + lateral.normalize_or_zero() * speed * 0.5 * (lateral.length() > 1e-3) as i32 as f32;
        if v.length() > speed {
            v = v.normalize() * speed;
        }
        // Pressing toward the ladder also moves the climber toward it (UE2 pushes into the
        // ladder geometry): against the wall the slide stops it, and once it has climbed past the
        // top it steps forward over the edge onto the roof and leaves the volume. Without this an
        // AI heading for a roof target rose out of the top of the volume straight up, fell back
        // in and re-grabbed, bobbing in the air (VincesHouse zombies on the house ladders).
        // Pulling away from the ladder lets go of it the same way. Added after the climb clamp
        // so the climb rate stays LadderSpeed.
        if into_wall.abs() > 1e-3 && std::env::var_os("P2PHYS_OLD_LADDER").is_none() {
            v += look * into_wall.signum() * speed * 0.5;
        }
        self.actor_mut(id).velocity = v;
        let mut pos = loc;
        let mut hits = Vec::new();
        self.slide(id, &mut pos, v * dt, false, 0.7, &mut hits, &[]);
        let landed = hits.iter().find(|h| h.normal.z >= 0.7 && v.z < 0.0).copied();
        self.commit(id, pos, None, &hits);
        if let Some(h) = landed {
            self.process_landed(id, &h);
        }
    }

    // ------------------------------------------------------------------
    // Projectile, karma
    // ------------------------------------------------------------------

    fn phys_projectile(&mut self, id: ActorId, dt: f32) {
        let info = self.volume_info(id).clone();
        let a = self.actor(id);
        let mut v = a.velocity + a.acceleration * dt;
        if v.length() > info.terminal_velocity.max(1.0) * 4.0 {
            v = v.normalize() * info.terminal_velocity * 4.0;
        }
        self.actor_mut(id).velocity = v;
        let start = self.actor(id).location;
        let (pos, hit) = self.sweep_pos(id, start, v * dt, &[]);
        let hits: Vec<MoveHit> = hit.into_iter().collect();
        self.commit(id, pos, None, &hits);
        if let Some(h) = hit {
            if self.get(id).is_some() {
                self.hit_wall(id, &h);
            }
        }
    }
}

/// `P2_KARMA_NO_FIT=1`: sweep Karma bodies with the script cylinder centred on Location (the
/// pre-fix behaviour), for A/B checks of the mesh-fitted cylinder.
fn karma_fit_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("P2_KARMA_NO_FIT").is_some())
}

impl PhysWorld {
    /// Cylinder (offset from Location, radius, half-height) fitted to a Karma body's placed trace
    /// mesh, or None when it has no mesh. The radius is the smaller horizontal half-extent so a
    /// long thin prop does not wedge in doorways; the height covers the whole mesh.
    fn karma_cylinder(&self, id: ActorId) -> Option<(Vec3, f32, f32)> {
        let a = self.actor(id);
        let Some(crate::actor::Collider::Mesh { placed, .. }) = &a.trace_collider else { return None };
        if placed.is_empty() {
            return None;
        }
        let b = placed.bounds;
        let half = (b.max - b.min) * 0.5;
        if !(half.x > 0.5 && half.y > 0.5 && half.z > 0.5) {
            return None;
        }
        let r = half.x.min(half.y).clamp(4.0, 200.0);
        Some(((b.min + b.max) * 0.5 - a.location, r, half.z))
    }

    /// Simple rigid body for PHYS_Karma / PHYS_KarmaRagDoll: gravity, sliding contact, energy
    /// loss on impact and rest on floors. (UE2 Karma is not reproduced; counted in stats.)
    fn phys_karma(&mut self, id: ActorId, dt: f32) {
        let v0 = self.actor(id).velocity;
        if self.actor(id).sleeping {
            if v0 == Vec3::ZERO {
                return;
            }
            self.actor_mut(id).sleeping = false;
        }
        self.stats.karma_ticks += 1;
        let info = self.volume_info(id).clone();
        // UE2 Karma runs its own, much stronger gravity than PHYS_Falling (Karma scale). Fitted to
        // swan's front kick on FX.KickableActor (orig_kf_*, real time): the chair leaves at
        // ~1700 uu/s up, peaks a few metres high and lands at ~0.8 s, ~25-30 m away. Level
        // gravity alone kept it airborne for ~3 s and 1300 uu up.
        let mut v = v0 + info.gravity * KARMA_GRAVITY_SCALE * dt;
        if v.length() > info.terminal_velocity {
            v = v.normalize() * info.terminal_velocity;
        }
        let start = self.actor(id).location;
        // UE2 Karma collides with the static mesh; our stand-in sweeps a cylinder. The script's
        // CollisionRadius/Height cylinder is centred on Location, which for most props is the
        // mesh's base (suburbs-3 dinner chairs: mesh z 0..92, cylinder half-height 90), so half of
        // it sat in the floor: the first kick found it start-solid and depenetrated it 90 units up
        // (playtest: kicked chairs "teleport" and float). Sweep a cylinder fitted to the placed
        // mesh bounds instead, offset from Location.
        let saved = (self.actor(id).collision_radius, self.actor(id).collision_height);
        let offset = match self.karma_cylinder(id).filter(|_| !karma_fit_disabled()) {
            Some((off, r, h)) => {
                let a = self.actor_mut(id);
                a.collision_radius = r;
                a.collision_height = h;
                off
            }
            None => Vec3::ZERO,
        };
        let start = start + offset;
        // A body at rest (map-placed, or settled) that already overlaps geometry is resting where
        // it was saved: in UE2 the saved location is the Karma rest pose, and our collider
        // (cylinder or render-mesh) can sit slightly inside the ground or a neighbour. Pushing it
        // out on its first tick lifted cars up to 200 units into the air (FIDELITY #30). Sleep it
        // in place; an impulse (non-zero Velocity) wakes it and it moves normally from there.
        if v0.length() < 1.0 {
            if let Some(h) = self.sweep(id, start, (v0 + v) * 0.5 * dt, &[]) {
                if h.start_solid {
                    let a = self.actor_mut(id);
                    (a.collision_radius, a.collision_height) = saved;
                    a.velocity = Vec3::ZERO;
                    a.sleeping = true;
                    return;
                }
            }
        }
        let mut pos = start;
        let mut hits = Vec::new();
        self.slide(id, &mut pos, (v0 + v) * 0.5 * dt, false, 0.7, &mut hits, &[]);
        {
            let a = self.actor_mut(id);
            (a.collision_radius, a.collision_height) = saved;
        }
        let (pos, start) = (pos - offset, start - offset);
        for h in &hits {
            let into = v.dot(h.normal);
            if into < 0.0 {
                // Bouncy above a small impact speed; resting contact (gravity steps) is inelastic.
                let restitution = if -into > 50.0 { 1.3 } else { 1.0 };
                v -= h.normal * into * restitution;
                if -into > 200.0 {
                    let hit_actor = h.actor.actor();
                    self.events.push(Event::KImpact { actor: id, other: hit_actor, pos, velocity: v, normal: h.normal });
                }
            }
            if h.normal.z > 0.7 {
                v *= (1.0 - 4.0 * dt).max(0.0);
            }
        }
        // At rest on a floor, or wedged (blocked without moving at all, e.g. a map-placed car
        // whose cylinder starts inside the ground): stop and sleep until disturbed.
        let stuck = !hits.is_empty() && (pos - start).length() < 0.5 && v.length() > 2.0;
        let resting = stuck || (v.length() < 2.0 && hits.iter().any(|h| h.normal.z > 0.7));
        if resting {
            v = Vec3::ZERO;
        }
        self.actor_mut(id).velocity = v;
        self.commit(id, pos, None, &hits);
        if std::env::var_os("P2_KARMA_TRACE").is_some() {
            let hs: Vec<String> = hits.iter().map(|h| format!("{:?} n={:.2} t={:.2} solid={} depth={:.1}", h.actor, h.normal, h.t, h.start_solid, h.depth)).collect();
            eprintln!("KARMA {} {:.0} -> {:.0} v0={:.0} v={:.0} rest={resting} hits={hs:?}", self.actor(id).class, start, pos, v0, v);
        }
        if resting {
            self.actor_mut(id).sleeping = true;
        }
    }

    // ------------------------------------------------------------------
    // Rotation
    // ------------------------------------------------------------------

    /// physicsRotation: PHYS_Rotating spins by `RotationRate`; `bRotateToDesired` turns toward
    /// `DesiredRotation` at `RotationRate` and sends `EndedRotation` on arrival.
    fn phys_rotation(&mut self, id: ActorId, dt: f32) {
        // Walking/falling pawns stay upright: UE2's APawn::rotateToward zeroes the desired pitch
        // for walkers, so a pawn spawned with a pitched rotation (CheatManager.Summon spawns at
        // the player's view rotation) does not keep leaning (its mesh and head bone would tilt).
        {
            let a = self.actor_mut(id);
            if a.pawn.is_some() && matches!(a.physics, Physics::Walking | Physics::Falling | Physics::None) && a.rotation[0] & 0xffff != 0 {
                a.desired_rotation[0] = 0;
                let r = [0, a.rotation[1], a.rotation[2]];
                self.set_rotation(id, r);
            }
        }
        let a = self.actor(id);
        let (rate, cur, desired, physics) = (a.rotation_rate, a.rotation, a.desired_rotation, a.physics);
        // UE2 physicsRotation: bFixedRotationDir turns by RotationRate every tick (flying limbs,
        // heads and other tumbling parts), bRotateToDesired turns toward DesiredRotation.
        let fixed = a.fixed_rotation_dir && matches!(physics, Physics::Projectile | Physics::Falling | Physics::Flying | Physics::Swimming);
        let to_desired = a.rotate_to_desired && !fixed;
        if let (true, Some(axis), false) = (fixed, a.spin_axis, rate == [0; 3]) {
            // Spin about a world axis at the rate's magnitude (a gib torqued by its hit).
            let speed = Vec3::new(rate[0] as f32, rate[1] as f32, rate[2] as f32).length() * std::f32::consts::TAU / 65536.0;
            let q = glam::Quat::from_axis_angle(axis, speed * dt) * rot::to_quat(cur);
            self.set_rotation(id, rot::from_quat(q));
            return;
        }
        if !(to_desired || fixed || physics == Physics::Rotating) || rate == [0; 3] {
            return;
        }
        let step = |r: i32| (r as f32 * dt).round() as i32;
        let new = if to_desired {
            [rot::approach(cur[0], desired[0], step(rate[0])), rot::approach(cur[1], desired[1], step(rate[1])), rot::approach(cur[2], desired[2], step(rate[2]))]
        } else {
            [cur[0] + step(rate[0]), cur[1] + step(rate[1]), cur[2] + step(rate[2])].map(|x| x & 0xffff)
        };
        if new != cur {
            self.set_rotation(id, new);
            if to_desired && (0..3).all(|k| rot::delta(new[k], desired[k]) == 0 || rate[k] == 0) {
                self.events.push(Event::EndedRotation { actor: id });
            }
        }
    }

    // ------------------------------------------------------------------
    // Interpolating (paths) and movers
    // ------------------------------------------------------------------

    fn phys_interpolating(&mut self, id: ActorId, dt: f32) {
        let Some(path) = self.actor(id).interp.clone() else { return };
        if path.points.len() < 2 {
            return;
        }
        let end = (path.points.len() - 1) as f32;
        let pos = (path.pos + path.rate * dt).min(end);
        let k = (pos.floor() as usize).min(path.points.len() - 2);
        let f = pos - k as f32;
        let (p0, r0) = path.points[k];
        let (p1, r1) = path.points[k + 1];
        let p = p0.lerp(p1, f);
        let r = [0, 1, 2].map(|i| r0[i] + (rot::delta(r0[i], r1[i]) as f32 * f) as i32);
        if let Some(ip) = self.actor_mut(id).interp.as_mut() {
            ip.pos = pos;
        }
        if self.actor(id).collider.is_cylinder() {
            self.commit(id, p, Some(r), &[]);
        } else {
            self.move_encroacher(id, p, r);
        }
        if pos >= end {
            self.set_physics(id, Physics::None);
            self.events.push(Event::InterpolateEnd { actor: id });
        }
    }

    /// `Mover.InterpolateTo(NewKeyNum, Seconds)` (the script function's effect on the physics
    /// state), for hosts and tests that drive movers without the script VM.
    pub fn mover_interpolate_to(&mut self, id: ActorId, key: u8, seconds: f32) {
        let (loc, rotn) = {
            let a = self.actor(id);
            (a.location, a.rotation)
        };
        let Some(m) = self.actor_mut(id).mover.as_mut() else { return };
        let key = key.min(7);
        if key == m.prev_key_num && m.key_num != m.prev_key_num {
            m.phys_alpha = 1.0 - m.phys_alpha;
            m.old_pos = m.base_pos + m.key_pos[m.key_num as usize];
            m.old_rot = add_rot(m.base_rot, m.key_rot[m.key_num as usize]);
        } else {
            m.old_pos = loc;
            m.old_rot = rotn;
            m.phys_alpha = 0.0;
        }
        m.interpolating = true;
        m.prev_key_num = m.key_num;
        m.key_num = key;
        m.phys_rate = 1.0 / seconds.max(0.005);
        self.set_physics(id, Physics::MovingBrush);
    }

    fn phys_moving_brush(&mut self, id: ActorId, dt: f32) {
        let Some(m) = self.actor(id).mover.clone() else { return };
        if !m.interpolating {
            return;
        }
        let alpha = (m.phys_alpha + m.phys_rate * dt).min(1.0);
        let shaped = if m.glide { alpha * alpha * (3.0 - 2.0 * alpha) } else { alpha };
        let dest = m.base_pos + m.key_pos[m.key_num as usize];
        let dest_rot = add_rot(m.base_rot, m.key_rot[m.key_num as usize]);
        let p = m.old_pos + (dest - m.old_pos) * shaped;
        let r = [0, 1, 2].map(|k| m.old_rot[k] + (rot::delta(m.old_rot[k], dest_rot[k]) as f32 * shaped).round() as i32);
        let ok = self.move_encroacher(id, p, r);
        if !ok {
            // Blocked: hold position; script (EncroachingOn) decides to stop or return.
            if m.encroach == EncroachType::Return || m.encroach == EncroachType::Stop {
                return;
            }
        }
        let Some(mm) = self.actor_mut(id).mover.as_mut() else { return };
        mm.phys_alpha = alpha;
        if alpha >= 1.0 {
            mm.interpolating = false;
            // Arrived: physics stops; script KeyFrameReached chains the next move (InterpolateTo).
            self.set_physics(id, Physics::None);
            self.events.push(Event::KeyFrameReached { actor: id });
        }
    }

    // ------------------------------------------------------------------
    // Crouching
    // ------------------------------------------------------------------

    /// Applies `bWantsToCrouch`: shrinks to the crouch cylinder (feet fixed) while walking, and
    /// stands back up when there is room. Sends StartCrouch / EndCrouch(HeightAdjust).
    fn update_crouch(&mut self, id: ActorId) {
        let a = self.actor(id);
        let Some(p) = a.pawn.clone() else { return };
        let want = p.wants_to_crouch && p.can_crouch && a.physics == Physics::Walking;
        if want == p.is_crouched {
            return;
        }
        let (h0, h1, r1) = if want { (p.stand_height, p.crouch_height, p.crouch_radius) } else { (p.crouch_height, p.stand_height, p.stand_radius) };
        let adjust = h0 - h1;
        let new_loc = a.location - Vec3::Z * adjust;
        if !want {
            let old = (a.collision_radius, a.collision_height);
            {
                let am = self.actor_mut(id);
                am.collision_radius = r1;
                am.collision_height = h1;
            }
            let fits = self.fits(id, new_loc);
            let am = self.actor_mut(id);
            am.collision_radius = old.0;
            am.collision_height = old.1;
            if !fits {
                return;
            }
        }
        {
            let am = self.actor_mut(id);
            am.collision_radius = r1;
            am.collision_height = h1;
            if let Some(pp) = am.pawn.as_mut() {
                pp.is_crouched = want;
            }
        }
        self.commit(id, new_loc, None, &[]);
        let adj = adjust.abs();
        self.events.push(if want { Event::StartCrouch { actor: id, height_adjust: adj } } else { Event::EndCrouch { actor: id, height_adjust: adj } });
    }

    /// Places a pawn on the floor below `p` (searching down `max_drop`): walking if found.
    pub fn place_on_floor(&mut self, id: ActorId, p: Vec3, max_drop: f32) -> bool {
        let start = p + Vec3::Z;
        let (pos, hit) = self.sweep_pos(id, start, Vec3::Z * -max_drop, &[]);
        match hit {
            Some(h) if !h.start_solid => {
                // Pawns float FLOOR_DIST above the floor like walking keeps them.
                let pos = if self.actor(id).is_pawn() { self.sweep_pos(id, pos, Vec3::Z * (FLOOR_DIST - SKIN), &[]).0 } else { pos };
                self.commit(id, pos, None, &[]);
                if self.actor(id).is_pawn() {
                    self.set_physics(id, Physics::Walking);
                    if let Some(pp) = self.actor_mut(id).pawn.as_mut() {
                        pp.floor = h.normal;
                    }
                }
                let base = h.actor.actor();
                if base.is_some() {
                    self.set_base(id, base);
                }
                true
            }
            _ => {
                self.commit(id, p, None, &[]);
                false
            }
        }
    }

    /// True when `id` stands on (or is based on) `other`.
    pub fn is_based_on(&self, id: ActorId, other: ActorId) -> bool {
        self.get(id).is_some_and(|a| a.base == Some(other))
    }

    /// The level or actor directly below a walking actor (diagnostics).
    pub fn floor_actor(&mut self, id: ActorId) -> Option<HitActor> {
        let loc = self.actor(id).location;
        self.sweep(id, loc, Vec3::Z * -(FLOOR_PROBE + 2.0), &[]).map(|h| h.actor)
    }
}

/// Component-wise rotator sum (BaseRot + KeyRot).
pub fn add_rot(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
