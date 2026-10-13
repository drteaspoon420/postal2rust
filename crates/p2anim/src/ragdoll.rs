//! Karma ragdolls: the skeletal physics assets in `KarmaData\*.ka` and a small solver that
//! drives an [`AnimInstance`]'s pose when a pawn dies.
//!
//! # `.ka` files
//!
//! Karma assets are XML (`<KARMA ka_file_version="1.0">`). An `<ASSET id=".." scale="..">`
//! (the name `KarmaParamsSkel.KSkeleton` refers to) holds:
//! - `<GEOMETRY id=bone><PRIMITIVE type="sphyl"|"sphere"|"box">` with `RADIUS`, `HEIGHT` and a
//!   4x4 `TM` (column-major, row vectors) placing the shape in the bone's frame;
//! - `<MODEL id=bone><DYNAMICS>` with `MASS`, `INERTIA`, damping;
//! - `<PART id=bone model=.. parent=..>` with the bind `TM` of each body;
//! - `<JOINT id part1 part2 type="hinge"|"skel"|"ball">` with limits (`HIGH_LIMIT`/`LOW_LIMIT` for
//!   hinges, `CONE_HALF_ANGLE_X/Y` and `TWIST_HALF_ANGLE` for cone limited joints), attachment
//!   points `POS1`/`POS2` and axes. Lengths are in Karma units: Unreal units times `scale`
//!   (0.02, so 1 Karma unit = 50 UU).
//!
//! # Solver
//!
//! Extended position-based dynamics (XPBD) on rigid bodies: each body (bone with a part) has a
//! position (centre of mass, `.ka` MASS_OFFSET), orientation, linear and angular velocity, mass
//! and principal inertia (`.ka` MASS / INERTIA). Small fixed substeps, each integrating
//! gravity and damping and then iterating:
//! - joint: the child's attachment (its bone origin at death) meets the same point fixed in
//!   the parent's frame;
//! - joint limits, hard: hinges keep their primary axes aligned and clamp the angle about them
//!   (`LOW/HIGH_LIMIT`, one-sided knees and elbows); cone joints keep the child's primary axis
//!   inside the elliptic cone (`CONE_HALF_ANGLE_X/Y`) and clamp twist (`TWIST_HALF_ANGLE`),
//!   all from direct axis comparisons (no swing/twist split that flips near pi);
//! - world: spheres along each `.ka` capsule are pushed out of the world by the host's
//!   collision callback ([`RagdollWorld`]) with static friction, then a velocity pass makes
//!   contacts inelastic with Coulomb friction (`KFriction`).
//! Bones without a body follow their parent rigidly (fingers, toes, dummies).

use crate::runtime::{AnimInstance, Skeleton};
use glam::{Mat3, Mat4, Quat, Vec3};
use std::collections::HashMap;

/// A primitive attached to a bone.
#[derive(Debug, Clone)]
pub struct KaGeom {
    pub kind: String,
    pub radius: f32,
    pub height: f32,
    pub tm: Mat4,
}

#[derive(Debug, Clone)]
pub struct KaJoint {
    pub id: String,
    pub child: String,
    pub parent: String,
    pub kind: String,
    pub low: f32,
    pub high: f32,
    pub cone_x: f32,
    pub cone_y: f32,
    pub twist: f32,
    /// `TWIST_TYPE` (Karma MdtSkeletal twist option). 0 is taken as free; 1 and 2 limit the
    /// twist to `twist` (`TWIST_HALF_ANGLE`). The Karma 1.2 guide does not document the enum;
    /// every stock cone joint uses `CONE_TYPE` 2 for its (limited) cone, so 2 reads as
    /// "limited" here too. `P2ANIM_TWIST2_FIXED=1` treats 2 as fixed instead (A/B checks).
    pub twist_type: i32,
    /// `CONE_TYPE`: 0 free, otherwise the elliptic cone limit (every stock joint uses 2).
    pub cone_type: i32,
    pub pos2: Vec3,
    /// Joint axes: `PRIMARY_AXIS1`/`ORTHOGONAL_AXIS1` in the child's (part1) frame,
    /// `..2` in the parent's (part2) frame. Hinges turn about the primary axis; cone joints
    /// swing the primary axis inside the cone and twist about it. Zero when absent.
    pub axis1: Vec3,
    pub axis2: Vec3,
    pub orth1: Vec3,
    pub orth2: Vec3,
}

#[derive(Debug, Clone)]
pub struct KaPart {
    pub bone: String,
    pub parent: Option<String>,
    pub tm: Mat4,
    pub mass: f32,
    /// Centre of mass in the body frame (`MASS_OFFSET`, Karma units).
    pub mass_offset: Vec3,
    /// Diagonal of the inertia tensor in the body frame (`INERTIA` xx, yy, zz; Karma units).
    pub inertia: Vec3,
    /// Collision geometry id of the part's model (`geometry=`; right limbs reuse the left
    /// limb's geometry in the stock assets).
    pub geometry: Option<String>,
    /// The model collides (`type="dynamics_and_geometry"`); `dynamics_only` parts (root,
    /// neck, clavicles, the dude's spine) only carry mass.
    pub collides: bool,
}

/// One `<ASSET>`.
#[derive(Debug, Clone, Default)]
pub struct KaAsset {
    pub id: String,
    pub scale: f32,
    pub geometry: HashMap<String, Vec<KaGeom>>,
    pub parts: Vec<KaPart>,
    pub joints: Vec<KaJoint>,
}

impl KaAsset {
    /// Unreal units per Karma unit.
    pub fn uu(&self) -> f32 {
        if self.scale > 0.0 { 1.0 / self.scale } else { 50.0 }
    }
}

fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let i = s.find(&open)? + open.len();
    let j = s[i..].find(&format!("</{name}>"))?;
    Some(&s[i..i + j])
}

fn attr(head: &str, name: &str) -> Option<String> {
    let k = format!("{name}=\"");
    let i = head.find(&k)? + k.len();
    let j = head[i..].find('"')?;
    Some(head[i..i + j].to_string())
}

fn floats(s: &str) -> Vec<f32> {
    s.split([',', ' ', '\n', '\r', '\t']).filter_map(|x| x.trim().parse().ok()).collect()
}

fn f(s: &str, name: &str) -> f32 {
    tag(s, name).and_then(|v| v.trim().parse().ok()).unwrap_or(0.0)
}

fn v3(s: &str, name: &str) -> Vec3 {
    let v = tag(s, name).map(floats).unwrap_or_default();
    Vec3::new(v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0))
}

fn tm(s: &str) -> Mat4 {
    let v = tag(s, "TM").map(floats).unwrap_or_default();
    if v.len() == 16 { Mat4::from_cols_array(&v.try_into().unwrap()) } else { Mat4::IDENTITY }
}

/// Splits `s` into the elements `<name ...>...</name>`: (head attributes, body).
fn elements<'a>(s: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let open = format!("<{name} ");
    let close = format!("</{name}>");
    let mut rest = s;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(h) = after.find('>') else { break };
        let head = &after[..h];
        if head.ends_with('/') {
            out.push((head, ""));
            rest = &after[h + 1..];
            continue;
        }
        let body_start = &after[h + 1..];
        let Some(e) = body_start.find(&close) else { break };
        out.push((head, &body_start[..e]));
        rest = &body_start[e + close.len()..];
    }
    out
}

