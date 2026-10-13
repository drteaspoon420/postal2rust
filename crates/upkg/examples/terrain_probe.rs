//! Prints per-layer details of TerrainInfo actors (research aid for terrain.rs).
//!
//! Usage: terrain_probe <game dir> <map name>

use std::sync::Arc;
use upkg::library::Library;
use upkg::terrain::{find_terrains, read_alpha_map, read_terrain};
use upkg::texture::read_texture;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let lib = Library::new(&a[0]).unwrap();
    let pkg = Arc::new(upkg::Package::open(lib.map_path(&a[1]).unwrap()).unwrap());
    lib.insert(pkg.clone());
    for i in find_terrains(&pkg) {
        let t = read_terrain(&lib, &pkg, i).unwrap();
        let vis = t.quad_visibility.as_ref().map(|v| v.iter().map(|w| w.count_ones()).sum::<u32>());
        let turn = t.edge_turn.as_ref().map(|v| v.iter().map(|w| w.count_ones()).sum::<u32>());
        let (lo, hi) = (t.heights.iter().min().unwrap(), t.heights.iter().max().unwrap());
        println!("{} {}x{} loc={:?} scale={:?} heights {lo}..{hi} vis_bits={vis:?} turn_bits={turn:?}", t.name, t.width, t.height, t.location, t.scale);
        for l in &t.layers {
            let tex = lib.resolve(&pkg, l.texture).and_then(|m| lib.material_texture(&m)).and_then(|m| m.texture);
            let tex_desc = tex.map(|o| {
                let t = read_texture(&o.pkg, o.export).unwrap();
                format!("{} {:?} {}x{}", o.name(), t.format, t.width, t.height)
            });
            let alpha = lib.resolve(&pkg, l.alpha_map).map(|o| {
                let t = read_texture(&o.pkg, o.export).unwrap();
                let s = read_alpha_map(&o).map(|(w, h, v)| {
                    let avg = v.iter().map(|&x| x as u64).sum::<u64>() / v.len().max(1) as u64;
                    format!("{w}x{h} avg={avg}")
                });
                format!("{} {:?} {:?}", o.name(), t.format, s)
            });
            println!("  layer tex={tex_desc:?} alpha={alpha:?} u={} v={} pan=({},{}) axis={} rot={}", l.u_scale, l.v_scale, l.u_pan, l.v_pan, l.map_axis, l.rotation);
        }
        for d in &t.deco_layers {
            let desc = |r: Option<upkg::ObjRef>| {
                r.and_then(|r| lib.resolve(&pkg, r)).map(|o| {
                    let tx = read_texture(&o.pkg, o.export).unwrap();
                    let stats = tx.decode_rgba(0).map(|(w, h, px)| {
                        let n = (w * h) as u64;
                        let avg = |c: usize| px.chunks_exact(4).map(|p| p[c] as u64).sum::<u64>() / n;
                        format!("{w}x{h} avg rgba=({},{},{},{})", avg(0), avg(1), avg(2), avg(3))
                    });
                    format!("{} {:?} {:?}", o.name(), tx.format, stats)
                })
            };
            println!(
                "  deco mesh={:?} show={} max_per_quad={} density={:?} scale={:?} fade={:?} seed={} align={} yaw={}",
                d.static_mesh.map(|m| pkg.full_path(m)),
                d.show_on_terrain,
                d.max_per_quad,
                d.density_multiplier,
                d.scale_multiplier,
                d.fadeout_radius,
                d.seed,
                d.align_to_terrain,
                d.random_yaw
            );
            println!("    density_map={:?}\n    scale_map={:?}\n    color_map={:?}", desc(d.density_map), desc(d.scale_map), desc(d.color_map));
        }
    }
}
