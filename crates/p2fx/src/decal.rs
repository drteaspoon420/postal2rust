//! Dynamic projector decals (`Projector.AttachProjector / AbandonProjector / DetachProjector`):
//! blood splats, bullet holes, scorch marks.
//!
//! A projector is treated as an orthographic box along its forward axis (X), `ProjTexture`'s size
//! times `DrawScale` wide and tall. World triangles (BSP, static meshes, terrain from
//! `p2collide`) inside the box that face the projector are clipped to it, given planar UVs and
//! lifted slightly off the surface. Depth is cut just past the first surface hit along the axis so
//! the decal does not bleed through to the back of thin walls.
//!
//! Lifetime follows the engine: an attached decal lives as long as its projector actor; after
//! `AbandonProjector(Lifetime)` it is owned by the level and disappears after `Lifetime` seconds
//! (forever when 0); `DetachProjector` removes it immediately.

use crate::geom::{Batch, to_bevy};
use crate::load::{FxLibrary, TexData};
use bevy::math::{Mat3, Vec3};
use std::collections::HashMap;
use std::sync::Arc;
use upkg::emitter::DrawStyle;
use uscript::vm::{ObjId, Value, Vm};

/// Projector parameters read from the VM actor.
#[derive(Clone)]
pub struct DecalSpec {
    pub location: Vec3,
    pub rotation: [i32; 3],
    pub half_w: f32,
    pub half_h: f32,
    pub depth: f32,
    pub texture: Option<Arc<TexData>>,
    pub style: DrawStyle,
    /// bProjectBSP / bProjectStaticMesh / bProjectTerrain: which surfaces receive the decal.
    pub targets: p2collide::TraceFlags,
}

/// One decal ready to draw (Bevy space).
pub struct DecalDraw {
    pub id: u64,
    pub batch: Batch,
    pub texture: Option<Arc<TexData>>,
    pub style: DrawStyle,
}

struct Live {
    owner: Option<ObjId>,
    /// Seconds left (None: unlimited).
    life: Option<f32>,
    draw: Arc<DecalDraw>,
}

/// All decals of the level.
#[derive(Default)]
pub struct Decals {
    live: Vec<Live>,
    next: u64,
    /// Cap so long fights cannot grow without bound (oldest abandoned decals go first).
    pub max: usize,
}

/// Resolves a material path ("Pkg.Group.Name") to a texture through the library.
fn texture_for(lib: &FxLibrary, path: &str) -> Option<Arc<TexData>> {
    let mut parts = path.split('.');
    let pkg = lib.lib.package(parts.next()?)?;
    let leaf = path.rsplit('.').next()?;
    let e = (0..pkg.exports.len()).find(|&i| pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(leaf) && !pkg.export_class(i).eq_ignore_ascii_case("Class"))?;
    lib.texture(&upkg::library::Obj { pkg, export: e })
}

fn texture_size(lib: &FxLibrary, path: &str) -> Option<(f32, f32)> {
    let t = texture_for(lib, path)?;
    Some((t.width as f32, t.height as f32))
}