/// Parses a `.ka` file into its assets.
pub fn parse_ka(text: &str) -> Vec<KaAsset> {
    let mut assets = Vec::new();
    for (head, body) in elements(text, "ASSET") {
        let mut a = KaAsset { id: attr(head, "id").unwrap_or_default(), scale: attr(head, "scale").and_then(|v| v.parse().ok()).unwrap_or(0.02), ..Default::default() };
        for (gh, gb) in elements(body, "GEOMETRY") {
            let bone = attr(gh, "id").unwrap_or_default().to_ascii_lowercase();
            let prims = elements(gb, "PRIMITIVE").into_iter().map(|(ph, pb)| KaGeom { kind: attr(ph, "type").unwrap_or_default(), radius: f(pb, "RADIUS"), height: f(pb, "HEIGHT"), tm: tm(pb) }).collect();
            a.geometry.insert(bone, prims);
        }
        let mut models: HashMap<String, (f32, Vec3, Option<String>, bool, Vec3)> = HashMap::new();
        for (mh, mb) in elements(body, "MODEL") {
            let collides = attr(mh, "type").is_some_and(|t| t.contains("geometry"));
            let i = tag(mb, "INERTIA").map(floats).unwrap_or_default();
            let inertia = if i.len() == 6 { Vec3::new(i[0], i[3], i[5]) } else { Vec3::ZERO };
            models.insert(attr(mh, "id").unwrap_or_default().to_ascii_lowercase(), (f(mb, "MASS"), v3(mb, "MASS_OFFSET"), attr(mh, "geometry").map(|g| g.to_ascii_lowercase()), collides, inertia));
        }
        for (ph, pb) in elements(body, "PART") {
            let bone = attr(ph, "id").unwrap_or_default();
            let model = attr(ph, "model").unwrap_or_else(|| bone.clone()).to_ascii_lowercase();
            let (mass, mass_offset, geometry, collides, inertia) = models.get(&model).cloned().unwrap_or((1.0, Vec3::ZERO, None, true, Vec3::ZERO));
            a.parts.push(KaPart { bone: bone.to_ascii_lowercase(), parent: attr(ph, "parent").map(|p| p.to_ascii_lowercase()), tm: tm(pb), mass, mass_offset, inertia, geometry, collides });
        }
        for (jh, jb) in elements(body, "JOINT") {
            a.joints.push(KaJoint {
                id: attr(jh, "id").unwrap_or_default(),
                child: attr(jh, "part1").unwrap_or_default().to_ascii_lowercase(),
                parent: attr(jh, "part2").unwrap_or_default().to_ascii_lowercase(),
                kind: attr(jh, "type").unwrap_or_default(),
                low: f(jb, "LOW_LIMIT"),
                high: f(jb, "HIGH_LIMIT"),
                cone_x: f(jb, "CONE_HALF_ANGLE_X"),
                cone_y: f(jb, "CONE_HALF_ANGLE_Y"),
                twist: f(jb, "TWIST_HALF_ANGLE"),
                twist_type: tag(jb, "TWIST_TYPE").and_then(|v| v.trim().parse().ok()).unwrap_or(1),
                cone_type: tag(jb, "CONE_TYPE").and_then(|v| v.trim().parse().ok()).unwrap_or(2),
                pos2: v3(jb, "POS2"),
                axis1: v3(jb, "PRIMARY_AXIS1"),
                axis2: v3(jb, "PRIMARY_AXIS2"),
                orth1: v3(jb, "ORTHOGONAL_AXIS1"),
                orth2: v3(jb, "ORTHOGONAL_AXIS2"),
            });
        }
        assets.push(a);
    }
    assets
}

/// All Karma assets of an install (`KarmaData\*.ka`), by lower-case asset id.
pub fn load_karma_dir(dir: &std::path::Path) -> HashMap<String, KaAsset> {
    let mut out = HashMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ka")) {
            if let Ok(t) = std::fs::read_to_string(&p) {
                for a in parse_ka(&t) {
                    out.insert(a.id.to_ascii_lowercase(), a);
                }
            }
        }
    }
    out
}

/// World collision for ragdoll particles (UU, world space).
pub trait RagdollWorld {
    /// Pushes a sphere at `p` out of the world; returns the corrected position and the contact
    /// normal when it was inside something.
    fn resolve(&mut self, p: Vec3, radius: f32) -> Option<(Vec3, Vec3)>;
    /// Sweeps a sphere from `from` to `to`; returns the stop position and contact normal on a
    /// hit (keeps fast particles from tunnelling through thin geometry).
    fn sweep(&mut self, _from: Vec3, _to: Vec3, _radius: f32) -> Option<(Vec3, Vec3)> {
        None
    }
}

/// A flat floor (tests and empty worlds).
pub struct Floor(pub f32);
impl RagdollWorld for Floor {
    fn resolve(&mut self, p: Vec3, r: f32) -> Option<(Vec3, Vec3)> {
        (p.z - r < self.0).then(|| (Vec3::new(p.x, p.y, self.0 + r), Vec3::Z))
    }
}

#[derive(Debug, Clone, Copy)]
enum Limit {
    /// Karma hinge: turns about the primary axis only, `lo..hi` radians (one-sided for knees and
    /// elbows).
    Hinge(f32, f32),
    /// Cone limited (skeletal) joint: swing half-angles about the joint frame's Z and Y axes, and
    /// the twist half-angle about the primary axis.
    Cone(f32, f32, f32),
}

/// One rigid body (a bone with a `.ka` part). Its frame is the bone's frame (where the `.ka`
/// joint axes, mass offset, inertia and collision shapes live); `x` is the centre of mass.
#[derive(Debug, Clone)]
struct Body {
    bone: usize,
    /// Lower-case bone name (`KAddImpulse` bone argument).
    name: String,
    x: Vec3,
    q: Quat,
    v: Vec3,
    /// Angular velocity (world, rad/s).
    w: Vec3,
    /// Pose at the start of the substep.
    xp: Vec3,
    qp: Quat,
    inv_m: f32,
    /// Inverse principal inertia in the body frame (`.ka` INERTIA, UU).
    inv_i: Vec3,
    /// Centre of mass relative to the bone origin, body frame (`.ka` MASS_OFFSET).
    com: Vec3,
    /// Bone matrix scale at death.
    scale: f32,
    /// Rough size for the speed measure.
    len: f32,
    parent: Option<usize>,
    /// Joint attachment (this bone's origin at death) relative to the parent's and this body's
    /// centre of mass, in their frames.
    rp: Vec3,
    rc: Vec3,
    /// Joint frames: primary and orthogonal axes in this body's frame and in the parent's
    /// (`PRIMARY/ORTHOGONAL_AXIS1/2`); they coincide in the bind pose.
    ja: Vec3,
    jo: Vec3,
    pa: Vec3,
    po: Vec3,
    limit: Option<Limit>,
    /// How far (radians) the pose at death was outside `limit`. The joint may stay that far
    /// out, never farther, and the allowance fades to zero over `LIMIT_RAMP` (see
    /// `solve_joints`).
    slack: f32,
    /// Collision spheres along the `.ka` capsules (centre relative to the centre of mass, body
    /// frame; radius). Empty for `dynamics_only` parts.
    spheres: Vec<(Vec3, f32)>,
}

