//! Material graph evaluation for the 2D canvas.
//!
//! A material is a small graph (modifiers wrapping textures, combiners, shaders...). Each frame
//! it is flattened into an [`Evaluated`] description: up to three sampled slots (texture or
//! constant colour, each with its own UV transform), how to combine them, a tint, and the
//! framebuffer blend. The GPU side (`render.rs`) draws exactly this description.
//!
//! UV transform conventions (all in normalised texture space, applied to the incoming UV):
//! - TexPanner: `uv + frac(dir * PanRate * t)` where `dir` is the PanDirection rotator's unit vector
//!   (`cos(pitch)cos(yaw)`, `cos(pitch)sin(yaw)`).
//! - TexRotator: rotation by the Yaw angle about the pivot `(UOffset/USize, VOffset/VSize)`;
//!   `TR_ConstantlyRotating` uses `Rotation * t`, `TR_OscillatingRotation` adds
//!   `Amplitude * sin(2pi * (frac(Rate * t / 65536) + Phase / 65536))` per component.
//! - TexScaler: `uv / Scale + Offset / Size`.
//! - TexOscillator, per axis: `s = sin((frac(Rate * t) + Phase * 2pi) * k * pi)` with `k = 2`
//!   (0.5 for OT_StretchRepeat); OT_Pan translates by `s * Amplitude`, the stretch modes scale by
//!   `1 + s * Amplitude` about the pivot `Offset / Size`.
//! - Nested modifiers compose from the outside in: the outermost transforms the canvas UV first.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use upkg::library::{Library, Obj};
use upkg::material::{AlphaOp, ColorOp, FadeType, FrameBufferBlending, Material, OscillationType, OutputBlending, RotationType, read_material};

/// 2D affine transform on UVs: `p' = m * p + t` with `m` = [[a, b], [c, d]] (row-major).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvXform {
    pub m: [f32; 4],
    pub t: [f32; 2],
}

impl Default for UvXform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl UvXform {
    pub const IDENTITY: Self = Self { m: [1.0, 0.0, 0.0, 1.0], t: [0.0, 0.0] };

    pub fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        [self.m[0] * p[0] + self.m[1] * p[1] + self.t[0], self.m[2] * p[0] + self.m[3] * p[1] + self.t[1]]
    }

    /// `self` applied after `first`.
    pub fn after(&self, first: &UvXform) -> UvXform {
        let a = &self.m;
        let b = &first.m;
        UvXform {
            m: [a[0] * b[0] + a[1] * b[2], a[0] * b[1] + a[1] * b[3], a[2] * b[0] + a[3] * b[2], a[2] * b[1] + a[3] * b[3]],
            t: self.apply(first.t),
        }
    }

    fn translate(x: f32, y: f32) -> Self {
        Self { m: [1.0, 0.0, 0.0, 1.0], t: [x, y] }
    }

    fn scale_about(sx: f32, sy: f32, px: f32, py: f32) -> Self {
        Self { m: [sx, 0.0, 0.0, sy], t: [px - sx * px, py - sy * py] }
    }

    fn rotate_about(angle: f32, px: f32, py: f32) -> Self {
        let (s, c) = angle.sin_cos();
        let m = [c, -s, s, c];
        let t = [px - (m[0] * px + m[1] * py), py - (m[2] * px + m[3] * py)];
        Self { m, t }
    }
}

#[derive(Clone)]
pub enum SlotSource {
    Empty,
    Texture(Obj),
    Color([f32; 4]),
}

/// Same object (same package instance and export)?
pub fn same_obj(a: &Obj, b: &Obj) -> bool {
    Arc::ptr_eq(&a.pkg, &b.pkg) && a.export == b.export
}

