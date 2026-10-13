//! Lists the materials used by a map's BSP surfaces with their wrapper chain (class, blend
//! properties) and surface count. Usage: bsp_materials <map> [name filter]
//! Run: cargo run --release -p upkg --example bsp_materials -- G:\...\Maps\suburbs-1.fuk wood

use std::collections::BTreeMap;
use upkg::model::{find_world_model, read_model};
use upkg::library::Library;
use upkg::{ObjRef, Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = std::path::Path::new(&a[0]);
    let filt = a.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let pkg = std::sync::Arc::new(Package::open(map).unwrap());
    let root = map.parent().unwrap().parent().unwrap();
    let lib = Library::new(root).unwrap();
    lib.insert(pkg.clone());
    let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
    let mut count: BTreeMap<(String, u32), usize> = BTreeMap::new();
    let mut refs: BTreeMap<String, ObjRef> = BTreeMap::new();
    for s in &m.surfs {
        let name = if s.material == ObjRef::Null { "None".into() } else { pkg.full_path(s.material) };
        *count.entry((name.clone(), s.flags)).or_default() += 1;
        refs.insert(name, s.material);
    }
    for ((name, flags), n) in &count {
        if !name.to_ascii_lowercase().contains(&filt) {
            continue;
        }
        println!("{n:5} {name} polyflags {flags:#x}");
        let Some(mut cur) = lib.resolve(&pkg, refs[name]) else { continue };
        for _ in 0..8 {
            let class = cur.class().to_string();
            let Ok((_, p)) = begin_object(&cur.pkg, cur.export) else { break };
            let mut line = format!("      {class} {}", cur.pkg.full_path(ObjRef::Export(cur.export)));
            for q in &p.0 {
                let v = format!("{:?}", q.value);
                line += &format!(" {}={}", q.name, if v.len() > 60 { &v[..60] } else { &v });
            }
            println!("{line}");
            if class == "Texture" {
                break;
            }
            let next = ["Diffuse", "Material", "Material1", "Material2", "FallbackMaterial"].iter().find_map(|k| p.object(k).filter(|o| *o != ObjRef::Null));
            match next.and_then(|n| lib.resolve(&cur.pkg, n)) {
                Some(n) => cur = n,
                None => break,
            }
        }
    }
}