impl DecalSpec {
    pub fn from_vm(vm: &Vm, a: ObjId, lib: &FxLibrary) -> Option<DecalSpec> {
        let tex_path = match vm.get_prop(a, "ProjTexture") {
            Some(Value::Resource(p)) => p.clone(),
            _ => return None,
        };
        let texture = texture_for(lib, &tex_path);
        let (w, h) = texture_size(lib, &tex_path).unwrap_or((256.0, 256.0));
        let ds = vm.get_prop(a, "DrawScale").map(|v| v.as_float()).unwrap_or(1.0);
        // The frustum is USize x VSize texels scaled by DrawScale * DrawScale3D.Y (width) and .Z
        // (height); 46 level projectors set DrawScale3D (pig's census).
        let ds3 = match vm.get_prop(a, "DrawScale3D") {
            Some(Value::Vector(v)) => Vec3::from(*v),
            _ => Vec3::ONE,
        };
        let flag = |n: &str, d: bool| vm.get_prop(a, n).map(|v| v.as_bool()).unwrap_or(d);
        let targets = p2collide::TraceFlags { world: flag("bProjectBSP", true), actors: flag("bProjectStaticMesh", true), terrain: flag("bProjectTerrain", true) };
        let depth = vm.get_prop(a, "MaxTraceDistance").map(|v| v.as_int() as f32).filter(|d| *d > 0.0).unwrap_or(1000.0);
        // FrameBufferBlendingOp: 0 none, 1 modulate, 2 alpha blend, 3 add.
        let fb = vm.get_prop(a, "FrameBufferBlendingOp").map(|v| v.as_int()).unwrap_or(2);
        if std::env::var_os("P2FX_DEBUG").is_some() {
            eprintln!("p2fx projector {} tex {tex_path} matop {:?} fbop {fb} alpha {:?}", vm.object_name(a), vm.get_prop(a, "MaterialBlendingOp"), texture.as_ref().map(|t| t.has_alpha));
        }
        // PB_Modulate projects the texture lerped toward neutral by its alpha (transparent texels
        // leave the surface unchanged); with an alpha-masked texture that is drawn as an alpha
        // blend of the texture, which keeps the splat shape and colour.
        let has_alpha = texture.as_ref().is_some_and(|t| t.has_alpha);
        // MaterialBlendingOp PB_Modulate multiplies the surface by the projected texture (2x, 128 =
        // unchanged): an alpha-less texture (MilitaryBase's cell-window grille,
        // zo-base_jailgrill_proj, fbop AlphaBlend) is a modulate decal, not an opaque one (it drew a
        // bright white grid on the cell floor, FIDELITY #52).
        let mat_op = vm.get_prop(a, "MaterialBlendingOp").map(|v| v.as_int()).unwrap_or(0);
        let style = match fb {
            _ if mat_op == 1 && !has_alpha && fb != 3 => DrawStyle::Modulated,
            1 if has_alpha => DrawStyle::AlphaBlend,
            1 => DrawStyle::Modulated,
            3 => DrawStyle::Translucent,
            _ => DrawStyle::AlphaBlend,
        };
        Some(DecalSpec {
            location: Vec3::from(vm.actor_loc(a)),
            rotation: vm.actor_rot(a),
            half_w: w * ds * ds3.y.abs() * 0.5,
            half_h: h * ds * ds3.z.abs() * 0.5,
            depth,
            texture,
            style,
            targets,
        })
    }

