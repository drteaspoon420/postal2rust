//! Static mesh sections: material, triangle count, bounds and UV range per section.
//! Usage: sm_sections <package path> <mesh name substring>
use upkg::Package;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let want = a[1].to_ascii_lowercase();
    for i in 0..pkg.exports.len() {
        if pkg.export_class(i) != "StaticMesh" || !pkg.name(pkg.exports[i].name).to_ascii_lowercase().contains(&want) {
            continue;
        }
        let Ok(m) = upkg::staticmesh::read_static_mesh(&pkg, i) else { continue };
        println!("== {} verts {} bounds {:?}..{:?} props {:?}", m.name, m.positions.len(), m.bounds_min, m.bounds_max, m.props.0.iter().map(|p| p.name.clone()).collect::<Vec<_>>());
        for (s, sec) in m.sections.iter().enumerate() {
            let idx = m.section_indices(s);
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            let (mut ulo, mut uhi) = ([f32::MAX; 2], [f32::MIN; 2]);
            let mut nz = 0.0f32;
            for &v in idx {
                let p = m.positions[v as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
                if let Some(uv) = m.uvs.get(v as usize) {
                    for k in 0..2 {
                        ulo[k] = ulo[k].min(uv[k]);
                        uhi[k] = uhi[k].max(uv[k]);
                    }
                }
                nz += m.normals.get(v as usize).map(|n| n[2]).unwrap_or(0.0);
            }
            let mat = m.materials.get(s).map(|r| pkg.full_path(*r)).unwrap_or_default();
            println!(
                "  s{s} tris {} mat {mat} box {:.0?}..{:.0?} uv {:.2?}..{:.2?} mean nz {:.2}",
                sec.num_triangles,
                lo,
                hi,
                ulo,
                uhi,
                nz / idx.len().max(1) as f32
            );
        }
    }
}
