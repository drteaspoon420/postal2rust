//! The physics world: the level's static collision (a p2collide [`World`]) plus dynamic actors
//! in a spatial hash, with the UE2 movement natives and their events.
//!
//! Semantics follow UE2's MoveActor / FarMoveActor:
//! - A cylinder actor sweeps from its location; it stops at the first blocking contact (world
//!   geometry when `bCollideWorld`, blocking actors when `bCollideActors`), backed off by a small
//!   skin. Blocking actor contacts send `Bump` to both actors.
//! - After every position change the `Touching` lists are refreshed: newly overlapping
//!   non-blocking colliding actors get `Touch` (both ways), separated ones `UnTouch`.
//! - The actor's `PhysicsVolume` (the highest priority PhysicsVolume containing its location,
//!   else the level default), head volume (pawns) and BSP zone are refreshed with
//!   `PhysicsVolumeChange` / `ActorLeavingVolume` / `PawnLeavingVolume` / `ActorEnteredVolume` /
//!   `PawnEnteredVolume`, `HeadVolumeChange` and `ZoneChange`.
//! - Actors based on a moving actor are carried with it (translation and rotation about it).
//! - Mesh and brush actors (movers) are encroachers: they move without sweeping, then every
//!   overlapped blocking cylinder actor is pushed out along the penetration normal; if that fails
//!   the encroacher gets `EncroachingOn`, and depending on its `MoverEncroachType` the move is
//!   undone (stop / return) or the victim gets `EncroachedBy` (crush / ignore).
//!
//! Script return values cannot be awaited inside a physics step, so `EncroachingOn` uses the
//! decision `Mover.EncroachingOn` makes for pawns with a controller (abort for stop/return types)
//! and the event is still delivered so script side effects (MakeGroupStop/Return, damage) happen.

use crate::actor::{Actor, Collider, VolumeInfo};
use crate::hash::SpatialHash;
use crate::shape::{cylinders_overlap, sweep_cylinder};
use crate::types::{ActorId, Event, HitActor, MoveHit, Physics, TraceHit};
use glam::Vec3;
use p2collide::world::rotator_matrix;
use p2collide::{Aabb, Shape, TraceFlags, World};
use std::sync::Arc;

/// Distance kept between a moving cylinder and what it hits.
pub const SKIN: f32 = 0.5;

/// Counters for diagnostics.
#[derive(Clone, Debug, Default)]
pub struct PhysStats {
    pub moves: u64,
    pub sweeps: u64,
    pub encroach_pushes: u64,
    pub encroach_blocked: u64,
    pub karma_ticks: u64,
    pub touch_events: u64,
}

pub struct PhysWorld {
    /// Static level collision (BSP, level static meshes, terrain).
    pub level: Arc<World>,
    actors: Vec<Option<Actor>>,
    free: Vec<ActorId>,
    pub(crate) hash: SpatialHash,
    pub(crate) events: Vec<Event>,
    moved: Vec<ActorId>,
    /// The level's DefaultPhysicsVolume actor (PhysicsVolume of actors in no other volume).
    pub default_volume: Option<ActorId>,
    /// Settings used when there is no default volume actor.
    pub default_volume_info: VolumeInfo,
    /// Actors below this Z get `FellOutOfWorld` (LevelInfo.KillZ in later engines; here a guard).
    pub kill_z: f32,
    pub stats: PhysStats,
}

/// Result of a movement native.
#[derive(Clone, Copy, Debug)]
pub struct MoveResult {
    /// False when the move was refused (encroachment) or could not start.
    pub ok: bool,
    pub hit: Option<MoveHit>,
}

impl PhysWorld {
    pub fn new(level: Arc<World>) -> PhysWorld {
        // Anything this far below the lowest geometry has fallen out of the world.
        let kill_z = level.bounds().map(|b| b.min.z - 4000.0).unwrap_or(-1.0e6);
        PhysWorld {
            level,
            actors: Vec::new(),
            free: Vec::new(),
            hash: SpatialHash::new(512.0),
            events: Vec::new(),
            moved: Vec::new(),
            default_volume: None,
            default_volume_info: VolumeInfo::default(),
            kill_z,
            stats: PhysStats::default(),
        }
    }

    // ------------------------------------------------------------------
    // Actor storage
    // ------------------------------------------------------------------

    /// Adds an actor. Its collider is placed, it enters the spatial hash, and its zone and
    /// physics volume are set without events (as at spawn). Touches with overlapping actors are
    /// reported like UE2's spawn-time touch check.
    pub fn add(&mut self, mut a: Actor) -> ActorId {
        a.collider.place(a.location, a.rotation);
        if let Some(t) = a.trace_collider.as_mut() {
            t.place(a.location, a.rotation);
        }
        let id = match self.free.pop() {
            Some(id) => {
                self.actors[id as usize] = Some(a);
                id
            }
            None => {
                self.actors.push(Some(a));
                (self.actors.len() - 1) as ActorId
            }
        };
        self.relink(id);
        self.update_volumes(id, false);
        self.update_touching(id);
        id
    }

    /// Removes an actor: untouches everything, detaches actors based on it.
    pub fn remove(&mut self, id: ActorId) {
        if self.get(id).is_none() {
            return;
        }
        let touching = std::mem::take(&mut self.actor_mut(id).touching);
        for o in touching {
            self.end_touch(id, o);
        }
        for b in self.based_actors(id) {
            self.set_base(b, None);
        }
        if let Some(base) = self.actor(id).base {
            self.events.push(Event::Detach { actor: base, other: id });
        }
        self.hash.remove(id);
        self.actors[id as usize] = None;
        self.free.push(id);
        if self.default_volume == Some(id) {
            self.default_volume = None;
        }
    }

    pub fn get(&self, id: ActorId) -> Option<&Actor> {
        self.actors.get(id as usize).and_then(|a| a.as_ref())
    }

