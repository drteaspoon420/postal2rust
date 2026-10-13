//! UE2-style skeletal animation runtime, independent of Bevy.
//!
//! One [`AnimInstance`] per animated actor implements the Actor animation natives with UE2
//! channel semantics:
//!
//! - Channel 0 is the base pose and covers the whole skeleton. Higher channels blend over it in
//!   index order with their own alpha, optionally restricted to the subtree under a blend root
//!   bone (`AnimBlendParams`). Alpha can ramp in and out automatically or be driven with
//!   `AnimBlendToAlpha`.
//! - `PlayAnim`/`LoopAnim` with a tween time blend from the channel's current pose to the first
//!   frame of the new sequence before playing it. `TweenAnim` only tweens, then stops.
//! - Frames are normalized like UE2's `AnimFrame` (0..1 over the sequence). Rates are normalized
//!   per second: `PlayAnim(Rate)` multiplies the sequence's own frames-per-second rate. A
//!   non-looping sequence ends at frame `(N-1)/N`.
//! - `tick` advances the channels and reports `AnimEnd(channel)` and notifies (decoded
//!   `AnimNotify_*` objects) through an [`AnimEventSink`], in time order within the tick.
//! - Bone controllers (`SetBoneRotation`, `SetBoneDirection`, `SetBoneLocation`, `SetBoneScale`)
//!   are applied after blending. Root motion can be locked out of the pose and read back as
//!   per-tick deltas (`LockRootMotion`, `GetRootLocationDelta`, ...).
//! - Attachments (`AttachToBone`) are tracked by host id with a relative location and rotation.
//!   [`AnimInstance::attachment_transform`] gives their actor-space matrix.
//!
//! Space conventions: everything is in Unreal space (Z up, units). "Mesh space" is the
//! skeleton's own space. "Actor space" applies the LodMesh transform (origin, rotation, scale).
//! The host multiplies by the actor's world transform.

use crate::notify::Notify;
use glam::{Mat3, Mat4, Quat, Vec3};
use std::collections::HashMap;
use std::sync::Arc;
use upkg::skeletal::{MeshAnimation, SkeletalMesh};

/// Pitch, yaw, roll in Unreal angle units (65536 = full turn).
pub type Rotator = [i32; 3];

const URAD: f32 = std::f32::consts::TAU / 65536.0;

/// Rotation matrix of a rotator. Columns are the rotated X (forward), Y (right) and Z (up) axes.
pub fn rotator_mat3(r: Rotator) -> Mat3 {
    let (sp, cp) = (r[0] as f32 * URAD).sin_cos();
    let (sy, cy) = (r[1] as f32 * URAD).sin_cos();
    let (sr, cr) = (r[2] as f32 * URAD).sin_cos();
    Mat3::from_cols(
        Vec3::new(cp * cy, cp * sy, sp),
        Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp),
        Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp),
    )
}

pub fn rotator_quat(r: Rotator) -> Quat {
    Quat::from_mat3(&rotator_mat3(r)).normalize()
}

/// Inverse of [`rotator_mat3`] (for orthonormal matrices).
pub fn mat3_to_rotator(m: Mat3) -> Rotator {
    let x = m.x_axis;
    let pitch = x.z.atan2((x.x * x.x + x.y * x.y).sqrt());
    let yaw = x.y.atan2(x.x);
    let roll = (-m.y_axis.z).atan2(m.z_axis.z);
    [(pitch / URAD).round() as i32, (yaw / URAD).round() as i32, (roll / URAD).round() as i32]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoneTransform {
    pub rot: Quat,
    pub pos: Vec3,
}

impl BoneTransform {
    pub const IDENTITY: Self = Self { rot: Quat::IDENTITY, pos: Vec3::ZERO };

    pub fn mat(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.rot, self.pos)
    }

    pub fn lerp(a: Self, b: Self, t: f32) -> Self {
        let b_rot = if a.rot.dot(b.rot) < 0.0 { -b.rot } else { b.rot };
        Self { rot: a.rot.slerp(b_rot, t).normalize(), pos: a.pos.lerp(b.pos, t) }
    }
}

/// UE2 `Coords`: an origin and three axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coords {
    pub origin: Vec3,
    pub x_axis: Vec3,
    pub y_axis: Vec3,
    pub z_axis: Vec3,
}

impl Coords {
    pub fn from_mat(m: Mat4) -> Self {
        Self { origin: m.w_axis.truncate(), x_axis: m.x_axis.truncate(), y_axis: m.y_axis.truncate(), z_axis: m.z_axis.truncate() }
    }
}

/// Reference skeleton: bone hierarchy and bind pose (local transforms, true rotations).
#[derive(Debug, Clone)]
pub struct Skeleton {
    pub names: Vec<String>,
    /// Parent index per bone (root points to itself).
    pub parents: Vec<usize>,
    pub ref_pose: Vec<BoneTransform>,
    /// Mesh space to actor space (LodMesh rotation, scale and origin).
    pub mesh_xform: Mat4,
}

/// Converts a stored bone quaternion to its true rotation. The root is used as stored; every
/// other bone stores the conjugate (measured in upkg's `skel_convention` example).
pub fn stored_quat(bone: usize, q: [f32; 4]) -> Quat {
    let q = Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize();
    if bone == 0 { q } else { q.conjugate() }
}

impl Skeleton {
    pub fn new(names: Vec<String>, parents: Vec<usize>, ref_pose: Vec<BoneTransform>, mesh_xform: Mat4) -> Self {
        Self { names, parents, ref_pose, mesh_xform }
    }

