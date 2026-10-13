//! World-level tests: state code, latent Sleep, foreach iterators, delegates/dynamic arrays,
//! host hooks, class defaults and a headless world built from Engine/GameTypes classes.
//! Skipped (pass trivially) when the game is not installed.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use uscript::host::{AnimRequest, HeadlessHost, Host, NativeCall, SoundRequest, SpawnParams};
use uscript::vm::{MissingNativePolicy, ObjId, Value, Vm};

fn system_dir() -> Option<PathBuf> {
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let p = PathBuf::from(root).join("System");
    p.join("Core.u").is_file().then_some(p)
}

macro_rules! vm_or_skip {
    () => {
        match system_dir() {
            Some(d) => Vm::new(d).expect("vm"),
            None => {
                eprintln!("POSTAL 2 install not found; skipping");
                return;
            }
        }
    };
}

/// Engine.Pickup is abstract (spawn refuses it); build one by hand to test its own states.
fn raw_pickup(vm: &mut Vm) -> ObjId {
    let c = vm.find_class("Engine", "Pickup").unwrap();
    let p = vm.new_object(c);
    vm.set_prop(p, "Level", Value::Object(vm.level));
    vm.actors.push(p);
    vm.goto_state(p, "Pickup", "").unwrap();
    p
}

#[test]
fn pickup_sleeping_state_code_sleeps_then_goes_to_pickup() {
    let mut vm = vm_or_skip!();
    vm.init_world(None).unwrap();
    let p = raw_pickup(&mut vm);
    assert_eq!(vm.state_name(p), "Pickup");
    vm.set_prop(p, "RespawnTime", Value::Float(1.0));
    // Sleeping: BeginState hides it, state code sleeps RespawnTime then PlaySpawnEffect() (0.1),
    // then GotoState('Pickup') whose EndState of Sleeping unhides it.
    vm.goto_state(p, "Sleeping", "").unwrap();
    assert_eq!(vm.state_name(p), "Sleeping");
    assert_eq!(vm.get_prop(p, "bHidden"), Some(&Value::Bool(true)));
    let mut t = 0.0;
    while vm.state_name(p) == "Sleeping" && t < 5.0 {
        vm.tick_world(0.05);
        t += 0.05;
        // ReadyToPickup reads LatentFloat (remaining sleep) through the state's override.
        if t > 0.2 && t < 0.8 {
            let r = vm.call_event(p, "ReadyToPickup", vec![Value::Float(2.0)]).unwrap();
            assert_eq!(r, Some(Value::Bool(true)));
        }
    }
    assert_eq!(vm.state_name(p), "Pickup", "left Sleeping after {t}s");
    assert!((1.05..1.3).contains(&t), "slept {t}s, expected about 1.1s");
    assert_eq!(vm.get_prop(p, "bHidden"), Some(&Value::Bool(false)));
    assert!(vm.script_errors.is_empty(), "{:?}", vm.script_errors);
}

#[test]
fn ignores_skip_probe_functions_and_disable_works() {
    let mut vm = vm_or_skip!();
    vm.init_world(None).unwrap();
    let p = raw_pickup(&mut vm);
    // Pickup.Sleeping declares `ignores Touch` (probe bit 13).
    vm.goto_state(p, "Sleeping", "").unwrap();
    assert!(vm.is_ignored(p, "Touch"));
    assert!(!vm.is_ignored(p, "Timer"));
    vm.goto_state(p, "Pickup", "").unwrap();
    assert!(!vm.is_ignored(p, "Touch"));
    vm.set_probe_enabled(p, "Timer", false);
    assert!(vm.is_ignored(p, "Timer"));
    vm.set_probe_enabled(p, "Timer", true);
    assert!(!vm.is_ignored(p, "Timer"));
}