impl Body {
    fn inv_inertia(&self, v: Vec3) -> Vec3 {
        self.q * (self.inv_i * (self.q.inverse() * v))
    }
    fn origin(&self) -> Vec3 {
        self.x - self.q * self.com
    }
    /// Generalised inverse mass of a positional correction along `n` at offset `r`.
    fn w_pos(&self, r: Vec3, n: Vec3) -> f32 {
        let rn = r.cross(n);
        self.inv_m + rn.dot(self.inv_inertia(rn))
    }
    fn w_rot(&self, n: Vec3) -> f32 {
        n.dot(self.inv_inertia(n))
    }
    fn rotate(&mut self, dphi: Vec3) {
        if dphi.length_squared() > 1e-14 {
            self.q = (Quat::from_scaled_axis(dphi) * self.q).normalize();
        }
    }
    /// Applies a positional impulse `p` at offset `r`.
    fn apply_pos(&mut self, p: Vec3, r: Vec3) {
        self.x += p * self.inv_m;
        let d = self.inv_inertia(r.cross(p));
        self.rotate(d);
    }
    fn speed(&self) -> f32 {
        self.v.length() + self.w.length() * self.len * 0.5
    }
}

/// XPBD positional correction: moves the point at offset `rb` of body `b` by `corr` (and the
/// point `ra` of body `a`, if any, the other way), split by generalised inverse masses.
fn pos_corr(bodies: &mut [Body], a: Option<usize>, b: usize, ra: Vec3, rb: Vec3, corr: Vec3) -> f32 {
    let c = corr.length();
    if c < 1e-5 {
        return 0.0;
    }
    let n = corr / c;
    let wa = a.map_or(0.0, |a| bodies[a].w_pos(ra, n));
    let wb = bodies[b].w_pos(rb, n);
    let l = c / (wa + wb);
    if !l.is_finite() {
        return 0.0;
    }
    bodies[b].apply_pos(n * l, rb);
    if let Some(a) = a {
        bodies[a].apply_pos(-n * l, ra);
    }
    l
}

/// XPBD angular correction: turns body `b` by `dphi` relative to body `a`.
fn rot_corr(bodies: &mut [Body], a: usize, b: usize, dphi: Vec3) {
    let th = dphi.length();
    if th < 1e-5 {
        return;
    }
    let n = dphi / th;
    let l = th / (bodies[a].w_rot(n) + bodies[b].w_rot(n));
    if !l.is_finite() {
        return;
    }
    let db = bodies[b].inv_inertia(n * l);
    bodies[b].rotate(db);
    let da = bodies[a].inv_inertia(-n * l);
    bodies[a].rotate(da);
}

/// Rotation vector turning direction `u` onto `v` (exact angle, no flip near pi).
fn rot_between(u: Vec3, v: Vec3) -> Vec3 {
    let c = u.cross(v);
    let s = c.length();
    let ang = s.atan2(u.dot(v));
    if s < 1e-6 {
        return if u.dot(v) > 0.0 { Vec3::ZERO } else { u.any_orthonormal_vector() * ang };
    }
    c / s * ang
}

/// Signed angle from `a` to `b` about `n` (both projected onto the plane normal to `n`).
fn signed_angle(n: Vec3, a: Vec3, b: Vec3) -> f32 {
    let a = a - n * n.dot(a);
    let b = b - n * n.dot(b);
    n.dot(a.cross(b)).atan2(a.dot(b))
}

/// Orthonormal joint basis from a primary and an orthogonal axis (None when absent).
fn joint_axes(primary: Vec3, orth: Vec3) -> Option<(Vec3, Vec3)> {
    if primary.length_squared() < 1e-6 || orth.length_squared() < 1e-6 {
        return None;
    }
    let x = primary.normalize();
    let y = (orth - x * x.dot(orth)).try_normalize()?;
    Some((x, y))
}

/// A world contact of the last substep: body, sphere, normal, push-out depth.
#[derive(Debug, Clone, Copy)]
struct Contact {
    body: usize,
    sphere: usize,
    n: Vec3,
    d: f32,
}

/// A running ragdoll for one actor. Positions are world-space UU.
pub struct Ragdoll {
    bodies: Vec<Body>,
    /// For each skeleton bone: the body that owns it, or None (follows parent rigidly).
    owner: Vec<Option<usize>>,
    /// Bone matrices at death (world space), for bones without bodies.
    death_world: Vec<Mat4>,
    /// World transform of the mesh at death (actor * mesh_xform), to convert back.
    mesh_to_world: Mat4,
    pub gravity: Vec3,
    /// Linear and angular damping (1/s).
    pub damping: f32,
    /// Solver iterations per substep.
    pub iterations: usize,
    /// Coulomb friction (`KFriction`).
    pub friction: f32,
    /// Seconds simulated since the last wake (`KWake`, velocity or impulse).
    pub time: f32,
    /// Seconds simulated since the ragdoll started (not reset by waking; the death-pose limit
    /// allowance fades with it).
    pub age: f32,
    /// Mean body speed of the last step (UU/s, angular part at the body's half length).
    pub speed: f32,
    /// Speed averaged over ~0.5 s (rest damping).
    pub speed_avg: f32,
    /// Seconds the doll has stayed under the sleep speed without a break.
    pub still: f32,
    contacts: Vec<Contact>,
}

/// Physics substep (s). XPBD is stable with one or two iterations at a small substep.
const SUBSTEP: f32 = 1.0 / 240.0;
/// Most a contact may push a body out per substep (UU). Deep box-penetration answers on
/// thin brushes and terrain edges otherwise teleported limbs through to the far side.
const MAX_PUSH: f32 = 1.5;
/// Most outward speed a contact leaves after pushing a body out (UU/s): penetration recovery
/// must not launch limbs (the old solver buzzed at 300-1000 uu/s on uneven ground).
const MAX_SEPARATION: f32 = 25.0;
/// Mean and per-body speed (UU/s) under which a doll counts as still.
const SLEEP_SPEED: f32 = 8.0;
/// Seconds over which joint limits ramp from soft to hard after death (see `solve_joints`).
const LIMIT_RAMP: f32 = 0.3;
const SLEEP_BODY_SPEED: f32 = 25.0;
/// Twist half-angle (rad) of a fixed-twist skeletal joint (`P2ANIM_TWIST2_FIXED`).
const FIXED_TWIST: f32 = 0.05;

