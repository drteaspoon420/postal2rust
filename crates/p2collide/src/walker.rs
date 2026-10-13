//! A walking player pawn: an upright collision cylinder moved through the world with UE2-style
//! walking (ground acceleration, step-up, wall sliding, floor snapping), falling (gravity, air
//! control, landing), jumping and crouching. Unreal units throughout; Z is up.

use crate::defaults::PawnParams;
use crate::geom::Shape;
use crate::world::{Hit, TraceFlags, World};
use glam::{Vec2, Vec3};

/// Distance kept between the cylinder and surfaces it touches.
const SKIN: f32 = 0.5;
/// Ground friction (UE2 default for walking pawns).
const GROUND_FRICTION: f32 = 8.0;
/// Maximum distance the pawn is snapped down to keep contact with descending floors.
const FLOOR_PROBE: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Physics {
    Walking,
    Falling,
}

/// Per-tick player input.
#[derive(Clone, Copy, Debug, Default)]
pub struct MoveInput {
    /// Desired horizontal direction (world XY, length <= 1).
    pub wish: Vec2,
    pub jump: bool,
    pub crouch: bool,
    /// Walk (slower) instead of run.
    pub walk: bool,
}

#[derive(Clone, Debug)]
pub struct Walker {
    /// Centre of the collision cylinder.
    pub location: Vec3,
    pub velocity: Vec3,
    pub physics: Physics,
    pub crouched: bool,
    pub params: PawnParams,
    /// Normal of the floor while walking.
    pub floor: Vec3,
    /// Smoothed eye height above the cylinder centre (eases steps).
    pub eye_offset: f32,
    jump_held: bool,
}

impl Walker {
    pub fn new(location: Vec3, params: PawnParams) -> Walker {
        Walker {
            location,
            velocity: Vec3::ZERO,
            physics: Physics::Falling,
            crouched: false,
            params,
            floor: Vec3::Z,
            eye_offset: params.base_eye_height,
            jump_held: false,
        }
    }

    pub fn radius(&self) -> f32 {
        if self.crouched { self.params.crouch_radius } else { self.params.collision_radius }
    }

    pub fn half_height(&self) -> f32 {
        if self.crouched { self.params.crouch_height } else { self.params.collision_height }
    }

    pub fn shape(&self) -> Shape {
        Shape::Cylinder { radius: self.radius(), half_height: self.half_height() }
    }

    /// Bottom of the cylinder.
    pub fn feet(&self) -> Vec3 {
        self.location - Vec3::Z * self.half_height()
    }

    /// Camera position. As in UE2 the eye height is measured from the cylinder centre
    /// (`Location + EyeHeight`): BaseEyeHeight standing, 0.8 * CrouchHeight crouched.
    pub fn eye(&self) -> Vec3 {
        self.location + Vec3::Z * self.eye_offset
    }

    fn target_eye_offset(&self) -> f32 {
        if self.crouched { 0.8 * self.params.crouch_height } else { self.params.base_eye_height }
    }

    /// Places the pawn so the cylinder stands on the floor below `p` (searching down up to
    /// `max_drop`); returns false if nothing was found below.
    pub fn place_on_floor(&mut self, world: &World, p: Vec3, max_drop: f32) -> bool {
        let shape = self.shape();
        let start = p + Vec3::Z * 1.0;
        let end = start - Vec3::Z * max_drop;
        match world.trace(start, end, &shape, TraceFlags::ALL) {
            Some(h) if !h.start_solid => {
                self.location = h.location + Vec3::Z * SKIN;
                self.physics = Physics::Walking;
                self.floor = h.normal;
                self.velocity = Vec3::ZERO;
                true
            }
            _ => {
                self.location = p;
                false
            }
        }
    }

