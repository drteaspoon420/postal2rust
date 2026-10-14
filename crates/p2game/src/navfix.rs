//! Collision-aware AI reachability and steering helpers (UE2 `APawn::walkReachable` /
//! `pointReachable`, simplified).
//!
//! The VM's default reachability ([`uscript::nav::StraightLine`]) only compares heights, so
//! `PointReachable` / `ActorReachable` said yes through walls and buildings. Postal's
//! LambController.SetActorTarget then walks straight at the goal instead of asking the path
//! network, and the NPC pushes into the wall until its move times out (playtest: "NPCs run into
//! walls and don't walk around obstacles").
//!
//! [`LevelReach`] tests a walk the way UE2 does: the pawn's collision cylinder is swept along
//! the ground toward the destination in short steps; each step may climb up to MaxStepHeight
//! and must find floor below it (no long drops). It sweeps against the level and the
//! non-moving blockers of the physics world (BlockingVolumes, static blocking actors, movers):
//! pawns and Karma props are steered around, not routed around.
//!
//! `P2GAME_NO_NAVFIX=1` restores the old behaviour (A/B).

use glam::Vec3;
use uscript::nav::{Mover, Reachability};
use uscript::vm::Vm;

/// Horizontal step per sweep segment (UU).
const STEP: f32 = 96.0;
/// Highest drop a walker accepts without counting the point as unreachable.
const MAX_DROP: f32 = 200.0;
/// Longest straight walk tested (UE2 caps reachability tests at about this distance).
const MAX_DIST: f32 = 3000.0;
/// Riser re-climbs allowed in one walk test (a flight of stairs is about 10 to 20 risers).
const MAX_CLIMBS: u32 = 48;

pub fn disabled() -> bool {
    std::env::var_os("P2GAME_NO_NAVFIX").is_some()
}

thread_local! {
    /// Collision reachability tests run / skipped because the physics world was busy (probes).
    pub static REACH_TESTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub static REACH_FALLBACK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Cylinder sweep: (start, end, radius, half height) -> first contact (t, normal, start_solid).
pub trait Sweep {
    fn sweep(&mut self, start: Vec3, end: Vec3, radius: f32, half_height: f32) -> Option<(f32, Vec3, bool)>;
}

impl Sweep for p2collide::World {
    fn sweep(&mut self, start: Vec3, end: Vec3, radius: f32, half_height: f32) -> Option<(f32, Vec3, bool)> {
        self.trace(start, end, &p2collide::Shape::Cylinder { radius, half_height }, p2collide::TraceFlags::ALL).map(|h| (h.t, h.normal, h.start_solid))
    }
}

impl Sweep for p2physics::PhysWorld {
    fn sweep(&mut self, start: Vec3, end: Vec3, radius: f32, half_height: f32) -> Option<(f32, Vec3, bool)> {
        self.nav_sweep(start, end, radius, half_height)
    }
}

/// Result of a walk test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Walk {
    Reached,
    /// Blocked by geometry at this fraction of the way.
    Blocked(f32),
    /// The floor drops away (ledge) at this fraction.
    Drop(f32),
}

/// Walks a cylinder (centre `from`) toward `to`. `step` is the climbable height.
pub fn walk_test(w: &mut impl Sweep, from: Vec3, to: Vec3, radius: f32, half_height: f32, step: f32) -> Walk {
    let (r, h) = ((radius * 0.9).max(1.0), half_height.max(1.0));
    let flat = Vec3::new(to.x - from.x, to.y - from.y, 0.0);
    let total = flat.length();
    if total < 1.0 {
        return Walk::Reached;
    }
    let (res, pos, wall_end) = walk_along(w, from, flat / total, total, radius, half_height, step);
    if res != Walk::Reached {
        return res;
    }
    // The walk must end at the destination's height, above or below: a node on the floor under
    // the pawn (Lucky Ganesh: upstairs terrorists vs the ground-floor HomeNode) is not reachable
    // by walking straight, the route has to take the stairs. Also when it ends against a wall
    // next to the goal (a ladder top or ledge above a wall is not reached from the floor below).
    if off_level(w, pos, to, r, h, step) {
        return Walk::Blocked(if wall_end { 0.99 } else { 1.0 });
    }
    Walk::Reached
}

