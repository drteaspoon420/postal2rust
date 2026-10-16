//! Matinee scene playback as a Bevy-independent runtime.
//!
//! The scene timeline is the concatenation of its actions (`TotalSceneTime` = sum of action
//! durations; with `bConstantPathVelocity` an action lasts `PathLength / PathVelocity`). Each
//! `ActionMoveCamera` moves the viewer from the previous action's interpolation point to its own,
//! in a straight line or along a cubic Bezier whose inner control points are the previous point
//! plus `StartControlPoint` and this point plus `EndControlPoint`. `ActionPause` holds still.
//! Sub-actions run in the window `[action start + Delay, + Duration]` and keep their final state
//! afterwards (fades stay faded, FOV stays changed). Orientation sub-actions switch the camera
//! rotation mode from their start time on, easing over `EaseInTime`.

use crate::Rotator;
use crate::scene::{Action, ActionKind, Affect, CamOrientation, Orientation, PathStyle, Scene, SubActionKind};
use glam::Vec3;

/// What an orientation's `LookAt` refers to, resolved by the host.
#[derive(Debug, Clone, PartialEq)]
pub enum LookTarget<'a> {
    Export(usize),
    Tag(&'a str),
}

/// Host services the runtime needs.
pub trait SceneHost {
    /// Current world location of an actor (UU), for `CAMORIENT_LookAtActor` / `Dolly`.
    fn actor_location(&mut self, target: LookTarget<'_>) -> Option<Vec3>;
    /// Random number in 0..1 (camera shake).
    fn frand(&mut self) -> f32 {
        0.5
    }
}

/// A host without actors (tests, previews).
pub struct NoActors;
impl SceneHost for NoActors {
    fn actor_location(&mut self, _: LookTarget<'_>) -> Option<Vec3> {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SceneEvent {
    /// `SceneStarted` (the script then takes the player's pawn, hides the HUD, ...).
    Started,
    /// `SceneEnded`.
    Ended,
    /// A `SubActionTrigger` fired.
    Trigger(String),
    /// The scene finished and wants this event triggered: `NextSceneTag`, or its own tag when
    /// looping.
    TriggerNext(String),
}

/// Camera output for one tick.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
    /// Viewer location (UU) including camera shake.
    pub location: Vec3,
    pub rotation: Rotator,
    /// FOV set by `SubActionFOV`, None when untouched.
    pub fov: Option<f32>,
    /// Screen fade: color and coverage 0..1 (1 = fully covered by the color).
    pub fade: Option<([u8; 4], f32)>,
    /// `bCinematicView`: draw letterbox bars.
    pub cinematic: bool,
    /// `SubActionGameSpeed`: requested game speed.
    pub game_speed: Option<f32>,
    /// Active camera effects (class name, alpha).
    pub camera_effects: Vec<(String, f32)>,
    /// `SubActionTitle` overlays running this tick, with their current alpha.
    pub titles: Vec<(crate::scene::Title, f32)>,
    /// 0..1 of the scene (`PctSceneComplete`).
    pub pct: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubStatus {
    Waiting,
    Running,
    Expired,
}

#[derive(Debug, Clone)]
struct SubTiming {
    action: usize,
    index: usize,
    start: f32,
    end: f32,
    status: SubStatus,
}

/// Playback state of one scene (one per `SceneManager` actor).
pub struct ScenePlayer {
    pub scene: Scene,
    durations: Vec<f32>,
    starts: Vec<f32>,
    total: f32,
    subs: Vec<SubTiming>,
    /// `CurrentTime`.
    pub time: f32,
    /// `SceneSpeed` (from SubActionSceneSpeed).
    pub scene_speed: f32,
    running: bool,
    started: bool,
    // Camera state.
    orientation: Orientation,
    orientation_start: f32,
    ease_from: Rotator,
    rotation: Rotator,
    fov: Option<f32>,
    fade: Option<([u8; 4], f32)>,
    game_speed: Option<f32>,
    effects: Vec<(String, f32)>,
    /// Set by `skip()` / `fast_forward`.
    skip_requested: bool,
    pub fast_forward: bool,
}

/// `GetTotalSceneTime`: sum of the action durations (constant-velocity actions derive theirs).
pub fn action_durations(scene: &Scene) -> Vec<f32> {
    scene
        .actions
        .iter()
        .map(|a| if a.constant_path_velocity && a.path_velocity > 0.0 && a.path_length > 0.0 { a.path_length / a.path_velocity } else { a.duration.max(0.0) })
        .collect()
}

pub fn total_scene_time(scene: &Scene) -> f32 {
    action_durations(scene).iter().sum()
}

fn lerp_rot(a: Rotator, b: Rotator, t: f32) -> Rotator {
    let mut out = [0; 3];
    for i in 0..3 {
        let d = norm_delta(b[i] - a[i]);
        out[i] = a[i] + (d as f32 * t).round() as i32;
    }
    out
}

/// Shortest signed angle difference in Unreal units.
fn norm_delta(d: i32) -> i32 {
    let d = d.rem_euclid(65536);
    if d > 32768 { d - 65536 } else { d }
}

/// Rotator pointing along `dir` (pitch, yaw; roll 0), Unreal conventions.
pub fn dir_to_rotator(dir: Vec3) -> Rotator {
    if dir.length_squared() < 1e-8 {
        return [0; 3];
    }
    let yaw = dir.y.atan2(dir.x);
    let pitch = dir.z.atan2((dir.x * dir.x + dir.y * dir.y).sqrt());
    let u = 32768.0 / std::f32::consts::PI;
    [(pitch * u).round() as i32, (yaw * u).round() as i32, 0]
}

impl ScenePlayer {
    pub fn new(scene: Scene) -> Self {
        let durations = action_durations(&scene);
        let mut starts = Vec::with_capacity(durations.len());
        let mut t = 0.0;
        for d in &durations {
            starts.push(t);
            t += d;
        }
        let mut subs = Vec::new();
        for (ai, a) in scene.actions.iter().enumerate() {
            for (si, s) in a.sub_actions.iter().enumerate() {
                let start = starts[ai] + s.delay;
                subs.push(SubTiming { action: ai, index: si, start, end: start + s.duration.max(0.0), status: SubStatus::Waiting });
            }
        }
        let rot0 = scene.actions.first().map(|a| a.point_rotation).unwrap_or([0; 3]);
        Self {
            durations,
            starts,
            total: t,
            subs,
            time: 0.0,
            scene_speed: 1.0,
            running: false,
            started: false,
            orientation: Orientation::default(),
            orientation_start: 0.0,
            ease_from: rot0,
            rotation: rot0,
            fov: None,
            fade: None,
            game_speed: None,
            effects: Vec::new(),
            skip_requested: false,
            fast_forward: false,
            scene,
        }
    }

