//! Validates BSP collision data (leaf hulls, leaves, bounds, child links) on every map.
//! Run: cargo run --release -p upkg --example bsp_collision_scan -- <Maps dir>

use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    let dir = std::env::args().nth(1).expect("maps dir");
    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    let (mut ok, mut bad, mut nodes, mut hulls, mut planes, mut leaves) = (0, 0, 0usize, 0usize, 0usize, 0usize);
    let mut bbox_contains = (0usize, 0usize);
    for p in entries {
        let Ok(pkg) = Package::open(&p) else { continue };
        let Some(i) = find_world_model(&pkg) else { continue };
        let m = match read_model(&pkg, i) {
            Ok(m) => m,
            Err(e) => {
                bad += 1;
                println!("FAILED {}: {e}", pkg.name);
                continue;
            }
        };
        match m.validate_collision() {
            Ok(h) if h > 0 && !m.leaves.is_empty() => {
                ok += 1;
                nodes += m.nodes.len();
                hulls += h;
                leaves += m.leaves.len();
                // Sanity: each hull box should contain its node's polygon.
                for n in &m.nodes {
                    if let Some((pl, lo, hi)) = m.hull(n) {
                        planes += pl.len();
                        if n.num_verts >= 3 {
                            bbox_contains.1 += 1;
                            if m.node_polygon(n).all(|v| (0..3).all(|k| v[k] >= lo[k] - 1.0 && v[k] <= hi[k] + 1.0)) {
                                bbox_contains.0 += 1;
                            }
                        }
                    }
                }
            }
            Ok(h) => {
                bad += 1;
                println!("EMPTY {}: hulls={h} leaves={}", pkg.name, m.leaves.len());
            }
            Err(e) => {
                bad += 1;
                println!("BAD {}: {e}", pkg.name);
            }
        }
    }
    println!(
        "BSP COLLISION SCAN: maps ok={ok} bad={bad} nodes={nodes} hulls={hulls} hull_planes={planes} leaves={leaves} hull boxes containing their polygon={}/{}",
        bbox_contains.0, bbox_contains.1
    );
}