/// Walks `total` UU along the horizontal unit `dir` like UE2's walking move: each segment steps
/// up by `step`, moves across, then drops to the floor. A wall met part way is tried again from
/// the contact (stairs are a run of risers under `step` each, closer together than a segment),
/// so only a wall the pawn cannot step onto blocks. A wall within a radius of the end counts as
/// arrival. Returns the result, the floor position reached and whether it ended at a wall.
fn walk_along(w: &mut impl Sweep, from: Vec3, dir: Vec3, total: f32, radius: f32, half_height: f32, step: f32) -> (Walk, Vec3, bool) {
    // Slightly thinner than the pawn: brushing a wall corner is not blocking.
    let (r, h) = ((radius * 0.9).max(1.0), half_height.max(1.0));
    let at = |a: Vec3, b: Vec3, t: f32| a + (b - a) * t;
    let mut pos = from;
    // Settle on the floor first (the pawn may be mid-step).
    let settle = pos - Vec3::Z * (step + 4.0);
    if let Some((t, _, solid)) = w.sweep(pos, settle, r, h) {
        if !solid {
            pos = at(pos, settle, t);
        }
    }
    let mut done = 0.0;
    let mut climbs = 0;
    while done < total {
        let seg = (total - done).min(STEP);
        // Up by the step height, across, then down to the floor (UE2 stepUp).
        let up_to = pos + Vec3::Z * step;
        let up = match w.sweep(pos, up_to, r, h) {
            Some((_, _, true)) => pos,
            Some((t, _, _)) => at(pos, up_to, t),
            None => up_to,
        };
        let across_to = up + dir * seg;
        let (across, got) = match w.sweep(up, across_to, r, h) {
            Some((_, _, true)) => return (Walk::Blocked(done / total), pos, true),
            Some((t, n, _)) => {
                // Stop a little short of the contact: a sweep that starts touching a wall can
                // miss it, letting the next segment pass through.
                let hit = at(up, across_to, t) - if n.z < 0.7 { dir * 2.0 } else { Vec3::ZERO };
                let got = Vec3::new(hit.x - up.x, hit.y - up.y, 0.0).length().max(0.0);
                // A walkable slope: continue from the contact (the down trace follows it).
                if n.z < 0.7 {
                    if got + done + radius >= total {
                        return (Walk::Reached, pos, true);
                    }
                    // A wall right in front (no headway at step height), or too many risers:
                    // blocked. Otherwise go on from the contact; the next segment steps up again.
                    if got < 4.0 || climbs >= MAX_CLIMBS {
                        return (Walk::Blocked((done + got) / total), pos, true);
                    }
                    climbs += 1;
                }
                (hit, got)
            }
            None => (across_to, seg),
        };
        let down_to = across - Vec3::Z * (step * 2.0 + MAX_DROP);
        match w.sweep(across, down_to, r, h) {
            Some((t, n, false)) => {
                let floor = at(across, down_to, t);
                if n.z < 0.7 && (across.z - floor.z) > step * 2.0 {
                    return (Walk::Drop((done + got) / total), pos, false);
                }
                pos = floor;
            }
            _ => return (Walk::Drop((done + got) / total), pos, false),
        }
        done += got;
    }
    (Walk::Reached, pos, false)
}

/// Whether a walk that ended standing at `pos` is on another level than `to`. Navigation points
/// sit at human height above the floor, so `to` is first dropped to where this pawn's cylinder
/// would stand under it (a cat's centre is 25 UU up, a node ~100: comparing raw heights made
/// every node unreachable for small pawns).
fn off_level(w: &mut impl Sweep, pos: Vec3, to: Vec3, r: f32, h: f32, step: f32) -> bool {
    let down = to - Vec3::Z * (h + 160.0);
    let stand = match w.sweep(to, down, r, h) {
        Some((t, _, false)) => to.z + (down.z - to.z) * t,
        _ => to.z,
    };
    (stand - pos.z).abs() > step * 2.0
}

/// Reachability over the current level's physics world (looked up per call: the level changes).
pub struct LevelReach;

impl Reachability for LevelReach {
    fn point_reachable(&self, vm: &mut Vm, who: &Mover, dest: [f32; 3]) -> bool {
        let base = uscript::nav::StraightLine.point_reachable(vm, who, dest);
        if !base || who.can_fly {
            return base;
        }
        let from = Vec3::from(who.location);
        let to = Vec3::from(dest);
        if Vec3::new(to.x - from.x, to.y - from.y, 0.0).length() > MAX_DIST {
            return false;
        }
        let Some(ph) = crate::physics::current() else { return base };
        let Ok(mut w) = ph.world.try_borrow_mut() else {
            REACH_FALLBACK.with(|c| c.set(c.get() + 1));
            return base;
        };
        REACH_TESTS.with(|c| c.set(c.get() + 1));
        matches!(walk_test(&mut *w, from, to, who.radius, who.height, who.max_step), Walk::Reached)
    }
}

