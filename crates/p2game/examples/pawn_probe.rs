//! Research probe: prints the render-related properties of the first few pawns of a map after
//! boot. Usage: pawn_probe [Map]
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;

fn main() {
    let map = std::env::args().nth(1).unwrap_or_else(|| "Police".into());
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().unwrap();
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, None);
    game.begin_play("");
    for _ in 0..5 {
        game.tick(1.0 / 30.0);
    }
    let vm = &game.world.vm;
    let mut n = 0;
    for &a in &vm.actors {
        if !vm.class_is_a(vm.objects[a].class, "Pawn") || vm.objects[a].deleted {
            continue;
        }
        n += 1;
        if n > 6 {
            break;
        }
        let p = |k: &str| vm.get_prop(a, k).map(|v| format!("{v:?}")).unwrap_or("-".into());
        println!(
            "{} {}: DrawType={} Mesh={} Skins={} bHidden={} Location={} anim={}",
            a,
            vm.classes[vm.objects[a].class].name,
            p("DrawType"),
            p("Mesh"),
            p("Skins"),
            p("bHidden"),
            p("Location"),
            game.host.anims.borrow().get(&a).map(|x| format!("{:?}", x.inst.get_anim_params(0))).unwrap_or("none".into())
        );
    }
}
