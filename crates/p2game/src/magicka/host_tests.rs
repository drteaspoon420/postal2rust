//! Cross-engine checks against original Postal classes and Magicka content.
use super::*;
use p2game::{boot::Game, config::BootConfig, level::World};
use std::{path::PathBuf, sync::Arc};
use uscript::host::SpawnParams;

fn fixture() -> (Game, WizardWorld, ObjId) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../postal2");
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::ragdoll::install(root.clone()).expect("native ragdoll host");
    let mut world = World::new(BootConfig::load(&root.join("System")).unwrap()).unwrap();
    let path = world.files.path("suburbs-1").unwrap();
    let level = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, level, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("?SetDay=1");
    // The native frame hooks link the local viewport used by GetKarmaSkeleton.
    game.tick(1./60.);
    game.tick(1./60.);
    let player = game.world.vm.get_prop(game.player_controller.unwrap(), "Pawn").and_then(Value::as_obj).unwrap();
    let at = game.world.vm.actor_loc(player);
    let floor = at[2] - 60.;
    let ground = p2magicka::postal_level([
        [[at[0]-2000.,at[1]-2000.,floor],[at[0]+2000.,at[1]-2000.,floor],[at[0]+2000.,at[1]+2000.,floor]],
        [[at[0]-2000.,at[1]-2000.,floor],[at[0]+2000.,at[1]+2000.,floor],[at[0]-2000.,at[1]+2000.,floor]],
    ]);
    let content = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../rust-bevy-magicka/target/Magicka/Content");
    let mut wizard = WizardWorld::new(&content, ground, Loadout::default(), at, [1.,0.,0.]).unwrap();
    for _ in 0..60 { wizard.step(1./60.).unwrap(); }
    (game, wizard, player)
}

#[test]
fn vortex_moves_and_consumes_native_physics_objects() {
    let (mut game, mut wizard, _) = fixture();
    let owner = wizard.avatar().handle;
    assert!(wizard.session.world.vortex_from(owner));
    let v = &mut wizard.session.world.vortices[0].vortex;
    v.begin_update(1./60.).unwrap();
    let center = v.center;
    let vm = &mut game.world.vm;
    let class = vm.find_class("Engine", "KActor").unwrap();
    let at = p2magicka::to_postal([center[0]+1.5,center[1],center[2]]);
    let actor = vm.spawn_actor(class, SpawnParams { location: at, ..Default::default() }, false).unwrap();
    vm.set_prop(actor, "bStatic", Value::Bool(false));
    vm.set_prop(actor, "bNoDelete", Value::Bool(false));
    vm.set_prop(actor, "CollisionRadius", Value::Float(10.));
    vm.set_prop(actor, "CollisionHeight", Value::Float(10.));
    vortex::props(vm, &mut wizard);
    let velocity = vm.get_prop(actor, "Velocity").unwrap().as_vec();
    assert!(velocity[0] < -100., "the native prop must accelerate into the vortex: {velocity:?}");
    vm.set_prop(actor, "Location", Value::Vector(p2magicka::to_postal(center)));
    let mass = wizard.session.world.vortices[0].vortex.target_mass;
    vortex::props(vm, &mut wizard);
    assert!(vm.objects[actor].deleted, "the core must consume the native actor");
    assert!(wizard.session.world.vortices[0].vortex.target_mass > mass, "consumed mass must feed the vortex");
}

#[test]
fn fire_spell_explodes_a_native_car_through_its_damage_script() {
    let (mut game, mut wizard, player) = fixture();
    let vm = &mut game.world.vm;
    let at = vm.actor_loc(player);
    let class = vm.find_class("FX", "CarExplodable").unwrap();
    let car = vm.spawn_actor(class, SpawnParams { location: [at[0]+280.,at[1],at[2]], ..Default::default() }, true).unwrap();
    // A damaged car must still obey its native BurnedDamage/BlowThisUp path.
    vm.set_prop(car, "Health", Value::Float(10.));
    let mut proxies = HashMap::new();
    let aim = [at[0]+500.,at[1],at[2]];
    for tick in 0..360 {
        props::before(vm, &mut wizard, &mut proxies);
        let mut input = PlayerInput::default();
        input.elements[7] = tick == 0;
        input.cast_down = tick >= 15;
        wizard.input(input, aim, 1./60.);
        wizard.set_staff_orb(at, [1.,0.,0.]);
        wizard.step(1./60.).unwrap();
        props::after(vm, player, &mut wizard, &mut proxies, p2magicka::magicka_sim::spells::Elements::FIRE.0);
        if vm.get_prop(car, "bBlownUp").is_some_and(Value::as_bool) { return; }
    }
    panic!("native car must explode from direct fire spell damage: hp={:?} state={}", vm.get_prop(car, "Health"), vm.state_name(car));
}