/// Installs [`LevelReach`] on the VM (unless `P2GAME_NO_NAVFIX`).
pub fn install(vm: &mut Vm) {
    if disabled() {
        return;
    }
    vm.set_reachability(Box::new(LevelReach));
}

/// Obstacle-aware steering direction for a walking pawn heading `dir` (unit, horizontal):
/// probes straight ahead and, if blocked within `look` UU, fans out left/right and returns the
/// first clear heading closest to `dir` (trying `prefer_side` +1 left / -1 right first).
/// Returns `dir` when the way is clear, and None when every probed heading is blocked.
#[allow(clippy::too_many_arguments)]
pub fn steer_dir(w: &mut impl Sweep, here: Vec3, dir: Vec3, look: f32, radius: f32, half_height: f32, step: f32, prefer_side: f32) -> Option<Vec3> {
    // Walk-tested (step up, across, down), so stairs and kerbs are not obstacles: a single sweep
    // at step height saw the third riser of a staircase as a wall and turned pawns away from
    // stairs their route goes up (suburbs-3 6200,-7000). Ledges are left to the route.
    let mut clear = |d: Vec3| -> bool { !matches!(walk_along(w, here, d, look, radius, half_height * 0.8, step).0, Walk::Blocked(_)) };
    if clear(dir) {
        return Some(dir);
    }
    for deg in [25.0f32, 45.0, 65.0, 90.0, 120.0] {
        for s in [prefer_side, -prefer_side] {
            let a = (deg * s).to_radians();
            let (sn, cs) = a.sin_cos();
            let d = Vec3::new(dir.x * cs - dir.y * sn, dir.x * sn + dir.y * cs, 0.0);
            if clear(d) {
                return Some(d);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2collide::World;

    fn quad(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> Vec<[Vec3; 3]> {
        vec![[a, b, c], [a, c, d]]
    }

    /// Floor z=0 from -1000..1000, wall across x=300 from y=-200..200 (height 300).
    fn world() -> World {
        let mut t = quad(Vec3::new(-1000.0, -1000.0, 0.0), Vec3::new(1000.0, -1000.0, 0.0), Vec3::new(1000.0, 1000.0, 0.0), Vec3::new(-1000.0, 1000.0, 0.0));
        for x in [290.0f32, 310.0] {
            t.extend(quad(Vec3::new(x, -200.0, 0.0), Vec3::new(x, 200.0, 0.0), Vec3::new(x, 200.0, 300.0), Vec3::new(x, -200.0, 300.0)));
        }
        t.extend(quad(Vec3::new(290.0, -200.0, 0.0), Vec3::new(310.0, -200.0, 0.0), Vec3::new(310.0, -200.0, 300.0), Vec3::new(290.0, -200.0, 300.0)));
        t.extend(quad(Vec3::new(290.0, 200.0, 0.0), Vec3::new(310.0, 200.0, 0.0), Vec3::new(310.0, 200.0, 300.0), Vec3::new(290.0, 200.0, 300.0)));
        World::from_triangles(t)
    }

    #[test]
    fn wall_blocks_walk_open_floor_does_not() {
        let mut w = world();
        let c = |x: f32, y: f32| Vec3::new(x, y, 44.0);
        assert!(matches!(walk_test(&mut w, c(0.0, 0.0), c(600.0, 0.0), 25.0, 44.0, 35.0), Walk::Blocked(_)));
        assert_eq!(walk_test(&mut w, c(0.0, 400.0), c(600.0, 400.0), 25.0, 44.0, 35.0), Walk::Reached);
    }

    #[test]
    fn steering_turns_around_the_wall() {
        let mut w = world();
        let here = Vec3::new(200.0, 50.0, 44.0);
        let d = steer_dir(&mut w, here, Vec3::X, 150.0, 25.0, 44.0, 35.0, 1.0).expect("a clear heading");
        assert!(d.x < 0.95 && d.y.abs() > 0.3, "deflected heading {d:?}");
        let open = steer_dir(&mut w, Vec3::new(0.0, 600.0, 44.0), Vec3::X, 150.0, 25.0, 44.0, 35.0, 1.0).unwrap();
        assert!((open - Vec3::X).length() < 1e-5);
    }

    /// Stairs: 12 risers of 16 UU every 24 UU from x=100 (steeper than one step per segment),
    /// then a landing.
    #[test]
    fn stairs_are_walkable_and_steerable() {
        let mut t = quad(Vec3::new(-1000.0, -1000.0, 0.0), Vec3::new(100.0, -1000.0, 0.0), Vec3::new(100.0, 1000.0, 0.0), Vec3::new(-1000.0, 1000.0, 0.0));
        for k in 0..12 {
            let (x0, z0, z1) = (100.0 + k as f32 * 24.0, k as f32 * 16.0, (k + 1) as f32 * 16.0);
            let x1 = if k == 11 { 1000.0 } else { x0 + 24.0 };
            t.extend(quad(Vec3::new(x0, -200.0, z0), Vec3::new(x0, 200.0, z0), Vec3::new(x0, 200.0, z1), Vec3::new(x0, -200.0, z1)));
            t.extend(quad(Vec3::new(x0, -200.0, z1), Vec3::new(x1, -200.0, z1), Vec3::new(x1, 200.0, z1), Vec3::new(x0, 200.0, z1)));
        }
        let mut w = World::from_triangles(t);
        let top = Vec3::new(700.0, 0.0, 192.0 + 44.0);
        assert_eq!(walk_test(&mut w, Vec3::new(0.0, 0.0, 44.0), top, 25.0, 44.0, 35.0), Walk::Reached);
        let d = steer_dir(&mut w, Vec3::new(60.0, 0.0, 44.0), Vec3::X, 150.0, 25.0, 44.0, 35.0, 1.0).unwrap();
        assert!((d - Vec3::X).length() < 1e-5, "stairs deflected steering: {d:?}");
    }

    #[test]
    fn ledge_is_not_walkable() {
        let mut w = World::from_triangles(quad(Vec3::new(-1000.0, -1000.0, 0.0), Vec3::new(200.0, -1000.0, 0.0), Vec3::new(200.0, 1000.0, 0.0), Vec3::new(-1000.0, 1000.0, 0.0)));
        assert!(matches!(walk_test(&mut w, Vec3::new(0.0, 0.0, 44.0), Vec3::new(600.0, 0.0, 44.0), 25.0, 44.0, 35.0), Walk::Drop(_)));
    }

    #[test]
    fn blocking_volume_counts_as_a_wall() {
        // Open floor, plus a BlockingVolume-style brush actor across the path in the physics world.
        let level = std::sync::Arc::new(World::from_triangles(quad(Vec3::new(-1000.0, -1000.0, 0.0), Vec3::new(1000.0, -1000.0, 0.0), Vec3::new(1000.0, 1000.0, 0.0), Vec3::new(-1000.0, 1000.0, 0.0))));
        let mut pw = p2physics::PhysWorld::new(level);
        let c = |x: f32, y: f32| Vec3::new(x, y, 44.0);
        assert_eq!(walk_test(&mut pw, c(0.0, 0.0), c(600.0, 0.0), 25.0, 44.0, 35.0), Walk::Reached);
        let mut a = p2physics::Actor::new("BlockingVolume", Vec3::new(300.0, 0.0, 150.0));
        a.collide_actors = true;
        a.block_actors = true;
        a.block_players = true;
        a.is_static = true;
        a.collider = p2physics::Collider::brush(std::sync::Arc::new(p2physics::shape::BrushModel::aabb(Vec3::new(-10.0, -200.0, -150.0), Vec3::new(10.0, 200.0, 150.0))), Vec3::ONE, Vec3::ZERO);
        let vol = pw.add(a);
        assert!(matches!(walk_test(&mut pw, c(0.0, 0.0), c(600.0, 0.0), 25.0, 44.0, 35.0), Walk::Blocked(_)));
        // The same blocker as a door (mover): AI opens doors by bumping, so it is passable.
        pw.remove(vol);
        let mut door = p2physics::Actor::new("DoorMover", Vec3::new(300.0, 0.0, 150.0));
        door.collide_actors = true;
        door.block_actors = true;
        door.block_players = true;
        door.is_static = false;
        door.mover = Some(Default::default());
        door.collider = p2physics::Collider::brush(std::sync::Arc::new(p2physics::shape::BrushModel::aabb(Vec3::new(-10.0, -200.0, -150.0), Vec3::new(10.0, 200.0, 150.0))), Vec3::ONE, Vec3::ZERO);
        pw.add(door);
        assert_eq!(walk_test(&mut pw, c(0.0, 0.0), c(600.0, 0.0), 25.0, 44.0, 35.0), Walk::Reached);
    }
}
