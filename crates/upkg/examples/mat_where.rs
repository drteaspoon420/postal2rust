//! Lists StaticMeshActors whose mesh materials or Skins reference a material name substring.
//! Usage: mat_where <map path> <substring>
use std::sync::Arc;
use upkg::library::Library;
use upkg::{ObjRef, Package, Value, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = std::path::Path::new(&a[0]);
    let want = a[1].to_ascii_lowercase();
    let pkg = Arc::new(Package::open(map).unwrap());
    let lib = Library::new(map.parent().unwrap().parent().unwrap()).unwrap();
    lib.insert(pkg.clone());
    let name_of = |r: ObjRef| -> String {
        match r {
            ObjRef::Null => String::new(),
            _ => pkg.full_path(r).to_ascii_lowercase(),
        }
    };
    for i in 0..pkg.exports.len() {
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let Some(sm) = p.object("StaticMesh") else { continue };
        let mut hits = Vec::new();
        for prop in &p.0 {
            if prop.name.eq_ignore_ascii_case("Skins") {
                if let Value::Object(r) = &prop.value {
                    let n = name_of(*r);
                    if n.contains(&want) {
                        hits.push(format!("skin[{}]", prop.index));
                    }
                }
            }
        }
        if let Some(o) = lib.resolve(&pkg, sm) {
            if let Ok(m) = upkg::staticmesh::read_static_mesh(&o.pkg, o.export) {
                for (k, r) in m.materials.iter().enumerate() {
                    let n = if Arc::ptr_eq(&o.pkg, &pkg) { name_of(*r) } else { o.pkg.full_path(*r).to_ascii_lowercase() };
                    if n.contains(&want) {
                        hits.push(format!("mat[{k}]"));
                    }
                }
            }
        }
        if !hits.is_empty() {
            let loc = p.vector("Location").unwrap_or([0.0; 3]);
            println!("{} {} mesh {} at {:.0},{:.0},{:.0} {}", i, pkg.name(pkg.exports[i].name), name_of(sm), loc[0], loc[1], loc[2], hits.join(" "));
        }
    }
}