#[test]
fn foreach_alldynamic_actors_and_trigger_event() {
    let mut vm = vm_or_skip!();
    vm.init_world(None).unwrap();
    // Two triggers tagged 'Door' and one untagged: TriggerEvent('Door') must reach exactly the
    // tagged ones through `foreach DynamicActors(class'Actor', A, EventName) A.Trigger(...)`.
    let counter = vm.find_class("GameTypes", "TriggerCounter").unwrap();
    let mut tagged = Vec::new();
    for k in 0..3 {
        let tag = if k < 2 { Some("Door".to_string()) } else { None };
        let a = vm.spawn_actor(counter, SpawnParams { tag, ..Default::default() }, true).unwrap();
        tagged.push(a);
    }
    let ti = vm.find_class("Engine", "TestInfo").unwrap();
    let l = vm.spawn_actor(ti, SpawnParams::default(), true).unwrap();
    let uses = |vm: &Vm| -> Vec<Value> { tagged.iter().map(|&a| vm.get_prop(a, "UseTimes").cloned().unwrap()).collect() };
    let before = uses(&vm);
    vm.call(ti, Some(l), "TriggerEvent", vec![Value::Name("Door".into()), Value::Object(Some(l)), Value::Object(None)]).unwrap();
    let after = uses(&vm);
    // TriggerCounter counts triggers (UseTimes++) in its CountTriggers state.
    assert_eq!(before, vec![Value::Int(0); 3]);
    assert_eq!(after, vec![Value::Int(1), Value::Int(1), Value::Int(0)]);
    assert_eq!(vm.state_name(tagged[0]), "CountTriggers");
}

#[test]
fn foreach_continue_and_break_inside_iterator() {
    let mut vm = vm_or_skip!();
    vm.init_world(None).unwrap();
    let light = vm.find_class("Engine", "Light").unwrap();
    for k in 0..3 {
        vm.spawn_actor(light, SpawnParams { location: [k as f32, 0.0, 0.0], ..Default::default() }, true);
    }
    // Engine.TestInfo.TestContinueForEach: logs each actor, '...' for non-lights.
    let ti = vm.find_class("Engine", "TestInfo").unwrap();
    let t = vm.spawn_actor(ti, SpawnParams::default(), true).unwrap();
    vm.log.clear();
    vm.call(ti, Some(t), "TestContinueForEach", vec![]).unwrap();
    let actors = vm.log.iter().filter(|l| l.contains("actor ")).count();
    assert_eq!(actors, vm.live_actor_count(), "{:?}", vm.log);
    assert_eq!(vm.log.last().map(String::as_str), Some("ScriptLog: DoneContinue"));
    // RadiusActors through the default host.
    let rows = uscript::host::default_iterate(&mut vm, Some(t), "RadiusActors", &[Value::Class(Some(light)), Value::Void, Value::Float(1.5), Value::Vector([0.0; 3])]).unwrap();
    assert_eq!(rows.len(), 2);
}

#[test]
fn string_and_array_natives() {
    let mut vm = vm_or_skip!();
    let object = vm.find_class("Core", "Object").unwrap();
    // SplitString appends to the out array and returns the count.
    let r = vm.call(object, None, "SplitString", vec![Value::Str("This|is|a|good string!".into()), Value::Str("|".into()), Value::Array(vec![])]);
    // Native functions are reachable through call() as well.
    assert_eq!(r.unwrap(), Value::Int(4));
    let ti = vm.find_class("Engine", "TestInfo").unwrap();
    let o = vm.new_object(ti);
    // TestInfo exercises dynamic arrays / optional out params in script.
    vm.call(ti, Some(o), "TestOptionalOut", vec![]).unwrap();
    let r = vm.call(object, None, "Localize", vec![Value::Str("General".into()), Value::Str("Nonexistent".into()), Value::Str("Core".into())]).unwrap();
    assert_eq!(r, Value::Str("<?int?Core.General.Nonexistent?>".into()));
}

