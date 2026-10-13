//! Builds a [`PhysWorld`] from a map package: the static level (BSP, level static meshes,
//! terrain) without movers, then movers (static mesh colliders driven by PHYS_MovingBrush),
//! volumes (brush colliders: PhysicsVolume, WaterVolume, PainCausing, LadderVolume,
//! BlockingVolume and other Volume subclasses) and the level's DefaultPhysicsVolume.
//!
//! Property values come from the map first, then the class defaults read with the script VM
//! (`p2collide::defaults::ScriptDefaults`), then the engine defaults documented below.
//!
//! Brush placement, measured on the shipped maps (`examples/brush_probe.rs`): a brush point `v`
//! lies at `Location + R(Rotation) * (MainScale * PostScale) * (v - PrePivot)`.

use crate::actor::{Actor, Collider, EncroachType, LadderInfo, MoverInfo, VolumeInfo};
use crate::shape::BrushModel;
use crate::types::{ActorId, Physics, rot};
use crate::world::PhysWorld;
use glam::Vec3;
use p2collide::defaults::{DefaultValue, ScriptDefaults};
use p2collide::world::mesh_collision_tris;
use p2collide::{BuildOptions, World};
use std::collections::HashMap;
use std::sync::Arc;
use upkg::library::Library;
use upkg::{ObjRef, Package, Properties, Value, begin_object};

#[derive(Clone, Debug, Default)]
pub struct LoadStats {
    pub movers: usize,
    pub volumes: usize,
    pub physics_volumes: usize,
    pub water_volumes: usize,
    pub ladder_volumes: usize,
    pub blocking_volumes: usize,
    pub brush_errors: usize,
    pub load_ms: u128,
}

/// A loaded map: the physics world and the actor created for each map export.
pub struct LoadedMap {
    pub world: PhysWorld,
    /// Map export index to physics actor.
    pub by_export: HashMap<u32, ActorId>,
    pub stats: LoadStats,
}

/// Map properties with a class-default fallback.
struct Props<'a> {
    p: &'a Properties,
    class: &'a str,
    defs: &'a mut ScriptDefaults,
}

impl Props<'_> {
    fn def(&mut self, n: &str) -> Option<DefaultValue> {
        self.defs.value(self.class, n)
    }
    fn f(&mut self, n: &str, d: f32) -> f32 {
        self.p.float(n).or_else(|| match self.def(n) {
            Some(DefaultValue::Float(f)) => Some(f),
            _ => None,
        })
        .unwrap_or(d)
    }
    fn b(&mut self, n: &str, d: bool) -> bool {
        self.p.bool(n).or_else(|| match self.def(n) {
            Some(DefaultValue::Bool(b)) => Some(b),
            _ => None,
        })
        .unwrap_or(d)
    }
    fn byte(&mut self, n: &str, d: u8) -> u8 {
        self.p.byte(n).or_else(|| match self.def(n) {
            Some(DefaultValue::Byte(b)) => Some(b),
            _ => None,
        })
        .unwrap_or(d)
    }
    fn v(&mut self, n: &str, d: Vec3) -> Vec3 {
        self.p.vector(n).map(Vec3::from).or_else(|| match self.def(n) {
            Some(DefaultValue::Vector(v)) => Some(Vec3::from(v)),
            _ => None,
        })
        .unwrap_or(d)
    }
    fn r(&mut self, n: &str) -> [i32; 3] {
        self.p.rotator(n).or_else(|| match self.def(n) {
            Some(DefaultValue::Rotator(r)) => Some(r),
            _ => None,
        })
        .unwrap_or([0; 3])
    }
}

/// Scale of a `Scale` struct property (`MainScale` / `PostScale`): the tagged `Scale` vector.
fn scale_prop(p: &Properties, name: &str) -> Vec3 {
    match p.get(name) {
        // Tagged struct body: name "Scale", info 0x3a (struct, 12 bytes), struct name, vector.
        Some(Value::Struct { raw, .. }) if raw.len() >= 15 && raw[1] == 0x3a => {
            let f = |o: usize| f32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
            let v = Vec3::new(f(3), f(7), f(11));
            if v.is_finite() && v.cmpne(Vec3::ZERO).all() { v } else { Vec3::ONE }
        }
        _ => Vec3::ONE,
    }
}