impl std::fmt::Debug for SlotSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlotSource::Empty => write!(f, "Empty"),
            SlotSource::Texture(o) => write!(f, "Texture({}.{})", o.pkg.name, o.name()),
            SlotSource::Color(c) => write!(f, "Color({c:?})"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Slot {
    pub source: SlotSource,
    pub xform: UvXform,
    /// Multiplies the sampled value (ColorModifier inside a branch).
    pub color: [f32; 4],
}

impl Default for Slot {
    fn default() -> Self {
        Self { source: SlotSource::Empty, xform: UvXform::IDENTITY, color: [1.0; 4] }
    }
}

/// Framebuffer blending, as the Canvas styles and FinalBlend/Shader settings resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Blend {
    #[default]
    Opaque,
    Alpha,
    /// src + dst * (1 - src)
    Translucent,
    /// 2x modulation: src * dst * 2
    Modulate,
    /// src + dst * (1 - srcAlpha) (premultiplied)
    AlphaModulate,
    Darken,
    Brighten,
    Invisible,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Combine {
    pub color_op: ColorOp,
    pub alpha_op: AlphaOp,
    pub invert_mask: bool,
    pub modulate: f32,
}

#[derive(Debug, Clone)]
pub struct Evaluated {
    /// Slot 0 is the main colour; slot 1 is Combiner.Material2; slot 2 is a mask / opacity source.
    pub slots: [Slot; 3],
    pub combine: Option<Combine>,
    /// Shader.Opacity: take alpha from slot 2.
    pub opacity_from_mask: bool,
    pub tint: [f32; 4],
    pub blend: Blend,
    /// Alpha-test threshold (0..1).
    pub alpha_test: Option<f32>,
    /// True when the description depends on time (re-evaluate every frame).
    pub animated: bool,
}

/// Canvas `Style` values (`Actor.ERenderStyle`).
const STY_MASKED: u8 = 2;
const STY_TRANSLUCENT: u8 = 3;
const STY_MODULATED: u8 = 4;
const STY_ALPHA: u8 = 5;

/// Parses and caches material objects and texture sizes.
pub struct MaterialDb {
    pub lib: Arc<Library>,
    cache: Mutex<HashMap<(String, usize), Option<Arc<Material>>>>,
    sizes: Mutex<HashMap<(String, usize), (i32, i32)>>,
}

impl MaterialDb {
    pub fn new(lib: Arc<Library>) -> Self {
        Self { lib, cache: Mutex::new(HashMap::new()), sizes: Mutex::new(HashMap::new()) }
    }

    /// Finds an object by dotted name: "Package.Name" or "Package.Group.Name" (class optional).
    /// When the exact group chain does not match, any export of that name in the package is
    /// accepted (script often names objects without their group, e.g. `P2Fonts.Plain24`).
    pub fn find(&self, full: &str, class: Option<&str>) -> Option<Obj> {
        self.find_exact(full, class).or_else(|| {
            let (pkg, rest) = full.split_once('.')?;
            let leaf = rest.rsplit('.').next()?;
            let p = self.lib.package(pkg)?;
            let export = (0..p.exports.len())
                .find(|&i| p.name(p.exports[i].name).eq_ignore_ascii_case(leaf) && class.is_none_or(|c| p.export_class(i).eq_ignore_ascii_case(c)) && !p.export_class(i).eq_ignore_ascii_case("Package"))?;
            Some(Obj { pkg: p, export })
        })
    }

    fn find_exact(&self, full: &str, class: Option<&str>) -> Option<Obj> {
        let mut parts: Vec<&str> = full.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        let pkg = self.lib.package(parts.remove(0))?;
        let leaf = parts.pop()?;
        let groups: Vec<&str> = parts.into_iter().rev().collect();
        let export = (0..pkg.exports.len()).find(|&i| {
            if !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(leaf) || class.is_some_and(|c| !pkg.export_class(i).eq_ignore_ascii_case(c)) {
                return false;
            }
            let mut outer = pkg.exports[i].outer;
            for g in &groups {
                match outer {
                    upkg::ObjRef::Export(o) if pkg.name(pkg.exports[o].name).eq_ignore_ascii_case(g) => outer = pkg.exports[o].outer,
                    _ => return false,
                }
            }
            true
        })?;
        Some(Obj { pkg, export })
    }

    pub fn material(&self, obj: &Obj) -> Option<Arc<Material>> {
        let key = obj.key();
        if let Some(m) = self.cache.lock().unwrap().get(&key) {
            return m.clone();
        }
        let m = read_material(&obj.pkg, obj.export).ok().map(Arc::new);
        self.cache.lock().unwrap().insert(key, m.clone());
        m
    }

    fn resolve(&self, from: &Obj, r: upkg::ObjRef) -> Option<Obj> {
        self.lib.resolve(&from.pkg, r)
    }

    fn fallback(&self, obj: &Obj) -> Option<Obj> {
        let (_, p) = upkg::begin_object(&obj.pkg, obj.export).ok()?;
        self.resolve(obj, p.object("FallbackMaterial")?)
    }

    /// `MaterialUSize/VSize`: modifiers report their input's size, Combiner its Material1, Shader its Diffuse.
    pub fn size(&self, obj: &Obj) -> (i32, i32) {
        let key = obj.key();
        if let Some(s) = self.sizes.lock().unwrap().get(&key) {
            return *s;
        }
        let s = self.size_uncached(obj);
        self.sizes.lock().unwrap().insert(key, s);
        s
    }

    fn size_uncached(&self, obj: &Obj) -> (i32, i32) {
        let mut cur = obj.clone();
        for _ in 0..16 {
            let Some(m) = self.material(&cur) else { return (0, 0) };
            let next = match &*m {
                Material::Texture(t) => return (t.usize, t.vsize),
                Material::ConstantColor { .. } | Material::FadeColor { .. } => return (0, 0),
                Material::Shader { diffuse, self_illumination, .. } => {
                    if *diffuse != upkg::ObjRef::Null {
                        *diffuse
                    } else {
                        *self_illumination
                    }
                }
                Material::Combiner { material1, .. } => *material1,
                Material::MaterialSwitch { material, materials, current } => switch_pick(*material, materials, *current),
                Material::FinalBlend { material, .. }
                | Material::ColorModifier { material, .. }
                | Material::TexPanner { material, .. }
                | Material::TexRotator { material, .. }
                | Material::TexScaler { material, .. }
                | Material::TexOscillator { material, .. }
                | Material::TexCoordModifier { material, .. }
                | Material::Modifier { material, .. } => *material,
            };
            let Some(n) = self.resolve(&cur, next) else { return (0, 0) };
            cur = n;
        }
        (0, 0)
    }

    /// Flattens `obj` for drawing with canvas `style` at time `t` (seconds).
    pub fn evaluate(&self, obj: &Obj, style: u8, t: f32) -> Option<Evaluated> {
        let mut ev = Evaluated {
            slots: Default::default(),
            combine: None,
            opacity_from_mask: false,
            tint: [1.0; 4],
            blend: Blend::Opaque,
            alpha_test: None,
            animated: false,
        };
        let mut explicit_blend: Option<Blend> = None;
        let mut xform = UvXform::IDENTITY;
        let mut cur = obj.clone();
        let mut done = false;
        for _ in 0..16 {
            let m = match self.material(&cur) {
                Some(m) => m,
                None => {
                    // Unsupported class (e.g. Cubemap): try its fallback.
                    cur = self.fallback(&cur)?;
                    continue;
                }
            };
            match &*m {
                Material::FinalBlend { material, blending, alpha_test, alpha_ref, .. } => {
                    explicit_blend.get_or_insert(fb_blend(*blending));
                    if *alpha_test {
                        ev.alpha_test.get_or_insert(*alpha_ref as f32 / 255.0);
                    }
                    cur = self.resolve(&cur, *material)?;
                }
                Material::ColorModifier { material, color, alpha_blend, .. } => {
                    let c = color.map(|v| v as f32 / 255.0);
                    for k in 0..3 {
                        ev.tint[k] *= c[k];
                    }
                    if *alpha_blend {
                        ev.tint[3] *= c[3];
                        explicit_blend.get_or_insert(Blend::Alpha);
                    }
                    cur = self.resolve(&cur, *material)?;
                }
                Material::Combiner { color_op, alpha_op, material1, material2, mask, invert_mask, modulate2x, modulate4x } => {
                    ev.slots[0] = self.leaf(&cur, *material1, xform, t, &mut ev.animated);
                    ev.slots[1] = self.leaf(&cur, *material2, xform, t, &mut ev.animated);
                    ev.slots[2] = self.leaf(&cur, *mask, xform, t, &mut ev.animated);
                    ev.combine = Some(Combine {
                        color_op: *color_op,
                        alpha_op: *alpha_op,
                        invert_mask: *invert_mask,
                        modulate: if *modulate4x {
                            4.0
                        } else if *modulate2x {
                            2.0
                        } else {
                            1.0
                        },
                    });
                    if explicit_blend.is_none() && !matches!(ev.slots[2].source, SlotSource::Empty) {
                        // A masked combiner produces meaningful alpha.
                        explicit_blend = Some(Blend::Alpha);
                    }
                    done = true;
                }
                Material::Shader { diffuse, opacity, self_illumination, output_blending, .. } => {
                    let main = if *diffuse != upkg::ObjRef::Null { *diffuse } else { *self_illumination };
                    ev.slots[0] = self.leaf(&cur, main, xform, t, &mut ev.animated);
                    if *opacity != upkg::ObjRef::Null {
                        ev.slots[2] = self.leaf(&cur, *opacity, xform, t, &mut ev.animated);
                        ev.opacity_from_mask = true;
                    }
                    match output_blending {
                        OutputBlending::Normal => {
                            if ev.opacity_from_mask {
                                explicit_blend.get_or_insert(Blend::Alpha);
                            }
                        }
                        OutputBlending::Masked => {
                            ev.alpha_test.get_or_insert(0.5);
                        }
                        OutputBlending::Modulate => {
                            explicit_blend.get_or_insert(Blend::Modulate);
                        }
                        OutputBlending::Translucent => {
                            explicit_blend.get_or_insert(Blend::Translucent);
                        }
                        OutputBlending::Invisible => {
                            explicit_blend.get_or_insert(Blend::Invisible);
                        }
                        OutputBlending::Brighten => {
                            explicit_blend.get_or_insert(Blend::Brighten);
                        }
                        OutputBlending::Darken => {
                            explicit_blend.get_or_insert(Blend::Darken);
                        }
                    }
                    done = true;
                }
                Material::Texture(info) => {
                    ev.slots[0] = self.leaf_obj(&cur, xform, t, &mut ev.animated);
                    if explicit_blend.is_none() {
                        if info.alpha_texture {
                            explicit_blend = Some(Blend::Alpha);
                        } else if info.masked {
                            ev.alpha_test.get_or_insert(0.5);
                        }
                    }
                    done = true;
                }
                Material::ConstantColor { .. } | Material::FadeColor { .. } => {
                    ev.slots[0] = self.leaf_obj(&cur, xform, t, &mut ev.animated);
                    done = true;
                }
                Material::MaterialSwitch { material, materials, current } => {
                    cur = self.resolve(&cur, switch_pick(*material, materials, *current))?;
                }
                _ => {
                    // UV modifiers.
                    let (next, x, anim) = self.modifier(&cur, &m, t)?;
                    xform = x.after(&xform);
                    ev.animated |= anim;
                    cur = self.resolve(&cur, next)?;
                }
            }
            if done {
                break;
            }
        }
        if !done {
            return None;
        }
        // Canvas style overrides (UCanvas::DrawTile wraps the material in a FinalBlend for these).
        ev.blend = match style {
            STY_MASKED => {
                ev.alpha_test = Some(127.0 / 255.0);
                Blend::Opaque
            }
            STY_TRANSLUCENT => Blend::Translucent,
            STY_MODULATED => Blend::Modulate,
            STY_ALPHA => Blend::Alpha,
            _ => explicit_blend.unwrap_or(Blend::Opaque),
        };
        Some(ev)
    }

    /// The UV transform of a single modifier node, plus its input and whether it animates.
    fn modifier(&self, obj: &Obj, m: &Material, t: f32) -> Option<(upkg::ObjRef, UvXform, bool)> {
        let size = || {
            let (w, h) = self.size(obj);
            (w.max(1) as f32, h.max(1) as f32)
        };
        Some(match m {
            Material::TexPanner { material, pan_direction, pan_rate } => {
                let (pitch, yaw) = (urot(pan_direction[0]), urot(pan_direction[1]));
                let (dx, dy) = (pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin());
                let (x, y) = (dx * pan_rate * t, dy * pan_rate * t);
                (*material, UvXform::translate(x - x.trunc(), y - y.trunc()), *pan_rate != 0.0)
            }
            Material::TexRotator { material, kind, rotation, u_offset, v_offset, oscillation_rate, oscillation_amplitude, oscillation_phase } => {
                let (w, h) = size();
                let yaw = match kind {
                    RotationType::Fixed => rotation[1] as f32,
                    RotationType::ConstantlyRotating => ((rotation[1] as f32 * t) as i64 & 0xFFFF) as f32,
                    RotationType::Oscillating => {
                        let f = oscillation_rate[1] as f32 * t / 65536.0;
                        let s = (std::f32::consts::TAU * ((f - f.trunc()) + oscillation_phase[1] as f32 / 65536.0)).sin();
                        rotation[1] as f32 + oscillation_amplitude[1] as f32 * s
                    }
                };
                let angle = yaw / 65536.0 * std::f32::consts::TAU;
                (*material, UvXform::rotate_about(angle, u_offset / w, v_offset / h), *kind != RotationType::Fixed)
            }
            Material::TexScaler { material, u_scale, v_scale, u_offset, v_offset } => {
                let (w, h) = size();
                let sx = if *u_scale != 0.0 { 1.0 / u_scale } else { 1.0 };
                let sy = if *v_scale != 0.0 { 1.0 / v_scale } else { 1.0 };
                (*material, UvXform { m: [sx, 0.0, 0.0, sy], t: [u_offset / w, v_offset / h] }, false)
            }
            Material::TexOscillator { material, u_rate, v_rate, u_phase, v_phase, u_amplitude, v_amplitude, u_kind, v_kind, u_offset, v_offset } => {
                let (w, h) = size();
                let wave = |rate: f32, phase: f32, kind: OscillationType| {
                    let k = if kind == OscillationType::StretchRepeat { 0.5 } else { 2.0 };
                    let f = rate * t;
                    (((f - f.trunc()) + phase * std::f32::consts::TAU) * k * std::f32::consts::PI).sin()
                };
                let su = wave(*u_rate, *u_phase, *u_kind) * u_amplitude;
                let sv = wave(*v_rate, *v_phase, *v_kind) * v_amplitude;
                let (mut sx, mut sy, mut tx, mut ty) = (1.0, 1.0, 0.0, 0.0);
                match u_kind {
                    OscillationType::Pan => tx = su,
                    _ => sx = 1.0 + su,
                }
                match v_kind {
                    OscillationType::Pan => ty = sv,
                    _ => sy = 1.0 + sv,
                }
                let x = UvXform::translate(tx, ty).after(&UvXform::scale_about(sx, sy, u_offset / w, v_offset / h));
                (*material, x, true)
            }
            Material::TexCoordModifier { material, .. } | Material::Modifier { material, .. } => (*material, UvXform::IDENTITY, false),
            _ => return None,
        })
    }

    /// Follows a referenced input of a Combiner/Shader down to one sampled slot.
    fn leaf(&self, from: &Obj, r: upkg::ObjRef, xform: UvXform, t: f32, animated: &mut bool) -> Slot {
        match self.resolve(from, r) {
            Some(o) => self.leaf_obj(&o, xform, t, animated),
            None => Slot::default(),
        }
    }

    fn leaf_obj(&self, obj: &Obj, mut xform: UvXform, t: f32, animated: &mut bool) -> Slot {
        let mut color = [1.0f32; 4];
        let mut cur = obj.clone();
        for _ in 0..16 {
            let Some(m) = self.material(&cur) else {
                match self.fallback(&cur) {
                    Some(f) => {
                        cur = f;
                        continue;
                    }
                    None => return Slot::default(),
                }
            };
            let next = match &*m {
                Material::Texture(info) => {
                    let tex = self.anim_frame(&cur, info, t, animated);
                    return Slot { source: SlotSource::Texture(tex), xform, color };
                }
                Material::ConstantColor { color: c } => {
                    let c = c.map(|v| v as f32 / 255.0);
                    return Slot { source: SlotSource::Color(mul4(color, c)), xform, color: [1.0; 4] };
                }
                Material::FadeColor { color1, color2, period, phase, kind } => {
                    *animated = true;
                    let f = if *period > 0.0 { ((t + phase) / period).rem_euclid(1.0) } else { 0.0 };
                    let w = match kind {
                        FadeType::Linear => 1.0 - (2.0 * f - 1.0).abs(),
                        FadeType::Sinusoidal => 0.5 - 0.5 * (f * std::f32::consts::TAU).cos(),
                    };
                    let mut c = [0.0; 4];
                    for k in 0..4 {
                        c[k] = (color1[k] as f32 * (1.0 - w) + color2[k] as f32 * w) / 255.0;
                    }
                    return Slot { source: SlotSource::Color(mul4(color, c)), xform, color: [1.0; 4] };
                }
                Material::Shader { diffuse, self_illumination, .. } => {
                    if *diffuse != upkg::ObjRef::Null {
                        *diffuse
                    } else {
                        *self_illumination
                    }
                }
                Material::Combiner { material1, .. } => *material1,
                Material::MaterialSwitch { material, materials, current } => switch_pick(*material, materials, *current),
                Material::FinalBlend { material, .. } => *material,
                Material::ColorModifier { material, color: c, .. } => {
                    color = mul4(color, c.map(|v| v as f32 / 255.0));
                    *material
                }
                other => {
                    let Some((next, x, anim)) = self.modifier(&cur, other, t) else { return Slot::default() };
                    xform = x.after(&xform);
                    *animated |= anim;
                    next
                }
            };
            match self.resolve(&cur, next) {
                Some(n) => cur = n,
                None => return Slot::default(),
            }
        }
        Slot::default()
    }

    /// Animated textures chain frames through `AnimNext`; pick the frame for time `t`.
    fn anim_frame(&self, tex: &Obj, info: &upkg::material::TextureInfo, t: f32, animated: &mut bool) -> Obj {
        if info.anim_next == upkg::ObjRef::Null {
            return tex.clone();
        }
        let mut frames = vec![tex.clone()];
        let mut cur = tex.clone();
        while frames.len() < 64 {
            let Some(Material::Texture(i)) = self.material(&cur).as_deref().cloned() else { break };
            let Some(n) = self.resolve(&cur, i.anim_next) else { break };
            if same_obj(&n, tex) {
                break;
            }
            frames.push(n.clone());
            cur = n;
        }
        *animated = true;
        let rate = if info.max_frame_rate > 0.0 { info.max_frame_rate } else if info.min_frame_rate > 0.0 { info.min_frame_rate } else { 10.0 };
        frames[((t * rate) as usize) % frames.len()].clone()
    }
}

fn mul4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]
}

