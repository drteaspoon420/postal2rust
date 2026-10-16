//! AI navigation natives (route finding over the map's navigation network).
//!
//! The VM does not know how maps are stored; a level host installs a [`NavGraphView`] (for POSTAL 2
//! maps: `p2nav::VmNav`) with [`Vm::set_nav`]. Reachability tests that need world collision go
//! through a [`Reachability`] implementation ([`StraightLine`] by default: open space, any
//! point within a pawn's step/jump height is reachable); an engine with collision installs its
//! own with [`Vm::set_reachability`].
//!
//! Route search follows the engine's documented behaviour:
//! - The search starts at the pawn's `Anchor` when it is set and still reachable, otherwise at
//!   every navigation point the pawn can reach directly (within 1200 units), each seeded with its
//!   straight-line distance.
//! - Dijkstra over the ReachSpecs: an edge is usable when the pawn fits (`CollisionRadius` and
//!   `CollisionHeight` of the spec are at least the pawn's), its movement flags are supported by the
//!   pawn (walk / fly / swim / jump / ladder / door), and the end point is not `bBlocked`. Edge
//!   weight is the spec `Distance` plus the end point's `cost` and `ExtraCost`, plus the result of
//!   the end point's `SpecialCost(Pawn, ReachSpec)` event when its class defines one in script.
//! - Every point keeps the search's accumulated weight in `visitedWeight` (the engine exposes it
//!   to script, and some AI reads it afterwards).
//! - The best goal's route is unwound into `RouteCache[0..16]` (first hop first). `RouteGoal` and
//!   `RouteDist` are set and the function returns the first hop (`RouteCache[0]`), or the goal
//!   itself when it is directly reachable.
//! - `bClearPaths` / `ClearPaths()` reset `visitedWeight` to a large value and clear `RouteCache`.
//!
//! `MoveTo` / `MoveToward` are latent (see `host::default_latent_tick`); they set `Destination` and
//! `MoveTarget` and move the pawn. `bAdvancedTactics` (serpentine strafing) is not modelled: pawns
//! move straight.

use crate::Result;
use crate::vm::{ClassId, ObjId, Value, Vm};

/// `reachFlags` bits used by the search.
pub mod reach {
    pub const WALK: i32 = 1;
    pub const FLY: i32 = 2;
    pub const SWIM: i32 = 4;
    pub const JUMP: i32 = 8;
    pub const DOOR: i32 = 16;
    pub const SPECIAL: i32 = 32;
    pub const LADDER: i32 = 64;
    pub const PROSCRIBED: i32 = 128;
    pub const FORCED: i32 = 256;
    pub const PLAYERONLY: i32 = 512;
}

/// One outgoing ReachSpec.
#[derive(Debug, Clone, Copy)]
pub struct NavEdge {
    pub end: u32,
    /// Index of the spec (for [`NavGraphView::spec_obj`]).
    pub spec: u32,
    pub distance: i32,
    pub collision_radius: i32,
    pub collision_height: i32,
    pub reach_flags: i32,
    pub max_landing_velocity: i32,
}

/// The navigation network of the current level, bound to VM objects.
pub trait NavGraphView {
    fn node_count(&self) -> usize;
    /// The VM object (NavigationPoint actor) of a node.
    fn node_obj(&self, node: u32) -> Option<ObjId>;
    fn obj_node(&self, obj: ObjId) -> Option<u32>;
    fn node_location(&self, node: u32) -> [f32; 3];
    fn edges(&self, node: u32) -> &[NavEdge];
    /// The VM object of a ReachSpec (passed to `SpecialCost`), if it was instantiated.
    fn spec_obj(&self, spec: u32) -> Option<ObjId>;
}

/// Movement capabilities and size of a pawn, read from its properties.
#[derive(Debug, Clone, Copy)]
pub struct Mover {
    pub pawn: ObjId,
    pub location: [f32; 3],
    pub radius: f32,
    pub height: f32,
    pub can_walk: bool,
    pub can_jump: bool,
    pub can_swim: bool,
    pub can_fly: bool,
    pub can_climb: bool,
    pub can_open_doors: bool,
    pub jump_z: f32,
    pub max_step: f32,
}

