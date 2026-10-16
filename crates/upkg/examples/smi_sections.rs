//! Per-section baked light of one static mesh actor: for every mesh section, the mean
//! StaticMeshInstance colour of its vertices and the mean world-space normal, to see whether a sign's
//! readable face is lit or shadowed in the map's own lighting data.
//! Usage: smi_sections <map path> <ActorName>
use upkg::library::Library;
use upkg::{ObjRef, Package, Value, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let path = std::path::Path::new(&a[0]);
    let pkg = std::sync::Arc::new(Package::open(path).unwrap());
    let lib = Library::new(path.parent().unwrap().parent().unwrap()).unwrap();
    lib.insert(pkg.clone());
    for i in 0..pkg.exports.len() {
        if !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(&a[1]) {
            continue;
        }
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let Some(smr) = p.object("StaticMesh") else { continue };
        let Some(sm_obj) = lib.resolve(&pkg, smr) else { continue };
        let sm = upkg::staticmesh::read_static_mesh(&sm_obj.pkg, sm_obj.export).unwrap();
        let Some(ObjRef::Export(smi)) = p.object("StaticMeshInstance") else { continue };
        let colors = upkg::staticmesh::read_instance_colors(&pkg, smi).unwrap_or_default();
        let rot = match p.get("Rotation") {
            Some(Value::Rotator(r)) => *r,
            _ => [0, 0, 0],
        };
        let yaw = rot[1] as f32 * std::f32::consts::TAU / 65536.0;
        let skins: Vec<ObjRef> = match p.get("Skins") {
            Some(Value::Array { raw }) => {
                let mut r = upkg::Reader::new(raw);
                let n = r.count().unwrap_or(0);
                (0..n).filter_map(|_| r.compact().ok().map(ObjRef::from_raw)).collect()
            }
            _ => vec![],
        };
        println!("{} {} yaw {:.0} deg, {} colours, {} verts", a[1], sm_obj.pkg.full_path(ObjRef::Export(sm_obj.export)), yaw.to_degrees(), colors.len(), sm.positions.len());
        for (s, sec) in sm.sections.iter().enumerate() {
            let mut verts: Vec<usize> = Vec::new();
            for t in 0..sec.num_triangles as usize {
                for k in 0..3 {
                    let ix = sm.indices.get(sec.first_index as usize + t * 3 + k).copied().unwrap_or(0) as usize;
                    if !verts.contains(&ix) {
                        verts.push(ix);
                    }
                }
            }
            let n = verts.len().max(1) as f32;
            let lum = verts.iter().map(|&v| colors.get(v).map(|c| (c[0] as f32 + c[1] as f32 + c[2] as f32) / 3.0).unwrap_or(0.0)).sum::<f32>() / n;
            let nm = verts.iter().fold([0f32; 3], |acc, &v| {
                let q = sm.normals.get(v).copied().unwrap_or_default();
                [acc[0] + q[0], acc[1] + q[1], acc[2] + q[2]]
            });
            // Rotate the mean normal by the actor yaw to world space.
            let (sy, cy) = yaw.sin_cos();
            let w = [nm[0] * cy - nm[1] * sy, nm[0] * sy + nm[1] * cy, nm[2]];
            let l = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt().max(1e-6);
            let mat = skins.get(s).copied().filter(|r| *r != ObjRef::Null).or_else(|| sm.materials.get(s).copied()).map(|r| if skins.get(s).is_some_and(|x| *x != ObjRef::Null) { pkg.full_path(r) } else { sm_obj.pkg.full_path(r) }).unwrap_or_default();
            println!("  section {s}: {} tris, {} verts, mean light {lum:.1}, normal ({:.2},{:.2},{:.2}) {mat}", sec.num_triangles, verts.len(), w[0] / l, w[1] / l, w[2] / l);
        }
    }
}