    /// `GetTotalSceneTime()`.
    pub fn total_scene_time(&self) -> f32 {
        self.total
    }

    /// `Trigger()`: starts the scene on the next tick.
    pub fn trigger(&mut self) {
        if !self.running {
            self.running = true;
            self.started = false;
        }
    }

    /// `bIsRunning`.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The player pressed skip (`bLetPlayerSkip`): jumps to the end.
    pub fn skip(&mut self) -> bool {
        if self.running && self.scene.let_player_skip {
            self.skip_requested = true;
            true
        } else {
            false
        }
    }

    /// `PctSceneComplete`.
    pub fn pct(&self) -> f32 {
        if self.total > 0.0 { self.time / self.total } else { 1.0 }
    }

    /// `SetCurrentTime`: jumps (e.g. after loading a save). Sub-actions are re-evaluated.
    pub fn set_current_time(&mut self, t: f32) {
        self.time = t.clamp(0.0, self.total);
        for s in &mut self.subs {
            s.status = if s.start > self.time { SubStatus::Waiting } else if s.end < self.time { SubStatus::Expired } else { SubStatus::Running };
        }
    }

    fn restart(&mut self) {
        self.time = 0.0;
        self.scene_speed = 1.0;
        self.orientation = Orientation::default();
        self.orientation_start = 0.0;
        let rot0 = self.scene.actions.first().map(|a| a.point_rotation).unwrap_or([0; 3]);
        self.rotation = rot0;
        self.ease_from = rot0;
        self.fov = None;
        self.fade = None;
        self.game_speed = None;
        self.effects.clear();
        for s in &mut self.subs {
            s.status = SubStatus::Waiting;
        }
    }