    pub fn from_mesh(mesh: &SkeletalMesh) -> Self {
        let mesh_xform = Mat4::from_mat3(rotator_mat3(mesh.rotation)) * Mat4::from_scale(Vec3::from(mesh.scale)) * Mat4::from_translation(-Vec3::from(mesh.origin));
        Self {
            names: mesh.bones.iter().map(|b| b.name.clone()).collect(),
            parents: mesh.bones.iter().map(|b| b.parent as usize).collect(),
            ref_pose: mesh.bones.iter().enumerate().map(|(i, b)| BoneTransform { rot: stored_quat(i, b.rotation), pos: Vec3::from(b.position) }).collect(),
            mesh_xform,
        }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn bone(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    /// True if `bone` is `root` or one of its descendants.
    pub fn in_subtree(&self, mut bone: usize, root: usize) -> bool {
        for _ in 0..self.len() {
            if bone == root {
                return true;
            }
            let p = self.parents[bone];
            if p == bone {
                return false;
            }
            bone = p;
        }
        false
    }
}

/// An animation set with its decoded notifies (shared between instances).
pub struct AnimSet {
    pub name: String,
    pub anim: Arc<MeshAnimation>,
    /// Notifies per sequence, sorted by normalized time.
    pub notifies: Vec<Vec<Notify>>,
}

impl AnimSet {
    pub fn new(anim: Arc<MeshAnimation>, notifies: Vec<Vec<Notify>>) -> Self {
        Self { name: anim.name.clone(), anim, notifies }
    }

    pub fn find(&self, seq: &str) -> Option<usize> {
        self.anim.find(seq)
    }

    /// Number of frames used for timing (at least 1).
    pub fn frames(&self, seq: usize) -> f32 {
        let s = &self.anim.sequences[seq];
        self.anim.moves.get(seq).map_or(0.0, |m| m.track_time).max(s.num_frames as f32).max(1.0)
    }
}

/// An animation set linked to a particular skeleton.
pub struct Linked {
    pub set: Arc<AnimSet>,
    /// Animation bone index to skeleton bone index.
    pub map: Vec<Option<usize>>,
}

impl Linked {
    pub fn new(set: Arc<AnimSet>, skel: &Skeleton) -> Self {
        let map = set.anim.bones.iter().map(|b| skel.bone(&b.name)).collect();
        Self { set, map }
    }

    /// Samples a sequence at a normalized frame. Unanimated bones are None.
    pub fn sample(&self, seq: usize, frame: f32, looping: bool, out: &mut [Option<BoneTransform>]) {
        let Some(m) = self.set.anim.moves.get(seq) else { return };
        let n = self.set.frames(seq);
        let f = frame * n;
        for (i, t) in m.tracks.iter().enumerate() {
            let ab = m.bone_indices.get(i).map_or(i, |&b| b as usize);
            let Some(Some(sb)) = self.map.get(ab) else { continue };
            if *sb >= out.len() {
                continue;
            }
            let (q, p) = t.sample(f, n, looping);
            out[*sb] = Some(BoneTransform { rot: stored_quat(*sb, q), pos: Vec3::from(p) });
        }
    }

    /// Samples only the track driving skeleton bone 0 (root motion bookkeeping).
    pub fn sample_root(&self, seq: usize, frame: f32, looping: bool) -> Option<BoneTransform> {
        let m = self.set.anim.moves.get(seq)?;
        let n = self.set.frames(seq);
        for (i, t) in m.tracks.iter().enumerate() {
            let ab = m.bone_indices.get(i).map_or(i, |&b| b as usize);
            if self.map.get(ab).copied().flatten() == Some(0) {
                let (q, p) = t.sample(frame * n, n, looping);
                return Some(BoneTransform { rot: stored_quat(0, q), pos: Vec3::from(p) });
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub enum AnimEvent {
    /// The channel's sequence reached its end (every loop for looping sequences, or the end of
    /// a tween started by `TweenAnim`).
    AnimEnd { channel: usize },
    /// A notify of the playing sequence was crossed.
    Notify { channel: usize, sequence: String, notify: Notify },
}

/// Receives animation events during [`AnimInstance::tick`].
pub trait AnimEventSink {
    fn event(&mut self, e: AnimEvent);
}

impl AnimEventSink for Vec<AnimEvent> {
    fn event(&mut self, e: AnimEvent) {
        self.push(e);
    }
}

/// Discards all events.
pub struct NoEvents;

impl AnimEventSink for NoEvents {
    fn event(&mut self, _: AnimEvent) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameUnit {
    /// 0..1 over the sequence (UE2 `AnimFrame`).
    Normalized,
    /// Frame number.
    Frames,
}

#[derive(Clone)]
pub struct Channel {
    pub seq: Option<(Arc<Linked>, usize)>,
    /// Normalized frame (0..1).
    pub frame: f32,
    /// Normalized frames per second.
    pub rate: f32,
    pub looping: bool,
    pub playing: bool,
    tween_from: Option<Vec<BoneTransform>>,
    tween_t: f32,
    tween_dur: f32,
    /// Stop when the tween finishes (TweenAnim).
    tween_only: bool,
    /// Current blend alpha (channels > 0).
    pub alpha: f32,
    target_alpha: f32,
    alpha_rate: f32,
    /// Alpha set by AnimBlendParams (reached after blend-in).
    pub blend_alpha: f32,
    pub blend_in: f32,
    pub blend_out: f32,
    /// Blend root bone; None = whole skeleton.
    pub root: Option<usize>,
    pub global_pose: bool,
    pub notify_enabled: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            seq: None,
            frame: 0.0,
            rate: 0.0,
            looping: false,
            playing: false,
            tween_from: None,
            tween_t: 0.0,
            tween_dur: 0.0,
            tween_only: false,
            alpha: 0.0,
            target_alpha: 0.0,
            alpha_rate: 0.0,
            blend_alpha: 0.0,
            blend_in: 0.0,
            blend_out: 0.0,
            root: None,
            global_pose: false,
            notify_enabled: true,
        }
    }
}

impl Channel {
    fn end_frame(&self) -> f32 {
        match &self.seq {
            Some((l, s)) if !self.looping => {
                let n = l.set.frames(*s);
                if n > 1.0 { (n - 1.0) / n } else { 1.0 }
            }
            _ => 1.0,
        }
    }

    pub fn sequence_name(&self) -> Option<&str> {
        self.seq.as_ref().map(|(l, s)| l.set.anim.sequences[*s].name.as_str())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RotSpace {
    /// Added on top of the animated local rotation (UE2 Space 0).
    Local,
    /// Replaces the bone's local rotation (UE2 Space 1).
    Replace,
    /// Absolute rotation in mesh space (UE2 Space 2, also SetBoneDirection).
    Component,
}

#[derive(Clone, Copy, Debug)]
struct BoneRot {
    rot: Quat,
    space: RotSpace,
    alpha: f32,
}

#[derive(Clone, Copy, Debug)]
struct BoneLoc {
    offset: Vec3,
    alpha: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Attachment {
    pub bone: usize,
    pub relative: Mat4,
}

pub struct AnimInstance {
    pub skeleton: Arc<Skeleton>,
    sets: Vec<Arc<Linked>>,
    pub channels: Vec<Channel>,
    rot_ctrl: HashMap<usize, BoneRot>,
    loc_ctrl: HashMap<usize, BoneLoc>,
    /// Scale slot -> (bone, scale).
    scale_slots: HashMap<i32, (usize, f32)>,
    attachments: HashMap<u64, Attachment>,
    root_lock: i32,
    root_prev: Option<BoneTransform>,
    root_delta_pos: Vec3,
    root_delta_rot: Quat,
    root_current: BoneTransform,
    locals: Vec<BoneTransform>,
    world: Vec<Mat4>,
    dirty: bool,
    /// Mesh-space bone matrices forced by physics (ragdoll); replaces the animated pose.
    physics_pose: Option<Vec<Mat4>>,
}

impl AnimInstance {
    pub fn new(skeleton: Arc<Skeleton>) -> Self {
        let n = skeleton.len();
        let mut s = Self {
            skeleton,
            sets: Vec::new(),
            channels: vec![Channel::default()],
            rot_ctrl: HashMap::new(),
            loc_ctrl: HashMap::new(),
            scale_slots: HashMap::new(),
            attachments: HashMap::new(),
            root_lock: 0,
            root_prev: None,
            root_delta_pos: Vec3::ZERO,
            root_delta_rot: Quat::IDENTITY,
            root_current: BoneTransform::IDENTITY,
            locals: vec![BoneTransform::IDENTITY; n],
            world: vec![Mat4::IDENTITY; n],
            dirty: true,
            physics_pose: None,
        };
        s.evaluate();
        s
    }

    // ---- animation sets -------------------------------------------------------------------

    /// `LinkSkelAnim`: makes `set` the primary animation set (searched first).
    pub fn link_skel_anim(&mut self, set: Arc<AnimSet>) {
        let l = Arc::new(Linked::new(set, &self.skeleton));
        self.sets.retain(|s| !Arc::ptr_eq(&s.set, &l.set));
        self.sets.insert(0, l);
    }

    /// Adds an extra animation set searched after the existing ones.
    pub fn add_anim_set(&mut self, set: Arc<AnimSet>) {
        if !self.sets.iter().any(|s| Arc::ptr_eq(&s.set, &set)) {
            let l = Arc::new(Linked::new(set, &self.skeleton));
            self.sets.push(l);
        }
    }

    pub fn anim_sets(&self) -> impl Iterator<Item = &Arc<AnimSet>> {
        self.sets.iter().map(|l| &l.set)
    }

    fn find_seq(&self, name: &str) -> Option<(Arc<Linked>, usize)> {
        self.sets.iter().find_map(|l| l.set.find(name).map(|s| (l.clone(), s)))
    }

    /// `HasAnim`.
    pub fn has_anim(&self, name: &str) -> bool {
        self.find_seq(name).is_some()
    }

    fn channel_mut(&mut self, c: usize) -> &mut Channel {
        if self.channels.len() <= c {
            self.channels.resize_with(c + 1, Channel::default);
        }
        &mut self.channels[c]
    }

    /// Current pose of one channel alone (used as the tween source).
    fn channel_pose(&self, c: usize) -> Vec<BoneTransform> {
        let mut pose = if c == 0 { self.locals.clone() } else { self.skeleton.ref_pose.clone() };
        if let Some(ch) = self.channels.get(c) {
            if c > 0 {
                // Start from the blended pose so higher channels tween from what is visible.
                pose = self.locals.clone();
            }
            if let Some(src) = self.sample_channel(ch) {
                for (p, s) in pose.iter_mut().zip(src) {
                    if let Some(s) = s {
                        *p = s;
                    }
                }
            }
        }
        pose
    }

    fn sample_channel(&self, ch: &Channel) -> Option<Vec<Option<BoneTransform>>> {
        let (l, s) = ch.seq.as_ref()?;
        let mut out = vec![None; self.skeleton.len()];
        l.sample(*s, ch.frame, ch.looping, &mut out);
        if let Some(from) = &ch.tween_from {
            let t = if ch.tween_dur > 0.0 { (ch.tween_t / ch.tween_dur).clamp(0.0, 1.0) } else { 1.0 };
            for (b, o) in out.iter_mut().enumerate() {
                let target = o.unwrap_or(self.skeleton.ref_pose[b]);
                *o = Some(BoneTransform::lerp(from[b], target, t));
            }
        }
        Some(out)
    }

    // ---- playback natives -----------------------------------------------------------------

    fn start(&mut self, name: &str, rate: f32, tween: f32, channel: usize, looping: bool, tween_only: bool) -> bool {
        let Some((l, s)) = self.find_seq(name) else { return false };
        if looping && !tween_only {
            if let Some(ch) = self.channels.get_mut(channel) {
                if ch.looping && ch.playing && ch.seq.as_ref().is_some_and(|(cl, cs)| Arc::ptr_eq(cl, &l) && *cs == s) {
                    // LoopAnim on the sequence already looping only updates the rate.
                    let rate = if rate > 0.0 { rate } else { 1.0 };
                    ch.rate = rate * l.set.anim.sequences[s].rate / l.set.frames(s);
                    return true;
                }
            }
        }
        if tween > 0.0 && self.dirty {
            // The tween source is the visible pose; bring it up to date first (lazy evaluation).
            self.evaluate();
        }
        let tween_from = (tween > 0.0).then(|| self.channel_pose(channel));
        let ch = self.channel_mut(channel);
        let seq_rate = l.set.anim.sequences[s].rate;
        let frames = l.set.frames(s);
        let rate = if rate > 0.0 { rate } else { 1.0 };
        ch.rate = rate * seq_rate / frames;
        ch.seq = Some((l, s));
        ch.frame = 0.0;
        ch.looping = looping;
        ch.playing = true;
        ch.tween_only = tween_only;
        ch.tween_from = tween_from;
        ch.tween_t = 0.0;
        ch.tween_dur = tween.max(0.0);
        if channel > 0 && ch.blend_in > 0.0 {
            ch.alpha = 0.0;
            ch.target_alpha = ch.blend_alpha;
            ch.alpha_rate = ch.blend_alpha / ch.blend_in;
        }
        self.dirty = true;
        true
    }

    /// `PlayAnim(Sequence, Rate, TweenTime, Channel)`. A rate of 0 means 1.
    pub fn play_anim(&mut self, name: &str, rate: f32, tween: f32, channel: usize) -> bool {
        self.start(name, rate, tween, channel, false, false)
    }

    /// `LoopAnim(Sequence, Rate, TweenTime, Channel)`.
    pub fn loop_anim(&mut self, name: &str, rate: f32, tween: f32, channel: usize) -> bool {
        self.start(name, rate, tween, channel, true, false)
    }

    /// `PlayAnimAt`: like PlayAnim but starting at a normalized frame.
    pub fn play_anim_at(&mut self, name: &str, rate: f32, tween: f32, channel: usize, at_frame: f32) -> bool {
        let ok = self.play_anim(name, rate, tween, channel);
        if ok {
            self.channels[channel].frame = at_frame.clamp(0.0, 1.0);
        }
        ok
    }

    /// `TweenAnim(Sequence, Time, Channel)`: blends to the first frame, then stops (AnimEnd).
    pub fn tween_anim(&mut self, name: &str, time: f32, channel: usize) -> bool {
        self.start(name, 0.0, time.max(1e-4), channel, false, true)
    }

    /// `StopAnimating`: freezes every channel at its current frame.
    pub fn stop_animating(&mut self) {
        for ch in &mut self.channels {
            ch.playing = false;
            ch.tween_from = None;
        }
    }

    /// `FreezeAnimAt(Time, Channel)`: normalized frame.
    pub fn freeze_anim_at(&mut self, time: f32, channel: usize) {
        let ch = self.channel_mut(channel);
        ch.frame = time.clamp(0.0, 1.0);
        ch.playing = false;
        ch.tween_from = None;
        self.dirty = true;
    }

    /// `SetAnimFrame(Time, Channel, UnitFlag)`.
    pub fn set_anim_frame(&mut self, time: f32, channel: usize, unit: FrameUnit) {
        let ch = self.channel_mut(channel);
        let n = ch.seq.as_ref().map_or(1.0, |(l, s)| l.set.frames(*s));
        ch.frame = match unit {
            FrameUnit::Normalized => time,
            FrameUnit::Frames => time / n,
        }
        .clamp(0.0, 1.0);
        self.dirty = true;
    }

    /// `IsAnimating(Channel)`: playing or tweening.
    pub fn is_animating(&self, channel: usize) -> bool {
        self.channels.get(channel).is_some_and(|c| c.playing)
    }

    /// `IsTweening(Channel)`.
    pub fn is_tweening(&self, channel: usize) -> bool {
        self.channels.get(channel).is_some_and(|c| c.tween_from.is_some())
    }

    /// `GetAnimParams(Channel)`: (sequence, normalized frame, normalized rate).
    pub fn get_anim_params(&self, channel: usize) -> Option<(String, f32, f32)> {
        let c = self.channels.get(channel)?;
        Some((c.sequence_name()?.to_string(), c.frame, if c.playing { c.rate } else { 0.0 }))
    }

    /// `AnimIsInGroup(Channel, GroupName)`.
    pub fn anim_is_in_group(&self, channel: usize, group: &str) -> bool {
        self.channels
            .get(channel)
            .and_then(|c| c.seq.as_ref())
            .is_some_and(|(l, s)| l.set.anim.sequences[*s].groups.iter().any(|g| g.eq_ignore_ascii_case(group)))
    }

    /// `EnableChannelNotify(Channel, Switch)`.
    pub fn enable_channel_notify(&mut self, channel: usize, on: bool) {
        self.channel_mut(channel).notify_enabled = on;
    }

    /// `AnimBlendParams(Stage, BlendAlpha, InTime, OutTime, BoneName, bGlobalPose)`.
    /// Returns false if the bone name is given but unknown.
    pub fn anim_blend_params(&mut self, channel: usize, alpha: f32, in_time: f32, out_time: f32, bone: Option<&str>, global_pose: bool) -> bool {
        let root = match bone {
            Some(b) if !b.is_empty() && !b.eq_ignore_ascii_case("None") => match self.skeleton.bone(b) {
                Some(i) => Some(i),
                None => return false,
            },
            _ => None,
        };
        let ch = self.channel_mut(channel);
        ch.blend_alpha = alpha.clamp(0.0, 1.0);
        ch.alpha = ch.blend_alpha;
        ch.target_alpha = ch.blend_alpha;
        ch.alpha_rate = 0.0;
        ch.blend_in = in_time.max(0.0);
        ch.blend_out = out_time.max(0.0);
        ch.root = root;
        ch.global_pose = global_pose;
        self.dirty = true;
        true
    }

    /// `AnimBlendToAlpha(Stage, TargetAlpha, TimeInterval)`.
    pub fn anim_blend_to_alpha(&mut self, channel: usize, target: f32, time: f32) {
        let ch = self.channel_mut(channel);
        ch.target_alpha = target.clamp(0.0, 1.0);
        if time <= 0.0 {
            ch.alpha = ch.target_alpha;
            ch.alpha_rate = 0.0;
        } else {
            ch.alpha_rate = (ch.target_alpha - ch.alpha).abs() / time;
        }
        self.dirty = true;
    }

    // ---- bone controllers -----------------------------------------------------------------

    /// `SetBoneRotation(BoneName, BoneTurn, Space, Alpha)`. Alpha 0 removes the controller.
    pub fn set_bone_rotation(&mut self, bone: &str, turn: Rotator, space: RotSpace, alpha: f32) -> bool {
        let Some(b) = self.skeleton.bone(bone) else { return false };
        if alpha <= 0.0 {
            self.rot_ctrl.remove(&b);
        } else {
            self.rot_ctrl.insert(b, BoneRot { rot: rotator_quat(turn), space, alpha: alpha.min(1.0) });
        }
        self.dirty = true;
        true
    }

    /// `SetBoneDirection(BoneName, BoneTurn, BoneTrans, Alpha)`: absolute mesh-space rotation
    /// plus a translation offset.
    pub fn set_bone_direction(&mut self, bone: &str, turn: Rotator, trans: Vec3, alpha: f32) -> bool {
        let ok = self.set_bone_rotation(bone, turn, RotSpace::Component, alpha);
        if ok {
            self.set_bone_location(bone, trans, alpha);
        }
        ok
    }

    /// `SetBoneLocation(BoneName, BoneTrans, Alpha)`: offset added to the local translation.
    pub fn set_bone_location(&mut self, bone: &str, trans: Vec3, alpha: f32) -> bool {
        let Some(b) = self.skeleton.bone(bone) else { return false };
        if alpha <= 0.0 || trans == Vec3::ZERO {
            self.loc_ctrl.remove(&b);
        } else {
            self.loc_ctrl.insert(b, BoneLoc { offset: trans, alpha: alpha.min(1.0) });
        }
        self.dirty = true;
        true
    }

    /// `SetBoneScale(Slot, BoneScale, BoneName)`. A scale of 1 or no bone clears the slot.
    pub fn set_bone_scale(&mut self, slot: i32, scale: f32, bone: Option<&str>) -> bool {
        let b = bone.and_then(|n| self.skeleton.bone(n));
        match b {
            Some(b) if (scale - 1.0).abs() > 1e-6 => {
                self.scale_slots.insert(slot, (b, scale));
            }
            _ => {
                self.scale_slots.remove(&slot);
            }
        }
        self.dirty = true;
        b.is_some() || bone.is_none()
    }

    // ---- root motion ----------------------------------------------------------------------

    /// `LockRootMotion(Lock)`: non-zero removes channel 0 root motion from the pose (the root
    /// stays at its first-frame position) and exposes it through the delta getters.
    pub fn lock_root_motion(&mut self, lock: i32) {
        self.root_lock = lock;
        self.root_prev = None;
        self.dirty = true;
    }

    /// `GetRootLocation`: current animated root location (actor space).
    pub fn get_root_location(&self) -> Vec3 {
        self.skeleton.mesh_xform.transform_point3(self.root_current.pos)
    }

    /// `GetRootRotation`: current animated root rotation (actor space).
    pub fn get_root_rotation(&self) -> Rotator {
        let m = Mat3::from_mat4(self.skeleton.mesh_xform) * Mat3::from_quat(self.root_current.rot);
        mat3_to_rotator(orthonormalize(m))
    }

    /// `GetRootLocationDelta`: root movement during the last tick (actor space).
    pub fn get_root_location_delta(&self) -> Vec3 {
        self.skeleton.mesh_xform.transform_vector3(self.root_delta_pos)
    }

    /// `GetRootRotationDelta`: root rotation during the last tick.
    pub fn get_root_rotation_delta(&self) -> Rotator {
        mat3_to_rotator(Mat3::from_quat(self.root_delta_rot))
    }

    // ---- attachments ----------------------------------------------------------------------

    /// `AttachToBone(Attachment, BoneName)` with the attachment's RelativeLocation/Rotation.
    pub fn attach_to_bone(&mut self, id: u64, bone: &str, rel_location: Vec3, rel_rotation: Rotator) -> bool {
        let Some(b) = self.skeleton.bone(bone) else { return false };
        let relative = Mat4::from_translation(rel_location) * Mat4::from_mat3(rotator_mat3(rel_rotation));
        self.attachments.insert(id, Attachment { bone: b, relative });
        true
    }

    /// Like [`Self::attach_to_bone`] with an arbitrary relative matrix (scale included).
    pub fn attach_with_matrix(&mut self, id: u64, bone: &str, relative: Mat4) -> bool {
        let Some(b) = self.skeleton.bone(bone) else { return false };
        self.attachments.insert(id, Attachment { bone: b, relative });
        true
    }

    /// `DetachFromBone(Attachment)`.
    pub fn detach_from_bone(&mut self, id: u64) -> bool {
        self.attachments.remove(&id).is_some()
    }

    pub fn attachments(&self) -> impl Iterator<Item = (&u64, &Attachment)> {
        self.attachments.iter()
    }

    /// Actor-space transform of an attachment.
    pub fn attachment_transform(&mut self, id: u64) -> Option<Mat4> {
        let a = *self.attachments.get(&id)?;
        Some(self.bone_actor_matrix(a.bone) * a.relative)
    }

    // ---- pose queries ---------------------------------------------------------------------

    /// Mesh-space bone matrices of the current pose.
    pub fn bone_matrices(&mut self) -> &[Mat4] {
        if self.dirty {
            self.evaluate();
        }
        &self.world
    }

    /// Mesh-space bone matrices of the animated pose without `SetBoneScale` slots (and without
    /// a physics pose): what a ragdoll is built from, so bones hidden by dismemberment (scale
    /// 0) still give it proper, invertible body frames.
    pub fn unscaled_bone_matrices(&mut self) -> Vec<Mat4> {
        if self.scale_slots.is_empty() && self.physics_pose.is_none() {
            return self.bone_matrices().to_vec();
        }
        let slots = std::mem::take(&mut self.scale_slots);
        let phys = self.physics_pose.take();
        self.evaluate();
        let out = self.world.clone();
        self.scale_slots = slots;
        self.physics_pose = phys;
        self.dirty = true;
        out
    }

    /// Local (parent-relative) bone transforms of the current pose.
    pub fn local_pose(&mut self) -> &[BoneTransform] {
        if self.dirty {
            self.evaluate();
        }
        &self.locals
    }

    pub fn bone_actor_matrix(&mut self, bone: usize) -> Mat4 {
        let x = self.skeleton.mesh_xform;
        x * self.bone_matrices()[bone]
    }

    /// `GetBoneCoords(BoneName)` in actor space (multiply by the actor's world transform).
    pub fn get_bone_coords(&mut self, bone: &str) -> Option<Coords> {
        let b = self.skeleton.bone(bone)?;
        Some(Coords::from_mat(self.bone_actor_matrix(b)))
    }

    /// `GetBoneRotation(BoneName, Space)`: Space 0 = actor space, 1 = parent-local.
    pub fn get_bone_rotation(&mut self, bone: &str, space: i32) -> Option<Rotator> {
        let b = self.skeleton.bone(bone)?;
        let m = if space == 0 { Mat3::from_mat4(self.bone_actor_matrix(b)) } else { Mat3::from_quat(self.local_pose()[b].rot) };
        Some(mat3_to_rotator(orthonormalize(m)))
    }

    // ---- time -----------------------------------------------------------------------------

    /// Advances all channels by `dt` seconds, emitting events, and updates the pose.
    pub fn tick(&mut self, dt: f32, sink: &mut dyn AnimEventSink) {
        let root_before = self.channel0_root();
        let mut wrapped = None;
        for c in 0..self.channels.len() {
            let w = advance_channel(&mut self.channels[c], c, dt, sink);
            if c == 0 {
                wrapped = w;
            }
        }
        // Root motion (channel 0) bookkeeping.
        let root_after = self.channel0_root();
        // `GetRootLocation/Rotation` read this without evaluating the pose.
        if let Some(r) = root_after {
            self.root_current = r;
        }
        self.root_delta_pos = Vec3::ZERO;
        self.root_delta_rot = Quat::IDENTITY;
        if let (Some(a), Some(b)) = (root_before, root_after) {
            match wrapped {
                Some((end, start)) => {
                    self.root_delta_pos = (end.pos - a.pos) + (b.pos - start.pos);
                    self.root_delta_rot = (b.rot * start.rot.inverse()) * (end.rot * a.rot.inverse());
                }
                None => {
                    self.root_delta_pos = b.pos - a.pos;
                    self.root_delta_rot = b.rot * a.rot.inverse();
                }
            }
        }
        // The pose is evaluated lazily (bone queries, rig posing), so idle or unseen actors cost
        // only the channel bookkeeping above.
        self.dirty = true;
    }

    /// Unlocked channel 0 root transform at the current frame.
    fn channel0_root(&self) -> Option<BoneTransform> {
        let ch = self.channels.first()?;
        let (l, s) = ch.seq.as_ref()?;
        l.sample_root(*s, ch.frame, ch.looping)
    }

    /// Recomputes local and mesh-space bone transforms.
    pub fn evaluate(&mut self) {
        let skel = self.skeleton.clone();
        let n = skel.len();
        let mut pose = skel.ref_pose.clone();
        for (c, ch) in self.channels.iter().enumerate() {
            let weight = if c == 0 { 1.0 } else { ch.alpha };
            if weight <= 0.0 {
                continue;
            }
            let Some(sampled) = self.sample_channel(ch) else { continue };
            for (b, s) in sampled.into_iter().enumerate() {
                let Some(s) = s else { continue };
                if c > 0 {
                    if let Some(r) = ch.root {
                        if !skel.in_subtree(b, r) {
                            continue;
                        }
                    }
                }
                pose[b] = if weight >= 1.0 { s } else { BoneTransform::lerp(pose[b], s, weight) };
            }
        }
        // Root motion lock: pin the root to the sequence's first frame.
        if let Some(r) = self.channel0_root() {
            self.root_current = r;
        } else {
            self.root_current = pose.first().copied().unwrap_or(BoneTransform::IDENTITY);
        }
        if self.root_lock != 0 && n > 0 {
            if let Some((l, s)) = self.channels.first().and_then(|c| c.seq.clone()) {
                let mut first = vec![None; n];
                l.sample(s, 0.0, false, &mut first);
                if let Some(f) = first[0] {
                    pose[0].pos = f.pos;
                    pose[0].rot = f.rot;
                }
            }
        }
        // Bone controllers, in bone order so parents are final before children.
        let mut world_rot = vec![Quat::IDENTITY; n];
        for b in 0..n {
            if let Some(l) = self.loc_ctrl.get(&b) {
                pose[b].pos += l.offset * l.alpha;
            }
            let parent_rot = if b == 0 || skel.parents[b] == b { Quat::IDENTITY } else { world_rot[skel.parents[b]] };
            if let Some(c) = self.rot_ctrl.get(&b) {
                let target = match c.space {
                    RotSpace::Local => pose[b].rot * c.rot,
                    RotSpace::Replace => c.rot,
                    RotSpace::Component => parent_rot.inverse() * c.rot,
                };
                pose[b].rot = BoneTransform::lerp(pose[b], BoneTransform { rot: target, pos: pose[b].pos }, c.alpha).rot;
            }
            world_rot[b] = parent_rot * pose[b].rot;
        }
        // Mesh-space matrices with bone scales.
        let mut scale = vec![1.0f32; n];
        for (b, s) in self.scale_slots.values() {
            scale[*b] *= *s;
        }
        let mut world: Vec<Mat4> = Vec::with_capacity(n);
        for b in 0..n {
            let local = pose[b].mat() * Mat4::from_scale(Vec3::splat(scale[b]));
            let m = if b == 0 || skel.parents[b] == b { local } else { world[skel.parents[b]] * local };
            world.push(m);
        }
        self.locals = pose;
        self.world = world;
        if let Some(p) = &self.physics_pose {
            if p.len() == n {
                self.world = p.clone();
                for b in 0..n {
                    let pm = if b == 0 || skel.parents[b] == b { Mat4::IDENTITY } else { self.world[skel.parents[b]] };
                    let (_, r, t) = (pm.inverse() * self.world[b]).to_scale_rotation_translation();
                    self.locals[b] = BoneTransform { rot: r, pos: t };
                }
                // `SetBoneScale` applies to ragdolls too (UE2 scales the bone after physics
                // posed it): a dismembered corpse hides the severed limb (scale 0) and keeps a
                // stump (0.2). The scaled bone keeps its origin; it and its subtree shrink
                // towards it. Without scale slots the physics pose is used untouched.
                if scale.iter().any(|s| (*s - 1.0).abs() > 1e-6) {
                    let mut out: Vec<Mat4> = Vec::with_capacity(n);
                    for b in 0..n {
                        let s = Mat4::from_scale(Vec3::splat(scale[b]));
                        let m = if b == 0 || skel.parents[b] == b {
                            p[b] * s
                        } else {
                            let pb = skel.parents[b];
                            out[pb] * (p[pb].inverse() * p[b]) * s
                        };
                        out.push(m);
                    }
                    self.world = out;
                }
            }
        }
        self.dirty = false;
    }

    /// Drives the pose from physics: mesh-space bone matrices (None returns to animation).
    pub fn set_physics_pose(&mut self, pose: Option<Vec<Mat4>>) {
        self.physics_pose = pose;
        self.dirty = true;
    }

    pub fn has_physics_pose(&self) -> bool {
        self.physics_pose.is_some()
    }
}

fn orthonormalize(m: Mat3) -> Mat3 {
    let x = m.x_axis.normalize_or_zero();
    let y = (m.y_axis - x * x.dot(m.y_axis)).normalize_or_zero();
    let z = x.cross(y);
    // Keep the handedness of the input.
    let z = if z.dot(m.z_axis) < 0.0 { -z } else { z };
    Mat3::from_cols(x, y, z)
}

/// Advances one channel. Returns (root at end, root at start) when channel 0 wrapped a loop.
fn advance_channel(ch: &mut Channel, c: usize, dt: f32, sink: &mut dyn AnimEventSink) -> Option<(BoneTransform, BoneTransform)> {
    // Alpha ramps.
    if ch.alpha_rate > 0.0 {
        let step = ch.alpha_rate * dt;
        if ch.alpha < ch.target_alpha {
            ch.alpha = (ch.alpha + step).min(ch.target_alpha);
        } else {
            ch.alpha = (ch.alpha - step).max(ch.target_alpha);
        }
        if (ch.alpha - ch.target_alpha).abs() < 1e-6 {
            ch.alpha_rate = 0.0;
        }
    }
    if !ch.playing {
        return None;
    }
    let (l, s) = ch.seq.clone()?;
    // Tween: a short crossfade from the previous pose. TweenAnim holds the target frame and
    // ends when the blend is done; PlayAnim/LoopAnim play the new sequence during the blend
    // (UE2 skeletal blending), so the tween is not a start delay.
    if ch.tween_from.is_some() {
        ch.tween_t += dt;
        if ch.tween_only {
            if ch.tween_t < ch.tween_dur {
                return None;
            }
            ch.tween_from = None;
            ch.playing = false;
            sink.event(AnimEvent::AnimEnd { channel: c });
            return None;
        }
        if ch.tween_t >= ch.tween_dur {
            ch.tween_from = None;
        }
    }
    let notifies_from_start = ch.frame == 0.0;
    let prev = if notifies_from_start { -1.0 } else { ch.frame };
    let mut frame = ch.frame + ch.rate * dt;
    let end = ch.end_frame();
    let mut wrapped = None;
    if ch.looping {
        if frame >= 1.0 {
            fire_notifies(ch, c, &l, s, prev, 1.0, sink);
            sink.event(AnimEvent::AnimEnd { channel: c });
            if c == 0 {
                if let (Some(e), Some(st)) = (l.sample_root(s, 1.0 - 1e-6, true), l.sample_root(s, 0.0, true)) {
                    wrapped = Some((e, st));
                }
            }
            frame = frame.rem_euclid(1.0);
            fire_notifies(ch, c, &l, s, -1.0, frame, sink);
        } else {
            fire_notifies(ch, c, &l, s, prev, frame, sink);
        }
        ch.frame = frame;
    } else if frame >= end {
        ch.frame = end;
        fire_notifies(ch, c, &l, s, prev, 1.0, sink);
        ch.playing = false;
        sink.event(AnimEvent::AnimEnd { channel: c });
        if c > 0 && ch.blend_out > 0.0 {
            ch.target_alpha = 0.0;
            ch.alpha_rate = ch.alpha / ch.blend_out;
        }
    } else {
        fire_notifies(ch, c, &l, s, prev, frame, sink);
        ch.frame = frame;
    }
    wrapped
}

/// Fires notifies with time in (from, to].
fn fire_notifies(ch: &Channel, c: usize, l: &Linked, s: usize, from: f32, to: f32, sink: &mut dyn AnimEventSink) {
    if !ch.notify_enabled {
        return;
    }
    let Some(list) = l.set.notifies.get(s) else { return };
    for n in list {
        if n.time > from && n.time <= to {
            sink.event(AnimEvent::Notify { channel: c, sequence: l.set.anim.sequences[s].name.clone(), notify: n.clone() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::NotifyKind;
    use upkg::Properties;
    use upkg::skeletal::{AnalogTrack, AnimBone, AnimSeq, MotionChunk};

    /// Three-bone chain: root -> spine -> arm, each 10 units along X.
    fn skel() -> Arc<Skeleton> {
        let p = |x| BoneTransform { rot: Quat::IDENTITY, pos: Vec3::new(x, 0.0, 0.0) };
        Arc::new(Skeleton::new(vec!["root".into(), "spine".into(), "arm".into()], vec![0, 0, 1], vec![p(0.0), p(10.0), p(10.0)], Mat4::IDENTITY))
    }

    fn track(rots: Vec<[f32; 4]>, pos: Vec<[f32; 3]>, times: Vec<f32>) -> AnalogTrack {
        AnalogTrack { flags: 0, rotations: rots, positions: pos, times }
    }

    /// Stored quaternion for a true rotation (non-root bones store the conjugate).
    fn stored(bone: usize, q: Quat) -> [f32; 4] {
        let q = if bone == 0 { q } else { q.conjugate() };
        [q.x, q.y, q.z, q.w]
    }

    /// Sequence "bend": arm rotates 0 -> 90 degrees about Z over 10 frames at 10 fps, root moves
    /// 0 -> 100 along X. Sequence "still": all identity. Notify at 0.5 in "bend".
    fn set() -> Arc<AnimSet> {
        let q0 = Quat::IDENTITY;
        let q90 = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let bend = MotionChunk {
            root_speed: [0.0; 3],
            track_time: 10.0,
            start_bone: 0,
            flags: 0,
            bone_indices: vec![],
            tracks: vec![
                track(vec![stored(0, q0), stored(0, q0)], vec![[0.0; 3], [100.0, 0.0, 0.0]], vec![0.0, 10.0]),
                track(vec![stored(1, q0)], vec![[10.0, 0.0, 0.0]], vec![0.0]),
                track(vec![stored(2, q0), stored(2, q90)], vec![[10.0, 0.0, 0.0]], vec![0.0, 10.0]),
            ],
            root_track: AnalogTrack::default(),
        };
        let still = MotionChunk { track_time: 10.0, ..bend.clone() };
        let still = MotionChunk {
            tracks: vec![
                track(vec![stored(0, q0)], vec![[0.0; 3]], vec![0.0]),
                track(vec![stored(1, q0)], vec![[10.0, 0.0, 0.0]], vec![0.0]),
                track(vec![stored(2, q0)], vec![[10.0, 0.0, 0.0]], vec![0.0]),
            ],
            ..still
        };
        let seq = |name: &str| AnimSeq { extra: 0.0, name: name.into(), groups: vec!["G".into()], start_frame: 0, num_frames: 10, notifies: vec![], rate: 10.0 };
        let anim = MeshAnimation {
            name: "test".into(),
            version: 0,
            bones: ["root", "spine", "arm"].iter().map(|n| AnimBone { name: n.to_string(), flags: 0, parent: 0 }).collect(),
            moves: vec![bend, still],
            sequences: vec![seq("bend"), seq("still")],
            props: Properties::default(),
        };
        let notifies = vec![vec![Notify { time: 0.5, kind: NotifyKind::Script { notify_name: "Half".into() } }], vec![]];
        Arc::new(AnimSet::new(Arc::new(anim), notifies))
    }

    fn inst() -> AnimInstance {
        let mut i = AnimInstance::new(skel());
        i.link_skel_anim(set());
        i
    }

    fn arm_angle(i: &mut AnimInstance) -> f32 {
        let q = i.local_pose()[2].rot;
        let (axis, angle) = q.to_axis_angle();
        angle.to_degrees() * axis.z.signum()
    }

    #[test]
    fn rotator_round_trip() {
        for r in [[0, 0, 0], [1000, 2000, 3000], [-8000, 16384, -12000], [4000, -30000, 20000]] {
            let back = mat3_to_rotator(rotator_mat3(r));
            for k in 0..3 {
                assert!((back[k] - r[k]).abs() <= 2, "{r:?} -> {back:?}");
            }
        }
        // Yaw 90 turns X into Y.
        let m = rotator_mat3([0, 16384, 0]);
        assert!((m.x_axis - Vec3::Y).length() < 1e-5);
    }

    #[test]
    fn play_advances_and_ends_once_with_notify() {
        let mut i = inst();
        assert!(i.has_anim("BEND"));
        assert!(!i.has_anim("nope"));
        assert!(i.play_anim("bend", 1.0, 0.0, 0));
        let mut ev = Vec::new();
        // 10 frames at 10 fps: normalized rate 1/s; ends at 0.9.
        i.tick(0.45, &mut ev);
        assert!((arm_angle(&mut i) - 40.5).abs() < 0.5, "angle {}", arm_angle(&mut i));
        assert!(ev.is_empty());
        i.tick(0.1, &mut ev);
        assert!(matches!(&ev[..], [AnimEvent::Notify { channel: 0, notify, .. }] if notify.time == 0.5));
        ev.clear();
        i.tick(1.0, &mut ev);
        assert_eq!(ev.iter().filter(|e| matches!(e, AnimEvent::AnimEnd { channel: 0 })).count(), 1);
        assert!(!i.is_animating(0));
        let (name, frame, rate) = i.get_anim_params(0).unwrap();
        assert_eq!(name, "bend");
        assert!((frame - 0.9).abs() < 1e-6 && rate == 0.0);
        ev.clear();
        i.tick(1.0, &mut ev);
        assert!(ev.is_empty(), "no events after the end");
        assert!(i.anim_is_in_group(0, "g"));
    }

    #[test]
    fn looping_fires_end_and_notifies_every_loop() {
        let mut i = inst();
        i.loop_anim("bend", 2.0, 0.0, 0);
        let mut ev = Vec::new();
        for _ in 0..30 {
            i.tick(0.05, &mut ev);
        }
        // 1.5 s at rate 2 = 3 loops.
        let ends = ev.iter().filter(|e| matches!(e, AnimEvent::AnimEnd { .. })).count();
        let notes = ev.iter().filter(|e| matches!(e, AnimEvent::Notify { .. })).count();
        assert_eq!(ends, 3);
        assert_eq!(notes, 3);
        assert!(i.is_animating(0));
        // LoopAnim again on the same sequence keeps the frame.
        let f = i.get_anim_params(0).unwrap().1;
        i.loop_anim("bend", 1.0, 0.0, 0);
        assert_eq!(i.get_anim_params(0).unwrap().1, f);
        i.enable_channel_notify(0, false);
        ev.clear();
        for _ in 0..20 {
            i.tick(0.05, &mut ev);
        }
        assert!(!ev.iter().any(|e| matches!(e, AnimEvent::Notify { .. })));
    }

    #[test]
    fn freeze_and_set_frame() {
        let mut i = inst();
        i.play_anim("bend", 1.0, 0.0, 0);
        i.freeze_anim_at(0.5, 0);
        assert!(!i.is_animating(0));
        assert!((arm_angle(&mut i) - 45.0).abs() < 0.5);
        i.set_anim_frame(2.5, 0, FrameUnit::Frames);
        assert!((arm_angle(&mut i) - 22.5).abs() < 0.5);
        i.stop_animating();
        i.tick(1.0, &mut NoEvents);
        assert!((arm_angle(&mut i) - 22.5).abs() < 0.5);
    }

    #[test]
    fn tween_blends_from_previous_pose() {
        let mut i = inst();
        i.play_anim("bend", 1.0, 0.0, 0);
        i.freeze_anim_at(0.9, 0); // arm at 81 degrees
        i.tick(0.0, &mut NoEvents);
        assert!((arm_angle(&mut i) - 81.0).abs() < 0.5);
        let mut ev = Vec::new();
        assert!(i.tween_anim("still", 1.0, 0));
        assert!(i.is_tweening(0) && i.is_animating(0));
        i.tick(0.5, &mut ev);
        assert!((arm_angle(&mut i) - 40.5).abs() < 0.6, "half-way {}", arm_angle(&mut i));
        i.tick(0.6, &mut ev);
        assert!(arm_angle(&mut i).abs() < 0.5);
        assert!(matches!(&ev[..], [AnimEvent::AnimEnd { channel: 0 }]));
        assert!(!i.is_animating(0) && !i.is_tweening(0));
        // PlayAnim with a tween crossfades while the new sequence already plays.
        i.freeze_anim_at(0.9, 0);
        i.play_anim("bend", 1.0, 0.2, 0);
        i.tick(0.1, &mut NoEvents);
        assert!(i.is_tweening(0));
        assert!(i.channels[0].frame > 0.0, "tween must not delay playback");
        i.tick(0.15, &mut NoEvents);
        assert!(!i.is_tweening(0) && i.is_animating(0));
    }

    #[test]
    fn blend_channel_respects_root_and_alpha() {
        let mut i = inst();
        i.loop_anim("still", 1.0, 0.0, 0);
        assert!(!i.anim_blend_params(1, 0.5, 0.0, 0.0, Some("missing"), false));
        assert!(i.anim_blend_params(1, 0.5, 0.0, 0.0, Some("arm"), false));
        i.play_anim("bend", 1.0, 0.0, 1);
        i.freeze_anim_at(0.9, 1); // channel 1 wants 81 degrees and root at x=90
        i.tick(0.0, &mut NoEvents);
        assert!((arm_angle(&mut i) - 40.5).abs() < 0.6, "alpha 0.5 -> {}", arm_angle(&mut i));
        // Root is outside the blend subtree: still at 0.
        assert!(i.local_pose()[0].pos.length() < 1e-4);
        i.anim_blend_to_alpha(1, 1.0, 1.0);
        i.tick(0.5, &mut NoEvents);
        assert!((i.channels[1].alpha - 0.75).abs() < 1e-5, "ramping {}", i.channels[1].alpha);
        i.tick(0.5, &mut NoEvents);
        assert!((arm_angle(&mut i) - 81.0).abs() < 0.6);
        // Whole-skeleton blend moves the root too.
        i.anim_blend_params(1, 1.0, 0.0, 0.0, None, false);
        i.tick(0.0, &mut NoEvents);
        assert!((i.local_pose()[0].pos.x - 90.0).abs() < 1e-3);
    }

    #[test]
    fn blend_in_and_out() {
        let mut i = inst();
        i.anim_blend_params(1, 1.0, 0.2, 0.4, Some("spine"), false);
        i.play_anim("bend", 1.0, 0.0, 1);
        assert_eq!(i.channels[1].alpha, 0.0);
        i.tick(0.1, &mut NoEvents);
        assert!((i.channels[1].alpha - 0.5).abs() < 1e-5);
        i.tick(0.1, &mut NoEvents);
        assert!((i.channels[1].alpha - 1.0).abs() < 1e-5);
        i.tick(0.8, &mut NoEvents); // reaches the end (0.9)
        assert!(!i.is_animating(1));
        i.tick(0.2, &mut NoEvents);
        assert!((i.channels[1].alpha - 0.5).abs() < 0.05, "fading out {}", i.channels[1].alpha);
    }

    #[test]
    fn bone_controllers() {
        let mut i = inst();
        assert!(i.set_bone_rotation("arm", [0, 16384, 0], RotSpace::Replace, 1.0));
        assert!((arm_angle(&mut i) - 90.0).abs() < 0.1);
        i.set_bone_rotation("arm", [0, 16384, 0], RotSpace::Replace, 0.5);
        assert!((arm_angle(&mut i) - 45.0).abs() < 0.1);
        i.set_bone_rotation("arm", [0, 0, 0], RotSpace::Replace, 0.0);
        assert!(arm_angle(&mut i).abs() < 0.1);
        // Component space: spine turned 90, arm forced to world yaw 0 => local -90.
        i.set_bone_rotation("spine", [0, 16384, 0], RotSpace::Replace, 1.0);
        i.set_bone_direction("arm", [0, 0, 0], Vec3::ZERO, 1.0);
        assert!((arm_angle(&mut i) + 90.0).abs() < 0.1);
        let r = i.get_bone_rotation("arm", 0).unwrap();
        assert!(r[1].abs() < 3, "actor-space yaw {r:?}");
        // Location offset and scale.
        i.set_bone_location("arm", Vec3::new(0.0, 0.0, 5.0), 1.0);
        let c = i.get_bone_coords("arm").unwrap();
        assert!((c.origin.z - 5.0).abs() < 1e-4);
        i.set_bone_scale(0, 2.0, Some("arm"));
        let c = i.get_bone_coords("arm").unwrap();
        assert!((c.x_axis.length() - 2.0).abs() < 1e-4);
        i.set_bone_scale(0, 1.0, None);
        let c = i.get_bone_coords("arm").unwrap();
        assert!((c.x_axis.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn bone_scale_applies_to_physics_pose() {
        // A ragdoll drives the pose; a limb severed with SetBoneScale(.., 0) must still vanish
        // (it used to stay visible: the physics matrices replaced the scaled ones).
        let mut i = inst();
        let n = i.skeleton.len();
        let arm = i.skeleton.bone("arm").unwrap();
        let phys: Vec<Mat4> = i.bone_matrices().iter().map(|m| Mat4::from_translation(Vec3::new(0.0, 0.0, 30.0)) * *m).collect();
        i.set_physics_pose(Some(phys.clone()));
        assert!((i.bone_matrices()[arm].x_axis.length() - 1.0).abs() < 1e-4);
        i.set_bone_scale(3, 0.0, Some("arm"));
        let m = i.bone_matrices()[arm];
        assert!(m.x_axis.length() < 1e-6 && m.y_axis.length() < 1e-6, "severed bone collapses");
        assert!((m.w_axis - phys[arm].w_axis).length() < 1e-3, "it keeps its physics origin");
        // Children of the scaled bone collapse onto it as well.
        for b in 0..n {
            if b != arm && i.skeleton.in_subtree(b, arm) {
                assert!(i.bone_matrices()[b].x_axis.length() < 1e-6);
            }
        }
        // The ragdoll is built from the unscaled pose.
        assert!(i.unscaled_bone_matrices().iter().all(|m| (m.x_axis.length() - 1.0).abs() < 1e-4));
        assert!(i.bone_matrices()[arm].x_axis.length() < 1e-6, "scale still applied afterwards");
    }

    #[test]
    fn root_motion_lock_and_delta_across_loop() {
        let mut i = inst();
        i.lock_root_motion(1);
        i.loop_anim("bend", 1.0, 0.0, 0);
        i.tick(0.5, &mut NoEvents);
        assert!(i.local_pose()[0].pos.length() < 1e-4, "root pinned");
        assert!((i.get_root_location().x - 50.0).abs() < 1e-3);
        i.tick(0.3, &mut NoEvents);
        assert!((i.get_root_location_delta().x - 30.0).abs() < 1e-3);
        // Wrap: 0.8 -> 1.1. The track goes 80 -> (100 at the loop point) then 0 -> 10.
        i.tick(0.3, &mut NoEvents);
        let d = i.get_root_location_delta().x;
        assert!((d - 30.0).abs() < 0.01, "delta across wrap {d}");
        i.lock_root_motion(0);
        i.tick(0.0, &mut NoEvents);
        assert!((i.local_pose()[0].pos.x - 10.0).abs() < 1e-3);
    }

    #[test]
    fn attachments_follow_bones() {
        let mut i = inst();
        assert!(i.attach_to_bone(7, "arm", Vec3::new(1.0, 0.0, 0.0), [0, 0, 0]));
        assert!(!i.attach_to_bone(8, "nope", Vec3::ZERO, [0, 0, 0]));
        let m = i.attachment_transform(7).unwrap();
        assert!((m.w_axis.truncate() - Vec3::new(21.0, 0.0, 0.0)).length() < 1e-4);
        i.set_bone_rotation("arm", [0, 16384, 0], RotSpace::Replace, 1.0);
        let m = i.attachment_transform(7).unwrap();
        assert!((m.w_axis.truncate() - Vec3::new(20.0, 1.0, 0.0)).length() < 1e-4);
        assert!(i.detach_from_bone(7));
        assert!(i.attachment_transform(7).is_none());
    }
}
