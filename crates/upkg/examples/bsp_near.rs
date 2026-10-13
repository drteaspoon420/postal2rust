//! Research aid: lists BSP polygons near a point with material, poly flags and lightmap index.
//! Usage: bsp_near <map> x y z radius

use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pkg = Package::open(&a[1]).unwrap();
    let p: Vec<f32> = a[2..5].iter().map(|s| s.parse().unwrap()).collect();
    let r: f32 = a[5].parse().unwrap();
    let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
    for (i, n) in m.nodes.iter().enumerate() {
        if n.num_verts < 3 {
            continue;
        }
        let poly: Vec<[f32; 3]> = m.node_polygon(n).collect();
        let c = poly.iter().fold([0.0f32; 3], |s, q| [s[0] + q[0], s[1] + q[1], s[2] + q[2]]);
        let c = c.map(|v| v / poly.len() as f32);
        let d = ((c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2) + (c[2] - p[2]).powi(2)).sqrt();
        if d > r {
            continue;
        }
        let s = m.surfs[n.surf as usize];
        println!(
            "node {i} centre {:?} normal {:?} mat {} flags {:#x} lightmap {} section {} zone {:?}",
            c.map(|v| v.round()),
            &n.plane[..3],
            pkg.ref_name(s.material),
            s.flags,
            n.light_map,
            n.section,
            n.zone
        );
    }
}
