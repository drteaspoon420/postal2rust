//! Lists static mesh actors whose baked vertex lighting is very dark, with their lighting props.
//!
//! Usage: smi_probe <map path>

use upkg::{ObjRef, Package, Value, begin_object};

fn main() {
    let path = std::env::args().nth(1).expect("map");
    let pkg = Package::open(&path).unwrap();
    let mut shown = 0;
    let mut dark = 0;
    let mut total = 0;
    for i in 0..pkg.exports.len() {
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let Some(ObjRef::Export(smi)) = p.object("StaticMeshInstance") else { continue };
        let Ok(c) = upkg::staticmesh::read_instance_colors(&pkg, smi) else { continue };
        if c.is_empty() {
            continue;
        }
        total += 1;
        let avg = c.iter().map(|x| x[0] as u32 + x[1] as u32 + x[2] as u32).sum::<u32>() as f32 / (3 * c.len()) as f32;
        let amax = c.iter().map(|x| x[3]).max().unwrap();
        if total <= 3 {
            let hist: Vec<String> = c.iter().take(12).map(|x| format!("{:?}", x)).collect();
            println!("sample {} {:?}", pkg.ref_name(ObjRef::Export(i)), hist);
        }
        if avg < 8.0 {
            dark += 1;
            if shown < 25 {
                shown += 1;
                let flags: Vec<String> = p
                    .0
                    .iter()
                    .filter(|q| ["bUnlit", "AmbientGlow", "bSpecialLit", "ScaleGlow", "bShadowCast", "Skins", "LightingTag", "bStatic", "bAcceptsProjectors"].contains(&q.name.as_str()))
                    .map(|q| format!("{}={:?}", q.name, match &q.value { Value::Array { raw } => format!("arr{}", raw.len()), v => format!("{v:?}") }))
                    .collect();
                println!("{} mesh={} n={} avg={avg:.1} amax={amax} {:?}", pkg.ref_name(ObjRef::Export(i)), pkg.full_path(p.object("StaticMesh").unwrap()), c.len(), flags);
            }
        }
    }
    println!("dark={dark} of {total}");
}
