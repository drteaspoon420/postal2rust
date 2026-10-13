//! Decoding of Matinee scenes from a map package: `SceneManager` actors with their `Actions`
//! (`ActionMoveCamera`, `ActionPause`), each action's `IntPoint` (an `InterpolationPoint`
//! actor's location and rotation) and its `SubActions`.

use crate::Rotator;
use glam::Vec3;
use std::sync::Arc;
use upkg::{ObjRef, Package, Properties, Reader, Value, begin_object, read_properties};

/// `ECamOrientation` (Object.uc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CamOrientation {
    #[default]
    None,
    LookAtActor,
    FacePath,
    Interpolate,
    Dolly,
}

impl CamOrientation {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::LookAtActor,
            2 => Self::FacePath,
            3 => Self::Interpolate,
            4 => Self::Dolly,
            _ => Self::None,
        }
    }
}

/// The `Orientation` struct.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Orientation {
    pub cam_orientation: CamOrientation,
    /// Export index of the `LookAt` actor in the map, if set.
    pub look_at: Option<usize>,
    /// RWS addition: look at the first actor with this tag (for spawned actors).
    pub look_at_tag: Option<String>,
    pub ease_in_time: f32,
    pub reverse: [bool; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathStyle {
    #[default]
    Linear,
    Bezier,
}

#[derive(Debug, Clone)]
pub enum SubActionKind {
    Fade { color: [u8; 4], fade_out: bool },
    Fov { min: f32, max: f32 },
    Orientation(Orientation),
    /// `RangeVector`: per axis (min, max) amplitude in UU.
    CameraShake { x: (f32, f32), y: (f32, f32), z: (f32, f32) },
    Trigger { event: String },
    SceneSpeed { min: f32, max: f32 },
    GameSpeed { min: f32, max: f32 },
    /// `SubActionCameraEffect` (a `CameraEffect` object such as MotionBlur), alpha ramp.
    CameraEffect { effect_class: String, start_alpha: f32, end_alpha: f32, disable_after: bool },
    /// Shell's `SubActionTitle`: a `CameraTitle` (a material drawn centred at a fraction of the
    /// screen width) faded in over `fade_in` and out over `fade_out` of the duration, alpha
    /// between `min_alpha` and `max_alpha` (the intro's credit titles).
    Title(Title),
    /// Other subclasses (e.g. Shell's SubActionTitle / SubActionSceneProps): class + raw props.
    Other { class: String, props: Properties },
}

/// A `CameraTitle` overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct Title {
    /// Full object path of `OverlayMaterial` (e.g. `P2Misc.Logos.staring`).
    pub material: String,
    pub color: [u8; 4],
    /// Width relative to the screen width.
    pub size_factor: f32,
    /// Centre as a fraction of the screen.
    pub x: f32,
    pub y: f32,
    /// `ERenderStyle` byte.
    pub style: u8,
    pub min_alpha: f32,
    pub max_alpha: f32,
    pub fade_in: f32,
    pub fade_out: f32,
}

impl Title {
    /// Alpha at fraction `p` (0..1) of the sub-action's duration.
    pub fn alpha(&self, p: f32) -> f32 {
        let ramp = if self.fade_in > 0.0 && p < self.fade_in {
            p / self.fade_in
        } else if self.fade_out > 0.0 && p > 1.0 - self.fade_out {
            (1.0 - p) / self.fade_out
        } else {
            1.0
        };
        self.min_alpha + (self.max_alpha - self.min_alpha) * ramp.clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone)]
pub struct SubAction {
    pub delay: f32,
    pub duration: f32,
    pub kind: SubActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    MoveCamera(PathStyle),
    Pause,
}

#[derive(Debug, Clone)]
pub struct Action {
    pub kind: ActionKind,
    /// Interpolation point location / rotation (UU, Unreal rotator).
    pub point: Vec3,
    pub point_rotation: Rotator,
    /// Export index of the interpolation point (if resolved).
    pub point_export: Option<usize>,
    pub duration: f32,
    pub start_control: Vec3,
    pub end_control: Vec3,
    pub constant_path_velocity: bool,
    pub path_velocity: f32,
    pub path_length: f32,
    pub sub_actions: Vec<SubAction>,
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Affect {
    #[default]
    ViewportCamera,
    Actor,
}

/// A decoded `SceneManager`.
#[derive(Debug, Clone)]
pub struct Scene {
    pub name: String,
    pub export: usize,
    pub tag: String,
    pub location: Vec3,
    pub actions: Vec<Action>,
    pub affect: Affect,
    pub affected_actor: Option<usize>,
    pub looping: bool,
    pub cinematic_view: bool,
    pub player_script_tag: Option<String>,
    pub next_scene_tag: Option<String>,
    pub let_player_skip: bool,
    pub let_player_fast_forward: bool,
    pub let_player_skip_if_seen: bool,
    pub forbid_pausing: bool,
    pub cant_skip_message: Vec<String>,
}

fn none_name(s: Option<String>) -> Option<String> {
    s.filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("None"))
}

