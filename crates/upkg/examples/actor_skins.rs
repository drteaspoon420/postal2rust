//! Prints a map actor's Skins (resolved through the package library) with each material's wrapper
//! chain and properties, to see e.g. whether a sign face is a Shader with SelfIllumination.
//! Usage: actor_skins <map path> <ActorName>
use upkg::library::Library;
use upkg::{ObjRef, Package, Value, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = std::path::Path::new(&a[0]);
    let pkg = std::sync::Arc::new(Package::open(map).unwrap());
    let lib = Library::new(map.parent().unwrap().parent().unwrap()).unwrap();
    lib.insert(pkg.clone());
    for i in 0..pkg.exports.len() {
        if !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(&a[1]) {
            continue;
        }
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let Some(Value::Array { raw }) = p.get("Skins") else {
            println!("no Skins");
            continue;
        };
        let mut r = upkg::Reader::new(raw);
        let n = r.count().unwrap_or(0);
        for k in 0..n {
            let Ok(c) = r.compact() else { break };
            let o = ObjRef::from_raw(c);
            if o == ObjRef::Null {
                println!("skin {k}: None");
                continue;
            }
            println!("skin {k}: {}", pkg.full_path(o));
            let mut cur = lib.resolve(&pkg, o);
            for _ in 0..6 {
                let Some(obj) = cur else { break };
                let Ok((_, q)) = begin_object(&obj.pkg, obj.export) else { break };
                let props: Vec<String> = q.0.iter().filter(|x| !x.name.starts_with("Internal")).map(|x| format!("{}={:?}", x.name, x.value)).map(|s| s.chars().take(70).collect()).collect();
                println!("    {} {} {}", obj.class(), obj.pkg.full_path(ObjRef::Export(obj.export)), props.join(" "));
                if obj.class() == "Texture" {
                    break;
                }
                let next = ["Diffuse", "Material", "Material1", "FallbackMaterial"].iter().find_map(|k| q.object(k).filter(|o| *o != ObjRef::Null));
                cur = next.and_then(|n| lib.resolve(&obj.pkg, n));
            }
        }
    }
}
