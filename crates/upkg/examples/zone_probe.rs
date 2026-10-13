//! Prints the BSP zone table of a map (research aid for sky zone support).
//!
//! Usage: zone_probe <map path>...

use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    for path in std::env::args().skip(1) {
        let pkg = Package::open(&path).unwrap();
        let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
        let mut counts = vec![0usize; 256];
        for n in &m.nodes {
            if n.num_verts >= 3 {
                counts[n.zone[0] as usize] += 1;
                counts[n.zone[1] as usize] += 1;
            }
        }
        println!("{}: {} zones", pkg.name, m.zones.len());
        for (i, z) in m.zones.iter().enumerate() {
            println!("  zone {i:2} {:<20} {:<14} polys={}", pkg.ref_name(*z), pkg.class_of(*z), counts[i]);
        }
    }
}