impl LoadedMap {
    /// Loads `map` (registering it with `lib`).
    pub fn load(lib: &Library, map: &Arc<Package>) -> upkg::Result<LoadedMap> {
        let t0 = std::time::Instant::now();
        let level = World::build_with(lib, map, BuildOptions { skip_movers: true, skip_dynamic: true })?;
        let mut world = PhysWorld::new(Arc::new(level));
        let mut defs = ScriptDefaults::new(&lib.root);
        let mut stats = LoadStats::default();
        let mut by_export = HashMap::new();
        let mut brushes: HashMap<usize, Arc<BrushModel>> = HashMap::new();
        let mut meshes: HashMap<(String, usize), Arc<Vec<[Vec3; 3]>>> = HashMap::new();

        for i in 0..map.exports.len() {
            let class = map.export_class(i).to_string();
            if class == "Class" || class.ends_with("Property") || matches!(class.as_str(), "Function" | "Polys" | "Model" | "Texture" | "State" | "TextBuffer") {
                continue;
            }
            let is_mover = defs.is_a(&class, "Mover");
            let is_volume = defs.is_a(&class, "Volume");
            let is_default_pv = defs.is_a(&class, "DefaultPhysicsVolume");
            if !is_mover && !is_volume && !is_default_pv {
                continue;
            }
            let Ok((_, p)) = begin_object(map, i) else { continue };
            let cf = defs.class_flags(&class);
            let mut pr = Props { p: &p, class: &class, defs: &mut defs };
            let loc = pr.v("Location", Vec3::ZERO);
            let rotation = pr.r("Rotation");
            let pivot = pr.v("PrePivot", Vec3::ZERO);
            let mut a = Actor::new(class.clone(), loc);
            a.user = i as u64;
            a.rotation = rotation;
            a.collide_actors = p.bool("bCollideActors").unwrap_or(cf.collide_actors);
            a.block_actors = p.bool("bBlockActors").unwrap_or(cf.block_actors);
            a.block_players = p.bool("bBlockPlayers").unwrap_or(cf.block_players);
            a.block_zero_extent = p.bool("bBlockZeroExtentTraces").unwrap_or(cf.block_zero_extent);
            a.block_nonzero_extent = p.bool("bBlockNonZeroExtentTraces").unwrap_or(cf.block_nonzero_extent);
            a.collision_radius = p.float("CollisionRadius").unwrap_or(cf.collision_radius);
            a.collision_height = p.float("CollisionHeight").unwrap_or(cf.collision_height);
            a.world_geometry = pr.b("bWorldGeometry", false);
            a.is_static = pr.b("bStatic", false);
            a.hidden = pr.b("bHidden", false);
            a.tag_physics(pr.byte("Physics", 0));

            if is_mover {
                let Some(sm) = p.object("StaticMesh").filter(|r| *r != ObjRef::Null).and_then(|r| lib.resolve(map, r)) else { continue };
                let local = meshes.entry(sm.key()).or_insert_with(|| Arc::new(mesh_collision_tris(&sm).0)).clone();
                let ds = pr.f("DrawScale", 1.0);
                let ds3 = pr.v("DrawScale3D", Vec3::ONE);
                a.collider = Collider::mesh(local, ds3 * ds, pivot);
                let mut m = MoverInfo { base_pos: pr.v("BasePos", loc), base_rot: pr.r("BaseRot"), ..Default::default() };
                for k in 0..8u32 {
                    if let Some(Value::Vector(v)) = p.get_at("KeyPos", k) {
                        m.key_pos[k as usize] = Vec3::from(*v);
                    }
                    if let Some(Value::Rotator(r)) = p.get_at("KeyRot", k) {
                        m.key_rot[k as usize] = *r;
                    }
                }
                m.key_num = pr.byte("KeyNum", 0).min(7);
                m.prev_key_num = m.key_num;
                m.num_keys = pr.byte("NumKeys", 2);
                m.move_time = pr.f("MoveTime", 1.0);
                m.glide = pr.byte("MoverGlideType", 1) == 1;
                m.encroach = EncroachType::from_u8(pr.byte("MoverEncroachType", 1));
                // Mover.BeginPlay: Move to BasePos + KeyPos[KeyNum], rotate to BaseRot + KeyRot[KeyNum].
                a.location = m.base_pos + m.key_pos[m.key_num as usize];
                a.rotation = crate::modes::add_rot(m.base_rot, m.key_rot[m.key_num as usize]);
                m.old_pos = a.location;
                m.old_rot = a.rotation;
                a.mover = Some(m);
                a.can_be_base = true;
                a.physics = Physics::None;
                stats.movers += 1;
            } else {
                let mut v = VolumeInfo { physics_volume: defs.is_a(&class, "PhysicsVolume"), ..Default::default() };
                let mut pr = Props { p: &p, class: &class, defs: &mut defs };
                if v.physics_volume {
                    v.priority = p.int("Priority").unwrap_or_else(|| match pr.def("Priority") {
                        Some(DefaultValue::Float(f)) => f as i32,
                        _ => 0,
                    });
                    v.gravity = pr.v("Gravity", v.gravity);
                    v.ground_friction = pr.f("GroundFriction", v.ground_friction);
                    v.fluid_friction = pr.f("FluidFriction", v.fluid_friction);
                    v.terminal_velocity = pr.f("TerminalVelocity", v.terminal_velocity);
                    v.zone_velocity = pr.v("ZoneVelocity", Vec3::ZERO);
                    v.water = pr.b("bWaterVolume", false);
                    v.pain_causing = pr.b("bPainCausing", false);
                    v.damage_per_sec = pr.f("DamagePerSec", 0.0);
                }
                let is_ladder = defs.is_a(&class, "LadderVolume");
                if is_ladder {
                    let mut pr = Props { p: &p, class: &class, defs: &mut defs };
                    let wall = pr.r("WallDir");
                    let look = rot::dir(wall);
                    let climb = if look.z != 0.0 { Vec3::Z } else { Vec3::Z };
                    v.ladder = Some(LadderInfo { climb_dir: climb, look_dir: look });
                    stats.ladder_volumes += 1;
                }
                if is_default_pv {
                    world.default_volume_info = v.clone();
                    if world.default_volume.is_none() {
                        let mut d = Actor::new(class.clone(), loc);
                        d.user = i as u64;
                        d.is_static = true;
                        d.volume = Some(v);
                        let id = world.add(d);
                        world.default_volume = Some(id);
                        by_export.insert(i as u32, id);
                    }
                    continue;
                }
                let Some(ObjRef::Export(b)) = p.object("Brush") else { continue };
                let model = match brushes.get(&b) {
                    Some(m) => m.clone(),
                    None => match upkg::model::read_model(map, b) {
                        Ok(m) => {
                            let bm = Arc::new(BrushModel::from_model(&m));
                            brushes.insert(b, bm.clone());
                            bm
                        }
                        Err(_) => {
                            stats.brush_errors += 1;
                            continue;
                        }
                    },
                };
                let scale = scale_prop(&p, "MainScale") * scale_prop(&p, "PostScale");
                a.collider = Collider::brush(model, scale, pivot);
                a.is_static = true;
                if v.water {
                    stats.water_volumes += 1;
                }
                if v.physics_volume {
                    stats.physics_volumes += 1;
                }
                if a.block_actors {
                    stats.blocking_volumes += 1;
                }
                a.volume = Some(v);
                stats.volumes += 1;
            }
            let id = world.add(a);
            by_export.insert(i as u32, id);
        }
        // Everything spawned before the volumes existed gets its volume now.
        for id in world.ids() {
            world.update_volumes(id, false);
        }
        world.drain_events();
        stats.load_ms = t0.elapsed().as_millis();
        Ok(LoadedMap { world, by_export, stats })
    }
}

impl Actor {
    fn tag_physics(&mut self, b: u8) {
        self.physics = Physics::from_u8(b);
    }
}
