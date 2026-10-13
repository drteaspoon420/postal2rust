//! FOV trace: boots a map and prints the local PlayerController's FOV state and the console
//! commands `PlayerController.GetResolution` relies on, to find where a NaN FOVAngle comes from.
//! `fov_probe [Map]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::vm::Value;

fn main() {
    let map = std::env::args().nth(1).unwrap_or_else(|| "suburbs-3".into());
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    if std::env::var_os("NO_VIEWPORT").is_none() {
        p2game::viewport::install();
    }
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("");
    for f in 0..3 {
        let vm = &mut game.world.vm;
        if let Some(pc) = p2game::ai::local_player_controller(vm) {
            let g = |n: &str| vm.get_prop(pc, n).cloned();
            println!("frame {f}: FOVAngle {:?} DesiredFOV {:?} DefaultFOV {:?} HackedFOV {:?}", g("FOVAngle"), g("DesiredFOV"), g("DefaultFOV"), g("HackedFOV"));
            if f == 0 {
                for cmd in [
                    "GetCurrentRes",
                    "GetFullScreen",
                    "get ini:Engine.Engine.ViewportManager StartupFullscreen",
                    "get ini:Engine.Engine.ViewportManager WindowedViewportX",
                    "get ini:Engine.Engine.ViewportManager WindowedViewportY",
                    "get ini:Engine.Engine.ViewportManager FullscreenViewportX",
                    "get ini:Engine.Engine.ViewportManager FullscreenViewportY",
                ] {
                    let cls = vm.objects[pc].class;
                    let r = vm.call(cls, Some(pc), "ConsoleCommand", vec![Value::Str(cmd.into()), Value::Bool(false)]);
                    println!("  ConsoleCommand({cmd:?}) = {r:?}");
                }
                let cls = vm.objects[pc].class;
                println!("  GetResolution = {:?}", vm.call(cls, Some(pc), "GetResolution", vec![]));
                println!("  ApplyAspectRatio(90) = {:?}", vm.call(cls, Some(pc), "ApplyAspectRatio", vec![Value::Float(90.0)]));
                println!("  CalculateFOV(90) = {:?}", vm.call(cls, Some(pc), "CalculateFOV", vec![Value::Float(90.0)]));
            }
        }
        game.tick(1.0 / 30.0);
        game.drain_log(false);
    }
}
