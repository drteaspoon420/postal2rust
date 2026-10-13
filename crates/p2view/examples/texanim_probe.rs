//! Lists animated-texture materials (AnimNext frame chains) a map's scene build resolves.
//! `texanim_probe [Map] [GameDir]` (defaults: suburbs-3, the Steam install).
use p2view::scene::{build_scene, Options};
use std::sync::Arc;
use upkg::library::Library;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().cloned().unwrap_or_else(|| "suburbs-3".into());
    let game = a.get(1).cloned().unwrap_or_else(|| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let lib = Library::new(std::path::Path::new(&game)).expect("game install");
    let path = lib.map_path(&map).expect("map");
    let pkg = Arc::new(upkg::Package::open(&path).expect("open map"));
    let data = build_scene(&lib, &pkg, &Options { gpu_bc: false, projectors: false }).expect("scene");
    let mut n = 0;
    for (i, m) in data.materials.iter().enumerate() {
        if m.frames.len() > 1 {
            n += 1;
            println!("material {i}: {} frames at {} fps", m.frames.len(), m.frame_rate);
        }
    }
    println!("TEXANIM {map}: {n} animated materials of {}", data.materials.len());
}
