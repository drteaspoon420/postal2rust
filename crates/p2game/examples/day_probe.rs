//! Prints the single-player GameInfo's day URLs and each day's errands (from class defaults and
//! the Days subobjects). Usage: day_probe [GameClass]
use p2game::config::BootConfig;
use p2game::level::World;
use uscript::vm::Value;

fn main() {
    let gc = std::env::args().nth(1).unwrap_or_else(|| "GameTypes.GameSinglePlayer".into());
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let (pkg, name) = gc.split_once('.').unwrap();
    let vm = &mut world.vm;
    let c = vm.find_class(pkg, name).expect("class");
    for k in ["IntroURL", "StartFirstDayURL", "StartNextDayURL", "FinishedDayURL", "JailURL", "MainMenuURL"] {
        println!("{k} = {:?}", vm.default_value(c, k));
    }
    let o = vm.new_object(c);
    let days = match vm.get_prop(o, "Days").cloned() {
        Some(Value::Array(a)) => a,
        other => {
            println!("Days = {other:?}");
            return;
        }
    };
    for (i, d) in days.iter().enumerate() {
        let Some(d) = d.as_obj() else {
            println!("day {i}: {d:?}");
            continue;
        };
        let g = |n: &str| vm.get_prop(d, n).map(|v| vm.value_string(v)).unwrap_or_default();
        println!("day {i}: {} UniqueName={} StartDayURL={} FinishedDayURL={} JailURL={} LoadTex={}", vm.object_name(d), g("UniqueName"), g("StartDayURL"), g("FinishedDayURL"), g("JailURL"), g("LoadTex"));
        if let Some(Value::Array(es)) = vm.get_prop(d, "Errands").cloned() {
            for e in es.iter().filter_map(|e| e.as_obj()) {
                let ge = |n: &str| vm.get_prop(e, n).map(|v| vm.value_string(v)).unwrap_or_default();
                println!("    errand {} UniqueName={} bInitiallyActive={}", vm.object_name(e), ge("UniqueName"), ge("bInitiallyActive"));
            }
        }
    }
}
