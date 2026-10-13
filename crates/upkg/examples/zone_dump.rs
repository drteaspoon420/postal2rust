//! Dumps zone actors (LevelInfo/ZoneInfo/SkyZoneInfo) with all their properties, plus the classes
//! of actors whose Region/zone is the sky zone. Research aid for lighting/fog/sky fidelity.
//! Usage: zone_dump <map path> [class filter]

use upkg::Package;
use upkg::props::begin_object;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pkg = Package::open(&a[1]).unwrap();
    let filt = a.get(2).map(|s| s.to_ascii_lowercase());
    if filt.as_deref() == Some("imports") {
        for i in 0..pkg.imports.len() {
            println!("import {i}: {}", pkg.full_path(upkg::ObjRef::Import(i)));
        }
        return;
    }
    for i in 0..pkg.exports.len() {
        let class = pkg.export_class(i).to_string();
        let lc = class.to_ascii_lowercase();
        let want = match &filt {
            Some(f) => lc.contains(f.as_str()),
            None => lc == "levelinfo" || lc.ends_with("zoneinfo"),
        };
        if !want {
            continue;
        }
        if let Some(name) = a.get(3) {
            if !pkg.name(pkg.exports[i].name).to_ascii_lowercase().contains(&name.to_ascii_lowercase()) {
                continue;
            }
        }
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        println!("== {} {} ({})", i, pkg.name(pkg.exports[i].name), class);
        for prop in &p.0 {
            println!("   {}[{}] = {:?}", prop.name, prop.index, prop.value);
        }
    }
}
