//! Counts BSP polygons whose first fan triangle is degenerate or faces against the node plane
//! (the old renderer flipped or culled these). Usage: fan_scan <maps dir>

use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    let dir = std::env::args().nth(1).expect("maps dir");
    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    let mut total = 0usize;
    for p in entries {
        let Ok(pkg) = Package::open(&p) else { continue };
        let Some(i) = find_world_model(&pkg) else { continue };
        let Ok(m) = read_model(&pkg, i) else { continue };
        let mut bad = 0;
        for n in &m.nodes {
            if n.num_verts < 3 {
                continue;
            }
            let v: Vec<[f32; 3]> = m.node_polygon(n).collect();
            let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            let first = cross(sub(v[1], v[0]), sub(v[2], v[0]));
            let mut sum = [0f32; 3];
            for k in 1..v.len() - 1 {
                let c = cross(sub(v[k], v[0]), sub(v[k + 1], v[0]));
                sum = [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2]];
            }
            let pl = [n.plane[0], n.plane[1], n.plane[2]];
            if (dot(first, pl) < 0.0) != (dot(sum, pl) < 0.0) {
                bad += 1;
            }
        }
        total += bad;
        if bad > 0 {
            println!("{:<28} {bad}", pkg.name);
        }
    }
    println!("FAN SCAN: {total} polygons were mis-oriented by the first-triangle test");
}
