//! Material graph objects (Texture wrappers and modifiers) decoded as plain data.
//!
//! These classes have no native payload beyond their tagged properties in v118: every field
//! below is read from the property list, with the class defaults (from Engine.u) applied when a
//! property is absent. References to other materials are kept as `ObjRef`s for the caller to
//! resolve (see `library::Library::resolve`).

use crate::package::{ObjRef, Package};
use crate::props::{Properties, Value, begin_object};
use crate::reader::Reader;
use crate::{Error, Result};

/// RGBA colour (the file stores Color structs as B, G, R, A; this is already reordered).
pub type Rgba = [u8; 4];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameBufferBlending {
    #[default]
    Overwrite,
    Modulate,
    AlphaBlend,
    AlphaModulate,
    Translucent,
    Darken,
    Brighten,
    Invisible,
}

impl FrameBufferBlending {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Modulate,
            2 => Self::AlphaBlend,
            3 => Self::AlphaModulate,
            4 => Self::Translucent,
            5 => Self::Darken,
            6 => Self::Brighten,
            7 => Self::Invisible,
            _ => Self::Overwrite,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputBlending {
    #[default]
    Normal,
    Masked,
    Modulate,
    Translucent,
    Invisible,
    Brighten,
    Darken,
}

impl OutputBlending {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Masked,
            2 => Self::Modulate,
            3 => Self::Translucent,
            4 => Self::Invisible,
            5 => Self::Brighten,
            6 => Self::Darken,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorOp {
    #[default]
    UseMaterial1,
    UseMaterial2,
    Multiply,
    Add,
    Subtract,
    AlphaBlendWithMask,
    AddWithMaskModulation,
    UseMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlphaOp {
    #[default]
    UseMask,
    Multiply,
    Add,
    UseMaterial1,
    UseMaterial2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RotationType {
    #[default]
    Fixed,
    ConstantlyRotating,
    Oscillating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OscillationType {
    #[default]
    Pan,
    Stretch,
    StretchRepeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FadeType {
    #[default]
    Linear,
    Sinusoidal,
}

/// The texture-level fields a 2D renderer needs (pixel data is read with `texture::read_texture`).
#[derive(Debug, Clone)]
pub struct TextureInfo {
    pub usize: i32,
    pub vsize: i32,
    pub masked: bool,
    pub alpha_texture: bool,
    pub two_sided: bool,
    /// Next frame of an animated texture chain.
    pub anim_next: ObjRef,
    pub min_frame_rate: f32,
    pub max_frame_rate: f32,
    /// 0 = wrap, 1 = clamp.
    pub u_clamp_mode: u8,
    pub v_clamp_mode: u8,
}

#[derive(Debug, Clone)]
pub enum Material {
    Texture(TextureInfo),
    Shader {
        diffuse: ObjRef,
        opacity: ObjRef,
        specular: ObjRef,
        specularity_mask: ObjRef,
        self_illumination: ObjRef,
        self_illumination_mask: ObjRef,
        detail: ObjRef,
        detail_scale: f32,
        output_blending: OutputBlending,
        two_sided: bool,
    },
    Combiner {
        color_op: ColorOp,
        alpha_op: AlphaOp,
        material1: ObjRef,
        material2: ObjRef,
        mask: ObjRef,
        invert_mask: bool,
        modulate2x: bool,
        modulate4x: bool,
    },
    FinalBlend {
        material: ObjRef,
        blending: FrameBufferBlending,
        z_write: bool,
        z_test: bool,
        alpha_test: bool,
        two_sided: bool,
        alpha_ref: u8,
    },
    ColorModifier {
        material: ObjRef,
        color: Rgba,
        two_sided: bool,
        alpha_blend: bool,
    },
    TexPanner {
        material: ObjRef,
        /// Pitch, Yaw, Roll (65536 = full turn).
        pan_direction: [i32; 3],
        pan_rate: f32,
    },
    TexRotator {
        material: ObjRef,
        kind: RotationType,
        rotation: [i32; 3],
        u_offset: f32,
        v_offset: f32,
        oscillation_rate: [i32; 3],
        oscillation_amplitude: [i32; 3],
        oscillation_phase: [i32; 3],
    },
    TexScaler {
        material: ObjRef,
        u_scale: f32,
        v_scale: f32,
        u_offset: f32,
        v_offset: f32,
    },
    TexOscillator {
        material: ObjRef,
        u_rate: f32,
        v_rate: f32,
        u_phase: f32,
        v_phase: f32,
        u_amplitude: f32,
        v_amplitude: f32,
        u_kind: OscillationType,
        v_kind: OscillationType,
        u_offset: f32,
        v_offset: f32,
    },
    /// TexMatrix, TexEnvMap, TexCoordSource: coordinate generators that a 2D canvas passes through.
    TexCoordModifier { class: String, material: ObjRef },
    ConstantColor { color: Rgba },
    FadeColor { color1: Rgba, color2: Rgba, period: f32, phase: f32, kind: FadeType },
    MaterialSwitch { material: ObjRef, current: i32, materials: Vec<ObjRef> },
    /// Any other Modifier subclass (e.g. OpacityModifier): follow `material`.
    Modifier { class: String, material: ObjRef },
}

impl Material {
    /// The classes `read_material` understands (exact class names as found in packages).
    pub const CLASSES: &'static [&'static str] = &[
        "Texture",
        "Shader",
        "Combiner",
        "FinalBlend",
        "ColorModifier",
        "TexPanner",
        "TexRotator",
        "TexScaler",
        "TexOscillator",
        "TexMatrix",
        "TexEnvMap",
        "TexCoordSource",
        "ConstantColor",
        "FadeColor",
        "MaterialSwitch",
        "OpacityModifier",
    ];

    /// Every material reference this node holds (inputs first, then FallbackMaterial is not included).
    pub fn inputs(&self) -> Vec<ObjRef> {
        match self {
            Material::Texture(t) => vec![t.anim_next],
            Material::Shader { diffuse, opacity, specular, specularity_mask, self_illumination, self_illumination_mask, detail, .. } => {
                vec![*diffuse, *opacity, *specular, *specularity_mask, *self_illumination, *self_illumination_mask, *detail]
            }
            Material::Combiner { material1, material2, mask, .. } => vec![*material1, *material2, *mask],
            Material::FinalBlend { material, .. }
            | Material::ColorModifier { material, .. }
            | Material::TexPanner { material, .. }
            | Material::TexRotator { material, .. }
            | Material::TexScaler { material, .. }
            | Material::TexOscillator { material, .. }
            | Material::TexCoordModifier { material, .. }
            | Material::Modifier { material, .. } => vec![*material],
            Material::MaterialSwitch { material, materials, .. } => {
                let mut v = vec![*material];
                v.extend(materials.iter().copied());
                v
            }
            Material::ConstantColor { .. } | Material::FadeColor { .. } => vec![],
        }
    }
}

fn color(p: &Properties, name: &str, default: Rgba) -> Rgba {
    match p.get(name) {
        Some(Value::Color(c)) => [c[2], c[1], c[0], c[3]],
        _ => default,
    }
}

fn obj(p: &Properties, name: &str) -> ObjRef {
    p.object(name).unwrap_or(ObjRef::Null)
}

fn object_array(p: &Properties, name: &str) -> Result<Vec<ObjRef>> {
    let Some(Value::Array { raw }) = p.get(name) else { return Ok(vec![]) };
    let mut r = Reader::new(raw);
    r.array(|r| Ok(ObjRef::from_raw(r.compact()?)))
}

/// Reads a material object of one of `Material::CLASSES`.
pub fn read_material(pkg: &Package, export: usize) -> Result<Material> {
    let class = pkg.export_class(export).to_string();
    let (_r, p) = begin_object(pkg, export)?;
    from_properties(&class, &p)
}

/// Builds a material from a class name and its property list (class defaults applied).
pub fn from_properties(class: &str, p: &Properties) -> Result<Material> {
    let f = |n: &str, d: f32| p.float(n).unwrap_or(d);
    let b = |n: &str, d: bool| p.bool(n).unwrap_or(d);
    let by = |n: &str| p.byte(n).unwrap_or(0);
    let rot = |n: &str| p.rotator(n).unwrap_or([0; 3]);
    Ok(match class {
        "Texture" => Material::Texture(TextureInfo {
            usize: p.int("USize").unwrap_or(0),
            vsize: p.int("VSize").unwrap_or(0),
            masked: b("bMasked", false),
            alpha_texture: b("bAlphaTexture", false),
            two_sided: b("bTwoSided", false),
            anim_next: obj(p, "AnimNext"),
            min_frame_rate: f("MinFrameRate", 0.0),
            max_frame_rate: f("MaxFrameRate", 0.0),
            u_clamp_mode: by("UClampMode"),
            v_clamp_mode: by("VClampMode"),
        }),
        "Shader" => Material::Shader {
            diffuse: obj(p, "Diffuse"),
            opacity: obj(p, "Opacity"),
            specular: obj(p, "Specular"),
            specularity_mask: obj(p, "SpecularityMask"),
            self_illumination: obj(p, "SelfIllumination"),
            self_illumination_mask: obj(p, "SelfIlluminationMask"),
            detail: obj(p, "Detail"),
            detail_scale: f("DetailScale", 8.0),
            output_blending: OutputBlending::from_byte(by("OutputBlending")),
            two_sided: b("TwoSided", false),
        },
        "Combiner" => Material::Combiner {
            color_op: match by("CombineOperation") {
                1 => ColorOp::UseMaterial2,
                2 => ColorOp::Multiply,
                3 => ColorOp::Add,
                4 => ColorOp::Subtract,
                5 => ColorOp::AlphaBlendWithMask,
                6 => ColorOp::AddWithMaskModulation,
                7 => ColorOp::UseMask,
                _ => ColorOp::UseMaterial1,
            },
            alpha_op: match by("AlphaOperation") {
                1 => AlphaOp::Multiply,
                2 => AlphaOp::Add,
                3 => AlphaOp::UseMaterial1,
                4 => AlphaOp::UseMaterial2,
                _ => AlphaOp::UseMask,
            },
            material1: obj(p, "Material1"),
            material2: obj(p, "Material2"),
            mask: obj(p, "Mask"),
            invert_mask: b("InvertMask", false),
            modulate2x: b("Modulate2X", false),
            modulate4x: b("Modulate4X", false),
        },
        "FinalBlend" => Material::FinalBlend {
            material: obj(p, "Material"),
            blending: FrameBufferBlending::from_byte(by("FrameBufferBlending")),
            z_write: b("ZWrite", true),
            z_test: b("ZTest", true),
            alpha_test: b("AlphaTest", false),
            two_sided: b("TwoSided", false),
            alpha_ref: by("AlphaRef"),
        },
        "ColorModifier" => Material::ColorModifier {
            material: obj(p, "Material"),
            color: color(p, "Color", [255; 4]),
            two_sided: b("RenderTwoSided", true),
            alpha_blend: b("AlphaBlend", true),
        },
        "TexPanner" => Material::TexPanner { material: obj(p, "Material"), pan_direction: rot("PanDirection"), pan_rate: f("PanRate", 0.1) },
        "TexRotator" => Material::TexRotator {
            material: obj(p, "Material"),
            kind: match by("TexRotationType") {
                1 => RotationType::ConstantlyRotating,
                2 => RotationType::Oscillating,
                // Deprecated flag kept by old packages.
                _ if b("ConstantRotation", false) => RotationType::ConstantlyRotating,
                _ => RotationType::Fixed,
            },
            rotation: rot("Rotation"),
            u_offset: f("UOffset", 0.0),
            v_offset: f("VOffset", 0.0),
            oscillation_rate: rot("OscillationRate"),
            oscillation_amplitude: rot("OscillationAmplitude"),
            oscillation_phase: rot("OscillationPhase"),
        },
        "TexScaler" => Material::TexScaler {
            material: obj(p, "Material"),
            u_scale: f("UScale", 1.0),
            v_scale: f("VScale", 1.0),
            u_offset: f("UOffset", 0.0),
            v_offset: f("VOffset", 0.0),
        },
        "TexOscillator" => {
            let kind = |n: &str| match by(n) {
                1 => OscillationType::Stretch,
                2 => OscillationType::StretchRepeat,
                _ => OscillationType::Pan,
            };
            Material::TexOscillator {
                material: obj(p, "Material"),
                u_rate: f("UOscillationRate", 1.0),
                v_rate: f("VOscillationRate", 1.0),
                u_phase: f("UOscillationPhase", 0.0),
                v_phase: f("VOscillationPhase", 0.0),
                u_amplitude: f("UOscillationAmplitude", 0.1),
                v_amplitude: f("VOscillationAmplitude", 0.1),
                u_kind: kind("UOscillationType"),
                v_kind: kind("VOscillationType"),
                u_offset: f("UOffset", 0.0),
                v_offset: f("VOffset", 0.0),
            }
        }
        "TexMatrix" | "TexEnvMap" | "TexCoordSource" => Material::TexCoordModifier { class: class.to_string(), material: obj(p, "Material") },
        "ConstantColor" => Material::ConstantColor { color: color(p, "Color", [0, 0, 0, 255]) },
        "FadeColor" => Material::FadeColor {
            color1: color(p, "Color1", [0, 0, 0, 255]),
            color2: color(p, "Color2", [0, 0, 0, 255]),
            period: f("FadePeriod", 0.0),
            phase: f("FadePhase", 0.0),
            kind: if by("ColorFadeType") == 1 { FadeType::Sinusoidal } else { FadeType::Linear },
        },
        "MaterialSwitch" => Material::MaterialSwitch { material: obj(p, "Material"), current: p.int("Current").unwrap_or(0), materials: object_array(p, "Materials")? },
        "OpacityModifier" => Material::Modifier { class: class.to_string(), material: obj(p, "Material") },
        other => return Err(Error::Unsupported(format!("material class {other}"))),
    })
}
