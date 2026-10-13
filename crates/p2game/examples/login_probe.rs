//! Research probe: boots a map and inspects how the GameInfo's Login resolves the player
//! controller. Usage: login_probe [Map]
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::vm::Value;

fn main() {
    let map = std::env::args().nth(1).unwrap_or_else(|| "Startup".into());
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().unwrap();
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, None);
    game.begin_play("");
    let vm = &mut game.world.vm;
    let g = game.game_info.unwrap();
    let gc = vm.objects[g].class;
    println!("game class {}", vm.classes[gc].name);
    println!("PlayerControllerClass = {:?}", vm.get_prop(g, "PlayerControllerClass"));
    println!("PlayerControllerClassName = {:?}", vm.get_prop(g, "PlayerControllerClassName"));
    let mut c = Some(gc);
    while let Some(k) = c {
        let has = vm.find_function(k, "Login").map(|f| vm.pkgs[f.pkg as usize].pkg.full_path(upkg::ObjRef::Export(f.export as usize)));
        println!("  {}.{}  Login -> {:?}", vm.classes[k].package, vm.classes[k].name, has);
        c = vm.classes[k].super_;
    }
    let pcc = match vm.get_prop(g, "PlayerControllerClass").cloned() {
        Some(Value::Class(Some(c))) => Some(c),
        _ => vm.find_class("GameTypes", "AWDudePlayer").ok(),
    };
    if let Some(c) = pcc {
        println!("spawn {} abstract={}", vm.classes[c].name, vm.classes[c].is_abstract());
        vm.log.clear();
        let a = vm.spawn_actor(c, uscript::host::SpawnParams::default(), true);
        println!("spawn_actor -> {a:?}");
        for l in vm.log.iter().take(30) {
            println!("  log {l}");
        }
        let mut errs: Vec<_> = vm.script_errors.iter().collect();
        errs.sort();
        for e in errs {
            println!("  err {e:?}");
        }
    }
}