#[test]
fn class_none_compares_equal_to_none() {
    let mut vm = vm_or_skip!();
    let object = vm.find_class("Core", "Object").unwrap();
    let eq = |vm: &mut Vm, a: Value, b: Value| vm.call(object, None, "EqualEqual_ObjectObject", vec![a, b]).unwrap();
    // `if (PlayerControllerClass == None)` in GameInfo.Login.
    assert_eq!(eq(&mut vm, Value::Class(None), Value::Object(None)), Value::Bool(true));
    assert_eq!(eq(&mut vm, Value::Class(Some(object)), Value::Object(None)), Value::Bool(false));
    // And the load it guards: DynamicLoadObject(Name, class'Class') then class<PlayerController>(...).
    let gi = vm.find_class("Engine", "GameInfo").unwrap();
    vm.init_world(None).unwrap();
    let game = vm.level.and_then(|l| vm.get_prop(l, "Game").and_then(|v| v.as_obj())).unwrap();
    let ps = vm.find_class("Engine", "PlayerStart").unwrap();
    let start = vm.new_object(ps);
    vm.actors.push(start);
    if let Some(l) = vm.level {
        vm.set_prop(l, "NavigationPointList", Value::Object(Some(start)));
    }
    vm.set_prop(game, "PlayerControllerClassName", Value::Str("Engine.PlayerController".into()));
    vm.set_prop(game, "PlayerControllerClass", Value::Class(None));
    let r = vm.call(gi, Some(game), "Login", vec![Value::Str(String::new()), Value::Str("?Name=Test".into()), Value::Str(String::new())]);
    let pcc = vm.get_prop(game, "PlayerControllerClass").cloned();
    assert!(matches!(pcc, Some(Value::Class(Some(_)))), "{pcc:?} {r:?} {:?}", vm.log);
}

#[test]
fn class_default_api_reads_meshes_and_skins() {
    let mut vm = vm_or_skip!();
    let mesh = vm.class_default("People", "UncleDave", "Mesh").unwrap();
    assert!(matches!(mesh, Some(Value::Resource(ref p)) if !p.is_empty()), "{mesh:?}");
    let c = vm.find_class_by_name("UncleDave").unwrap();
    assert_eq!(vm.classes[c].package.to_ascii_lowercase(), "people");
    let skins = vm.default_value(c, "Skins").cloned();
    let Some(Value::Array(skins)) = skins else { panic!("Skins {skins:?}") };
    assert!(skins.iter().any(|s| matches!(s, Value::Resource(_))), "{skins:?}");
    assert_eq!(vm.class_default("People", "UncleDave", "NoSuchProperty").unwrap(), None);
    // Struct defaults decode into fields (Actor.DrawScale3D is a vector, Light has LightColor bytes).
    let ds = vm.class_default("Engine", "Actor", "DrawScale3D").unwrap();
    assert_eq!(ds, Some(Value::Vector([1.0, 1.0, 1.0])));
}

/// A host that records hook calls.
#[derive(Default)]
struct Recorder {
    moves: RefCell<Vec<(ObjId, [f32; 3])>>,
    sounds: RefCell<Vec<String>>,
    anims: RefCell<Vec<String>>,
    natives: RefCell<Vec<String>>,
}

impl Host for Recorder {
    fn move_actor(&self, vm: &mut Vm, actor: ObjId, to: [f32; 3], _sweep: bool) -> bool {
        self.moves.borrow_mut().push((actor, to));
        vm.set_prop(actor, "Location", Value::Vector(to));
        true
    }
    fn play_sound(&self, _vm: &mut Vm, _a: Option<ObjId>, req: &SoundRequest) {
        self.sounds.borrow_mut().push(req.sound.clone());
    }
    fn play_anim(&self, _vm: &mut Vm, _a: ObjId, req: &AnimRequest) -> bool {
        self.anims.borrow_mut().push(req.sequence.clone());
        true
    }
    fn native(&self, _vm: &mut Vm, call: &NativeCall, _args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        self.natives.borrow_mut().push(format!("{}.{}", call.class, call.name));
        (call.name == "OnlyAffectPawns").then_some(Ok(Value::Void))
    }
}