    /// Sweeps the cylinder by `delta`, backing off from the contact by `SKIN`. Resolves a
    /// start-solid overlap by pushing out first.
    fn sweep(&mut self, world: &World, delta: Vec3) -> Option<Hit> {
        let shape = self.shape();
        let len = delta.length();
        if len < 1e-4 {
            return None;
        }
        let end = self.location + delta;
        match world.trace(self.location, end, &shape, TraceFlags::ALL) {
            None => {
                self.location = end;
                None
            }
            Some(h) if h.start_solid => {
                // Already overlapping: depenetrate along the contact normal.
                self.location += h.normal * (h.depth + SKIN).min(self.radius());
                Some(h)
            }
            Some(h) => {
                let back = (SKIN / (-delta.normalize().dot(h.normal)).max(0.05)).min(len);
                let t = ((h.t * len - back) / len).max(0.0);
                self.location += delta * t;
                Some(h)
            }
        }
    }

    /// Moves by `delta`, sliding along up to three blocking surfaces. Returns the hit normals.
    fn slide_move(&mut self, world: &World, mut delta: Vec3, walking: bool) -> Vec<Vec3> {
        let mut normals = Vec::new();
        let original = delta;
        for _ in 0..4 {
            let before = self.location;
            let Some(h) = self.sweep(world, delta) else { break };
            let moved = self.location - before;
            let mut n = h.normal;
            if walking && n.z.abs() < self.params.walkable_floor_z {
                // Walls are treated as vertical while walking so the pawn does not climb them.
                n = Vec3::new(n.x, n.y, 0.0).normalize_or_zero();
                if n == Vec3::ZERO {
                    break;
                }
            }
            normals.push(n);
            let mut rest = delta - moved;
            rest -= n * rest.dot(n);
            if normals.len() >= 2 {
                // Crease between two planes: move along their intersection line.
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
        normals
    }

    /// Tries to step up onto an obstacle: up by the step height, forward, then back down.
    fn step_up(&mut self, world: &World, delta: Vec3) -> bool {
        let saved = self.location;
        let step = self.params.max_step_height;
        let up = Vec3::Z * step;
        if self.sweep(world, up).is_some() && self.location.z - saved.z < 1.0 {
            self.location = saved;
            return false;
        }
        let lifted = self.location.z - saved.z;
        self.slide_move(world, delta, true);
        let down = self.sweep(world, Vec3::Z * -(lifted + FLOOR_PROBE));
        match down {
            Some(h) if h.normal.z >= self.params.walkable_floor_z && (self.location - saved).truncate().length() > 0.1 => {
                self.floor = h.normal;
                true
            }
            _ => {
                self.location = saved;
                false
            }
        }
    }

    fn set_crouch(&mut self, world: &World, want: bool) {
        if want == self.crouched {
            return;
        }
        let (h0, h1) = if want { (self.params.collision_height, self.params.crouch_height) } else { (self.params.crouch_height, self.params.collision_height) };
        // Keep the feet in place: the centre moves by the change in half height.
        let new_loc = self.location + Vec3::Z * (h1 - h0);
        let shape = if want {
            Shape::Cylinder { radius: self.params.crouch_radius, half_height: h1 }
        } else {
            Shape::Cylinder { radius: self.params.collision_radius, half_height: h1 }
        };
        if !want && world.encroaches(new_loc, &shape) {
            return; // no headroom to stand up
        }
        self.location = new_loc;
        self.crouched = want;
        self.eye_offset -= h1 - h0;
    }

    /// Advances the pawn by `dt` seconds.
    pub fn tick(&mut self, world: &World, input: &MoveInput, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        if dt <= 0.0 {
            return;
        }
        if self.physics == Physics::Walking || !input.crouch {
            self.set_crouch(world, input.crouch);
        }
        let p = self.params;
        let wish = if input.wish.length_squared() > 1.0 { input.wish.normalize() } else { input.wish };
        let mut max_speed = p.ground_speed;
        if self.crouched {
            max_speed *= p.crouched_pct;
        } else if input.walk {
            max_speed *= p.walking_pct;
        }
        let jump_pressed = input.jump && !self.jump_held;
        self.jump_held = input.jump;

        match self.physics {
            Physics::Walking => {
                let accel = Vec3::new(wish.x, wish.y, 0.0) * p.accel_rate;
                let mut v = Vec3::new(self.velocity.x, self.velocity.y, 0.0);
                if accel.length_squared() > 0.0 {
                    // Friction steers velocity toward the acceleration direction.
                    let dir = accel.normalize();
                    let speed = v.length();
                    v -= (v - dir * speed) * (dt * GROUND_FRICTION).min(1.0);
                    v += accel * dt;
                } else {
                    let speed = v.length();
                    let drop = speed * GROUND_FRICTION * dt;
                    v *= ((speed - drop).max(0.0)) / speed.max(1e-3);
                }
                if v.length() > max_speed {
                    v = v.normalize() * max_speed;
                }
                self.velocity = v;
                if jump_pressed && !self.crouched {
                    self.velocity.z = p.jump_z;
                    self.physics = Physics::Falling;
                    self.fall(world, input, dt, max_speed);
                    return;
                }
                self.walk(world, dt);
            }
            Physics::Falling => self.fall(world, input, dt, max_speed),
        }
        let target = self.target_eye_offset();
        self.eye_offset += (target - self.eye_offset) * (dt * 12.0).min(1.0);
    }

    fn walk(&mut self, world: &World, dt: f32) {
        let delta = self.velocity * dt;
        // Follow the floor slope so walking down ramps keeps contact.
        let start = self.location;
        if delta.length_squared() > 0.0 {
            let before = self.location;
            let normals = self.slide_move(world, delta, true);
            let moved = (self.location - before).truncate().length();
            let wanted = delta.truncate().length();
            if !normals.is_empty() && moved < wanted * 0.9 {
                // Blocked: try stepping up from the original position with the remaining motion.
                let after = self.location;
                self.location = before;
                if !self.step_up(world, delta) {
                    self.location = after;
                } else if (self.location - before).truncate().length() <= moved {
                    self.location = after;
                }
            }
            let actual = self.location - start;
            // Remove velocity into walls.
            let hv = actual.truncate() / dt;
            if hv.length() < self.velocity.truncate().length() {
                self.velocity = Vec3::new(hv.x, hv.y, 0.0);
            }
        }
        // Floor check: snap down onto floors within the step height, else start falling.
        let probe = self.params.max_step_height + FLOOR_PROBE;
        let saved = self.location;
        match self.sweep(world, Vec3::Z * -probe) {
            Some(h) if !h.start_solid && h.normal.z >= self.params.walkable_floor_z => {
                self.floor = h.normal;
            }
            Some(h) if h.start_solid => {
                self.floor = Vec3::Z;
            }
            _ => {
                self.location = saved;
                self.physics = Physics::Falling;
            }
        }
    }

    fn fall(&mut self, world: &World, input: &MoveInput, dt: f32, max_speed: f32) {
        let p = self.params;
        let wish = Vec3::new(input.wish.x, input.wish.y, 0.0);
        let mut hv = self.velocity.truncate();
        hv += wish.truncate() * p.accel_rate * p.air_control * dt;
        if hv.length() > max_speed.max(self.velocity.truncate().length()) {
            hv = hv.normalize() * max_speed.max(self.velocity.truncate().length());
        }
        self.velocity.x = hv.x;
        self.velocity.y = hv.y;
        let vz0 = self.velocity.z;
        self.velocity.z = (self.velocity.z + p.gravity_z * dt).max(-4000.0);
        let delta = Vec3::new(self.velocity.x, self.velocity.y, (vz0 + self.velocity.z) * 0.5) * dt;
        let normals = self.slide_move(world, delta, false);
        for n in &normals {
            if n.z >= p.walkable_floor_z && self.velocity.z <= 0.0 {
                self.physics = Physics::Walking;
                self.floor = *n;
                self.velocity.z = 0.0;
                return;
            }
            // Lose velocity into the surface.
            let into = self.velocity.dot(*n);
            if into < 0.0 {
                self.velocity -= *n * into;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> Vec<[Vec3; 3]> {
        vec![[a, b, c], [a, c, d]]
    }

    /// Flat floor at z = 0 with a 20-unit step at x >= 200 and a wall at x = 600.
    fn test_world() -> World {
        let mut t = Vec::new();
        t.extend(quad(Vec3::new(-1000.0, -1000.0, 0.0), Vec3::new(200.0, -1000.0, 0.0), Vec3::new(200.0, 1000.0, 0.0), Vec3::new(-1000.0, 1000.0, 0.0)));
        // Step riser and top.
        t.extend(quad(Vec3::new(200.0, -1000.0, 0.0), Vec3::new(200.0, 1000.0, 0.0), Vec3::new(200.0, 1000.0, 20.0), Vec3::new(200.0, -1000.0, 20.0)));
        t.extend(quad(Vec3::new(200.0, -1000.0, 20.0), Vec3::new(200.0, 1000.0, 20.0), Vec3::new(1000.0, 1000.0, 20.0), Vec3::new(1000.0, -1000.0, 20.0)));
        // Wall.
        t.extend(quad(Vec3::new(600.0, -1000.0, 0.0), Vec3::new(600.0, 1000.0, 0.0), Vec3::new(600.0, 1000.0, 500.0), Vec3::new(600.0, -1000.0, 500.0)));
        World::from_triangles(t)
    }

    #[test]
    fn lands_steps_and_stops_at_wall() {
        let w = test_world();
        let p = PawnParams::default();
        let mut wk = Walker::new(Vec3::new(0.0, 0.0, 300.0), p);
        for _ in 0..120 {
            wk.tick(&w, &MoveInput::default(), 1.0 / 60.0);
        }
        assert_eq!(wk.physics, Physics::Walking);
        assert!((wk.feet().z - 0.0).abs() < 2.0, "feet {}", wk.feet().z);
        let input = MoveInput { wish: Vec2::X, ..Default::default() };
        for _ in 0..240 {
            wk.tick(&w, &input, 1.0 / 60.0);
        }
        assert_eq!(wk.physics, Physics::Walking);
        assert!((wk.feet().z - 20.0).abs() < 2.0, "should be on the step, feet {}", wk.feet().z);
        let max_x = 600.0 - p.collision_radius;
        assert!(wk.location.x <= max_x + 0.01 && wk.location.x > max_x - 5.0, "x {}", wk.location.x);
    }

    #[test]
    fn slides_along_angled_wall() {
        let w = test_world();
        let mut wk = Walker::new(Vec3::new(400.0, 0.0, 200.0), PawnParams::default());
        for _ in 0..60 {
            wk.tick(&w, &MoveInput::default(), 1.0 / 60.0);
        }
        let input = MoveInput { wish: Vec2::new(1.0, 1.0).normalize(), ..Default::default() };
        let y0 = wk.location.y;
        for _ in 0..120 {
            wk.tick(&w, &input, 1.0 / 60.0);
        }
        assert!(wk.location.y - y0 > 300.0, "should slide along the wall, dy {}", wk.location.y - y0);
        assert!(wk.location.x < 600.0 - 27.0);
    }

    #[test]
    fn jumps_and_crouches() {
        let w = test_world();
        let p = PawnParams::default();
        let mut wk = Walker::new(Vec3::new(0.0, 0.0, 100.0), p);
        for _ in 0..60 {
            wk.tick(&w, &MoveInput::default(), 1.0 / 60.0);
        }
        let ground = wk.location.z;
        let mut peak: f32 = 0.0;
        wk.tick(&w, &MoveInput { jump: true, ..Default::default() }, 1.0 / 60.0);
        for _ in 0..90 {
            wk.tick(&w, &MoveInput::default(), 1.0 / 60.0);
            peak = peak.max(wk.location.z - ground);
        }
        // v^2 / 2g = 550^2 / 3000 = 100.8
        assert!((peak - 100.8).abs() < 8.0, "jump apex {peak}");
        assert_eq!(wk.physics, Physics::Walking);
        let feet = wk.feet().z;
        wk.tick(&w, &MoveInput { crouch: true, ..Default::default() }, 1.0 / 60.0);
        assert!(wk.crouched);
        assert!((wk.feet().z - feet).abs() < 1.0);
        wk.tick(&w, &MoveInput::default(), 1.0 / 60.0);
        assert!(!wk.crouched);
    }
}