fn name(p: &Properties, k: &str) -> Option<String> {
    match p.get(k)? {
        Value::Name(n) => none_name(Some(n.clone())),
        _ => None,
    }
}

fn export_of(r: ObjRef) -> Option<usize> {
    match r {
        ObjRef::Export(e) => Some(e),
        _ => None,
    }
}

/// Parses a tagged struct body (UE2 stores non-native structs as nested property lists).
fn struct_props(pkg: &Package, p: &Properties, k: &str) -> Option<Properties> {
    match p.get(k)? {
        Value::Struct { raw, .. } => read_properties(&mut Reader::new(raw), pkg).ok(),
        _ => None,
    }
}

fn range(pkg: &Package, p: &Properties, k: &str) -> (f32, f32) {
    struct_props(pkg, p, k).map(|r| (r.float("Min").unwrap_or(0.0), r.float("Max").unwrap_or(0.0))).unwrap_or((0.0, 0.0))
}

fn object_array(raw: &[u8]) -> Vec<ObjRef> {
    let mut r = Reader::new(raw);
    r.array(|r| Ok(ObjRef::from_raw(r.compact()?))).unwrap_or_default()
}

fn string_array(raw: &[u8]) -> Vec<String> {
    let mut r = Reader::new(raw);
    r.array(|r| r.fstring()).unwrap_or_default()
}

pub fn parse_orientation(o: &Properties) -> Orientation {
    Orientation {
        cam_orientation: CamOrientation::from_byte(o.byte("CamOrientation").unwrap_or(0)),
        look_at: o.object("LookAt").and_then(export_of),
        look_at_tag: name(o, "LookAtTag"),
        ease_in_time: o.float("EaseIntime").unwrap_or(0.0),
        reverse: [o.int("bReversePitch").unwrap_or(0) != 0, o.int("bReverseYaw").unwrap_or(0) != 0, o.int("bReverseRoll").unwrap_or(0) != 0],
    }
}

fn parse_sub_action(pkg: &Package, e: usize) -> Option<SubAction> {
    let class = pkg.export_class(e).to_string();
    let (_, p) = begin_object(pkg, e).ok()?;
    let kind = match class.to_ascii_lowercase().as_str() {
        "subactionfade" => SubActionKind::Fade {
            color: match p.get("FadeColor") {
                Some(Value::Color(c)) => *c,
                _ => [0, 0, 0, 255],
            },
            fade_out: p.bool("bFadeOut").unwrap_or(false),
        },
        "subactionfov" => {
            let (min, max) = range(pkg, &p, "FOV");
            SubActionKind::Fov { min, max }
        }
        "subactionorientation" => SubActionKind::Orientation(struct_props(pkg, &p, "CamOrientation").map(|o| parse_orientation(&o)).unwrap_or_default()),
        "subactioncamerashake" => {
            let s = struct_props(pkg, &p, "Shake").unwrap_or_default();
            SubActionKind::CameraShake { x: range(pkg, &s, "X"), y: range(pkg, &s, "Y"), z: range(pkg, &s, "Z") }
        }
        "subactiontrigger" => SubActionKind::Trigger { event: name(&p, "EventName").unwrap_or_default() },
        "subactionscenespeed" => {
            let (min, max) = range(pkg, &p, "SceneSpeed");
            SubActionKind::SceneSpeed { min, max }
        }
        "subactiongamespeed" => {
            let (min, max) = range(pkg, &p, "GameSpeed");
            SubActionKind::GameSpeed { min, max }
        }
        "subactioncameraeffect" => SubActionKind::CameraEffect {
            effect_class: p.object("CameraEffect").map(|r| pkg.class_of(r).to_string()).unwrap_or_default(),
            start_alpha: p.float("StartAlpha").unwrap_or(1.0),
            end_alpha: p.float("EndAlpha").unwrap_or(1.0),
            disable_after: p.bool("DisableAfterDuration").unwrap_or(false),
        },
        "subactiontitle" => match p.object("CameraEffect").and_then(|r| parse_title(pkg, r, &p)) {
            Some(t) => SubActionKind::Title(t),
            None => SubActionKind::Other { class: class.clone(), props: p.clone() },
        },
        _ => SubActionKind::Other { class: class.clone(), props: p.clone() },
    };
    Some(SubAction { delay: p.float("Delay").unwrap_or(0.0), duration: p.float("Duration").unwrap_or(0.0), kind })
}