#[test]
fn host_hooks_receive_world_natives() {
    let mut vm = vm_or_skip!();
    let rec = Rc::new(Recorder::default());
    vm.set_host(rec.clone());
    vm.init_world(None).unwrap();
    // Lights are bStatic/bNoDelete; use a plain Info actor.
    let light = vm.find_class("Engine", "TestInfo").unwrap();
    let l = vm.spawn_actor(light, SpawnParams::default(), true).unwrap();
    let actor = vm.find_class("Engine", "Actor").unwrap();
    vm.call(actor, Some(l), "SetLocation", vec![Value::Vector([1.0, 2.0, 3.0])]).unwrap();
    assert_eq!(rec.moves.borrow().last(), Some(&(l, [1.0, 2.0, 3.0])));
    vm.call(actor, Some(l), "PlaySound", vec![Value::Resource("Sounds.Test".into())]).unwrap();
    assert_eq!(rec.sounds.borrow().as_slice(), ["Sounds.Test"]);
    vm.call(actor, Some(l), "PlayAnim", vec![Value::Name("Walk".into())]).unwrap();
    assert_eq!(rec.anims.borrow().as_slice(), ["Walk"]);
    // Unknown natives reach Host::native; ones it declines are counted and fail in Strict mode.
    let e = vm.call(actor, Some(l), "KWake", vec![]).unwrap_err().to_string();
    assert!(e.starts_with("native function KWake (index"), "{e}");
    assert!(rec.natives.borrow().iter().any(|n| n == "Actor.KWake"));
    assert_eq!(vm.missing_natives.get("Actor.KWake"), Some(&1));
    vm.missing_native_policy = MissingNativePolicy::Lenient;
    assert_eq!(vm.call(actor, Some(l), "KGetMass", vec![]).unwrap(), Value::Float(0.0));
    // Destroy clears references at the end of the frame.
    let ti = vm.find_class("Engine", "TestInfo").unwrap();
    let t = vm.spawn_actor(ti, SpawnParams { owner: Some(l), ..Default::default() }, true).unwrap();
    assert_eq!(vm.get_prop(t, "Owner"), Some(&Value::Object(Some(l))));
    vm.call(actor, Some(l), "Destroy", vec![]).unwrap();
    vm.tick_world(0.01);
    assert_eq!(vm.get_prop(t, "Owner"), Some(&Value::Object(None)));
}

#[test]
fn timers_and_lifespan() {
    let mut vm = vm_or_skip!();
    vm.init_world(None).unwrap();
    let light = vm.find_class("Engine", "TestInfo").unwrap();
    let l = vm.spawn_actor(light, SpawnParams::default(), true).unwrap();
    let actor = vm.find_class("Engine", "Actor").unwrap();
    vm.call(actor, Some(l), "SetTimer", vec![Value::Float(0.5), Value::Bool(true)]).unwrap();
    assert_eq!(vm.get_prop(l, "TimerRate"), Some(&Value::Float(0.5)));
    vm.set_prop(l, "LifeSpan", Value::Float(1.0));
    for _ in 0..9 {
        vm.tick_world(0.1);
    }
    assert!(!vm.objects[l].deleted);
    for _ in 0..3 {
        vm.tick_world(0.1);
    }
    assert!(vm.objects[l].deleted, "LifeSpan did not destroy the actor");
}