fn mirror(handle: u16) -> Mirror {
    Mirror { handle, health: 100., remainder: 0., burning: false, frozen_speed: None,
        gibbed: false, ragdoll: None, statuses: 0, shocked: 0., charm: None }
}
#[test]
fn native_hostility_and_charm_are_reversible() {
    let (mut game, mut wizard, player) = fixture();
    let vm = &mut game.world.vm;
    let class = vm.find_class("BasePeople", "Bystanders").unwrap();
    let controller_class = vm.find_class("Postal2Game", "PersonController").unwrap();
    let mut mirrors = HashMap::new();
    let at = vm.actor_loc(player);
    let mut actors = Vec::new();
    for i in 0..2 {
        let actor = vm.spawn_actor(class, SpawnParams { location: [at[0]+150.,at[1]+i as f32*100.,at[2]], ..Default::default() }, false).unwrap();
        let c = vm.new_object(controller_class);
        vm.set_prop(actor, "Controller", Value::Object(Some(c)));
        vm.set_prop(c, "Pawn", Value::Object(Some(actor)));
        vm.set_prop(c, "MyPawn", Value::Object(Some(actor)));
        vm.set_prop(actor, "Health", Value::Int(100));
        let h = wizard.attach_pawn(vm.actor_loc(actor), 1000.,24.,40.).unwrap();
        mirrors.insert(actor, mirror(h));
        actors.push(actor);
    }
    let neutral = actors[0];
    let enemy = actors[1];
    vm.set_prop(enemy, "bPlayerIsEnemy", Value::Bool(true));
    vm.set_prop(enemy, "bPlayerIsFriend", Value::Bool(false));
    allegiance::sync(vm, player, &mut mirrors, &mut wizard);
    let faction = |w: &WizardWorld, h| { let e = w.session.world.entities.iter().find(|e| e.handle == h).unwrap(); w.session.world.faction(e) };
    assert_eq!(faction(&wizard, mirrors[&neutral].handle), wizard.avatar().faction);
    assert_eq!(faction(&wizard, mirrors[&enemy].handle) & wizard.avatar().faction, 0);
    let handle = mirrors[&enemy].handle;
    let owner = wizard.avatar().handle;
    assert!(wizard.session.world.charm_character(handle, owner, 10.));
    allegiance::sync(vm, player, &mut mirrors, &mut wizard);
    assert!(vm.get_prop(enemy, "bPlayerIsFriend").unwrap().as_bool());
    assert!(!vm.get_prop(enemy, "bPlayerIsEnemy").unwrap().as_bool());
    assert_ne!(vm.get_prop(vm.get_prop(enemy,"Controller").and_then(Value::as_obj).unwrap(),"Attacker").and_then(Value::as_obj), Some(player));
    wizard.session.world.npcs.get_mut(&handle).unwrap().character.charm = None;
    allegiance::sync(vm, player, &mut mirrors, &mut wizard);
    assert!(!vm.get_prop(enemy, "bPlayerIsFriend").unwrap().as_bool());
    assert!(vm.get_prop(enemy, "bPlayerIsEnemy").unwrap().as_bool());
}

#[test]
fn strong_knockback_ragdolls_a_living_postal_pawn() {
    let (mut game, _, player) = fixture();
    let vm = &mut game.world.vm;
    let pc = game.player_controller.unwrap();
    let pc_class = vm.objects[pc].class;
    vm.call(pc_class, Some(pc), "InitRagdollSkels", vec![Value::Int(8)]).unwrap();
    let class = vm.find_class("BasePeople", "Bystanders").unwrap();
    let at = vm.actor_loc(player);
    let actor = vm.spawn_actor(class, SpawnParams { location: [at[0]+150.,at[1],at[2]], ..Default::default() }, true).unwrap();
    assert!(!start_ragdoll(vm, actor, Some(Vec3::new(3.,0.,0.)), false), "light pushes stay upright");
    assert!(start_ragdoll(vm, actor, Some(Vec3::new(12.,4.,0.)), false), "a fast launch must ragdoll without a knockdown animation: player={:?} kparams={:?} logs={:?}",
        vm.get_prop(game.player_controller.unwrap(), "Player"), vm.get_prop(actor,"KParams"), vm.log.iter().rev().take(8).collect::<Vec<_>>());
    assert_eq!(vm.get_prop(actor,"Physics").map(Value::as_int),Some(p2game::ragdoll::PHYS_KARMA_RAGDOLL as i32));
    assert!(vm.get_prop(actor,"Health").unwrap().as_float() > 0., "ragdoll must not kill the pawn");
}