impl Ragdoll {
    /// Builds a ragdoll from the current pose of `inst` (placed in the world by `actor_world`),
    /// with an initial velocity (UU/s, e.g. the momentum of the killing hit).
    pub fn new(asset: &KaAsset, inst: &mut AnimInstance, actor_world: Mat4, velocity: Vec3) -> Self {
        let skel = inst.skeleton.clone();
        let n = skel.len();
        let mesh_to_world = actor_world * skel.mesh_xform;
        // Unscaled: a limb severed before death (`SetBoneScale(.., 0)`) would otherwise give
        // zero-scale, non-invertible body frames. The scale slots are re-applied on top of the
        // physics pose by `AnimInstance::evaluate`.
        let world: Vec<Mat4> = inst.unscaled_bone_matrices().iter().map(|m| mesh_to_world * *m).collect();
        let uu = asset.uu();
        let mut bodies: Vec<Body> = Vec::new();
        let mut owner = vec![None; n];
        let mut body_of_bone: HashMap<usize, usize> = HashMap::new();
        let parts: HashMap<String, &KaPart> = asset.parts.iter().map(|p| (p.bone.clone(), p)).collect();
        // Bodies in skeleton order so parents come first.
        for b in 0..n {
            let name = skel.names[b].to_ascii_lowercase();
            let Some(part) = parts.get(&name) else { continue };
            let gname = part.geometry.clone().unwrap_or_else(|| name.clone());
            let geoms = asset.geometry.get(&gname).or_else(|| asset.geometry.get(&name));
            let m = world[b];
            let scale = m.x_axis.truncate().length().max(1e-6);
            let q = Quat::from_mat3(&Mat3::from_mat4(m).mul_scalar(1.0 / scale)).normalize();
            let origin = m.w_axis.truncate();
            let com = part.mass_offset * uu;
            let x = origin + q * com;
            let mass = part.mass.max(0.05);
            // Segment length: the farthest child joint attachment, else the capsule extent.
            let child_len = asset.joints.iter().filter(|j| j.parent == name).map(|j| j.pos2.length()).fold(0.0f32, f32::max) * uu;
            let mut spheres = Vec::new();
            let mut extent = 0.0f32;
            if part.collides {
                for g in geoms.into_iter().flatten() {
                    // The capsule's real radius, capped so the few oversized authoring capsules
                    // (Avg_Dude calf 0.32 = 16 uu) do not prop limbs up.
                    let r = (g.radius * uu).clamp(2.5, 13.0);
                    let c = g.tm.w_axis.truncate() * uu;
                    let axis = g.tm.z_axis.truncate().normalize_or(Vec3::X);
                    let hh = if g.kind.eq_ignore_ascii_case("sphyl") { g.height * uu * 0.5 } else { 0.0 };
                    let k = if hh > 0.75 * r { 3 } else if hh > 0.0 { 2 } else { 1 };
                    for i in 0..k {
                        let t = if k == 1 { 0.0 } else { -1.0 + 2.0 * i as f32 / (k - 1) as f32 };
                        let p = c + axis * hh * t;
                        extent = extent.max(p.length() + r);
                        spheres.push((p - com, r));
                    }
                }
            }
            let len = if child_len > 0.0 { child_len } else if extent > 0.0 { extent } else { 8.0 }.max(4.0);
            // Inertia: the .ka principal moments (Karma units, mass * length^2), floored at a
            // 3 uu radius of gyration (the clavicles' are ~0.6 uu).
            let floor_i = mass * 9.0;
            let i_ka = part.inertia * uu * uu;
            let i_cap = Vec3::splat(mass * (len * len / 12.0 + 16.0));
            let inertia = Vec3::new(
                if i_ka.x > 0.0 { i_ka.x } else { i_cap.x },
                if i_ka.y > 0.0 { i_ka.y } else { i_cap.y },
                if i_ka.z > 0.0 { i_ka.z } else { i_cap.z },
            )
            .max(Vec3::splat(floor_i));
            // Parent body: nearest ancestor with a body.
            let mut pb = None;
            let mut cur = skel.parents[b];
            while cur != b && cur < n {
                if let Some(&i) = body_of_bone.get(&cur) {
                    pb = Some(i);
                    break;
                }
                if cur == 0 || skel.parents[cur] == cur {
                    break;
                }
                cur = skel.parents[cur];
            }
            let joint = asset.joints.iter().find(|j| j.child == name);
            let (mut rp, mut rc) = (Vec3::ZERO, Vec3::ZERO);
            let (mut ja, mut jo, mut pa, mut po) = (Vec3::X, Vec3::Y, Vec3::X, Vec3::Y);
            if let Some(p) = pb {
                let pbd: &Body = &bodies[p];
                // The attachment point is fixed in both frames (hips and shoulders sit off the
                // spine's axis).
                rp = pbd.q.inverse() * (origin - pbd.x);
                rc = -com;
                let pname = &pbd.name;
                match joint.filter(|j| &j.parent == pname).and_then(|j| Some((joint_axes(j.axis1, j.orth1)?, joint_axes(j.axis2, j.orth2)?))) {
                    Some(((a1, o1), (a2, o2))) => (ja, jo, pa, po) = (a1, o1, a2, o2),
                    // No axes: limits are relative to the pose at death.
                    None => {
                        let rel = pbd.q.inverse() * q;
                        (pa, po) = (rel * Vec3::X, rel * Vec3::Y);
                    }
                }
            }
            let limit = joint.and_then(|j| match j.kind.as_str() {
                "hinge" => Some(Limit::Hinge(j.low.min(j.high), j.high.max(j.low))),
                _ if j.cone_type != 0 && (j.cone_x > 0.0 || j.cone_y > 0.0) => {
                    thread_local!(static TWIST2_FIXED: bool = std::env::var_os("P2ANIM_TWIST2_FIXED").is_some_and(|v| v != "0"));
                    let twist = match j.twist_type {
                        0 => std::f32::consts::PI,
                        2 if TWIST2_FIXED.with(|f| *f) => FIXED_TWIST,
                        _ if j.twist > 0.0 => j.twist,
                        _ => std::f32::consts::PI,
                    };
                    Some(Limit::Cone(j.cone_x.max(0.02), j.cone_y.max(0.02), twist))
                }
                _ => None,
            });
            let i = bodies.len();
            bodies.push(Body {
                bone: b,
                name: name.clone(),
                x,
                q,
                v: velocity,
                w: Vec3::ZERO,
                xp: x,
                qp: q,
                inv_m: 1.0 / mass,
                inv_i: Vec3::ONE / inertia,
                com,
                scale,
                len,
                parent: pb,
                rp,
                rc,
                ja,
                jo,
                pa,
                po,
                limit,
                slack: 0.0,
                spheres,
            });
            if let Some(p) = pb {
                // Allowance for a death pose already outside the limits (capped: a pose that
                // far out is broken anyway).
                let out = Self::limit_state(&bodies, i, p, 0.0).iter().map(|d| d.length()).fold(0.0f32, f32::max);
                bodies[i].slack = if out > 1e-4 { (out + 0.01).min(1.0) } else { 0.0 };
            }
            if std::env::var_os("P2ANIM_RAGDOLL_JOINTS").is_some() {
                if let (Some(p), Some(l)) = (pb, limit) {
                    eprintln!("ragdoll joint {name}: {l:?} at death {:?} slack {:.3}", Self::limit_state(&bodies, i, p, 0.0), bodies[i].slack);
                }
            }
            body_of_bone.insert(b, i);
            owner[b] = Some(i);
        }
        // Bones without bodies belong to the nearest ancestor body.
        for b in 0..n {
            if owner[b].is_none() {
                let mut cur = b;
                for _ in 0..64 {
                    if let Some(o) = owner[cur] {
                        owner[b] = Some(o);
                        break;
                    }
                    if cur == 0 || skel.parents[cur] == cur {
                        break;
                    }
                    cur = skel.parents[cur];
                }
            }
        }
        Self { bodies, owner, death_world: world, mesh_to_world, gravity: Vec3::new(0.0, 0.0, -1300.0), damping: 0.1, iterations: 2, friction: 0.8, time: 0.0, age: 0.0, speed: 0.0, speed_avg: 1000.0, still: 0.0, contacts: Vec::new() }
    }