/// World reachability tests (`actorReachable` / `pointReachable` and search seeding).
pub trait Reachability {
    /// True if `who` can move directly from its location to `dest`.
    fn point_reachable(&self, vm: &mut Vm, who: &Mover, dest: [f32; 3]) -> bool;
}

/// No world geometry: reachable when within the walkable height difference (step height, or
/// jump height for jumpers) or for flyers. Used until an engine installs collision-based tests.
pub struct StraightLine;

impl Reachability for StraightLine {
    fn point_reachable(&self, _vm: &mut Vm, who: &Mover, dest: [f32; 3]) -> bool {
        if who.can_fly {
            return true;
        }
        let dz = dest[2] - who.location[2];
        let up = if who.can_jump { who.max_step.max(who.jump_z * who.jump_z / (2.0 * 950.0)) } else { who.max_step };
        dz <= up + who.height
    }
}

/// Search state kept between calls (the engine stores it on the points).
#[derive(Default)]
pub struct NavState {
    pub graph: Option<Box<dyn NavGraphView>>,
    pub reach: Option<Box<dyn Reachability>>,
    /// Classes whose script defines `SpecialCost`, cached by class id.
    special_cost: std::collections::HashMap<ClassId, bool>,
    pub stats: NavCounters,
}

#[derive(Debug, Default, Clone)]
pub struct NavCounters {
    pub searches: u64,
    pub found: u64,
    pub nodes_expanded: u64,
}

const UNVISITED: i32 = 10_000_000;
const SEED_RADIUS: f32 = 1200.0;
const ROUTE_LEN: usize = 16;
/// Route searches seed from at most this many reachable points, nearest first.
const SEED_MAX: usize = 3;