    /// Index of the action active at scene time `t`.
    pub fn action_at(&self, t: f32) -> usize {
        let n = self.starts.len();
        (0..n).rev().find(|&i| self.starts[i] <= t).unwrap_or(0)
    }

    /// Path position of action `i` at its local fraction `u` (0..1).
    fn path_point(&self, i: usize, u: f32) -> Vec3 {
        let a: &Action = &self.scene.actions[i];
        let prev = if i == 0 { a.point } else { self.scene.actions[i - 1].point };
        match a.kind {
            ActionKind::Pause => if i == 0 { a.point } else { prev },
            ActionKind::MoveCamera(PathStyle::Linear) => prev.lerp(a.point, u),
            ActionKind::MoveCamera(PathStyle::Bezier) => {
                let (p0, p1, p2, p3) = (prev, prev + a.start_control, a.point + a.end_control, a.point);
                let v = 1.0 - u;
                p0 * (v * v * v) + p1 * (3.0 * v * v * u) + p2 * (3.0 * v * u * u) + p3 * (u * u * u)
            }
        }
    }

    /// Viewer location at scene time `t` (no shake).
    pub fn location_at(&self, t: f32) -> Vec3 {
        if self.scene.actions.is_empty() {
            return self.scene.location;
        }
        let i = self.action_at(t);
        let d = self.durations[i];
        let u = if d > 0.0 { ((t - self.starts[i]) / d).clamp(0.0, 1.0) } else { 1.0 };
        self.path_point(i, u)
    }

    fn base_rotation(&self, host: &mut dyn SceneHost, t: f32, loc: Vec3) -> Rotator {
        let o = &self.orientation;
        match o.cam_orientation {
            CamOrientation::None => self.rotation,
            CamOrientation::FacePath => {
                let ahead = self.location_at((t + 0.05).min(self.total));
                let behind = self.location_at((t - 0.05).max(0.0));
                let r = dir_to_rotator(ahead - behind);
                if ahead.distance_squared(behind) < 1e-6 { self.rotation } else { r }
            }
            CamOrientation::LookAtActor | CamOrientation::Dolly => {
                let target = match (&o.look_at, &o.look_at_tag) {
                    (Some(e), _) => host.actor_location(LookTarget::Export(*e)),
                    (None, Some(tag)) => host.actor_location(LookTarget::Tag(tag)),
                    _ => None,
                };
                target.map(|p| dir_to_rotator(p - loc)).unwrap_or(self.rotation)
            }
            CamOrientation::Interpolate => {
                let i = self.action_at(t);
                let a = &self.scene.actions[i];
                let prev = if i == 0 { a.point_rotation } else { self.scene.actions[i - 1].point_rotation };
                let d = self.durations[i];
                let u = if d > 0.0 { ((t - self.starts[i]) / d).clamp(0.0, 1.0) } else { 1.0 };
                lerp_rot(prev, a.point_rotation, u)
            }
        }
    }

