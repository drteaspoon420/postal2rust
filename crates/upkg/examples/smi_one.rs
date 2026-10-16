//! Prints one static mesh actor's baked vertex lighting summary (StaticMeshInstance colours):
//! average RGB, alpha range, and the darkest/brightest vertices, to compare a sign that renders
//! dark with its baked data. Usage: smi_one <map path> <ActorName>
use upkg::{ObjRef, Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    for i in 0..pkg.exports.len() {
        if !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(&a[1]) {
            continue;
        }
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        println!("{} mesh {:?}", a[1], p.object("StaticMesh").map(|m| pkg.full_path(m)));
        let Some(ObjRef::Export(smi)) = p.object("StaticMeshInstance") else {
            println!("  no StaticMeshInstance");
            continue;
        };
        let c = upkg::staticmesh::read_instance_colors(&pkg, smi).unwrap_or_default();
        let n = c.len().max(1) as f32;
        let avg = [0, 1, 2, 3].map(|k| c.iter().map(|x| x[k] as f32).sum::<f32>() / n);
        let lum = |x: &[u8; 4]| x[0] as u32 + x[1] as u32 + x[2] as u32;
        println!("  {} colours, avg rgba {:?}, alpha {}..{}", c.len(), avg.map(|v| v as u32), c.iter().map(|x| x[3]).min().unwrap_or(0), c.iter().map(|x| x[3]).max().unwrap_or(0));
        let mut s = c.clone();
        s.sort_by_key(lum);
        println!("  darkest {:?}", &s[..s.len().min(6)]);
        println!("  brightest {:?}", &s[s.len().saturating_sub(6)..]);
    }
}
