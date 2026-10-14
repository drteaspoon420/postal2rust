//! Day/holiday filtering check (headless): boots a map the way a new game reaches it and lists the
//! actors still alive whose `Group` names a day or a holiday (`P2GameInfoSingle.PrepActorsByGroup`
//! should have destroyed the ones not needed today).
//! `group_probe [Map] [--day N]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::collections::BTreeMap;
use std::sync::Arc;
use uscript::vm::Value;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let game_class = get("--game").unwrap_or_else(|| "GameTypes.GameSinglePlayer".into());
    let mut game = Game::new(world, lv, Some(&game_class));
    game.begin_play("");
    for _ in 0..10 {
        game.tick(1.0 / 30.0);
    }
    let vm = &game.world.vm;
    let gi = vm.actors.iter().copied().find(|&x| !vm.objects[x].deleted && vm.class_is_a(vm.objects[x].class, "P2GameInfoSingle"));
    let gs = gi.and_then(|g| vm.get_prop(g, "TheGameState")).and_then(|v| v.as_obj());
    println!("gameinfo {:?} gamestate {:?} day {:?}", gi.map(|g| vm.object_name(g)), gs.map(|g| vm.object_name(g)), gs.and_then(|g| vm.get_prop(g, "CurrentDay")).map(|v| vm.value_string(v)));
    if let Some(g) = gi {
        if let Some(Value::Array(days)) = vm.get_prop(g, "Days") {
            for (i, d) in days.iter().enumerate() {
                if let Some(o) = d.as_obj() {
                    println!("day {i}: {} unique {:?} exclude {:?}", vm.object_name(o), vm.get_prop(o, "UniqueName").map(|v| vm.value_string(v)), vm.get_prop(o, "ExcludeDays").map(|v| vm.value_string(v)));
                }
            }
        }
        if let Some(Value::Array(h)) = vm.get_prop(g, "Holidays") {
            println!("holidays: {}", h.len());
        }
    }
    let mut groups: BTreeMap<String, (usize, usize, Vec<String>)> = BTreeMap::new();
    for o in 0..vm.objects.len() {
        if vm.classes[vm.objects[o].class].slot("Group").is_none() {
            continue;
        }
        let g = vm.get_prop(o, "Group").map(|v| v.as_name()).unwrap_or_default();
        if g.is_empty() || g.eq_ignore_ascii_case("none") {
            continue;
        }
        let e = groups.entry(g.to_ascii_uppercase()).or_default();
        if vm.objects[o].deleted {
            e.1 += 1;
        } else {
            e.0 += 1;
            if e.2.len() < 4 {
                e.2.push(vm.object_name(o));
            }
        }
    }
    let mut wrong = 0;
    for (g, (alive, dead, ex)) in &groups {
        let days: Vec<&str> = g.split(',').filter(|t| t.starts_with("DAY_")).collect();
        if *alive > 0 && !days.is_empty() && !days.contains(&"DAY_A") {
            wrong += alive;
            println!("WRONGDAY {g:40} alive {alive:4} destroyed {dead:4} {}", ex.join(" "));
        }
    }
    println!("wrong-day alive total: {wrong}");
    for (g, (alive, dead, ex)) in groups {
        println!("{g:40} alive {alive:4} destroyed {dead:4} {}", ex.join(" "));
    }
    if let Some(names) = get("--show") {
        for n in names.split(',') {
            let Some(o) = (0..vm.objects.len()).find(|&o| vm.object_name(o).eq_ignore_ascii_case(n)) else { continue };
            let p = |k: &str| vm.get_prop(o, k).map(|v| vm.value_string(v)).unwrap_or_default();
            println!("{n}: deleted {} bStatic {} bNoDelete {} bHidden {} Group {} StaticMesh {} Tag {}", vm.objects[o].deleted, p("bStatic"), p("bNoDelete"), p("bHidden"), p("Group"), p("StaticMesh"), p("Tag"));
        }
    }
}
