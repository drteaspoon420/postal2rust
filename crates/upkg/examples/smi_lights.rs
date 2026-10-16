//! Compares an actor's saved StaticMeshInstance colours with its per-light visibility bits: which
//! lights reach how many vertices, to tell "baked dark because shadowed" from "colours not baked".
//! Usage: smi_lights <map path> <ActorName>
use upkg::{ObjRef, Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    for i in 0..pkg.exports.len() {
        if !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(&a[1]) {
            continue;
        }
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let Some(ObjRef::Export(smi)) = p.object("StaticMeshInstance") else { continue };
        let colors = upkg::staticmesh::read_instance_colors(&pkg, smi).unwrap_or_default();
        let lights = upkg::staticmesh::read_instance_lights(&pkg, smi).unwrap_or_default();
        println!("{} {} colours, {} lights", a[1], colors.len(), lights.len());
        for (r, bits) in &lights {
            let lit = (0..colors.len()).filter(|&v| upkg::terrain::SectorLighting::visible(bits, v)).count();
            let (cls, extra) = match r {
                ObjRef::Export(e) => {
                    let props = begin_object(&pkg, *e).map(|(_, q)| q).ok();
                    let get = |k: &str| props.as_ref().and_then(|q| q.get(k)).map(|v| format!("{v:?}")).unwrap_or_default();
                    (pkg.export_class(*e).to_string(), format!("Loc {} Bright {} Radius {} Type {}", get("Location"), get("LightBrightness"), get("LightRadius"), get("LightEffect")))
                }
                _ => (String::new(), String::new()),
            };
            let lum = |v: usize| colors.get(v).map(|c| (c[0] as f32 + c[1] as f32 + c[2] as f32) / 3.0).unwrap_or(0.0);
            let (mut a, mut na, mut b, mut nb) = (0.0, 0, 0.0, 0);
            for v in 0..colors.len() {
                if upkg::terrain::SectorLighting::visible(bits, v) {
                    a += lum(v);
                    na += 1;
                } else {
                    b += lum(v);
                    nb += 1;
                }
            }
            println!("  {} ({cls}) reaches {lit}/{} verts, mean light reached {:.1} vs not {:.1}  {extra}", pkg.ref_name(*r), colors.len(), a / na.max(1) as f32, b / nb.max(1) as f32);
        }
    }
}