fn urot(u: i32) -> f32 {
    u as f32 / 65536.0 * std::f32::consts::TAU
}

fn switch_pick(material: upkg::ObjRef, materials: &[upkg::ObjRef], current: i32) -> upkg::ObjRef {
    if material != upkg::ObjRef::Null {
        return material;
    }
    materials.get(current.max(0) as usize).copied().unwrap_or(upkg::ObjRef::Null)
}

fn fb_blend(b: FrameBufferBlending) -> Blend {
    match b {
        FrameBufferBlending::Overwrite => Blend::Opaque,
        FrameBufferBlending::Modulate => Blend::Modulate,
        FrameBufferBlending::AlphaBlend => Blend::Alpha,
        FrameBufferBlending::AlphaModulate => Blend::AlphaModulate,
        FrameBufferBlending::Translucent => Blend::Translucent,
        FrameBufferBlending::Darken => Blend::Darken,
        FrameBufferBlending::Brighten => Blend::Brighten,
        FrameBufferBlending::Invisible => Blend::Invisible,
    }
}

impl crate::canvas::MaterialSizer for MaterialDb {
    fn material_size(&self, mat: &Obj) -> (i32, i32) {
        self.size(mat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xform_compose_order() {
        let pan = UvXform::translate(0.25, 0.0);
        let scale = UvXform { m: [2.0, 0.0, 0.0, 2.0], t: [0.0, 0.0] };
        // Pan first, then scale.
        let c = scale.after(&pan);
        assert_eq!(c.apply([0.5, 0.5]), [1.5, 1.0]);
    }

    #[test]
    fn rotate_about_pivot_keeps_pivot() {
        let r = UvXform::rotate_about(1.0, 0.5, 0.5);
        let p = r.apply([0.5, 0.5]);
        assert!((p[0] - 0.5).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6);
        let q = UvXform::rotate_about(std::f32::consts::FRAC_PI_2, 0.5, 0.5).apply([1.0, 0.5]);
        assert!((q[0] - 0.5).abs() < 1e-6 && (q[1] - 1.0).abs() < 1e-6);
    }
}