    /// Advances the scene by `dt` seconds of game time and returns the viewer state (None when
    /// the scene is not running). Events are appended to `events`.
    pub fn tick(&mut self, dt: f32, host: &mut dyn SceneHost, events: &mut Vec<SceneEvent>) -> Option<ViewState> {
        if !self.running {
            return None;
        }
        if !self.started {
            self.started = true;
            self.restart();
            events.push(SceneEvent::Started);
        } else {
            let speed = if self.fast_forward && self.scene.let_player_fast_forward { 4.0 } else { 1.0 };
            self.time += dt * self.scene_speed * speed;
        }
        if self.skip_requested {
            // Skipping runs the rest of the timeline quickly instead of jumping to the end:
            // each tick jumps to the next trigger sub-action that has not fired yet, so the
            // scripts waiting on those events (ScriptedSequences on ACTION_WaitForEvent)
            // advance one event per frame in their authored order. Firing them all in one
            // frame let a later sequence take the pawn before an earlier one ran its next
            // action: skipping the intro's TrailerScene2 left the Dude hidden (DudeHeadsOut's
            // ACTION_SetHidden ran after DudeMapScene had taken him over).
            let next = (0..self.subs.len())
                .filter(|&k| self.subs[k].status == SubStatus::Waiting && matches!(&self.scene.actions[self.subs[k].action].sub_actions[self.subs[k].index].kind, SubActionKind::Trigger { event } if !event.is_empty()))
                .map(|k| self.subs[k].start)
                .min_by(|a, b| a.total_cmp(b));
            match next {
                Some(s) if s <= self.total => self.time = self.time.max(s),
                _ => {
                    self.skip_requested = false;
                    self.time = self.total + 1e-3;
                }
            }
        }
        if self.time > self.total {
            self.running = false;
            self.started = false;
            events.push(SceneEvent::Ended);
            if let Some(next) = &self.scene.next_scene_tag {
                events.push(SceneEvent::TriggerNext(next.clone()));
            } else if self.scene.looping {
                events.push(SceneEvent::TriggerNext(self.scene.tag.clone()));
            }
            return None;
        }
        let t = self.time;
        let loc = self.location_at(t);
        // Sub-actions.
        let mut shake = Vec3::ZERO;
        let mut titles = Vec::new();
        for k in 0..self.subs.len() {
            let st = self.subs[k].clone();
            if st.status == SubStatus::Expired {
                continue;
            }
            if t < st.start {
                continue;
            }
            let first = st.status == SubStatus::Waiting;
            let ending = t > st.end;
            let span = (st.end - st.start).max(1e-4);
            let p = if ending { 1.0 } else { ((t - st.start) / span).clamp(0.0, 1.0) };
            let kind = self.scene.actions[st.action].sub_actions[st.index].kind.clone();
            match kind {
                SubActionKind::Fade { color, fade_out } => self.fade = Some((color, if fade_out { p } else { 1.0 - p })),
                SubActionKind::Fov { min, max } => self.fov = Some(min + (max - min) * p),
                SubActionKind::SceneSpeed { min, max } => self.scene_speed = (min + (max - min) * p).max(0.0),
                SubActionKind::GameSpeed { min, max } => self.game_speed = Some(min + (max - min) * p),
                SubActionKind::CameraShake { x, y, z } => {
                    if !ending {
                        let mut r = |(a, b): (f32, f32)| a + (b - a) * host.frand();
                        shake += Vec3::new(r(x), r(y), r(z));
                    }
                }
                SubActionKind::Trigger { event } => {
                    if first && !event.is_empty() {
                        events.push(SceneEvent::Trigger(event));
                    }
                }
                SubActionKind::Orientation(o) => {
                    if first {
                        self.ease_from = self.rotation;
                        self.orientation = o;
                        self.orientation_start = st.start;
                    }
                }
                SubActionKind::CameraEffect { effect_class, start_alpha, end_alpha, disable_after } => {
                    self.effects.retain(|(c, _)| *c != effect_class);
                    if !(ending && disable_after) {
                        self.effects.push((effect_class, start_alpha + (end_alpha - start_alpha) * p));
                    }
                }
                SubActionKind::Title(t) => {
                    if !ending {
                        let a = t.alpha(p);
                        titles.push((t, a));
                    }
                }
                SubActionKind::Other { .. } => {}
            }
            self.subs[k].status = if ending { SubStatus::Expired } else { SubStatus::Running };
        }
        // Rotation with ease-in from the previous orientation.
        let target = self.base_rotation(host, t, loc);
        let ease = self.orientation.ease_in_time;
        self.rotation = if ease > 0.0 && t - self.orientation_start < ease {
            let u = ((t - self.orientation_start) / ease).clamp(0.0, 1.0);
            let s = u * u * (3.0 - 2.0 * u);
            lerp_rot(self.ease_from, target, s)
        } else {
            target
        };
        Some(ViewState {
            location: loc + shake,
            rotation: self.rotation,
            fov: self.fov,
            fade: self.fade,
            cinematic: self.scene.cinematic_view && self.scene.affect == Affect::ViewportCamera,
            game_speed: self.game_speed,
            camera_effects: self.effects.clone(),
            titles,
            pct: self.pct(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{SubAction, SubActionKind};

    fn act(kind: ActionKind, p: [f32; 3], dur: f32, subs: Vec<SubAction>) -> Action {
        Action {
            kind,
            point: Vec3::from(p),
            point_rotation: [0, 0, 0],
            point_export: None,
            duration: dur,
            start_control: Vec3::ZERO,
            end_control: Vec3::ZERO,
            constant_path_velocity: false,
            path_velocity: 0.0,
            path_length: 0.0,
            sub_actions: subs,
            comment: String::new(),
        }
    }

    fn scene(actions: Vec<Action>) -> Scene {
        Scene {
            name: "SceneManager0".into(),
            export: 0,
            tag: "Intro".into(),
            location: Vec3::ZERO,
            actions,
            affect: Affect::ViewportCamera,
            affected_actor: None,
            looping: false,
            cinematic_view: true,
            player_script_tag: None,
            next_scene_tag: Some("Next".into()),
            let_player_skip: true,
            let_player_fast_forward: false,
            let_player_skip_if_seen: false,
            forbid_pausing: false,
            cant_skip_message: vec![],
        }
    }

    #[test]
    fn timeline_path_and_end() {
        let mut a2 = act(ActionKind::MoveCamera(PathStyle::Linear), [100.0, 0.0, 0.0], 0.0, vec![]);
        a2.constant_path_velocity = true;
        a2.path_velocity = 50.0;
        a2.path_length = 100.0;
        let s = scene(vec![act(ActionKind::Pause, [0.0, 0.0, 0.0], 1.0, vec![]), a2, act(ActionKind::Pause, [999.0, 0.0, 0.0], 0.5, vec![])]);
        let mut p = ScenePlayer::new(s);
        assert!((p.total_scene_time() - 3.5).abs() < 1e-5);
        let mut ev = Vec::new();
        assert!(p.tick(0.1, &mut NoActors, &mut ev).is_none(), "not triggered yet");
        p.trigger();
        let v = p.tick(0.0, &mut NoActors, &mut ev).unwrap();
        assert_eq!(ev, vec![SceneEvent::Started]);
        assert_eq!(v.location, Vec3::ZERO);
        assert!(v.cinematic);
        p.tick(2.0, &mut NoActors, &mut ev);
        assert!((p.location_at(2.0).x - 50.0).abs() < 1e-3, "halfway along the move");
        // The final pause holds the previous point (it does not jump to its own).
        assert!((p.location_at(3.2).x - 100.0).abs() < 1e-3);
        ev.clear();
        assert!(p.tick(2.0, &mut NoActors, &mut ev).is_none());
        assert_eq!(ev, vec![SceneEvent::Ended, SceneEvent::TriggerNext("Next".into())]);
        assert!(!p.is_running());
    }

    #[test]
    fn bezier_passes_endpoints() {
        let mut a = act(ActionKind::MoveCamera(PathStyle::Bezier), [100.0, 0.0, 0.0], 1.0, vec![]);
        a.start_control = Vec3::new(0.0, 50.0, 0.0);
        a.end_control = Vec3::new(0.0, 50.0, 0.0);
        let p = ScenePlayer::new(scene(vec![act(ActionKind::Pause, [0.0; 3], 0.0, vec![]), a]));
        assert!(p.location_at(0.0).length() < 1e-3);
        assert!((p.location_at(1.0) - Vec3::new(100.0, 0.0, 0.0)).length() < 1e-3);
        let mid = p.location_at(0.5);
        assert!((mid.x - 50.0).abs() < 1e-3 && (mid.y - 37.5).abs() < 1e-3, "{mid}");
    }

    #[test]
    fn sub_actions() {
        let subs = vec![
            SubAction { delay: 0.5, duration: 1.0, kind: SubActionKind::Fade { color: [0, 0, 0, 255], fade_out: true } },
            SubAction { delay: 0.0, duration: 2.0, kind: SubActionKind::Fov { min: 90.0, max: 50.0 } },
            SubAction { delay: 1.0, duration: 0.0, kind: SubActionKind::Trigger { event: "Boom".into() } },
            SubAction { delay: 0.0, duration: 1.0, kind: SubActionKind::SceneSpeed { min: 1.0, max: 1.0 } },
        ];
        let mut p = ScenePlayer::new(scene(vec![act(ActionKind::Pause, [0.0; 3], 3.0, subs)]));
        p.trigger();
        let mut ev = Vec::new();
        p.tick(0.0, &mut NoActors, &mut ev);
        let v = p.tick(1.0, &mut NoActors, &mut ev).unwrap();
        assert!((v.fade.unwrap().1 - 0.5).abs() < 1e-4);
        assert!((v.fov.unwrap() - 70.0).abs() < 1e-3);
        assert!(ev.contains(&SceneEvent::Trigger("Boom".into())));
        let n = ev.iter().filter(|e| matches!(e, SceneEvent::Trigger(_))).count();
        let v = p.tick(1.5, &mut NoActors, &mut ev).unwrap();
        assert_eq!(v.fade.unwrap().1, 1.0, "fade holds after it ends");
        assert_eq!(v.fov.unwrap(), 50.0);
        assert_eq!(ev.iter().filter(|e| matches!(e, SceneEvent::Trigger(_))).count(), n, "trigger fires once");
    }

    #[test]
    fn orientation_modes() {
        struct Host;
        impl SceneHost for Host {
            fn actor_location(&mut self, t: LookTarget<'_>) -> Option<Vec3> {
                (t == LookTarget::Tag("Dude")).then_some(Vec3::new(0.0, 100.0, 0.0))
            }
        }
        let look = Orientation { cam_orientation: CamOrientation::LookAtActor, look_at_tag: Some("Dude".into()), ..Default::default() };
        let face = Orientation { cam_orientation: CamOrientation::FacePath, ..Default::default() };
        let subs = vec![SubAction { delay: 0.0, duration: 0.0, kind: SubActionKind::Orientation(look) }];
        let subs2 = vec![SubAction { delay: 0.0, duration: 0.0, kind: SubActionKind::Orientation(face) }];
        let mut p = ScenePlayer::new(scene(vec![
            act(ActionKind::Pause, [0.0; 3], 1.0, subs),
            act(ActionKind::MoveCamera(PathStyle::Linear), [-100.0, 0.0, 0.0], 1.0, subs2),
        ]));
        p.trigger();
        let mut ev = Vec::new();
        let v = p.tick(0.0, &mut Host, &mut ev).unwrap();
        assert_eq!(v.rotation, [0, 16384, 0], "looking along +Y at the tagged actor");
        let v = p.tick(1.5, &mut Host, &mut ev).unwrap();
        assert_eq!(v.rotation[1].rem_euclid(65536), 32768, "facing the path (-X)");
    }

    #[test]
    fn skip_and_loop() {
        let mut s = scene(vec![act(ActionKind::Pause, [0.0; 3], 10.0, vec![])]);
        s.next_scene_tag = None;
        s.looping = true;
        let mut p = ScenePlayer::new(s);
        p.trigger();
        let mut ev = Vec::new();
        p.tick(0.0, &mut NoActors, &mut ev);
        assert!(p.skip());
        p.tick(0.02, &mut NoActors, &mut ev);
        assert_eq!(ev.last(), Some(&SceneEvent::TriggerNext("Intro".into())));
    }
}
