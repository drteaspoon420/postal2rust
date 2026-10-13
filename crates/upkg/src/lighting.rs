//! Map lighting data as plain values: light actors, zone ambient/fog and sky zones.
//!
//! Property names and enum orders follow the script-visible fields stored in the maps. Values
//! omitted from a map fall back to the engine's class defaults for Light actors, noted below.

use crate::package::Package;
use crate::props::{Properties, Value, begin_object};

/// `ELightType` (light modulation over time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightType {
    None,
    Steady,
    Pulse,
    Blink,
    Flicker,
    Strobe,
    BackdropLight,
    SubtlePulse,
    TexturePaletteOnce,
    TexturePaletteLoop,
    Other(u8),
}

impl LightType {
    pub fn from_byte(b: u8) -> Self {
        use LightType::*;
        match b {
            0 => None,
            1 => Steady,
            2 => Pulse,
            3 => Blink,
            4 => Flicker,
            5 => Strobe,
            6 => BackdropLight,
            7 => SubtlePulse,
            8 => TexturePaletteOnce,
            9 => TexturePaletteLoop,
            o => Other(o),
        }
    }
}

/// Selected `ELightEffect` values that change how a light is shaped.
pub mod light_effect {
    pub const NONE: u8 = 0;
    pub const SEARCHLIGHT: u8 = 4;
    pub const STATIC_SPOT: u8 = 8;
    pub const SPOTLIGHT: u8 = 12;
    pub const NON_INCIDENCE: u8 = 13;
    pub const CYLINDER: u8 = 17;
    pub const SUNLIGHT: u8 = 20;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightKind {
    Point,
    Spot,
    /// Directional light covering the whole level (LE_Sunlight or a Sunlight actor).
    Sun,
}

#[derive(Debug, Clone)]
pub struct MapLight {
    pub class: String,
    pub export: usize,
    pub kind: LightKind,
    pub location: [f32; 3],
    /// Pitch, yaw, roll (65536 = full turn).
    pub rotation: [i32; 3],
    pub light_type: LightType,
    pub light_effect: u8,
    /// 0-255 nominally; stored as float in these maps.
    pub brightness: f32,
    pub hue: u8,
    /// Unreal convention: 255 = white, lower = more saturated.
    pub saturation: u8,
    /// Radius in units of 25 Unreal units (engine convention: world radius = 25 * (LightRadius + 1)).
    pub radius: f32,
    /// Spot cone, 0-255 maps to 0-90 degrees half-angle.
    pub cone: u8,
    /// Modulation rate for animated light types (engine default 32).
    pub period: u8,
    /// Modulation phase offset (0-255 = one cycle).
    pub phase: u8,
    pub directional: bool,
    pub dynamic: bool,
    pub special_lit: bool,
    pub actor_shadows: bool,
    pub corona: bool,
}

impl MapLight {
    /// Linear-ish RGB colour in 0..1 from hue/saturation (brightness not applied).
    pub fn color(&self) -> [f32; 3] {
        hsv_to_rgb(self.hue, self.saturation)
    }

    /// Influence radius in Unreal units.
    pub fn world_radius(&self) -> f32 {
        25.0 * (self.radius + 1.0)
    }

    /// Unit vector (Unreal space) the light points along, derived from its rotation.
    pub fn direction(&self) -> [f32; 3] {
        let k = std::f32::consts::TAU / 65536.0;
        let (sp, cp) = (self.rotation[0] as f32 * k).sin_cos();
        let (sy, cy) = (self.rotation[1] as f32 * k).sin_cos();
        [cp * cy, cp * sy, sp]
    }

