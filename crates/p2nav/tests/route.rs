//! Route finding over a real map's navigation network through the VM natives.
//! Skipped when the game is not installed.

use p2nav::{NavGraph, VmNav};
use std::collections::HashMap;
use std::path::PathBuf;
use uscript::vm::{ObjId, Value, Vm};

fn root() -> Option<PathBuf> {
    let r = PathBuf::from(std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into()));
    r.join("System").join("Core.u").is_file().then_some(r)
}

/// Builds VM objects for every nav point of a map (class from the map, saved location) and a
/// walking pawn with an AIController.
fn setup(map: &str) -> Option<(Vm, NavGraph, HashMap<usize, ObjId>, ObjId, ObjId)> {
    let r = root()?;
    let pkg = upkg::Package::open(r.join("Maps").join(map)).ok()?;
    let g = NavGraph::from_package(&pkg);
    let mut vm = Vm::new(r.join("System")).unwrap();
    vm.missing_native_policy = uscript::vm::MissingNativePolicy::Lenient;
    vm.init_world(None).unwrap();
    let mut objs = HashMap::new();
    for n in &g.nodes {
        let c = vm.find_class_by_name(&n.class).or_else(|_| vm.find_class("Engine", "PathNode")).unwrap();
        let o = vm.new_object(c);
        vm.set_prop(o, "Location", Value::Vector(n.location));
        vm.set_prop(o, "Name", Value::Name(n.name.clone()));
        objs.insert(n.export, o);
    }
    let pc = vm.find_class("Engine", "Pawn").unwrap();
    let pawn = vm.new_object(pc);
    vm.set_prop(pawn, "CollisionRadius", Value::Float(25.0));
    vm.set_prop(pawn, "CollisionHeight", Value::Float(44.0));
    vm.set_prop(pawn, "bCanWalk", Value::Bool(true));
    let cc = vm.find_class("Engine", "AIController").unwrap();
    let ctrl = vm.new_object(cc);
    vm.set_prop(ctrl, "Pawn", Value::Object(Some(pawn)));
    vm.set_prop(pawn, "Controller", Value::Object(Some(ctrl)));
    Some((vm, g, objs, pawn, ctrl))
}

fn call(vm: &mut Vm, ctrl: ObjId, f: &str, args: Vec<Value>) -> Value {
    let c = vm.objects[ctrl].class;
    vm.call(c, Some(ctrl), f, args).unwrap()
}

#[test]
fn find_path_toward_follows_reachspecs() {
    let Some((mut vm, g, objs, pawn, ctrl)) = setup("suburbs-3.fuk") else { return };
    let graph = VmNav::new(g, &objs);
    // Two PathNodes far apart that are both on the network.
    let (a, b) = {
        let nodes: Vec<_> = graph.graph.nodes.iter().enumerate().filter(|(_, n)| n.class == "PathNode" && !n.paths.is_empty()).collect();
        let a = nodes[0].0;
        let b = nodes.iter().max_by(|x, y| {
            let d = |n: &p2nav::NavNode| (0..3).map(|i| (n.location[i] - nodes[0].1.location[i]).powi(2)).sum::<f32>();
            d(x.1).partial_cmp(&d(y.1)).unwrap()
        }).unwrap().0;
        (a, b)
    };
    let start = graph.graph.nodes[a].location;
    let goal = objs[&graph.graph.nodes[b].export];
    let names: HashMap<ObjId, String> = graph.graph.nodes.iter().map(|n| (objs[&n.export], n.name.clone())).collect();
    vm.set_nav(Box::new(graph));
    vm.set_prop(pawn, "Location", Value::Vector([start[0], start[1], start[2] + 10.0]));
    let first = call(&mut vm, ctrl, "FindPathToward", vec![Value::Object(Some(goal)), Value::Bool(true)]);
    let Some(first) = first.as_obj() else { panic!("no route") };
    let Some(Value::Array(cache)) = vm.get_prop(ctrl, "RouteCache").cloned() else { panic!() };
    let route: Vec<ObjId> = cache.iter().filter_map(|v| v.as_obj()).collect();
    assert_eq!(route[0], first);
    assert!(route.len() >= 2, "{route:?}");
    assert_eq!(vm.get_prop(ctrl, "RouteGoal"), Some(&Value::Object(Some(goal))));
    let dist = vm.get_prop(ctrl, "RouteDist").unwrap().as_float();
    assert!(dist > 1000.0, "{dist}");
    // visitedWeight was written on the goal.
    assert!(vm.get_prop(goal, "visitedWeight").unwrap().as_int() < 10_000_000);
    println!("route ({} hops shown, dist {dist}): {:?}", route.len(), route.iter().map(|o| &names[o]).collect::<Vec<_>>());

    // FindRandomDest picks a reachable network point; FindPathTo a point next to the goal.
    let r = call(&mut vm, ctrl, "FindRandomDest", vec![Value::Bool(true)]);
    assert!(r.as_obj().is_some());
    let gl = vm.actor_loc(goal);
    let r = call(&mut vm, ctrl, "FindPathTo", vec![Value::Vector([gl[0] + 30.0, gl[1], gl[2]])]);
    assert!(r.as_obj().is_some());
    // FindPathTowardNearest(class'PlayerStart').
    let ps = vm.find_class("Engine", "PlayerStart").unwrap();
    let r = call(&mut vm, ctrl, "FindPathTowardNearest", vec![Value::Class(Some(ps))]);
    let target = vm.get_prop(ctrl, "RouteGoal").and_then(|v| v.as_obj());
    assert!(r.as_obj().is_some() && target.is_some_and(|t| vm.objects[t].class == ps), "{r:?} {target:?}");
    // ClearPaths empties the cache.
    call(&mut vm, ctrl, "ClearPaths", vec![]);
    assert_eq!(vm.get_prop(ctrl, "RouteGoal"), Some(&Value::Object(None)));
    assert!(vm.nav.stats.searches >= 4);
}

#[test]
fn reachability_and_blocked_points() {
    let Some((mut vm, g, objs, pawn, ctrl)) = setup("suburbs-3.fuk") else { return };
    let graph = VmNav::new(g, &objs);
    let k = graph.graph.nodes.iter().position(|n| n.class == "PathNode" && n.paths.len() >= 2).unwrap();
    let here = graph.graph.nodes[k].location;
    let next_spec = &graph.graph.specs[graph.graph.nodes[k].paths[0] as usize];
    let next = objs[&graph.graph.nodes[next_spec.end as usize].export];
    vm.set_nav(Box::new(graph));
    vm.set_prop(pawn, "Location", Value::Vector(here));
    assert_eq!(call(&mut vm, ctrl, "pointReachable", vec![Value::Vector([here[0] + 100.0, here[1], here[2]])]), Value::Bool(true));
    assert_eq!(call(&mut vm, ctrl, "pointReachable", vec![Value::Vector([here[0], here[1], here[2] + 2000.0])]), Value::Bool(false));
    assert_eq!(call(&mut vm, ctrl, "actorReachable", vec![Value::Object(Some(next))]), Value::Bool(true));
    // A blocked point is never used as an intermediate hop.
    vm.set_prop(next, "bBlocked", Value::Bool(true));
    let far = vm.live_actors().len(); // unrelated: keep the world alive
    let _ = far;
    call(&mut vm, ctrl, "FindRandomDest", vec![Value::Bool(true)]);
    let Some(Value::Array(cache)) = vm.get_prop(ctrl, "RouteCache").cloned() else { panic!() };
    assert!(cache.iter().all(|v| v.as_obj() != Some(next)));
}