    /// Mutable access. After changing location, rotation, collision size or flags directly,
    /// call [`PhysWorld::relink`] (or use the movement natives instead).
    pub fn get_mut(&mut self, id: ActorId) -> Option<&mut Actor> {
        self.actors.get_mut(id as usize).and_then(|a| a.as_mut())
    }

    pub(crate) fn actor(&self, id: ActorId) -> &Actor {
        self.actors[id as usize].as_ref().expect("live actor")
    }

    pub(crate) fn actor_mut(&mut self, id: ActorId) -> &mut Actor {
        self.actors[id as usize].as_mut().expect("live actor")
    }

    /// Ids of all live actors in insertion-slot order.
    pub fn ids(&self) -> Vec<ActorId> {
        (0..self.actors.len() as ActorId).filter(|&i| self.actors[i as usize].is_some()).collect()
    }

    pub fn len(&self) -> usize {
        self.actors.iter().filter(|a| a.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Finds the actor whose `user` handle is `user`.
    pub fn find_user(&self, user: u64) -> Option<ActorId> {
        (0..self.actors.len() as ActorId).find(|&i| self.actors[i as usize].as_ref().is_some_and(|a| a.user == user))
    }

    /// Re-places the collider and updates the spatial hash (after direct edits).
    pub fn relink(&mut self, id: ActorId) {
        let a = self.actor_mut(id);
        a.sleeping = false;
        let (loc, rot) = (a.location, a.rotation);
        a.collider.place(loc, rot);
        if let Some(t) = a.trace_collider.as_mut() {
            t.place(loc, rot);
        }
        let b = a.bounds();
        self.hash.insert(id, b);
        self.moved.push(id);
    }

    /// Number of relinks queued since the last `take_moved` (may contain duplicates).
    pub fn moved_count(&self) -> usize {
        self.moved.len()
    }

    /// Actors moved (relinked) since the last call, without duplicates.
    pub fn take_moved(&mut self) -> Vec<ActorId> {
        let mut v = std::mem::take(&mut self.moved);
        v.sort_unstable();
        v.dedup();
        v.retain(|&i| self.get(i).is_some());
        v
    }

    /// Takes the events produced since the last call, in order.
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Events produced since the last drain (not removed).
    pub fn pending_events(&self) -> &[Event] {
        &self.events
    }

    /// Volume settings governing an actor (its PhysicsVolume or the level default).
    pub fn volume_info(&self, id: ActorId) -> &VolumeInfo {
        self.get(id).and_then(|a| a.physics_volume).and_then(|v| self.get(v)).and_then(|v| v.volume.as_ref()).unwrap_or(&self.default_volume_info)
    }

    // ------------------------------------------------------------------
    // Collision rules
    // ------------------------------------------------------------------

    /// Does `b` block the movement of `a` (UE2 `IsBlockedBy`)?
    pub fn blocks(&self, a_id: ActorId, b_id: ActorId) -> bool {
        if a_id == b_id {
            return false;
        }
        let (a, b) = (self.actor(a_id), self.actor(b_id));
        if !a.collide_actors || !b.collide_actors {
            return false;
        }
        if b.owner == Some(a_id) || a.owner == Some(b_id) {
            return false;
        }
        let b_blocks = b.block_actors && (!a.is_player() || b.block_players);
        if !b_blocks {
            return false;
        }
        // Moves are non-zero-extent checks in UE2: an actor with bBlockNonZeroExtentTraces off
        // (DayBlocker on days it is not needed; script turns it on) does not block movement.
        if !b.block_nonzero_extent && !b.is_volume() {
            return false;
        }
        if b.world_geometry || b.is_mover() || b.is_volume() || !b.collider.is_cylinder() {
            // Karma bodies collide with geometry whatever bCollideWorld says (as in `sweep`).
            return a.collide_world || a.is_pawn() || a.physics == Physics::Karma;
        }
        a.block_actors
    }

    /// Would `a` and `b` touch (both collide, neither blocks the other, not both static)?
    fn touchable(&self, a_id: ActorId, b_id: ActorId) -> bool {
        if a_id == b_id {
            return false;
        }
        let (a, b) = (self.actor(a_id), self.actor(b_id));
        if !a.collide_actors || !b.collide_actors {
            return false;
        }
        let a_fixed = a.is_static || a.is_volume() || a.world_geometry;
        let b_fixed = b.is_static || b.is_volume() || b.world_geometry;
        if a_fixed && b_fixed {
            return false;
        }
        // Movers and other encroachers do not touch each other.
        if !a.collider.is_cylinder() && !b.collider.is_cylinder() {
            return false;
        }
        !self.blocks(a_id, b_id) && !self.blocks(b_id, a_id)
    }

    /// Geometric overlap of two actors' colliders.
    pub fn overlaps(&self, a_id: ActorId, b_id: ActorId) -> bool {
        let (a, b) = (self.actor(a_id), self.actor(b_id));
        if !a.bounds().overlaps(&b.bounds()) {
            return false;
        }
        match (&a.collider, &b.collider) {
            (Collider::Cylinder, Collider::Cylinder) => cylinders_overlap(a.location, a.collision_radius, a.collision_height, b.location, b.collision_radius, b.collision_height, 0.0),
            (Collider::Cylinder, Collider::Mesh { placed, .. }) => placed.overlaps(a.location, &a.cylinder()),
            (Collider::Mesh { placed, .. }, Collider::Cylinder) => placed.overlaps(b.location, &b.cylinder()),
            (Collider::Cylinder, Collider::Brush { placed, .. }) => placed.overlaps_cylinder(a.location, a.collision_radius, a.collision_height),
            (Collider::Brush { placed, .. }, Collider::Cylinder) => placed.overlaps_cylinder(b.location, b.collision_radius, b.collision_height),
            _ => true,
        }
    }

    // ------------------------------------------------------------------
    // Sweeps
    // ------------------------------------------------------------------

    /// Sweeps actor `id`'s cylinder from `start` by `delta` (world if `bCollideWorld`, blocking
    /// actors if `bCollideActors`), ignoring `ignore`. Returns the first contact.
    pub fn sweep(&mut self, id: ActorId, start: Vec3, delta: Vec3, ignore: &[ActorId]) -> Option<MoveHit> {
        let a = self.actor(id);
        let shape = a.cylinder();
        let (r, h) = (a.collision_radius, a.collision_height);
        // Karma bodies collide with the world through Karma, whatever bCollideWorld says.
        let (collide_world, collide_actors) = (a.collide_world || a.physics == Physics::Karma, a.collide_actors);
        let is_pawn = a.is_pawn();
        self.stats.sweeps += 1;
        let mut best: Option<MoveHit> = None;
        let consider = |h: MoveHit, best: &mut Option<MoveHit>| {
            let better = match best {
                None => true,
                Some(b) => h.t < b.t || (h.t == b.t && h.start_solid && (!b.start_solid || h.depth > b.depth)),
            };
            if better {
                *best = Some(h);
            }
        };
        if collide_world && delta.length_squared() > 0.0 {
            if let Some(hit) = self.level.trace(start, start + delta, &shape, TraceFlags::ALL) {
                consider(MoveHit { actor: HitActor::World, t: hit.t, normal: hit.normal, start_solid: hit.start_solid, depth: hit.depth }, &mut best);
            }
        }
        if collide_actors {
            let qb = Aabb::swept(start, start + delta, shape.extent() + Vec3::splat(1.0));
            let cands = self.hash.query_vec(qb);
            for b in cands {
                if ignore.contains(&b) || self.actor(b).base == Some(id) || !self.blocks(id, b) {
                    continue;
                }
                let o = self.actor(b);
                // A pawn runs into a Karma body's mesh (UE2 collides KActors on their static
                // mesh), not the cylinder our Karma stand-in moves as (parked cars: r 150).
                let col = match &o.trace_collider {
                    Some(t) if is_pawn => t,
                    _ => &o.collider,
                };
                let c = match col {
                    Collider::Cylinder => sweep_cylinder(start, delta, r, h, o.location, o.collision_radius, o.collision_height).map(|c| (c.t, c.normal, c.start_solid, c.depth)),
                    Collider::Mesh { placed, .. } => placed.trace(start, start + delta, &shape).map(|c| (c.t, c.normal, c.start_solid, c.depth)),
                    Collider::Brush { placed, .. } => placed.mesh.trace(start, start + delta, &shape).map(|c| (c.t, c.normal, c.start_solid, c.depth)),
                };
                if let Some((t, normal, start_solid, depth)) = c {
                    consider(MoveHit { actor: HitActor::Actor(b), t, normal, start_solid, depth }, &mut best);
                }
            }
        }
        best
    }

    /// Sweeps and returns the end position backed off from the contact by [`SKIN`].
    pub(crate) fn sweep_pos(&mut self, id: ActorId, start: Vec3, delta: Vec3, ignore: &[ActorId]) -> (Vec3, Option<MoveHit>) {
        let len = delta.length();
        if len < 1e-4 {
            return (start, None);
        }
        match self.sweep(id, start, delta, ignore) {
            None => (start + delta, None),
            Some(h) if h.start_solid => {
                // Overlapping already: depenetrate along the contact normal.
                let r = self.actor(id).collision_radius.max(1.0);
                (start + h.normal * (h.depth + SKIN).min(r), Some(h))
            }
            Some(h) => {
                let back = (SKIN / (-delta.normalize().dot(h.normal)).max(0.05)).min(len);
                let t = ((h.t * len - back) / len).max(0.0);
                (start + delta * t, Some(h))
            }
        }
    }

    /// Moves along `delta` from `pos`, sliding along up to four blocking surfaces. With
    /// `walking`, non-walkable surfaces are treated as vertical walls. Hits are appended.
    pub(crate) fn slide(&mut self, id: ActorId, pos: &mut Vec3, mut delta: Vec3, walking: bool, walkable_z: f32, hits: &mut Vec<MoveHit>, ignore: &[ActorId]) {
        let original = delta;
        let mut normals: Vec<Vec3> = Vec::new();
        for _ in 0..4 {
            let before = *pos;
            let (p, hit) = self.sweep_pos(id, *pos, delta, ignore);
            *pos = p;
            let Some(h) = hit else { break };
            hits.push(h);
            let moved = *pos - before;
            let mut n = h.normal;
            if walking && n.z.abs() < walkable_z {
                n = Vec3::new(n.x, n.y, 0.0).normalize_or_zero();
                if n == Vec3::ZERO {
                    break;
                }
            }
            normals.push(n);
            let mut rest = delta - moved;
            rest -= n * rest.dot(n);
            if normals.len() >= 2 {
                let a = normals[normals.len() - 2];
                let dir = a.cross(n);
                if dir.length_squared() > 1e-6 {
                    let dir = dir.normalize();
                    rest = dir * rest.dot(dir);
                }
            }
            if rest.dot(original) <= 0.0 || rest.length_squared() < 1e-4 {
                break;
            }
            delta = rest;
        }
    }

    // ------------------------------------------------------------------
    // Committing moves (events)
    // ------------------------------------------------------------------

    /// Places a cylinder actor at `to` (already validated by sweeps) and runs the post-move
    /// bookkeeping: hash, carried actors, touches, volumes, zone, and `Bump` for actor hits.
    pub(crate) fn commit(&mut self, id: ActorId, to: Vec3, rot: Option<[i32; 3]>, hits: &[MoveHit]) {
        self.stats.moves += 1;
        let (from, old_rot) = {
            let a = self.actor(id);
            (a.location, a.rotation)
        };
        {
            let a = self.actor_mut(id);
            a.location = to;
            if let Some(r) = rot {
                a.rotation = r;
            }
        }
        self.relink(id);
        let new_rot = self.actor(id).rotation;
        self.carry_based(id, from, old_rot, to, new_rot);
        let mut bumped: Vec<ActorId> = Vec::new();
        for h in hits {
            if let HitActor::Actor(o) = h.actor {
                if !bumped.contains(&o) && self.get(o).is_some() {
                    bumped.push(o);
                    let (v_me, v_o) = (self.actor(id).velocity, self.actor(o).velocity);
                    self.events.push(Event::Bump { actor: o, other: id, other_velocity: v_me });
                    self.events.push(Event::Bump { actor: id, other: o, other_velocity: v_o });
                }
            }
        }
        self.update_touching(id);
        self.update_volumes(id, true);
        if to.z < self.kill_z {
            self.events.push(Event::FellOutOfWorld { actor: id });
            // Stop simulating it (script usually destroys it in FellOutOfWorld).
            if self.actor(id).physics != Physics::None {
                self.set_physics(id, Physics::None);
                self.actor_mut(id).velocity = Vec3::ZERO;
            }
        }
    }

    /// Moves every actor based on `id` along with it.
    fn carry_based(&mut self, id: ActorId, from: Vec3, old_rot: [i32; 3], to: Vec3, new_rot: [i32; 3]) {
        if from == to && old_rot == new_rot {
            return;
        }
        let based = self.based_actors(id);
        if based.is_empty() {
            return;
        }
        let r_old = rotator_matrix(old_rot);
        let r_new = rotator_matrix(new_rot);
        let rel = r_new * r_old.transpose();
        let dyaw = crate::types::rot::delta(old_rot[1], new_rot[1]);
        for b in based {
            let (bl, br) = {
                let a = self.actor(b);
                (a.location, a.rotation)
            };
            let target = to + rel * (bl - from);
            let nrot = [br[0], br[1] + dyaw, br[2]];
            if self.actor(b).collider.is_cylinder() {
                let (p, hit) = self.sweep_pos(b, bl, target - bl, &[id]);
                let hits: Vec<MoveHit> = hit.into_iter().collect();
                self.commit(b, p, Some(nrot), &hits);
            } else {
                self.move_encroacher(b, target, nrot);
            }
        }
    }

    /// Actors whose `Base` is `id`.
    pub fn based_actors(&self, id: ActorId) -> Vec<ActorId> {
        (0..self.actors.len() as ActorId).filter(|&i| self.actors[i as usize].as_ref().is_some_and(|a| a.base == Some(id))).collect()
    }

    fn begin_touch(&mut self, a: ActorId, b: ActorId) {
        self.actor_mut(a).touching.push(b);
        if !self.actor(b).touching.contains(&a) {
            self.actor_mut(b).touching.push(a);
        }
        self.stats.touch_events += 2;
        self.events.push(Event::Touch { actor: a, other: b });
        self.events.push(Event::Touch { actor: b, other: a });
    }

    fn end_touch(&mut self, a: ActorId, b: ActorId) {
        if let Some(x) = self.get_mut(a) {
            x.touching.retain(|&o| o != b);
        }
        if let Some(x) = self.get_mut(b) {
            x.touching.retain(|&o| o != a);
        }
        self.events.push(Event::UnTouch { actor: a, other: b });
        self.events.push(Event::UnTouch { actor: b, other: a });
    }

    /// Recomputes `Touching` for `id` against nearby actors, sending Touch / UnTouch.
    pub fn update_touching(&mut self, id: ActorId) {
        let mut now: Vec<ActorId> = Vec::new();
        if self.actor(id).collide_actors {
            let qb = self.actor(id).bounds().expand(Vec3::splat(1.0));
            for b in self.hash.query_vec(qb) {
                if self.touchable(id, b) && self.overlaps(id, b) {
                    now.push(b);
                }
            }
        }
        let old = self.actor(id).touching.clone();
        for o in old {
            if !now.contains(&o) {
                self.end_touch(id, o);
            }
        }
        for b in now {
            if !self.actor(id).touching.contains(&b) {
                self.begin_touch(id, b);
            }
        }
    }

    /// The PhysicsVolume containing `p`: highest priority brush PhysicsVolume, else the default.
    pub fn volume_at(&mut self, p: Vec3) -> Option<ActorId> {
        let mut best: Option<(i32, ActorId)> = None;
        for v in self.hash.query_vec(Aabb { min: p, max: p }) {
            let a = self.actor(v);
            let Some(info) = &a.volume else { continue };
            if !info.physics_volume {
                continue;
            }
            let Collider::Brush { placed, .. } = &a.collider else { continue };
            if placed.contains_point(p) && best.is_none_or(|(pr, _)| info.priority > pr) {
                best = Some((info.priority, v));
            }
        }
        best.map(|b| b.1).or(self.default_volume)
    }

    /// Refreshes PhysicsVolume, HeadVolume and zone (with events when `events`).
    pub fn update_volumes(&mut self, id: ActorId, events: bool) {
        let (loc, head, is_pawn, skip) = {
            let a = self.actor(id);
            (a.location, a.head(), a.is_pawn(), a.is_volume() || a.is_static)
        };
        if skip {
            return;
        }
        let nv = self.volume_at(loc);
        let old = self.actor(id).physics_volume;
        if nv != old {
            if events {
                if let Some(o) = old {
                    self.events.push(Event::ActorLeavingVolume { actor: o, other: id });
                    if is_pawn {
                        self.events.push(Event::PawnLeavingVolume { actor: o, other: id });
                    }
                }
            }
            self.actor_mut(id).physics_volume = nv;
            if events {
                self.events.push(Event::PhysicsVolumeChange { actor: id, volume: nv });
                if let Some(n) = nv {
                    self.events.push(Event::ActorEnteredVolume { actor: n, other: id });
                    if is_pawn {
                        self.events.push(Event::PawnEnteredVolume { actor: n, other: id });
                    }
                }
            }
        }
        if is_pawn {
            let hv = self.volume_at(head);
            if hv != self.actor(id).head_volume {
                self.actor_mut(id).head_volume = hv;
                if events {
                    self.events.push(Event::HeadVolumeChange { actor: id, volume: hv });
                }
            }
        }
        let zone = self.level.bsp.point_zone(loc);
        if zone != self.actor(id).zone {
            self.actor_mut(id).zone = zone;
            if events {
                let zone_export = self.level.zones.get(zone).copied().flatten();
                self.events.push(Event::ZoneChange { actor: id, zone, zone_export });
            }
        }
    }

    // ------------------------------------------------------------------
    // Movement natives
    // ------------------------------------------------------------------

    /// `Move(Delta)`: sweeps by `delta`, stopping at the first blocking contact.
    pub fn move_by(&mut self, id: ActorId, delta: Vec3) -> MoveResult {
        let a = self.actor(id);
        if !a.collider.is_cylinder() {
            let (to, rot) = (a.location + delta, a.rotation);
            let ok = self.move_encroacher(id, to, rot);
            return MoveResult { ok, hit: None };
        }
        let start = a.location;
        let (p, hit) = self.sweep_pos(id, start, delta, &[]);
        let hits: Vec<MoveHit> = hit.into_iter().collect();
        self.commit(id, p, None, &hits);
        MoveResult { ok: true, hit }
    }

    /// `MoveSmooth(Delta)`: like Move, but slides along what it hits (walking actors step up
    /// small ledges). The first world or non-pawn contact sends `HitWall`. Returns whether the
    /// actor moved.
    pub fn move_smooth(&mut self, id: ActorId, delta: Vec3) -> MoveResult {
        let a = self.actor(id);
        if !a.collider.is_cylinder() {
            return self.move_by(id, delta);
        }
        let start = a.location;
        let walking = a.physics == Physics::Walking;
        let wz = a.pawn.as_ref().map(|p| p.walkable_floor_z).unwrap_or(0.7);
        let mut pos = start;
        let mut hits = Vec::new();
        self.slide(id, &mut pos, delta, walking, wz, &mut hits, &[]);
        if walking && !hits.is_empty() && (pos - start).truncate().length() < delta.truncate().length() * 0.9 {
            if let Some(p) = self.try_step_up(id, start, delta) {
                if (p - start).truncate().length() > (pos - start).truncate().length() {
                    pos = p;
                }
            }
        }
        if let Some(h) = hits.first() {
            self.hit_wall(id, h);
        }
        self.commit(id, pos, None, &hits);
        MoveResult { ok: (pos - start).length_squared() > 1e-6, hit: hits.first().copied() }
    }

    /// Sends `HitWall` for a contact with the world or a non-pawn blocker.
    pub(crate) fn hit_wall(&mut self, id: ActorId, h: &MoveHit) {
        let wall = h.actor.actor();
        if wall.is_some_and(|w| self.get(w).is_some_and(|o| o.is_pawn())) {
            return;
        }
        self.events.push(Event::HitWall { actor: id, normal: h.normal, wall });
    }

    /// `SetLocation(NewLocation)`: teleports. Cylinder actors may be nudged out of slight wall
    /// penetration (FindSpot); they fail if still inside the world or overlapping a blocking
    /// actor. Encroachers run the encroachment check.
    pub fn set_location(&mut self, id: ActorId, to: Vec3) -> bool {
        let a = self.actor(id);
        if !a.collider.is_cylinder() {
            let rot = a.rotation;
            return self.move_encroacher(id, to, rot);
        }
        let mut p = to;
        if a.collide_world {
            let shape = a.cylinder();
            match self.find_spot(p, &shape) {
                Some(q) => p = q,
                None => return false,
            }
        }
        if self.actor(id).collide_actors && self.actor(id).block_actors {
            let shape_e = self.actor(id).cylinder().extent();
            let (r, h) = (self.actor(id).collision_radius, self.actor(id).collision_height);
            for b in self.hash.query_vec(Aabb { min: p - shape_e, max: p + shape_e }) {
                if b == id || !self.blocks(id, b) {
                    continue;
                }
                let o = self.actor(b);
                let hit = match &o.collider {
                    Collider::Cylinder => cylinders_overlap(p, r, h, o.location, o.collision_radius, o.collision_height, -0.01),
                    Collider::Mesh { placed, .. } => placed.overlaps(p, &Shape::Cylinder { radius: r.max(0.01), half_height: h.max(0.01) }),
                    Collider::Brush { placed, .. } => placed.overlaps_cylinder(p, r, h),
                };
                if hit && self.actor(id).is_pawn() {
                    return false;
                }
            }
        }
        self.commit(id, p, None, &[]);
        true
    }

    /// Nudges `p` out of world geometry it slightly penetrates (at most half the shape's smaller
    /// extent, like UE2's FindSpot); None if it cannot be freed.
    pub fn find_spot(&self, p: Vec3, shape: &Shape) -> Option<Vec3> {
        let e = shape.extent();
        let max_adjust = 0.5 * e.x.min(e.z).max(1.0);
        let mut q = p;
        for _ in 0..4 {
            let Some((n, depth)) = self.level.penetration(q, shape) else { return Some(q) };
            q += n * (depth + SKIN);
            if q.distance(p) > max_adjust {
                return None;
            }
        }
        (!self.level.encroaches(q, shape)).then_some(q)
    }

    /// `SetRotation(NewRotation)`. Encroachers check encroachment; cylinders just rotate.
    pub fn set_rotation(&mut self, id: ActorId, rot: [i32; 3]) -> bool {
        let a = self.actor(id);
        if a.rotation == rot {
            return true;
        }
        if !a.collider.is_cylinder() {
            let loc = a.location;
            return self.move_encroacher(id, loc, rot);
        }
        let (loc, old) = (a.location, a.rotation);
        self.actor_mut(id).rotation = rot;
        self.carry_based(id, loc, old, loc, rot);
        true
    }

    /// `SetCollision(bCollideActors, bBlockActors, bBlockPlayers)`.
    pub fn set_collision(&mut self, id: ActorId, collide_actors: bool, block_actors: bool, block_players: bool) {
        {
            let a = self.actor_mut(id);
            a.collide_actors = collide_actors;
            a.block_actors = block_actors;
            a.block_players = block_players;
        }
        self.update_touching(id);
    }

    /// `SetCollisionSize(Radius, Height)`. Always succeeds (as in UE2); touches refresh.
    pub fn set_collision_size(&mut self, id: ActorId, radius: f32, height: f32) -> bool {
        {
            let a = self.actor_mut(id);
            a.collision_radius = radius;
            a.collision_height = height;
        }
        self.relink(id);
        self.update_touching(id);
        true
    }

    /// `SetBase(NewBase)`: `Detach` on the old base, `Attach` on the new one, `BaseChange` on
    /// the actor.
    pub fn set_base(&mut self, id: ActorId, base: Option<ActorId>) {
        let old = self.actor(id).base;
        if old == base || base == Some(id) {
            return;
        }
        self.actor_mut(id).base = base;
        if let Some(o) = old.filter(|o| self.get(*o).is_some()) {
            self.events.push(Event::Detach { actor: o, other: id });
        }
        if let Some(n) = base {
            self.events.push(Event::Attach { actor: n, other: id });
        }
        self.events.push(Event::BaseChange { actor: id });
    }

    /// `SetPhysics(NewPhysics)`.
    pub fn set_physics(&mut self, id: ActorId, p: Physics) {
        let a = self.actor_mut(id);
        if a.physics == p {
            return;
        }
        a.physics = p;
        a.sleeping = false;
        if p != Physics::Walking && p != Physics::Ladder {
            if let Some(pw) = a.pawn.as_mut() {
                pw.floor = Vec3::Z;
            }
        }
    }

    // ------------------------------------------------------------------
    // Encroachment (movers, mesh actors)
    // ------------------------------------------------------------------

    /// Moves a mesh/brush actor to (`to`, `rot`) with UE2 encroachment: overlapped blocking
    /// cylinders are pushed out of the way; if one cannot be pushed, `EncroachingOn` is sent and
    /// the move is undone for stop/return movers (returns false) or the victim gets
    /// `EncroachedBy` otherwise. Actors based on the encroacher are carried.
    pub fn move_encroacher(&mut self, id: ActorId, to: Vec3, rot: [i32; 3]) -> bool {
        let (from, old_rot) = {
            let a = self.actor(id);
            (a.location, a.rotation)
        };
        if from == to && old_rot == rot {
            return true;
        }
        {
            let a = self.actor_mut(id);
            a.location = to;
            a.rotation = rot;
            a.collider.place(to, rot);
        }
        let blocking = self.actor(id).collide_actors && self.actor(id).block_actors;
        if blocking {
            let qb = self.actor(id).bounds().expand(Vec3::splat(2.0));
            let cands = self.hash.query_vec(qb);
            for c in cands {
                if c == id || self.get(c).is_none() {
                    continue;
                }
                let o = self.actor(c);
                if !o.collider.is_cylinder() || o.base == Some(id) || o.is_static || !o.collide_actors || !self.blocks(c, id) {
                    continue;
                }
                if !self.overlaps(id, c) {
                    continue;
                }
                if self.push_out(id, c) {
                    self.stats.encroach_pushes += 1;
                    continue;
                }
                self.stats.encroach_blocked += 1;
                self.events.push(Event::EncroachingOn { actor: id, other: c });
                let abort = match self.actor(id).mover.as_ref().map(|m| m.encroach) {
                    Some(crate::actor::EncroachType::Stop) | Some(crate::actor::EncroachType::Return) => self.actor(c).is_pawn(),
                    _ => false,
                };
                if abort {
                    let a = self.actor_mut(id);
                    a.location = from;
                    a.rotation = old_rot;
                    a.collider.place(from, old_rot);
                    return false;
                }
                self.events.push(Event::EncroachedBy { actor: c, other: id });
            }
        }
        self.relink(id);
        self.carry_based(id, from, old_rot, to, rot);
        self.update_touching(id);
        self.update_volumes(id, true);
        true
    }

    /// Pushes cylinder actor `c` out of encroacher `e` along the penetration normal.
    fn push_out(&mut self, e: ActorId, c: ActorId) -> bool {
        let (loc, shape, r) = {
            let o = self.actor(c);
            (o.location, o.cylinder(), o.collision_radius)
        };
        let pen = match &self.actor(e).collider {
            Collider::Mesh { placed, .. } => placed.penetration(loc, &shape),
            Collider::Brush { placed, .. } => placed.mesh.penetration(loc, &shape),
            Collider::Cylinder => None,
        };
        let (n, depth) = pen.unwrap_or_else(|| {
            let d = loc - self.actor(e).location;
            (Vec3::new(d.x, d.y, 0.0).normalize_or(Vec3::X), r)
        });
        // Push mostly sideways (UE2 movers shove pawns horizontally; lifts carry based pawns).
        let mut dir = Vec3::new(n.x, n.y, n.z.max(0.0));
        if dir.length_squared() < 1e-6 {
            dir = Vec3::Z;
        }
        let dir = dir.normalize();
        let push = dir * (depth / dir.dot(n).max(0.2) + SKIN * 2.0).min(r * 2.0 + 8.0);
        let (p, hit) = self.sweep_pos(c, loc, push, &[e]);
        let hits: Vec<MoveHit> = hit.into_iter().collect();
        let old = loc;
        self.actor_mut(c).location = p;
        self.relink(c);
        if self.overlaps(e, c) {
            self.actor_mut(c).location = old;
            self.relink(c);
            return false;
        }
        self.actor_mut(c).location = old;
        self.relink(c);
        self.actor_mut(c).pushed += 1;
        self.commit(c, p, None, &hits);
        true
    }

    // ------------------------------------------------------------------
    // Traces and queries
    // ------------------------------------------------------------------

    /// `Trace(HitLocation, HitNormal, End, Start, bTraceActors, Extent)`: the first hit of a
    /// point (zero extent) or box (extent = half size) from `start` to `end`. World geometry
    /// always counts; actors when `trace_actors` and they collide and block this kind of trace
    /// (`bBlockZeroExtentTraces` / `bBlockNonZeroExtentTraces`; non-blocking actors only when
    /// `bProjTarget`). `source` and its owner are ignored.
    pub fn trace(&mut self, start: Vec3, end: Vec3, extent: Vec3, trace_actors: bool, source: Option<ActorId>) -> Option<TraceHit> {
        let mut hits = self.trace_all(start, end, extent, trace_actors, true, source, true, false);
        hits.truncate(1);
        hits.pop()
    }

    /// All hits along a trace, nearest first. `with_world` includes the level (first contact
    /// only); `stop_at_world` drops actor hits beyond it.
    fn trace_all(&mut self, start: Vec3, end: Vec3, extent: Vec3, trace_actors: bool, with_world: bool, source: Option<ActorId>, nearest_only: bool, all_colliding: bool) -> Vec<TraceHit> {
        let d = end - start;
        let zero = extent == Vec3::ZERO;
        let shape = Shape::Box(extent);
        let mut out: Vec<TraceHit> = Vec::new();
        if with_world {
            if let Some(h) = self.level.trace(start, end, &shape, TraceFlags::ALL) {
                out.push(TraceHit { actor: HitActor::World, t: h.t, location: h.location, normal: h.normal, material: h.material, level_export: match h.source {
                    p2collide::Source::Actor(e) => Some(e),
                    _ => None,
                } });
            }
        }
        if trace_actors || with_world {
            let src_owner = source.and_then(|s| self.get(s)).and_then(|s| s.owner);
            let limit = out.first().map(|h| h.t).unwrap_or(1.0);
            let qb = Aabb::swept(start, start + d * limit, extent + Vec3::splat(1.0));
            for b in self.hash.query_vec(qb) {
                if Some(b) == source || Some(b) == src_owner {
                    continue;
                }
                let o = self.actor(b);
                // Without bTraceActors only world geometry actors count (UE2: bWorldGeometry
                // static mesh actors such as the UrineBucket grave mounds act as level geometry).
                if !trace_actors && !o.world_geometry {
                    continue;
                }
                if o.owner.is_some() && o.owner == source {
                    continue;
                }
                let kind_ok = if zero { o.block_zero_extent } else { o.block_nonzero_extent };
                // `TraceActors` (all_colliding) reports every colliding actor the line touches,
                // blocking or not (fire streaks/puddles, fluids), like UE2's MultiLineCheck;
                // `Trace` only stops at blocking actors.
                if !o.collide_actors || (!all_colliding && (!kind_ok || !(o.block_actors || o.proj_target || o.world_geometry))) {
                    continue;
                }
                let c = match o.trace_collider.as_ref().unwrap_or(&o.collider) {
                    Collider::Cylinder => {
                        let r = extent.x.max(extent.y);
                        // A line (zero extent) that starts inside a cylinder does not hit it, as in
                        // UE2's primitive line check: script traces from an actor's own centre
                        // (Trace(.., End, Start=Other.Location)) and expects to pass out of it.
                        sweep_cylinder(start, d, r, extent.z, o.location, o.collision_radius, o.collision_height).filter(|c| !(zero && c.start_solid)).map(|c| (c.t, c.normal))
                    }
                    Collider::Mesh { placed, .. } => placed.trace(start, end, &shape).map(|c| (c.t, c.normal)),
                    Collider::Brush { placed, .. } => placed.mesh.trace(start, end, &shape).map(|c| (c.t, c.normal)),
                };
                if let Some((t, normal)) = c {
                    if !nearest_only || t <= limit {
                        out.push(TraceHit { actor: HitActor::Actor(b), t, location: start + d * t, normal, material: None, level_export: None });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.t.total_cmp(&b.t));
        out
    }

    /// `FastTrace(End, Start)`: true if no world geometry (level, movers, blocking world-geometry
    /// actors) blocks the line.
    pub fn fast_trace(&mut self, end: Vec3, start: Vec3) -> bool {
        if self.level.line(start, end).is_some() {
            return false;
        }
        let qb = Aabb::swept(start, end, Vec3::splat(1.0));
        for b in self.hash.query_vec(qb) {
            let o = self.actor(b);
            if !(o.world_geometry || o.is_mover()) || !o.collide_actors || !o.block_actors || !o.block_zero_extent {
                continue;
            }
            let hit = match &o.collider {
                Collider::Mesh { placed, .. } => placed.trace(start, end, &Shape::POINT).is_some(),
                Collider::Brush { placed, .. } => placed.mesh.trace(start, end, &Shape::POINT).is_some(),
                Collider::Cylinder => sweep_cylinder(start, end - start, 0.0, 0.0, o.location, o.collision_radius, o.collision_height).is_some(),
            };
            if hit {
                return false;
            }
        }
        true
    }

    /// AI navigation sweep: a cylinder against the level plus the non-moving blockers that a
    /// walking pawn cannot pass (BlockingVolumes and other static / world-geometry blocking
    /// actors). Pawns, Karma props and movers are left out: AI steers around the first two as
    /// they move and opens doors by bumping them. Returns (t, normal, start_solid) of the first contact.
    pub fn nav_sweep(&mut self, start: Vec3, end: Vec3, radius: f32, half_height: f32) -> Option<(f32, Vec3, bool)> {
        let shape = Shape::Cylinder { radius, half_height };
        let mut best: Option<(f32, Vec3, bool)> = self.level.trace(start, end, &shape, TraceFlags::ALL).map(|h| (h.t, h.normal, h.start_solid));
        let d = end - start;
        let qb = Aabb::swept(start, end, shape.extent() + Vec3::splat(1.0));
        for b in self.hash.query_vec(qb) {
            let o = self.actor(b);
            if !o.collide_actors || !o.block_actors || !o.block_players || o.is_pawn() || o.physics == Physics::Karma {
                continue;
            }
            // Movers (doors, lifts) are left out: AI opens doors by walking into them, so a
            // closed door must neither block reachability nor deflect steering.
            if o.is_mover() || !(o.is_static || o.world_geometry || o.volume.is_some()) {
                continue;
            }
            let c = match o.trace_collider.as_ref().unwrap_or(&o.collider) {
                Collider::Cylinder => sweep_cylinder(start, d, radius, half_height, o.location, o.collision_radius, o.collision_height).map(|c| (c.t, c.normal, c.start_solid)),
                Collider::Mesh { placed, .. } => placed.trace(start, end, &shape).map(|c| (c.t, c.normal, c.start_solid)),
                Collider::Brush { placed, .. } => placed.mesh.trace(start, end, &shape).map(|c| (c.t, c.normal, c.start_solid)),
            };
            if let Some(c) = c {
                if best.is_none_or(|b| c.0 < b.0) {
                    best = Some(c);
                }
            }
        }
        best
    }

    /// `TraceActors(class, Actor, HitLoc, HitNorm, End, Start, Extent)`: every actor hit along
    /// the trace, nearest first, up to the first world hit.
    pub fn trace_actors(&mut self, end: Vec3, start: Vec3, extent: Vec3, source: Option<ActorId>) -> Vec<TraceHit> {
        let mut v = self.trace_all(start, end, extent, true, true, source, true, true);
        if let Some(k) = v.iter().position(|h| h.actor == HitActor::World) {
            v.truncate(k);
        }
        v
    }

    /// `RadiusActors(class, Actor, Radius, Loc)`: actors whose location is within `radius` plus
    /// their own `CollisionRadius` (as in UE2, so a small searcher still finds a wide target).
    pub fn radius_actors(&mut self, center: Vec3, radius: f32) -> Vec<ActorId> {
        // The hash holds each actor's bounds (location +- its radius), so this box finds them.
        let qb = Aabb { min: center - Vec3::splat(radius), max: center + Vec3::splat(radius) };
        let mut v: Vec<ActorId> = self
            .hash
            .query_vec(qb)
            .into_iter()
            .filter(|&a| {
                let ac = self.actor(a);
                ac.location.distance(center) <= radius + ac.collision_radius.max(0.0)
            })
            .collect();
        v.sort();
        v
    }

    /// `CollidingActors(class, Actor, Radius, Loc)`: colliding actors within `radius`.
    pub fn colliding_actors(&mut self, center: Vec3, radius: f32) -> Vec<ActorId> {
        let mut v = self.radius_actors(center, radius);
        v.retain(|&a| self.actor(a).collide_actors);
        v
    }

    /// `VisibleActors(class, Actor, Radius, Loc)`: non-hidden actors within `radius` (0 = any
    /// distance) with a clear `FastTrace` from `center`.
    pub fn visible_actors(&mut self, center: Vec3, radius: f32) -> Vec<ActorId> {
        let cands = if radius > 0.0 { self.radius_actors(center, radius) } else { self.ids() };
        cands.into_iter().filter(|&a| !self.actor(a).hidden && { let l = self.actor(a).location; self.fast_trace(l, center) }).collect()
    }

    /// `VisibleCollidingActors(class, Actor, Radius, Loc, bIgnoreHidden)`.
    pub fn visible_colliding_actors(&mut self, center: Vec3, radius: f32, ignore_hidden: bool) -> Vec<ActorId> {
        let cands = self.colliding_actors(center, radius);
        cands.into_iter().filter(|&a| (!ignore_hidden || !self.actor(a).hidden) && { let l = self.actor(a).location; self.fast_trace(l, center) }).collect()
    }

    /// `TouchingActors`.
    pub fn touching_actors(&self, id: ActorId) -> Vec<ActorId> {
        self.get(id).map(|a| a.touching.clone()).unwrap_or_default()
    }

    /// True if a cylinder of the actor's size fits at `p` (no world or blocking actor overlap).
    pub fn fits(&mut self, id: ActorId, p: Vec3) -> bool {
        self.blocker_at(id, p).is_none()
    }

    /// What prevents the actor from standing at `p`: the level, a blocking actor, or None.
    pub fn blocker_at(&mut self, id: ActorId, p: Vec3) -> Option<HitActor> {
        let a = self.actor(id);
        let shape = a.cylinder();
        if a.collide_world && self.level.encroaches(p, &shape) {
            return Some(HitActor::World);
        }
        let (r, h) = (a.collision_radius, a.collision_height);
        let e = shape.extent();
        for b in self.hash.query_vec(Aabb { min: p - e, max: p + e }) {
            if b == id || !self.blocks(id, b) {
                continue;
            }
            let o = self.actor(b);
            let hit = match &o.collider {
                Collider::Cylinder => cylinders_overlap(p, r, h, o.location, o.collision_radius, o.collision_height, -0.01),
                Collider::Mesh { placed, .. } => placed.overlaps(p, &shape),
                Collider::Brush { placed, .. } => placed.overlaps_cylinder(p, r, h),
            };
            if hit {
                return Some(HitActor::Actor(b));
            }
        }
        None
    }
}