    /// Clipped decal geometry against the level (None if nothing was hit).
    pub fn build(&self, world: &p2collide::World) -> Option<Batch> {
        let m = crate::sim::rotator_matrix(self.rotation);
        let (fx, fy, fz) = (m.x_axis, m.y_axis, m.z_axis);
        // Depth: just past the deepest first surface hit over a 5x5 grid of rays spanning the
        // whole box (edges included). The old single centre ray + 12 clipped decals whose
        // projector is not square to the surface: suburbs-1 Projector18 (graffiti) is yawed 6 deg
        // off its wall, so the far edge lies ~27 uu deeper than the centre and was cut off.
        // Cutting just past the surface (instead of the full MaxTraceDistance box) keeps decals
        // off the back faces of thin walls: collision triangles carry no reliable winding.
        let mut depth = self.depth;
        // P2FX_DECAL_LEGACY=1: the old placement (centre ray only, every surface) for A/B probes.
        let legacy = std::env::var_os("P2FX_DECAL_LEGACY").is_some();
        let targets = if legacy { p2collide::TraceFlags::ALL } else { self.targets };
        let grid: &[f32] = if legacy { &[0.0] } else { &[-1.0, -0.5, 0.0, 0.5, 1.0] };
        let mut far: Option<f32> = None;
        let mut missed = false;
        for &a in grid {
            for &b in grid {
                let o = self.location + fy * (a * self.half_w * 0.98) + fz * (b * self.half_h * 0.98);
                if let Some(h) = world.trace(o - fx * 8.0, o + fx * self.depth, &p2collide::Shape::POINT, targets) {
                    let d = (h.location - self.location).dot(fx);
                    far = Some(far.map_or(d, |f: f32| f.max(d)));
                } else {
                    missed = true;
                }
            }
        }
        // A ray that finds nothing means part of the box has no surface within reach of the grid
        // (an opening, or a surface between rays): keep the full depth there, as UE2 does
        // (library1's 1500 uu skylight lost a third of its floor with the cut).
        if let Some(d) = far.filter(|_| !missed || legacy) {
            depth = (d + 12.0).clamp(4.0, self.depth);
        }
        let near = -8.0;
        let to_local = Mat3::from_cols(fx, fy, fz).transpose();
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for x in [near, depth] {
            for y in [-self.half_w, self.half_w] {
                for z in [-self.half_h, self.half_h] {
                    let p = self.location + fx * x + fy * y + fz * z;
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
            }
        }
        let bb = p2collide::geom::Aabb { min: lo, max: hi };
        let mut out = Batch::default();
        world.for_tris_in_flags(&bb, targets, |t| {
            let mut n = (t[1] - t[0]).cross(t[2] - t[0]);
            if n.length_squared() < 1e-8 {
                return;
            }
            n = n.normalize();
            // Collision triangles carry no reliable winding: orient toward the projector.
            if n.dot(fx) > 0.0 {
                n = -n;
            }
            if n.dot(fx) > -0.2 {
                return;
            }
            let local: Vec<Vec3> = t.iter().map(|&q| to_local * (q - self.location)).collect();
            let mut poly = clip(local, 0, near, depth);
            if poly.len() >= 3 {
                poly = clip(poly, 1, -self.half_w, self.half_w);
            }
            if poly.len() >= 3 {
                poly = clip(poly, 2, -self.half_h, self.half_h);
            }
            if poly.len() < 3 {
                return;
            }
            let start = out.positions.len() as u32;
            for q in &poly {
                let w = self.location + fx * q.x + fy * q.y + fz * q.z + n * 0.4;
                out.positions.push(to_bevy(w).to_array());
                out.uv0.push([0.5 + q.y / (2.0 * self.half_w), 0.5 - q.z / (2.0 * self.half_h)]);
                out.uv1.push([0.0, 0.0]);
                out.colors.push([1.0, 1.0, 1.0, 1.0]);
                out.blend.push(0.0);
            }
            for k in 1..poly.len() as u32 - 1 {
                out.indices.extend([start, start + k, start + k + 1]);
            }
        });
        (!out.indices.is_empty()).then_some(out)
    }
}

/// Sutherland-Hodgman clip of a polygon to min <= p[axis] <= max.
fn clip(mut poly: Vec<Vec3>, axis: usize, min: f32, max: f32) -> Vec<Vec3> {
    for (sign, bound) in [(1.0f32, max), (-1.0f32, -min)] {
        let inside = |p: &Vec3| sign * p[axis] <= bound;
        let mut out = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (ia, ib) = (inside(&a), inside(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (bound - sign * a[axis]) / (sign * (b[axis] - a[axis]));
                out.push(a + (b - a) * t);
            }
        }
        poly = out;
        if poly.len() < 3 {
            return vec![];
        }
    }
    poly
}

impl Decals {
    /// `AttachProjector`: (re)builds the decal of projector `a`.
    pub fn attach(&mut self, a: ObjId, spec: DecalSpec, world: &p2collide::World) -> bool {
        self.detach(a);
        let Some(batch) = spec.build(world) else { return false };
        if std::env::var_os("P2FX_DEBUG").is_some() {
            eprintln!("p2fx decal at {:?} rot {:?} half {}x{} depth {} -> {} tris, tex {:?}", spec.location, spec.rotation, spec.half_w, spec.half_h, spec.depth, batch.indices.len() / 3, spec.texture.as_ref().map(|t| (&t.name, t.width, t.height)));
        }
        self.next += 1;
        let draw = Arc::new(DecalDraw { id: self.next, batch, texture: spec.texture, style: spec.style });
        self.live.push(Live { owner: Some(a), life: None, draw });
        let max = if self.max == 0 { 512 } else { self.max };
        while self.live.len() > max {
            match self.live.iter().position(|l| l.owner.is_none()) {
                Some(i) => {
                    self.live.remove(i);
                }
                None => {
                    self.live.remove(0);
                }
            }
        }
        true
    }

    /// `AbandonProjector(Lifetime)`: the decal outlives the actor, for `life` seconds if given.
    pub fn abandon(&mut self, a: ObjId, life: Option<f32>) {
        for l in self.live.iter_mut().filter(|l| l.owner == Some(a)) {
            l.owner = None;
            l.life = life;
        }
    }

    /// `DetachProjector`: removes the actor's attached decal.
    pub fn detach(&mut self, a: ObjId) {
        self.live.retain(|l| l.owner != Some(a));
    }

    /// The projector actor was destroyed: attached (not abandoned) decals go with it.
    pub fn owner_destroyed(&mut self, a: ObjId) {
        self.detach(a);
    }

    pub fn tick(&mut self, dt: f32) {
        for l in &mut self.live {
            if let Some(t) = &mut l.life {
                *t -= dt;
            }
        }
        self.live.retain(|l| l.life.is_none_or(|t| t > 0.0));
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub fn draws(&self) -> Vec<Arc<DecalDraw>> {
        self.live.iter().map(|l| l.draw.clone()).collect()
    }
}

/// Map from decal id to anything (render side bookkeeping).
pub type DecalMap<T> = HashMap<u64, T>;
