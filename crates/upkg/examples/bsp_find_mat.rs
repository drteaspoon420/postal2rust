//! Lists a map's BSP surfaces whose material path contains a pattern, with the polygon centre
//! (world units) and normal, to find where a sign or poster sits.
//! Usage: bsp_find_mat <map> <pattern>
use upkg::Package;
use upkg::model::{find_world_model, read_model};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let pat = a[1].to_ascii_lowercase();
    let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
    for n in &m.nodes {
        let Some(s) = m.surfs.get(n.surf as usize) else { continue };
        let name = pkg.full_path(s.material);
        if !name.to_ascii_lowercase().contains(&pat) {
            continue;
        }
        let pts: Vec<[f32; 3]> = m.node_polygon(n).collect();
        if pts.is_empty() {
            continue;
        }
        let c = pts.iter().fold([0f32; 3], |acc, p| [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]]).map(|v| v / pts.len() as f32);
        let nrm = m.vectors.get(s.normal as usize).copied().unwrap_or_default();
        println!("{name} centre ({:.0},{:.0},{:.0}) normal ({:.2},{:.2},{:.2}) flags {:#x} verts {}", c[0], c[1], c[2], nrm[0], nrm[1], nrm[2], s.flags, pts.len());
    }
}
