//! Travel test: boots a map, requests `open <map>?Game=...` through the host's console command
//! path, and checks that the new level boots with the requested GameInfo.
//! Usage: travel_probe [FromMap] [ToURL]
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::host::Host;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let from = a.first().cloned().unwrap_or_else(|| "Startup".into());
    let to = a.get(1).cloned().unwrap_or_else(|| "intro?Game=GameTypes.GameSinglePlayer".into());
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&from).cloned().unwrap();
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, None);
    game.begin_play("");
    game.tick(1.0 / 30.0);
    println!("before: {} ({}), {} live actors, objects {}", game.level.map.name, game.game_class, game.live_actors(), game.world.vm.objects.len());
    let host = game.host.clone();
    host.console_command(&mut game.world.vm, None, &format!("open {to}"));
    let (map, opts) = game.take_travel().expect("travel was not recorded");
    assert!(game.travel(&map, &opts), "travel failed");
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
    }
    println!(
        "after: {} ({}), {} live actors, objects {}, player {:?}, script errors {}",
        game.level.map.name,
        game.game_class,
        game.live_actors(),
        game.world.vm.objects.len(),
        game.player_controller,
        game.world.vm.script_errors.values().sum::<u64>()
    );
}