    /// World position of the bone origin of the body owning `bone`, for probes.
    pub fn bone_origin(&self, bone: usize) -> Option<Vec3> {
        let i = (*self.owner.get(bone)?)?;
        Some(self.bodies[i].origin())
    }

    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    /// World transform of the mesh when the ragdoll started.
    pub fn start_mesh_to_world(&self) -> Mat4 {
        self.mesh_to_world
    }

    /// `KSetSkelVel`: sets every body's linear velocity (UU/s).
    pub fn set_velocity(&mut self, v: Vec3) {
        for b in &mut self.bodies {
            b.v = v;
        }
        self.wake();
    }

    /// Restarts the settle timers (`KWake`, new velocity or impulse).
    pub fn wake(&mut self) {
        self.time = 0.0;
        self.speed_avg = 1000.0;
        self.still = 0.0;
    }

    /// `KAddImpulse`: an impulse worth a whole-doll velocity change `dv` (UU/s), applied at
    /// `at` to one body: the one nearest the hit point (the bone that was shot), or the named
    /// `bone` when the point is away from every body. Like Karma, the hit body is kicked (and
    /// spun about its centre) and the joints pass the push on, so a shot limb leads the fall.
    pub fn add_impulse(&mut self, dv: Vec3, at: Vec3, bone: Option<&str>) {
        if self.bodies.is_empty() || !dv.is_finite() || !at.is_finite() {
            return;
        }
        let total: f32 = self.bodies.iter().map(|b| 1.0 / b.inv_m).sum();
        let dist = |b: &Body| {
            // Distance from the hit point to the body's segment (bone origin to far end).
            let o = b.origin();
            let tip = o + b.q * Vec3::X * b.len;
            let s = tip - o;
            let t = ((at - o).dot(s) / s.length_squared().max(1e-6)).clamp(0.0, 1.0);
            (at - (o + s * t)).length()
        };
        let (near, nd) = self.bodies.iter().enumerate().map(|(i, b)| (i, dist(b))).fold((0, f32::MAX), |a, x| if x.1 < a.1 { x } else { a });
        let named = bone.and_then(|n| self.bodies.iter().position(|b| b.name.eq_ignore_ascii_case(n)));
        let i = if nd > 40.0 { named.unwrap_or(near) } else { near };
        // Karma's joints pass a kick on within the step (the hit body's effective mass is the
        // whole doll), so: the whole doll gets the velocity change, the hit body an extra
        // `HIT_LEAD` times it (and a spin about its centre), the rest a little less, keeping
        // the total momentum the script asked for.
        const HIT_LEAD: f32 = 1.5;
        let m_hit = 1.0 / self.bodies[i].inv_m;
        let rest = if total - m_hit > 1e-3 { (total - m_hit * (1.0 + HIT_LEAD)) / (total - m_hit) } else { 1.0 };
        for (k, b) in self.bodies.iter_mut().enumerate() {
            if k != i {
                b.v += dv * rest.max(0.0);
            }
        }
        let b = &mut self.bodies[i];
        // Lever arm limited to the body's size (a far-off point would spin it wildly).
        let mut r = at - b.x;
        let lim = b.len.max(10.0) * 0.5;
        if r.length() > lim {
            r *= lim / r.length();
        }
        b.v += dv * (1.0 + HIT_LEAD);
        let p = dv * HIT_LEAD * m_hit;
        b.w = (b.w + b.inv_inertia(r.cross(p))).clamp_length_max(15.0);
        self.wake();
    }

    /// Advances the simulation by `dt` seconds (fixed substeps).
    pub fn step(&mut self, dt: f32, world: &mut dyn RagdollWorld) {
        if self.bodies.is_empty() || dt <= 0.0 {
            return;
        }
        let steps = ((dt / SUBSTEP).ceil() as usize).clamp(1, 16);
        let h = dt / steps as f32;
        // Karma-style settling: a slow doll gets strong damping so residual motion dies out
        // (Karma also damps and auto-disables slow bodies).
        let rest = self.time > 0.8 && self.speed_avg < 60.0;
        let (lin_d, ang_d) = if rest { (3.0, 6.0) } else { (self.damping, self.damping * 4.0) };
        for _ in 0..steps {
            for b in &mut self.bodies {
                b.v += self.gravity * h;
                b.v *= (1.0 - lin_d * h).max(0.0);
                b.w *= (1.0 - ang_d * h).max(0.0);
                // Karma's speed limits.
                b.v = b.v.clamp_length_max(4000.0);
                b.w = b.w.clamp_length_max(40.0);
                b.xp = b.x;
                b.qp = b.q;
                b.x += b.v * h;
                b.rotate(b.w * h);
            }
            self.contacts.clear();
            for it in 0..self.iterations {
                self.solve_joints();
                self.solve_contacts(world, it == 0);
            }
            let inv_h = 1.0 / h;
            for b in &mut self.bodies {
                b.v = (b.x - b.xp) * inv_h;
                let dq = b.q * b.qp.inverse();
                let dq = if dq.w < 0.0 { -dq } else { dq };
                b.w = Vec3::new(dq.x, dq.y, dq.z) * (2.0 * inv_h);
            }
            self.contact_velocities(h);
        }
        // NaN guard: a degenerate configuration freezes the doll where it was.
        if self.bodies.iter().any(|b| !(b.x.is_finite() && b.q.is_finite() && b.v.is_finite() && b.w.is_finite())) {
            for b in &mut self.bodies {
                b.x = b.xp;
                b.q = b.qp;
                b.v = Vec3::ZERO;
                b.w = Vec3::ZERO;
                if !(b.x.is_finite() && b.q.is_finite()) {
                    b.x = Vec3::ZERO;
                    b.q = Quat::IDENTITY;
                }
            }
        }
        self.speed = self.bodies.iter().map(Body::speed).sum::<f32>() / self.bodies.len() as f32;
        let max = self.bodies.iter().map(Body::speed).fold(0.0, f32::max);
        let k = (dt / 0.5).min(1.0);
        self.speed_avg += (self.speed - self.speed_avg) * k;
        self.still = if self.speed < SLEEP_SPEED && max < SLEEP_BODY_SPEED { self.still + dt } else { 0.0 };
        self.time += dt;
        self.age += dt;
    }

