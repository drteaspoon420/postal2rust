//! `usworld [--game Pkg.Class] [--frames N] [--dt S] [Pkg.Class ...]`
//!
//! Headless world: creates a LevelInfo and a GameInfo, spawns the listed actor classes (default:
//! a sample of Engine and GameTypes classes), runs PreBeginPlay/BeginPlay/PostBeginPlay and the
//! initial state, then ticks the world for N frames. Unimplemented natives do not stop the run
//! (lenient mode); the report lists them with call counts, plus script errors and final states.

use std::rc::Rc;
use uscript::host::HeadlessHost;
use uscript::vm::{MissingNativePolicy, Vm};

const DEFAULT_CLASSES: &[&str] = &[
    "Engine.Light",
    "Engine.Trigger",
    "Engine.PathNode",
    "Engine.PlayerStart",
    "Engine.Emitter",
    "Engine.AIController",
    "Engine.Pawn",
    "Engine.Pickup",
    "Engine.Mover",
    "GameTypes.TriggerCounter",
    "GameTypes.DialogLoader",
    "GameTypes.AWDynaFOV",
    "GameTypes.checkpoint",
    "GameTypes.AWTrigger",
    "GameTypes.ProjSpawner",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let mut system = format!(r"{root}\System");
    let (mut frames, mut dt, mut game) = (60usize, 1.0f32 / 30.0, None::<String>);
    let mut classes = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                frames = args[i + 1].parse().expect("frames");
                i += 1;
            }
            "--dt" => {
                dt = args[i + 1].parse().expect("dt");
                i += 1;
            }
            "--game" => {
                game = Some(args[i + 1].clone());
                i += 1;
            }
            "--system" => {
                system = args[i + 1].clone();
                i += 1;
            }
            "--log" => {}
            c => classes.push(c.to_string()),
        }
        i += 1;
    }
    if classes.is_empty() {
        classes = DEFAULT_CLASSES.iter().map(|s| s.to_string()).collect();
    }
    let mut vm = Vm::new(&system).expect("vm");
    vm.missing_native_policy = MissingNativePolicy::Lenient;
    let host = Rc::new(HeadlessHost::default());
    vm.set_host(host.clone());
    let report = uscript::host::run_headless(&mut vm, game.as_deref(), &classes, frames, dt);
    println!("{report}");
    if args.iter().any(|a| a == "--log") {
        for l in &vm.log {
            println!("LOG {l}");
        }
    }
    println!("sounds played: {}, animations started: {}", host.sounds.borrow().len(), host.anims.borrow().len());
}
