//! Parses the world BSP of every map and validates node/surface/vertex references.

use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    let dir = std::env::args().nth(1).expect("maps dir");
    let (mut ok, mut bad, mut polys) = (0, 0, 0usize);
    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    let t0 = std::time::Instant::now();
    for p in entries {
        let Ok(pkg) = Package::open(&p) else { continue };
        let Some(i) = find_world_model(&pkg) else { continue };
        match read_model(&pkg, i) {
            Ok(m) => {
                ok += 1;
                polys += m.polygon_count();
            }
            Err(e) => {
                bad += 1;
                println!("FAILED {}: {e}", pkg.name);
            }
        }
    }
    println!("BSP SCAN: maps ok={ok} failed={bad} polygons={polys} in {:.2}s", t0.elapsed().as_secs_f64());
}
