//! Lists BSP polygons near a map's PlayerStart (or a given point) with their surface flags,
//! zones and materials, to find surfaces the renderer drops.
//! Usage: ceiling_probe <map.fuk> [x y z] [radius]

use upkg::Package;
use upkg::model::{find_world_model, read_model};
use upkg::props::begin_object;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pkg = Package::open(std::path::Path::new(&a[1])).expect("open");
    let mut at = [0f32; 3];
    if a.len() >= 5 {
        at = [a[2].parse().unwrap(), a[3].parse().unwrap(), a[4].parse().unwrap()];
    } else {
        for i in 0..pkg.exports.len() {
            if pkg.export_class(i) == "PlayerStart" {
                let (_, p) = begin_object(&pkg, i).unwrap();
                at = p.vector("Location").unwrap();
                println!("PlayerStart {:?} rot {:?}", at, p.rotator("Rotation"));
            }
        }
    }
    let radius: f32 = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(1200.0);
    let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
    for (i, z) in m.zones.iter().enumerate() {
        println!("zone {i}: {:?} {}", z, pkg.class_of(*z));
    }
    println!("sections {} lightmaps {} lmtex {}", m.sections.len(), m.light_maps.len(), m.light_map_textures.len());
    for (ni, n) in m.nodes.iter().enumerate() {
        if n.num_verts < 3 {
            continue;
        }
        let poly: Vec<[f32; 3]> = m.node_polygon(n).collect();
        let mut c = [0f32; 3];
        for p in &poly {
            for k in 0..3 {
                c[k] += p[k] / poly.len() as f32;
            }
        }
        let d = ((c[0] - at[0]).powi(2) + (c[1] - at[1]).powi(2)).sqrt();
        let zone_filter: Option<u8> = std::env::var("ZONE").ok().and_then(|z| z.parse().ok());
        if let Some(z) = zone_filter {
            if n.zone[0] != z && n.zone[1] != z {
                continue;
            }
        } else if d > radius || (c[2] - at[2]).abs() > 400.0 {
            continue;
        }
        let s = m.surfs[n.surf as usize];
        let mat = pkg.full_path(s.material);
        if std::env::var_os("VERTS").is_some() {
            let v: Vec<String> = poly.iter().map(|p| format!("({:.0},{:.0},{:.0})", p[0], p[1], p[2])).collect();
            println!("  verts {}", v.join(" "));
        }
        println!(
            "node {ni:5} c=({:7.0},{:7.0},{:6.0}) n=({:5.2},{:5.2},{:5.2}) nv={} flags={:08x} zone={:?} sect={} fv={} lm={} nflags={:02x} surf={} mat={}",
            c[0], c[1], c[2], n.plane[0], n.plane[1], n.plane[2], n.num_verts, s.flags, n.zone, n.section, n.first_vertex, n.light_map, n.flags, n.surf, mat
        );
    }
}