    /// Spot cone attenuation (0..1) for a point at `to` (unit vector from the light to the point,
    /// Unreal space); 1 for non-spot lights. LightCone 0-255 is a 0-90 degree half-angle, full
    /// strength on the axis falling smoothly to 0 at the cone edge. Without it the downward
    /// static spots over suburbs-3 window bays lit the windows beside them at full strength
    /// (frames saturated white vs orig ~120).
    pub fn spot(&self, to: [f32; 3]) -> f32 {
        if self.kind != LightKind::Spot || std::env::var_os("P2VIEW_NO_SPOT_CONE").is_some() {
            return 1.0;
        }
        let d = self.direction();
        let c = d[0] * to[0] + d[1] * to[1] + d[2] * to[2];
        let edge = (self.cone as f32 / 255.0 * std::f32::consts::FRAC_PI_2).cos();
        if c <= edge {
            return 0.0;
        }
        let t = ((c - edge) / (1.0 - edge).max(1e-4)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

/// Unreal hue/saturation (both 0-255, saturation 255 = white) to RGB in 0..1.
///
/// UE2's hue is a linear three-segment ramp (red to green over 0-85, green to blue over 85-170,
/// blue to red over 170-255), blended toward white by saturation (FGetHSV, same constants). It
/// is less yellow than an HSV wheel: hue 27 / saturation 92 (the suburbs-3 sun) is 0.80, 0.56,
/// 0.36, not 1.0, 0.76, 0.36.
pub fn hsv_to_rgb(hue: u8, saturation: u8) -> [f32; 3] {
    let (h, s) = (hue as f32, saturation as f32 / 255.0);
    let c = if h < 86.0 {
        [(85.0 - h) / 85.0, h / 85.0, 0.0]
    } else if h < 171.0 {
        [0.0, (170.0 - h) / 85.0, (h - 85.0) / 85.0]
    } else {
        [(h - 170.0) / 85.0, 0.0, (255.0 - h) / 84.0]
    };
    c.map(|v| v + (1.0 - v) * s)
}

#[allow(dead_code)]
fn hsv_wheel(hue: u8, saturation: u8) -> [f32; 3] {
    let h = hue as f32 / 256.0 * 6.0;
    let s = 1.0 - saturation as f32 / 255.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (1.0 - s, 1.0 - s * f, 1.0 - s * (1.0 - f));
    match i as i32 % 6 {
        0 => [1.0, t, p],
        1 => [q, 1.0, p],
        2 => [p, 1.0, t],
        3 => [p, q, 1.0],
        4 => [t, p, 1.0],
        _ => [1.0, p, q],
    }
}

fn byte_or(p: &Properties, n: &str, d: u8) -> u8 {
    p.byte(n).unwrap_or(d)
}

/// Brightness may be stored as a byte (stock engine) or float (this game's maps).
fn num(p: &Properties, n: &str) -> Option<f32> {
    match p.get(n)? {
        Value::Float(f) => Some(*f),
        Value::Byte(b) => Some(*b as f32),
        Value::Int(i) => Some(*i as f32),
        _ => None,
    }
}

/// Class names treated as light sources. Subclasses of Light in the shipped scripts.
const LIGHT_CLASSES: &[&str] = &["Light", "Spotlight", "Sunlight", "TriggerLight", "AttachLight"];

fn is_light_class(c: &str) -> bool {
    LIGHT_CLASSES.iter().any(|l| l.eq_ignore_ascii_case(c))
}

/// Reads every light actor in a map. Actors with LightType None or zero brightness are skipped.
pub fn read_lights(pkg: &Package) -> Vec<MapLight> {
    let mut out = Vec::new();
    for i in 0..pkg.exports.len() {
        let class = pkg.export_class(i);
        if !is_light_class(class) {
            continue;
        }
        let Ok((_, p)) = begin_object(pkg, i) else { continue };
        if let Some(l) = light_from_props(class, i, &p) {
            out.push(l);
        }
    }
    out
}

/// Builds a light from an actor's properties, using Light class defaults for missing fields.
pub fn light_from_props(class: &str, export: usize, p: &Properties) -> Option<MapLight> {
    // Light class defaults: steady, brightness 64, radius 64, saturation 255 (white), cone 128.
    let light_type = LightType::from_byte(byte_or(p, "LightType", 1));
    if light_type == LightType::None {
        return None;
    }
    let brightness = num(p, "LightBrightness").unwrap_or(64.0);
    if brightness <= 0.0 {
        return None;
    }
    let effect_default = match class.to_ascii_lowercase().as_str() {
        "sunlight" => light_effect::SUNLIGHT,
        "spotlight" => light_effect::SPOTLIGHT,
        _ => light_effect::NONE,
    };
    let light_effect = byte_or(p, "LightEffect", effect_default);
    let kind = match light_effect {
        light_effect::SUNLIGHT => LightKind::Sun,
        light_effect::SPOTLIGHT | light_effect::STATIC_SPOT | light_effect::SEARCHLIGHT => LightKind::Spot,
        _ => LightKind::Point,
    };
    Some(MapLight {
        class: class.to_string(),
        export,
        kind,
        location: p.vector("Location").unwrap_or([0.0; 3]),
        rotation: p.rotator("Rotation").unwrap_or([0; 3]),
        light_type,
        light_effect,
        brightness,
        hue: byte_or(p, "LightHue", 0),
        saturation: byte_or(p, "LightSaturation", 255),
        radius: num(p, "LightRadius").unwrap_or(64.0),
        cone: byte_or(p, "LightCone", 128),
        period: byte_or(p, "LightPeriod", 32),
        phase: byte_or(p, "LightPhase", 0),
        directional: p.bool("bDirectional").unwrap_or(false),
        dynamic: p.bool("bDynamicLight").unwrap_or(false),
        special_lit: p.bool("bSpecialLit").unwrap_or(false),
        actor_shadows: p.bool("bActorShadows").unwrap_or(false),
        corona: p.bool("bCorona").unwrap_or(false),
    })
}

#[derive(Debug, Clone)]
pub struct ZoneLight {
    pub class: String,
    pub export: usize,
    pub tag: Option<String>,
    pub location: [f32; 3],
    pub ambient_brightness: u8,
    pub ambient_hue: u8,
    pub ambient_saturation: u8,
    pub distance_fog: bool,
    /// RGBA (stored on disk as BGRA).
    pub fog_color: [u8; 4],
    pub fog_start: f32,
    pub fog_end: f32,
    pub terrain_zone: bool,
    pub sky_zone: bool,
}

impl ZoneLight {
    /// Ambient colour scaled by brightness, 0..1 per channel.
    pub fn ambient_rgb(&self) -> [f32; 3] {
        let c = hsv_to_rgb(self.ambient_hue, self.ambient_saturation);
        let b = self.ambient_brightness as f32 / 255.0;
        [c[0] * b, c[1] * b, c[2] * b]
    }
}

/// Reads LevelInfo, ZoneInfo and SkyZoneInfo actors (all carry zone light/fog settings).
pub fn read_zones(pkg: &Package) -> Vec<ZoneLight> {
    let mut out = Vec::new();
    for i in 0..pkg.exports.len() {
        let class = pkg.export_class(i);
        let lc = class.to_ascii_lowercase();
        if !(lc == "levelinfo" || lc.ends_with("zoneinfo")) {
            continue;
        }
        let Ok((_, p)) = begin_object(pkg, i) else { continue };
        let fog_color = match p.get("DistanceFogColor") {
            // Color is stored as B, G, R, A.
            Some(Value::Color(c)) => [c[2], c[1], c[0], c[3]],
            _ => [0, 0, 0, 0],
        };
        out.push(ZoneLight {
            class: class.to_string(),
            export: i,
            tag: match p.get("Tag") {
                Some(Value::Name(n)) => Some(n.clone()),
                _ => None,
            },
            location: p.vector("Location").unwrap_or([0.0; 3]),
            ambient_brightness: byte_or(&p, "AmbientBrightness", 0),
            ambient_hue: byte_or(&p, "AmbientHue", 0),
            ambient_saturation: byte_or(&p, "AmbientSaturation", 255),
            distance_fog: p.bool("bDistanceFog").unwrap_or(false),
            fog_color,
            fog_start: p.float("DistanceFogStart").unwrap_or(0.0),
            fog_end: p.float("DistanceFogEnd").unwrap_or(0.0),
            terrain_zone: p.bool("bTerrainZone").unwrap_or(false),
            sky_zone: lc == "skyzoneinfo",
        });
    }
    out
}

/// The zone settings that best represent the level's outdoor area: the LevelInfo if it sets fog
/// or ambient, else the first terrain zone, else the first zone with fog.
pub fn primary_zone(zones: &[ZoneLight]) -> Option<&ZoneLight> {
    let li = zones.iter().find(|z| z.class.eq_ignore_ascii_case("LevelInfo"));
    if let Some(l) = li {
        if l.distance_fog || l.ambient_brightness > 0 {
            return Some(l);
        }
    }
    zones
        .iter()
        .find(|z| z.terrain_zone && !z.sky_zone)
        .or_else(|| zones.iter().find(|z| z.distance_fog && !z.sky_zone))
        .or(li)
}