/// The `CameraTitle` a `SubActionTitle` points at; class defaults (Shell.CameraTitle /
/// SubActionTitle defaultproperties) where the map does not override.
fn parse_title(pkg: &Package, r: ObjRef, sub: &Properties) -> Option<Title> {
    let e = export_of(r)?;
    if !pkg.export_class(e).eq_ignore_ascii_case("CameraTitle") {
        return None;
    }
    let (_, p) = begin_object(pkg, e).ok()?;
    let material = pkg.full_path(p.object("OverlayMaterial")?);
    Some(Title {
        material,
        color: match p.get("OverlayColor") {
            Some(Value::Color(c)) => *c,
            _ => [255, 255, 255, 255],
        },
        size_factor: p.float("OverlaySizeFactor").unwrap_or(0.5),
        x: p.float("OverlayXPos").unwrap_or(0.5),
        y: p.float("OverlayYPos").unwrap_or(0.5),
        style: p.byte("OverlayStyle").unwrap_or(0),
        min_alpha: sub.float("MinAlpha").unwrap_or(0.0),
        max_alpha: sub.float("MaxAlpha").unwrap_or(1.0),
        fade_in: sub.float("FadeInTimePercent").unwrap_or(0.2),
        fade_out: sub.float("FadeOutTimePercent").unwrap_or(0.2),
    })
}

fn parse_action(pkg: &Package, e: usize) -> Option<Action> {
    let class = pkg.export_class(e).to_ascii_lowercase();
    let (_, p) = begin_object(pkg, e).ok()?;
    let kind = match class.as_str() {
        "actionmovecamera" => ActionKind::MoveCamera(if p.byte("PathStyle") == Some(1) { PathStyle::Bezier } else { PathStyle::Linear }),
        "actionpause" => ActionKind::Pause,
        _ => return None,
    };
    let point_export = p.object("IntPoint").and_then(export_of);
    let (point, point_rotation) = point_export
        .and_then(|ip| begin_object(pkg, ip).ok())
        .map(|(_, ip)| (Vec3::from(ip.vector("Location").unwrap_or([0.0; 3])), ip.rotator("Rotation").unwrap_or([0; 3])))
        .unwrap_or((Vec3::ZERO, [0; 3]));
    let sub_actions = match p.get("SubActions") {
        Some(Value::Array { raw }) => object_array(raw).into_iter().filter_map(export_of).filter_map(|s| parse_sub_action(pkg, s)).collect(),
        _ => Vec::new(),
    };
    Some(Action {
        kind,
        point,
        point_rotation,
        point_export,
        duration: p.float("Duration").unwrap_or(0.0),
        start_control: Vec3::from(p.vector("StartControlPoint").unwrap_or([0.0; 3])),
        end_control: Vec3::from(p.vector("EndControlPoint").unwrap_or([0.0; 3])),
        constant_path_velocity: p.bool("bConstantPathVelocity").unwrap_or(false),
        path_velocity: p.float("PathVelocity").unwrap_or(0.0),
        path_length: p.float("PathLength").unwrap_or(0.0),
        sub_actions,
        comment: match p.get("Comment") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        },
    })
}

/// Decodes one `SceneManager` export.
pub fn parse_scene(pkg: &Package, e: usize) -> Option<Scene> {
    let (_, p) = begin_object(pkg, e).ok()?;
    let actions = match p.get("Actions") {
        Some(Value::Array { raw }) => object_array(raw).into_iter().filter_map(export_of).filter_map(|a| parse_action(pkg, a)).collect(),
        _ => Vec::new(),
    };
    Some(Scene {
        name: pkg.name(pkg.exports[e].name).to_string(),
        export: e,
        tag: name(&p, "Tag").unwrap_or_else(|| "SceneManager".into()),
        location: Vec3::from(p.vector("Location").unwrap_or([0.0; 3])),
        actions,
        affect: if p.byte("Affect") == Some(1) { Affect::Actor } else { Affect::ViewportCamera },
        affected_actor: p.object("AffectedActor").and_then(export_of),
        looping: p.bool("bLooping").unwrap_or(false),
        cinematic_view: p.bool("bCinematicView").unwrap_or(false),
        player_script_tag: name(&p, "PlayerScriptTag"),
        next_scene_tag: name(&p, "NextSceneTag"),
        let_player_skip: p.bool("bLetPlayerSkip").unwrap_or(false),
        let_player_fast_forward: p.bool("bLetPlayerFastForward").unwrap_or(false),
        let_player_skip_if_seen: p.bool("bLetPlayerSkipIfSeen").unwrap_or(false),
        forbid_pausing: p.bool("bForbidPausing").unwrap_or(false),
        cant_skip_message: match p.get("CantSkipMessage") {
            Some(Value::Array { raw }) => string_array(raw),
            _ => Vec::new(),
        },
    })
}

/// All `SceneManager`s of a map, in export order.
pub fn scenes_in(pkg: &Arc<Package>) -> Vec<Scene> {
    (0..pkg.exports.len()).filter(|&i| pkg.export_class(i).eq_ignore_ascii_case("SceneManager")).filter_map(|i| parse_scene(pkg, i)).collect()
}