    /// Joint state for limits: (child, parent, limit violation in radians and the correction
    /// to apply to the child relative to the parent) for each part of the limit.
    ///
    /// `slack` widens every limit by that many radians (the allowance for a pose at death that
    /// was already outside the `.ka` limits). Widened limits stay well short of pi: the angle
    /// measures wrap there, and a joint let past it would read a full turn as no turn at all.
    fn limit_state(bodies: &[Body], c: usize, p: usize, slack: f32) -> Vec<Vec3> {
        const MAX_ANGLE: f32 = std::f32::consts::PI - 0.35;
        let (bp, bc) = (&bodies[p], &bodies[c]);
        let Some(limit) = bc.limit else { return Vec::new() };
        let (a_p, o_p) = (bp.q * bc.pa, bp.q * bc.po);
        let (a_c, o_c) = (bc.q * bc.ja, bc.q * bc.jo);
        let s = slack.max(0.0);
        let mut out = Vec::new();
        match limit {
            Limit::Hinge(lo, hi) => {
                // Keep the hinge axes aligned (within the slack).
                let r = rot_between(a_c, a_p);
                let rl = r.length();
                if rl > s {
                    out.push(r * (1.0 - s / rl));
                }
                let phi = signed_angle(a_p, o_p, o_c);
                let t = phi.clamp((lo - s).max(-MAX_ANGLE), (hi + s).min(MAX_ANGLE));
                if t != phi {
                    out.push(a_p * (t - phi));
                }
            }
            Limit::Cone(cx, cy, ct) => {
                let (cx, cy) = ((cx + s).min(MAX_ANGLE), (cy + s).min(MAX_ANGLE));
                let ct = if ct < std::f32::consts::PI - 1e-3 { (ct + s).min(MAX_ANGLE) } else { ct };
                let z_p = a_p.cross(o_p);
                let loc = Vec3::new(a_c.dot(a_p), a_c.dot(o_p), a_c.dot(z_p));
                let ang = loc.x.clamp(-1.0, 1.0).acos();
                if ang > 1e-4 {
                    // Swing vector in the joint frame: about Z tilts the primary axis towards
                    // the orthogonal one (X half-angle), about Y away from it (Y half-angle).
                    let sw = Vec3::new(0.0, -loc.z, loc.y).normalize_or(Vec3::Z) * ang;
                    let e = (sw.z / cx).powi(2) + (sw.y / cy).powi(2);
                    if e > 1.0 {
                        let t = Quat::from_scaled_axis(sw / e.sqrt()) * Vec3::X;
                        let target = a_p * t.x + o_p * t.y + z_p * t.z;
                        out.push(rot_between(a_c, target));
                    }
                }
                if ct < std::f32::consts::PI - 1e-3 {
                    // Twist about the bisector of the two primary axes (well defined for any
                    // swing short of pi; the old quaternion swing/twist split flipped there).
                    let n = (a_p + a_c).normalize_or(a_p);
                    let phi = signed_angle(n, o_p, o_c);
                    let t = phi.clamp(-ct, ct);
                    if t != phi {
                        out.push(n * (t - phi));
                    }
                }
            }
        }
        out
    }

    fn solve_joints(&mut self) {
        // Death poses start some joints outside their .ka limits (suburbs-3: both clavicles and
        // the neck ~0.22 rad past +-0.1 / +-0.4). Correcting that fully on the first substep
        // flung the arms out into a T and stiffened the fall; the limits ramp in over
        // LIMIT_RAMP seconds instead, so the body goes limp first and is pulled into range.
        // `P2ANIM_LIMIT_RAMP=0` restores the old instant snap (A/B checks).
        thread_local!(static RAMP: f32 = std::env::var("P2ANIM_LIMIT_RAMP").ok().and_then(|v| v.parse().ok()).unwrap_or(LIMIT_RAMP));
        //
        // The limits are hard from the first substep: only the amount a joint was already out
        // at death is tolerated, fading to nothing over the ramp. (The old ramp scaled every
        // correction down to 5% instead, keyed to `time`, which `wake()` resets on every
        // KAddImpulse/KSetSkelVel/KWake: during the fast first part of a fall, and again after
        // each hit on a corpse, the joints were nearly free and spun right round, twisting
        // past pi where the angle measures wrap and a full turn reads as no turn.)
        let ramp = RAMP.with(|r| *r);
        let fade = if ramp <= 0.0 { 0.0 } else { (1.0 - self.age / ramp).clamp(0.0, 1.0) };
        for c in 0..self.bodies.len() {
            let Some(p) = self.bodies[c].parent else { continue };
            // Joint position: the child's attachment point meets the parent's.
            let ra = self.bodies[p].q * self.bodies[c].rp;
            let rb = self.bodies[c].q * self.bodies[c].rc;
            let err = (self.bodies[p].x + ra) - (self.bodies[c].x + rb);
            pos_corr(&mut self.bodies, Some(p), c, ra, rb, err);
            // Limits, hard (XPBD weighs them by the real inertias, so they no longer fight the
            // contacts the way the old particle solver did).
            let slack = self.bodies[c].slack * fade;
            for _ in 0..2 {
                let corr = Self::limit_state(&self.bodies, c, p, slack);
                if corr.iter().all(|d| d.length_squared() < 1e-8) {
                    break;
                }
                for d in corr {
                    rot_corr(&mut self.bodies, p, c, d);
                }
            }
        }
    }

    fn solve_contacts(&mut self, world: &mut dyn RagdollWorld, first: bool) {
        for bi in 0..self.bodies.len() {
            for si in 0..self.bodies[bi].spheres.len() {
                let b = &self.bodies[bi];
                let (s, r) = b.spheres[si];
                let rs = b.q * s;
                let c = b.x + rs;
                let mut hit = None;
                if first {
                    // Fast spheres sweep from where they were (no tunnelling through floors).
                    let cp = b.xp + b.qp * s;
                    if (c - cp).length_squared() > (r * 0.5).powi(2) {
                        if let Some((stop, n)) = world.sweep(cp, c, r) {
                            let d = (stop - c).dot(n);
                            if d > 0.0 {
                                hit = Some((n, d));
                            }
                        }
                    }
                }
                if hit.is_none() {
                    if let Some((np, n)) = world.resolve(c, r) {
                        let d = (np - c).dot(n);
                        if d > 0.0 {
                            hit = Some((n, d));
                        }
                    }
                }
                let Some((n, d)) = hit else { continue };
                let d = d.min(MAX_PUSH);
                let rc = rs - n * r;
                pos_corr(&mut self.bodies, None, bi, Vec3::ZERO, rc, n * d);
                // Static friction (Karma's bKStaticFriction): the contact point does not slide
                // this substep while the tangential correction is within mu times the normal one.
                let b = &self.bodies[bi];
                let now = b.x + b.q * s - n * r;
                let prev = b.xp + b.qp * s - n * r;
                let dp = now - prev;
                let dpt = dp - n * dp.dot(n);
                if dpt.length() < self.friction * d {
                    let rc = b.q * s - n * r;
                    pos_corr(&mut self.bodies, None, bi, Vec3::ZERO, rc, -dpt);
                }
                match self.contacts.iter_mut().find(|k| k.body == bi && k.sphere == si) {
                    Some(k) => k.d += d,
                    None => self.contacts.push(Contact { body: bi, sphere: si, n, d }),
                }
            }
        }
    }

