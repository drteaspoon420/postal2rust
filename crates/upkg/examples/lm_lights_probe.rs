//! Research aid: prints a map's BSP lightmap records (fields, texel origin/axes, light bitmaps).
//! Usage: lm_lights_probe <map path> [first] [count]

use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pkg = Package::open(&a[1]).unwrap();
    let first: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let count: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);
    let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
    let with = m.light_maps.iter().filter(|l| !l.lights.is_empty()).count();
    println!("{} lightmaps, {} with lights, textures {}", m.light_maps.len(), with, m.light_map_textures.len());
    for (i, l) in m.light_maps.iter().enumerate().skip(first).take(count) {
        println!("lm{i} fields {:?} origin {:?} x {:?} y {:?}", l.fields, l.origin, l.x_axis, l.y_axis);
        for b in &l.lights {
            let set: u32 = b.bits.iter().map(|x| x.count_ones()).sum();
            println!("   light {:?} {} bytes rows {} stride {} min {:?} max {:?} set {set}", b.actor, b.bits.len(), b.rows, b.stride, b.min, b.max);
        }
    }
}
