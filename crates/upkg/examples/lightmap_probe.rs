//! Research aid / validator for BSP render data: prints section, lightmap and lightmap texture
//! counts per map and checks node -> section / lightmap references.
//!
//! Usage: lightmap_probe <map path>...

use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    let mut ok = 0;
    let mut bad = 0;
    for path in std::env::args().skip(1) {
        let Ok(pkg) = Package::open(&path) else { continue };
        let Some(mi) = find_world_model(&pkg) else { continue };
        let m = read_model(&pkg, mi).unwrap();
        let e = &pkg.exports[mi];
        let nodes_with_lm = m.nodes.iter().filter(|n| n.light_map >= 0).count();
        let bad_sec = m.nodes.iter().filter(|n| n.num_verts >= 3 && (n.section < 0 || n.section as usize >= m.sections.len().max(1))).count();
        let bad_lm = m.nodes.iter().filter(|n| n.light_map >= 0 && n.light_map as usize >= m.light_maps.len()).count();
        // Do section vertices match node polygon points (in order, starting at first_vertex)?
        let mut pos_ok = 0;
        let mut pos_bad = 0;
        for n in m.nodes.iter().filter(|n| n.num_verts >= 3 && n.section >= 0) {
            let Some(s) = m.sections.get(n.section as usize) else { continue };
            let poly: Vec<[f32; 3]> = m.node_polygon(n).collect();
            let f = n.first_vertex as usize;
            if f + poly.len() > s.vertices.len() {
                pos_bad += 1;
                continue;
            }
            let same = poly.iter().zip(&s.vertices[f..]).all(|(a, b)| (0..3).all(|k| (a[k] - b.position[k]).abs() < 0.5));
            if same {
                pos_ok += 1
            } else {
                pos_bad += 1
            }
        }
        println!("   node->section vertex match ok={pos_ok} bad={pos_bad}");
        let verts: usize = m.sections.iter().map(|s| s.vertices.len()).sum();
        let tex: Vec<String> = m.light_map_textures.iter().take(3).map(|t| format!("{}x{} f{} mips={:?}", t.width, t.height, t.format, t.mips.iter().map(|x| x.len()).collect::<Vec<_>>())).collect();
        let good = !m.sections.is_empty() && bad_sec == 0 && bad_lm == 0;
        if good {
            ok += 1
        } else {
            bad += 1
        }
        println!(
            "{:<24} sections={} verts={} lightmaps={} lm_textures={} nodes_lm={} bad_sec={} bad_lm={} tail_left={} {:?}",
            pkg.name,
            m.sections.len(),
            verts,
            m.light_maps.len(),
            m.light_map_textures.len(),
            nodes_with_lm,
            bad_sec,
            bad_lm,
            e.offset + e.size - m.tail,
            tex
        );
        if let Some(lm) = m.light_maps.first() {
            println!("   lm0 tex={} off={:?} size={:?} m={:?}", lm.texture, lm.offset, lm.size, &lm.world_to_lightmap[..8]);
        }
        for (ti, t) in m.light_map_textures.iter().enumerate() {
            if let Some(d) = t.mips.first() {
                if let Ok(px) = upkg::texture::decode_raw(upkg::texture::TexFormat::from_byte(t.format), d, t.width, t.height) {
                    let n = px.len() / 4;
                    let avg: Vec<u64> = (0..4).map(|c| px.chunks_exact(4).map(|p| p[c] as u64).sum::<u64>() / n as u64).collect();
                    println!("   lmtex{ti} {}x{} avg rgba {:?}", t.width, t.height, avg);
                }
            }
        }
        for (i, s) in m.sections.iter().enumerate().take(6) {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for v in &s.vertices {
                for k in 0..2 {
                    lo[k] = lo[k].min(v.lightmap_uv[k]);
                    hi[k] = hi[k].max(v.lightmap_uv[k]);
                }
            }
            println!("   sec{i} verts={} lmtex={} flags={:#x} mat={} lmuv {:?}..{:?} uv0={:?}", s.vertices.len(), s.light_map_texture, s.poly_flags, pkg.ref_name(s.material), lo, hi, s.vertices.first().map(|v| v.uv));
        }
    }
    println!("LIGHTMAP SCAN ok={ok} bad={bad}");
}