    /// Velocity pass: inelastic contacts with Coulomb dynamic friction, and bounded
    /// penetration recovery.
    fn contact_velocities(&mut self, h: f32) {
        let mu = self.friction * 0.8;
        for k in &self.contacts {
            let b = &mut self.bodies[k.body];
            let (s, r) = b.spheres[k.sphere];
            let rc = b.q * s - k.n * r;
            let u = b.v + b.w.cross(rc);
            let vn = u.dot(k.n);
            let ut = u - k.n * vn;
            let mut dv = Vec3::ZERO;
            if vn < 0.0 {
                dv -= k.n * vn;
            } else if vn > MAX_SEPARATION {
                dv -= k.n * (vn - MAX_SEPARATION);
            }
            let utl = ut.length();
            if utl > 1e-6 {
                dv -= ut / utl * (mu * k.d / h).min(utl);
            }
            let l = dv.length();
            if l > 1e-6 {
                let n = dv / l;
                let p = n * (l / b.w_pos(rc, n));
                b.v += p * b.inv_m;
                b.w += b.inv_inertia(rc.cross(p));
            }
        }
    }

    /// Mesh-space bone matrices for [`AnimInstance::set_physics_pose`], relative to the actor's
    /// current mesh-to-world transform (`actor_world * skel.mesh_xform`; the bodies stay where
    /// physics put them whatever the actor does).
    pub fn bone_matrices(&self, skel: &Skeleton, mesh_to_world: Mat4) -> Vec<Mat4> {
        let n = skel.len();
        let inv = mesh_to_world.inverse();
        let body_world: Vec<Mat4> = self.bodies.iter().map(|b| Mat4::from_scale_rotation_translation(Vec3::splat(b.scale), b.q, b.origin())).collect();
        let mut world = vec![Mat4::IDENTITY; n];
        for b in 0..n {
            world[b] = match self.owner[b] {
                Some(i) if self.bodies[i].bone == b => body_world[i],
                // Follows its owning body rigidly, relative to the pose at death.
                Some(i) => {
                    let ob = self.bodies[i].bone;
                    body_world[i] * self.death_world[ob].inverse() * self.death_world[b]
                }
                None => self.death_world[b],
            };
        }
        world.iter().map(|m| inv * *m).collect()
    }

    /// Per joint: (child body, anchor error in UU, limit violation in radians). For tests and
    /// probes.
    pub fn joint_errors(&self) -> Vec<(usize, f32, f32)> {
        let mut out = Vec::new();
        for (i, bd) in self.bodies.iter().enumerate() {
            let Some(p) = bd.parent else { continue };
            let pb = &self.bodies[p];
            let ae = ((pb.x + pb.q * bd.rp) - (bd.x + bd.q * bd.rc)).length();
            let v = Self::limit_state(&self.bodies, i, p, 0.0).iter().map(|d| d.length()).sum::<f32>();
            out.push((i, ae, v));
        }
        out
    }

    /// World position of the root body's bone origin (for moving the actor along with the body).
    pub fn root_position(&self) -> Vec3 {
        self.bodies.first().map(Body::origin).unwrap_or(Vec3::ZERO)
    }