#[test]
fn headless_world_reports_missing_natives() {
    let Some(dir) = system_dir() else { return };
    let mut vm = Vm::new(dir).unwrap();
    vm.missing_native_policy = MissingNativePolicy::Lenient;
    vm.set_host(Rc::new(HeadlessHost::default()));
    let classes: Vec<String> = [
        "Engine.Light",
        "Engine.Trigger",
        "Engine.Mover",
        "Engine.AIController",
        "GameTypes.TriggerCounter",
        "GameTypes.checkpoint",
        "People.UncleDave",
        "Inventory.PistolPickup",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let rep = uscript::host::run_headless(&mut vm, Some("GameTypes.GameSinglePlayer"), &classes, 120, 1.0 / 30.0);
    println!("{rep}");
    assert!(rep.spawned.iter().all(|s| s.1.is_some()), "{rep}");
    assert!(rep.script_errors.is_empty(), "{rep}");
    // Auto states entered through SetInitialState.
    let state = |c: &str| rep.spawned.iter().find(|s| s.0 == c).map(|s| s.2.clone()).unwrap();
    assert_eq!(state("Engine.Trigger"), "NormalTrigger");
    assert_eq!(state("Inventory.PistolPickup"), "Pickup");
    assert_eq!(state("People.UncleDave"), "Living");
    // Missing natives are counted, not fatal.
    assert!(rep.missing_natives.iter().all(|(n, c)| n.contains('.') && *c > 0));
}

/// `class PlayerInput within PlayerController`: `new(self) InputClass` sets Outer, and the
/// PlayerInput's script reads/writes the controller's input variables through it.
#[test]
fn within_class_reaches_outer_variables() {
    let mut vm = vm_or_skip!();
    vm.missing_native_policy = MissingNativePolicy::Lenient;
    let pcc = vm.find_class("Engine", "PlayerController").unwrap();
    let pc = vm.new_object(pcc);
    vm.fire_event(pc, "InitInputSystem", vec![]);
    let pi = vm.get_prop(pc, "PlayerInput").and_then(|v| v.as_obj()).expect("PlayerInput created");
    assert_eq!(vm.get_prop(pi, "Outer").and_then(|v| v.as_obj()), Some(pc));
    vm.set_prop(pc, "aBaseY", Value::Float(100.0));
    vm.set_prop(pc, "DesiredFOV", Value::Float(90.0));
    let r = vm.call_event(pi, "PlayerInput", vec![Value::Float(0.016)]);
    assert!(r.is_ok(), "{r:?}");
    // aBaseY is folded into aForward and cleared.
    assert_eq!(vm.get_prop(pc, "aForward").map(|v| v.as_float()), Some(100.0));
    assert_eq!(vm.get_prop(pc, "aBaseY").map(|v| v.as_float()), Some(0.0));
}

/// Class defaults that point at package subobjects (P2GameInfoSingle.Days -> DayBase objects
/// defined in GameTypes.u) become script objects with their saved properties, so day logic and
/// the loading texture work.
#[test]
fn class_default_subobjects_are_instantiated() {
    let mut vm = vm_or_skip!();
    vm.missing_native_policy = MissingNativePolicy::Lenient;
    let c = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    let days = vm.default_value(c, "Days").cloned().expect("Days default");
    let Value::Array(days) = days else { panic!("Days is not an array: {days:?}") };
    assert!(days.len() >= 5, "{days:?}");
    let d0 = days[0].as_obj().unwrap_or_else(|| panic!("Days[0] is not an object: {:?}", days[0]));
    assert!(vm.class_is_a(vm.objects[d0].class, "DayBase"));
    // Saved properties applied (each day names its load texture).
    let lt = vm.get_prop(d0, "LoadTex").cloned();
    assert!(lt.as_ref().is_some_and(|v| !v.as_name().is_empty() && !v.is_none_name()), "LoadTex {lt:?}");
    // Same package object for every default that references it.
    let parent = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    if let Some(Value::Array(pd)) = vm.default_value(parent, "Days").cloned() {
        assert!(pd.iter().all(|v| v.as_obj().is_some()), "{pd:?}");
    }
    let class = vm.objects[d0].class;
    let tex = vm.call(class, Some(d0), "GetLoadTexture", vec![]).unwrap();
    assert!(matches!(tex, Value::Resource(_)), "GetLoadTexture -> {tex:?}");
}

/// A level reset (`truncate_objects`) must repoint every class default that holds a dropped
/// subobject, including instances a second load of the same export superseded in the cache
/// (AWPGameInfo and GameSinglePlayer both reference DayBase0 but were loaded separately).
#[test]
fn truncate_repoints_superseded_subobjects() {
    let mut vm = vm_or_skip!();
    vm.missing_native_policy = MissingNativePolicy::Lenient;
    let base = vm.objects.len();
    let a = vm.find_class("GameTypes", "AWPGameInfo").unwrap();
    let s = vm.find_class("GameTypes", "GameSinglePlayer").unwrap();
    // Simulate a level that dropped the cache entries and re-created subobjects (as a previous
    // truncate does), then filled the object table with unrelated objects.
    vm.truncate_objects(base);
    let obj_class = vm.find_class("Core", "Object").unwrap();
    for _ in 0..600 {
        vm.new_object(obj_class);
    }
    vm.truncate_objects(base);
    for c in [a, s] {
        let Some(Value::Array(days)) = vm.default_value(c, "Days").cloned() else { panic!("no Days") };
        for d in days {
            let o = d.as_obj().expect("day object");
            assert!(o < vm.objects.len(), "dangling Days entry {o}");
            assert!(vm.class_is_a(vm.objects[o].class, "DayBase"), "Days entry {o} is {}", vm.classes[vm.objects[o].class].name);
        }
    }
}

