//! Research: level exits (Telepad URL and Tag, with location) of the given maps.
//! Usage: telepads <map.fuk>...

use upkg::{ObjRef, Package, begin_object};

fn main() {
    for path in std::env::args().skip(1) {
        let Ok(pkg) = Package::open(&path) else {
            println!("{path}: open failed");
            continue;
        };
        for (i, e) in pkg.exports.iter().enumerate() {
            let ObjRef::Import(c) = e.class else { continue };
            if !pkg.name(pkg.imports[c].name).eq_ignore_ascii_case("Telepad") {
                continue;
            }
            let Ok((_, p)) = begin_object(&pkg, i) else { continue };
            let mut url = String::new();
            let mut tag = String::new();
            let mut loc = String::new();
            for q in &p.0 {
                match q.name.as_str() {
                    "URL" => url = format!("{:?}", q.value),
                    "Tag" => tag = format!("{:?}", q.value),
                    "Location" => loc = format!("{:?}", q.value),
                    _ => {}
                }
            }
            println!("{}\t{}\t{url}\t{tag}\t{loc}", pkg.name, pkg.name(e.name));
        }
    }
}