    /// True when the ragdoll has come to rest (to stop simulating, as Karma does).
    pub fn is_asleep(&self) -> bool {
        // Karma freezes bodies that stay slow for a short while.
        self.time > 0.7 && self.still > 0.4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::BoneTransform;
    use std::sync::Arc;

    const KA: &str = r#"<?xml version="1.0"?>
<KARMA ka_file_version="1.0">
	<ASSET id="Test_Skel" graphic="x.PSK" scale="0.02" mass_scale="1" length_scale="1">
		<GEOMETRY id="Bip pelvis"><PRIMITIVE id="p1" type="sphyl"><RADIUS>0.2</RADIUS><HEIGHT>0.4</HEIGHT><TM>1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1</TM></PRIMITIVE></GEOMETRY>
		<GEOMETRY id="Bip thigh"><PRIMITIVE id="t1" type="sphyl"><RADIUS>0.2</RADIUS><HEIGHT>0.6</HEIGHT><TM>0,0,-1,0,0,1,0,0,1,0,0,0,0.3,0,0,1</TM></PRIMITIVE></GEOMETRY>
		<MODEL id="Bip pelvis" type="dynamics_and_geometry"><DYNAMICS><MASS>2</MASS></DYNAMICS></MODEL>
		<MODEL id="Bip thigh" type="dynamics_and_geometry"><DYNAMICS><MASS>1</MASS></DYNAMICS></MODEL>
		<PART id="Bip pelvis" model="Bip pelvis"><TM>1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1</TM></PART>
		<PART id="Bip thigh" model="Bip thigh" parent="Bip pelvis"><TM>1,0,0,0,0,1,0,0,0,0,1,0,0,1,0,1</TM></PART>
		<JOINT id="Bip thigh" part1="Bip thigh" part2="Bip pelvis" type="hinge"><HIGH_LIMIT>1.5</HIGH_LIMIT><LOW_LIMIT>-0.1</LOW_LIMIT><POS2>0.5,0,0</POS2></JOINT>
	</ASSET>
</KARMA>"#;

    #[test]
    fn install_assets_parse_and_settle() {
        let dir = std::path::Path::new(r"G:\SteamLibrary\steamapps\common\POSTAL2Complete\KarmaData");
        if !dir.is_dir() {
            return;
        }
        let all = load_karma_dir(dir);
        for id in ["avg_dude_skel", "avg_fem_skel", "avg_fat_skel", "avg_big_skel", "avg_mini_skel"] {
            let a = all.get(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!(a.parts.len() >= 15, "{id}: {} parts", a.parts.len());
            assert!(a.joints.len() >= 14, "{id}: {} joints", a.joints.len());
        }
        // A skeleton in the dude asset's bind pose (the PART TMs, Karma Y up turned to Unreal Z
        // up), so the .ka joint frames line up as in game.
        let a = &all["avg_dude_skel"];
        let tms: HashMap<&str, Mat4> = a.parts.iter().map(|p| (p.bone.as_str(), p.tm)).collect();
        let local = |child: &str, parent: Option<&str>| {
            let m = match parent { Some(p) => tms[p].inverse() * tms[child], None => Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2) * tms[child] };
            let (_, rot, pos) = m.to_scale_rotation_translation();
            BoneTransform { rot, pos: pos * a.uu() + if parent.is_none() { Vec3::new(0.0, 0.0, 40.0) } else { Vec3::ZERO } }
        };
        let root = a.parts.iter().find(|p| p.parent.is_none()).unwrap().bone.clone();
        let mut pose = vec![local(&root, None)];
        let mut names: Vec<String> = vec![root];
        let mut parents = vec![0usize];
        let mut k = 0;
        while k < names.len() {
            let parent = names[k].clone();
            for p in a.parts.iter().filter(|p| p.parent.as_deref() == Some(parent.as_str())) {
                pose.push(local(&p.bone, Some(&parent)));
                names.push(p.bone.clone());
                parents.push(k);
            }
            k += 1;
        }
        let skel = Arc::new(Skeleton::new(names, parents, pose, Mat4::IDENTITY));
        let mut inst = AnimInstance::new(skel.clone());
        {
            // Free fall keeps the bind pose exactly (joint frames and anchors consistent).
            let mut r = Ragdoll::new(a, &mut inst, Mat4::IDENTITY, Vec3::ZERO);
            let mut far = Floor(-1e7);
            for _ in 0..30 {
                r.step(1.0 / 30.0, &mut far);
            }
            assert!(r.joint_errors().iter().all(|&(_, e, v)| e < 0.5 && v < 0.01), "free fall: {:?}", r.joint_errors());
        }
        {
            // Limits hold under violent spin even when the doll keeps being woken (corpse hits:
            // KAddImpulse/KWake). The old ramp keyed to the wake time left the joints at 5%
            // strength after every wake, so they spun freely (corkscrewed spine and hips).
            let mut r = Ragdoll::new(a, &mut inst, Mat4::IDENTITY, Vec3::ZERO);
            for (k, b) in r.bodies.iter_mut().enumerate() {
                let s = if k % 2 == 0 { 1.0 } else { -1.0 };
                b.w = (b.q * Vec3::X) * 35.0 * s + Vec3::new(0.0, 5.0 * s, 3.0);
            }
            let mut far = Floor(-1e7);
            let mut worst = 0.0f32;
            for f in 0..60 {
                r.wake();
                r.step(1.0 / 60.0, &mut far);
                if f >= 20 {
                    worst = worst.max(r.joint_errors().iter().map(|&(_, _, v)| v).fold(0.0, f32::max));
                }
            }
            eprintln!("spin + wake: worst limit violation {worst:.3} rad");
            assert!(worst < 0.15, "limits hold under spin and repeated wakes: {worst}");
            // Twist stays within the .ka half-angle (+ a little solver give) on every cone joint.
            for (i, b) in r.bodies.iter().enumerate() {
                let (Some(p), Some(Limit::Cone(_, _, ct))) = (b.parent, b.limit) else { continue };
                if ct >= std::f32::consts::PI - 1e-3 {
                    continue;
                }
                let pb = &r.bodies[p];
                let (a_p, o_p, a_c, o_c) = (pb.q * b.pa, pb.q * b.po, b.q * b.ja, b.q * b.jo);
                let tw = signed_angle((a_p + a_c).normalize_or(a_p), o_p, o_c);
                assert!(tw.abs() < ct + 0.15, "{} twist {tw} limit {ct}", r.bodies[i].name);
            }
        }
        let mut r = Ragdoll::new(a, &mut inst, Mat4::IDENTITY, Vec3::new(200.0, 0.0, 0.0));
        assert_eq!(r.body_count(), a.parts.len());
        let mut floor = Floor(0.0);
        let mut speeds = Vec::new();
        let mut last = r.root_position();
        let mut drift = Vec::new();
        for k in 0..240 {
            r.step(1.0 / 30.0, &mut floor);
            if k % 15 == 14 {
                speeds.push(r.speed as i32);
                drift.push(((r.root_position() - last).length() * 2.0) as i32);
                last = r.root_position();
            }
        }
        eprintln!("dude settle speeds every 0.5s: {speeds:?} root drift uu/s {drift:?} asleep {}", r.is_asleep());
        let errs = r.joint_errors();
        eprintln!("joint errors (body:anchor uu/limit rad) {:?}", errs.iter().map(|(i, a, v)| format!("{i}:{a:.1}/{v:.2}")).collect::<Vec<_>>());
        assert!(r.is_asleep(), "settles within 8 s (speeds {speeds:?})");
        // Soft limits may sit a little outside under load, never folded backwards.
        assert!(errs.iter().all(|&(_, e, v)| e < 4.0 && v < 0.8), "joints hold: {errs:?}");
        let mats = r.bone_matrices(&skel, Mat4::IDENTITY);
        let top = mats.iter().map(|m| m.w_axis.z).fold(f32::MIN, f32::max);
        assert!(mats.iter().all(|m| m.is_finite() && m.w_axis.z > -1.0));
        assert!(top < 40.0, "the body lies on the floor (highest bone at {top})");
    }

    #[test]
    fn parse() {
        let a = &parse_ka(KA)[0];
        assert_eq!(a.id, "Test_Skel");
        assert!((a.uu() - 50.0).abs() < 1e-3);
        assert_eq!(a.parts.len(), 2);
        assert_eq!(a.parts[1].parent.as_deref(), Some("bip pelvis"));
        assert_eq!(a.parts[0].mass, 2.0);
        assert_eq!(a.joints[0].child, "bip thigh");
        assert_eq!(a.joints[0].high, 1.5);
        assert_eq!(a.geometry["bip thigh"][0].height, 0.6);
        assert_eq!(a.parts[1].tm.w_axis.truncate(), Vec3::new(0.0, 1.0, 0.0));
    }

    #[test]
    fn falls_and_rests_on_floor() {
        let a = &parse_ka(KA)[0];
        let skel = Arc::new(Skeleton::new(
            vec!["Bip pelvis".into(), "Bip thigh".into(), "Bip toe".into()],
            vec![0, 0, 1],
            vec![
                BoneTransform { rot: Quat::IDENTITY, pos: Vec3::new(0.0, 0.0, 100.0) },
                BoneTransform { rot: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2), pos: Vec3::new(10.0, 0.0, 0.0) },
                BoneTransform { rot: Quat::IDENTITY, pos: Vec3::new(30.0, 0.0, 0.0) },
            ],
            Mat4::IDENTITY,
        ));
        let mut inst = AnimInstance::new(skel.clone());
        let mut r = Ragdoll::new(a, &mut inst, Mat4::IDENTITY, Vec3::new(100.0, 0.0, 0.0));
        assert_eq!(r.body_count(), 2);
        let mut floor = Floor(0.0);
        for _ in 0..300 {
            r.step(1.0 / 30.0, &mut floor);
        }
        let mats = r.bone_matrices(&skel, Mat4::IDENTITY);
        for m in &mats {
            assert!(m.w_axis.z > -1.0, "no bone below the floor: {}", m.w_axis.z);
            assert!(m.is_finite());
        }
        assert!(mats[0].w_axis.z < 40.0, "fell from 100 to the floor: {}", mats[0].w_axis.z);
        assert!(mats[0].w_axis.x > 0.0, "carried by the initial velocity");
        assert!(r.is_asleep(), "came to rest (speed {})", r.speed);
        inst.set_physics_pose(Some(mats.clone()));
        assert!((inst.bone_matrices()[2].w_axis - mats[2].w_axis).length() < 1e-3);
    }
}