fn all_seeds() -> bool {
    static ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALL.get_or_init(|| std::env::var_os("P2_NAV_ALL_SEEDS").is_some())
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn edge_ok(e: &NavEdge, m: &Mover) -> bool {
    if (e.collision_radius as f32) < m.radius || (e.collision_height as f32) < m.height {
        return false;
    }
    let f = e.reach_flags;
    if f & reach::PROSCRIBED != 0 || f & reach::PLAYERONLY != 0 {
        return false;
    }
    let needs = [(reach::WALK, m.can_walk), (reach::FLY, m.can_fly), (reach::SWIM, m.can_swim), (reach::JUMP, m.can_jump), (reach::LADDER, m.can_climb), (reach::DOOR, m.can_open_doors)];
    // A spec lists every movement mode it needs; walking specs may be used by flyers too.
    for (bit, ok) in needs {
        if f & bit != 0 && !ok && !(bit == reach::WALK && m.can_fly) {
            return false;
        }
    }
    true
}

/// What a route search is looking for.
pub enum Goal {
    Actor(ObjId),
    Point([f32; 3]),
    /// Nearest navigation point of this class (or a subclass).
    Class(ClassId),
    /// Any random reachable navigation point (`FindRandomDest`).
    Random,
}

impl Vm {
    /// Installs the level's navigation network (replaces the previous one).
    pub fn set_nav(&mut self, graph: Box<dyn NavGraphView>) {
        self.nav.graph = Some(graph);
        self.nav.special_cost.clear();
    }

    /// Installs collision-based reachability tests (default: [`StraightLine`]).
    pub fn set_reachability(&mut self, r: Box<dyn Reachability>) {
        self.nav.reach = Some(r);
    }

    /// Movement description of a pawn from its properties.
    pub fn mover(&self, pawn: ObjId, controller: Option<ObjId>) -> Mover {
        let f = |n: &str, d: f32| self.get_prop(pawn, n).map(|v| v.as_float()).unwrap_or(d);
        let b = |n: &str, d: bool| self.get_prop(pawn, n).map(|v| v.as_bool()).unwrap_or(d);
        Mover {
            pawn,
            location: self.actor_loc(pawn),
            radius: f("CollisionRadius", 25.0),
            height: f("CollisionHeight", 44.0),
            can_walk: b("bCanWalk", true),
            can_jump: b("bCanJump", false),
            can_swim: b("bCanSwim", false),
            can_fly: b("bCanFly", false),
            can_climb: b("bCanClimbLadders", false),
            can_open_doors: controller.and_then(|c| self.get_prop(c, "bCanOpenDoors")).map(|v| v.as_bool()).unwrap_or(true),
            jump_z: f("JumpZ", 420.0),
            max_step: 35.0,
        }
    }

    /// PointReachable with the installed reachability test (research probes).
    pub fn point_reachable(&mut self, m: &Mover, dest: [f32; 3]) -> bool {
        self.reachable(m, dest)
    }

    fn reachable(&mut self, m: &Mover, dest: [f32; 3]) -> bool {
        match self.nav.reach.take() {
            Some(r) => {
                let ok = r.point_reachable(self, m, dest);
                self.nav.reach = Some(r);
                ok
            }
            None => StraightLine.point_reachable(self, m, dest),
        }
    }

    fn has_special_cost(&mut self, c: ClassId) -> bool {
        if let Some(&b) = self.nav.special_cost.get(&c) {
            return b;
        }
        let b = match self.find_function(c, "SpecialCost") {
            // Engine.NavigationPoint declares an empty event; only script overrides count.
            Some(f) => self.func_info(f).map(|fi| !fi.owner.eq_ignore_ascii_case("NavigationPoint") && fi.flags & crate::objects::func_flags::DEFINED != 0).unwrap_or(false),
            None => false,
        };
        self.nav.special_cost.insert(c, b);
        b
    }

    /// Route search from `pawn` towards `goal` (see the module docs). Fills `RouteCache`,
    /// `RouteGoal`, `RouteDist` on the controller and returns the first hop.
    pub fn find_route(&mut self, controller: ObjId, goal: Goal, clear: bool) -> Result<Option<ObjId>> {
        let Some(pawn) = self.get_prop(controller, "Pawn").and_then(|v| v.as_obj()).filter(|p| !self.objects[*p].deleted) else {
            return Ok(None);
        };
        let m = self.mover(pawn, Some(controller));
        if clear {
            self.clear_paths(controller);
        }
        // A directly reachable actor/point goal needs no route.
        let goal_loc = match &goal {
            Goal::Actor(a) => Some(self.actor_loc(*a)),
            Goal::Point(p) => Some(*p),
            _ => None,
        };
        if let Some(gl) = goal_loc {
            if dist(gl, m.location) < 1200.0 && self.reachable(&m, gl) {
                if let Goal::Actor(a) = goal {
                    self.set_route(controller, &[], Some(a), dist(gl, m.location));
                    return Ok(Some(a));
                }
            }
        }
        let Some(graph) = self.nav.graph.take() else { return Ok(None) };
        let r = self.search(&*graph, controller, &m, &goal);
        self.nav.graph = Some(graph);
        r
    }

    fn search(&mut self, g: &dyn NavGraphView, controller: ObjId, m: &Mover, goal: &Goal) -> Result<Option<ObjId>> {
        self.nav.stats.searches += 1;
        let n = g.node_count();
        let mut weight = vec![UNVISITED; n];
        let mut prev = vec![u32::MAX; n];
        let mut done = vec![false; n];
        let mut heap = std::collections::BinaryHeap::new();
        // Seeds: the anchor, else every nearby reachable point.
        let anchor = self.get_prop(m.pawn, "Anchor").and_then(|v| v.as_obj()).and_then(|a| g.obj_node(a));
        let mut seeds = Vec::new();
        if let Some(a) = anchor.filter(|&a| dist(g.node_location(a), m.location) < SEED_RADIUS && self.reachable(m, g.node_location(a))) {
            seeds.push((a, dist(g.node_location(a), m.location) as i32));
        } else {
            // Nearest first, stopping after SEED_MAX reachable points (UE2 seeds from one anchor,
            // the nearest reachable point). Each reach test is a collision walk (~0.1-0.3 ms);
            // testing every point within SEED_RADIUS (~40) made FindRandomDest cost 3-14 ms per
            // call and ~0.7 s over the first ticks after a level load.
            // `P2_NAV_ALL_SEEDS=1` restores seeding from every reachable point in range.
            let mut cand: Vec<(u32, f32)> = (0..n as u32).map(|k| (k, dist(g.node_location(k), m.location))).filter(|&(_, d)| d < SEED_RADIUS).collect();
            cand.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let max = if all_seeds() { usize::MAX } else { SEED_MAX };
            for (k, d) in cand {
                if seeds.len() >= max {
                    break;
                }
                if self.reachable(m, g.node_location(k)) {
                    seeds.push((k, d as i32));
                }
            }
        }
        for (k, d) in seeds {
            if d < weight[k as usize] {
                weight[k as usize] = d;
                heap.push(std::cmp::Reverse((d, k)));
            }
        }
        // Goal test per node.
        let goal_node = match goal {
            Goal::Actor(a) => g.obj_node(*a),
            _ => None,
        };
        let goal_point = match goal {
            Goal::Actor(a) if goal_node.is_none() => Some(self.actor_loc(*a)),
            Goal::Point(p) => Some(*p),
            _ => None,
        };
        let mut best: Option<(u32, i32)> = None;
        let mut random_pick = Vec::new();
        while let Some(std::cmp::Reverse((w, k))) = heap.pop() {
            if done[k as usize] || w > weight[k as usize] {
                continue;
            }
            done[k as usize] = true;
            self.nav.stats.nodes_expanded += 1;
            let hit = match goal {
                Goal::Actor(_) if goal_node.is_some() => Some(k) == goal_node,
                Goal::Class(c) => g.node_obj(k).is_some_and(|o| self.is_child_of(self.objects[o].class, *c)),
                Goal::Random => {
                    random_pick.push((k, w));
                    false
                }
                _ => false,
            };
            if hit {
                best = Some((k, w));
                break;
            }
            if let Some(gp) = goal_point {
                // Points/actors off the network: finish at the node with the smallest total cost
                // from which the goal is directly reachable.
                let d = dist(g.node_location(k), gp);
                if d < SEED_RADIUS {
                    let total = w + d as i32;
                    let probe = Mover { location: g.node_location(k), ..*m };
                    if best.is_none_or(|b| total < b.1) && self.reachable(&probe, gp) {
                        best = Some((k, total));
                    }
                }
                if best.is_some_and(|b| w > b.1) {
                    break;
                }
            }
            for e in g.edges(k).to_vec() {
                if done[e.end as usize] || !edge_ok(&e, m) {
                    continue;
                }
                let Some(end_obj) = g.node_obj(e.end) else { continue };
                if self.get_prop(end_obj, "bBlocked").is_some_and(|v| v.as_bool()) {
                    continue;
                }
                let mut cost = e.distance.max(1);
                cost += self.get_prop(end_obj, "cost").map(|v| v.as_int()).unwrap_or(0);
                cost += self.get_prop(end_obj, "ExtraCost").map(|v| v.as_int()).unwrap_or(0);
                let ec = self.objects[end_obj].class;
                if self.has_special_cost(ec) {
                    let spec = Value::Object(g.spec_obj(e.spec));
                    let extra = self.call_event(end_obj, "SpecialCost", vec![Value::Object(Some(m.pawn)), spec])?;
                    let extra = extra.map(|v| v.as_int()).unwrap_or(0);
                    if extra >= UNVISITED {
                        continue;
                    }
                    cost += extra;
                }
                let nw = w + cost;
                if nw < weight[e.end as usize] {
                    weight[e.end as usize] = nw;
                    prev[e.end as usize] = k;
                    heap.push(std::cmp::Reverse((nw, e.end)));
                }
            }
        }
        // visitedWeight on every point, as script may read it.
        for k in 0..n as u32 {
            if let Some(o) = g.node_obj(k) {
                self.set_prop(o, "visitedWeight", Value::Int(weight[k as usize]));
            }
        }
        if let Goal::Random = goal {
            // Weighted toward farther points like the engine's random roam: pick uniformly among
            // reached points, excluding the start area.
            random_pick.retain(|&(_, w)| w > 200);
            if random_pick.is_empty() {
                return Ok(None);
            }
            let i = ((self.frand() * random_pick.len() as f32) as usize).min(random_pick.len() - 1);
            best = Some(random_pick[i]);
        }
        let Some((end, total)) = best else { return Ok(None) };
        self.nav.stats.found += 1;
        let mut path = vec![end];
        let mut cur = end;
        while prev[cur as usize] != u32::MAX && path.len() < 4096 {
            cur = prev[cur as usize];
            path.push(cur);
        }
        path.reverse();
        // SetRouteCache: a start node the pawn already stands on is not kept as the first hop,
        // or a mover that "arrives" there at once asks again and gets the same node forever
        // (FIDELITY #55: AW zombies looped WalkToAttacker/AttackTarget without moving).
        if path.len() >= 2 && (self.at_node(g, m, path[0]) || self.on_nodes_ladder(g, m, path[0])) {
            path.remove(0);
        }
        let objs: Vec<ObjId> = path.iter().filter_map(|&k| g.node_obj(k)).collect();
        let route_goal = match goal {
            Goal::Actor(a) => Some(*a),
            _ => objs.last().copied(),
        };
        self.set_route(controller, &objs, route_goal, total as f32);
        Ok(match goal {
            Goal::Random => objs.last().copied(),
            _ => objs.first().copied().or(route_goal),
        })
    }

    /// A Ladder point whose LadderVolume the pawn is climbing counts as reached (UE2
    /// ReachedDestination for ladders): a zombie on the rungs was otherwise sent back to the
    /// bottom Ladder point forever.
    fn on_nodes_ladder(&self, g: &dyn NavGraphView, m: &Mover, k: u32) -> bool {
        let Some(o) = g.node_obj(k) else { return false };
        let my = self.get_prop(o, "MyLadder").and_then(|v| v.as_obj());
        my.is_some() && my == self.get_prop(m.pawn, "OnLadder").and_then(|v| v.as_obj())
    }

    /// ReachedDestination for a navigation point: the pawn's cylinder overlaps the point's.
    fn at_node(&self, g: &dyn NavGraphView, m: &Mover, k: u32) -> bool {
        let l = g.node_location(k);
        let (r, h) = g.node_obj(k).map_or((0.0, 0.0), |o| {
            let f = |n: &str| self.get_prop(o, n).map(|v| v.as_float()).unwrap_or(0.0);
            (f("CollisionRadius"), f("CollisionHeight"))
        });
        let (dx, dy) = (l[0] - m.location[0], l[1] - m.location[1]);
        (dx * dx + dy * dy).sqrt() < m.radius.max(r) && (l[2] - m.location[2]).abs() < m.height + h
    }

    fn set_route(&mut self, controller: ObjId, route: &[ObjId], goal: Option<ObjId>, d: f32) {
        let mut cache: Vec<Value> = route.iter().take(ROUTE_LEN).map(|&o| Value::Object(Some(o))).collect();
        cache.resize(ROUTE_LEN, Value::Object(None));
        self.set_prop(controller, "RouteCache", Value::Array(cache));
        self.set_prop(controller, "RouteGoal", Value::Object(goal));
        self.set_prop(controller, "RouteDist", Value::Float(d));
    }

    /// `ClearPaths()`: forget the previous search.
    pub fn clear_paths(&mut self, controller: ObjId) {
        if let Some(g) = self.nav.graph.take() {
            for k in 0..g.node_count() as u32 {
                if let Some(o) = g.node_obj(k) {
                    self.set_prop(o, "visitedWeight", Value::Int(UNVISITED));
                }
            }
            self.nav.graph = Some(g);
        }
        self.set_route(controller, &[], None, 0.0);
    }
}

fn controller_of(vm: &Vm) -> Result<ObjId> {
    vm.native_self().ok_or_else(|| crate::ScriptError::Bad("controller native without a controller".into()))
}

fn pawn_of(vm: &Vm, c: ObjId) -> Option<ObjId> {
    vm.get_prop(c, "Pawn").and_then(|v| v.as_obj()).filter(|p| !vm.objects[*p].deleted)
}

macro_rules! reg {
    ($vm:expr, $($name:literal => $f:expr),* $(,)?) => {
        $( $vm.register_native($name, $f); )*
    };
}

/// Registers the Controller / Pawn navigation natives.
pub fn register_nav(vm: &mut Vm) {
    reg!(vm,
        "FindPathToward" => |vm, a| {
            let c = controller_of(vm)?;
            let Some(t) = a.first().and_then(|v| v.as_obj()) else { return Ok(Value::Object(None)) };
            Ok(Value::Object(vm.find_route(c, Goal::Actor(t), a.get(1).is_some_and(|v| v.as_bool()))?))
        },
        "FindPathTo" => |vm, a| {
            let c = controller_of(vm)?;
            let p = a.first().map(|v| v.as_vec()).unwrap_or_default();
            Ok(Value::Object(vm.find_route(c, Goal::Point(p), a.get(1).is_some_and(|v| v.as_bool()))?))
        },
        "FindPathTowardNearest" => |vm, a| {
            let c = controller_of(vm)?;
            let Some(cl) = a.first().and_then(|v| v.as_class()) else { return Ok(Value::Object(None)) };
            Ok(Value::Object(vm.find_route(c, Goal::Class(cl), false)?))
        },
        "FindRandomDest" => |vm, a| {
            let c = controller_of(vm)?;
            Ok(Value::Object(vm.find_route(c, Goal::Random, a.first().is_some_and(|v| v.as_bool()))?))
        },
        "ClearPaths" => |vm, _| { let c = controller_of(vm)?; vm.clear_paths(c); Ok(Value::Void) },
        "pointReachable" => |vm, a| {
            let c = controller_of(vm)?;
            let Some(p) = pawn_of(vm, c) else { return Ok(Value::Bool(false)) };
            let m = vm.mover(p, Some(c));
            let d = a.first().map(|v| v.as_vec()).unwrap_or_default();
            Ok(Value::Bool(vm.reachable(&m, d)))
        },
        "actorReachable" => |vm, a| {
            let c = controller_of(vm)?;
            let (Some(p), Some(t)) = (pawn_of(vm, c), a.first().and_then(|v| v.as_obj())) else { return Ok(Value::Bool(false)) };
            let m = vm.mover(p, Some(c));
            let d = vm.actor_loc(t);
            Ok(Value::Bool(vm.reachable(&m, d)))
        },
        "EAdjustJump" => |vm, a| {
            // Velocity to jump toward Destination: horizontal speed XYSpeed, vertical BaseZ.
            let c = controller_of(vm)?;
            let Some(p) = pawn_of(vm, c) else { return Ok(Value::Vector([0.0; 3])) };
            let (base_z, xy) = (a.first().map(|v| v.as_float()).unwrap_or(0.0), a.get(1).map(|v| v.as_float()).unwrap_or(0.0));
            let from = vm.actor_loc(p);
            let to = vm.get_prop(c, "Destination").map(|v| v.as_vec()).unwrap_or(from);
            let d = [to[0] - from[0], to[1] - from[1]];
            let l = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-3);
            Ok(Value::Vector([d[0] / l * xy, d[1] / l * xy, base_z]))
        },
        "PickWallAdjust" => |vm, a| {
            // Step around an obstacle: aim 90 degrees off the wall normal toward the destination
            // side and set AdjustLoc / bAdjusting.
            let c = controller_of(vm)?;
            let Some(p) = pawn_of(vm, c) else { return Ok(Value::Bool(false)) };
            let n = a.first().map(|v| v.as_vec()).unwrap_or_default();
            let from = vm.actor_loc(p);
            let dest = vm.get_prop(c, "Destination").map(|v| v.as_vec()).unwrap_or(from);
            let side = [-n[1], n[0], 0.0];
            let dir = [dest[0] - from[0], dest[1] - from[1]];
            let s = if side[0] * dir[0] + side[1] * dir[1] >= 0.0 { 1.0 } else { -1.0 };
            let r = vm.get_prop(p, "CollisionRadius").map(|v| v.as_float()).unwrap_or(25.0) * 3.0;
            let adj = [from[0] + side[0] * s * r, from[1] + side[1] * s * r, from[2]];
            let m = vm.mover(p, Some(c));
            if !vm.reachable(&m, adj) {
                return Ok(Value::Bool(false));
            }
            vm.set_prop(c, "AdjustLoc", Value::Vector(adj));
            vm.set_prop(c, "bAdjusting", Value::Bool(true));
            Ok(Value::Bool(true))
        },
        "EndClimbLadder" => |_, _| Ok(Value::Void),
        "FindBestInventoryPath" => |_, _| Ok(Value::Object(None)),
        "PickTarget" => |_, _| Ok(Value::Object(None)),
        "PickAnyTarget" => |_, _| Ok(Value::Object(None)),
    );
}
