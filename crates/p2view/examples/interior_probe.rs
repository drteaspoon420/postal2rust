//! `interior_probe <map> [pattern]`: actors whose object properties reference `pattern`
//! (import/export path, e.g. a sound or static mesh name) with their location and BSP zone,
//! plus per-zone counts of BSP polys and static mesh actors. Research aid for interior/zone
//! visibility issues (suburbs-3 laundromat).
use std::sync::Arc;
use upkg::library::Library;
use upkg::props::Value;
use upkg::{Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let lib = Library::new("G:\\SteamLibrary\\steamapps\\common\\POSTAL2Complete").unwrap();
    let map = Arc::new(Package::open(lib.map_path(&a[0]).unwrap()).unwrap());
    let m = upkg::model::read_model(&map, upkg::model::find_world_model(&map).unwrap()).unwrap();
    let bsp = p2collide::bsp::Bsp::new(&m);
    // `portals Z`: portal polygons touching zone Z (centre, zones, material).
    if a.get(1).map(String::as_str) == Some("portals") {
        let z: u8 = a[2].parse().unwrap();
        for n in &m.nodes {
            if n.num_verts < 3 || (n.zone[0] != z && n.zone[1] != z) {
                continue;
            }
            let s = &m.surfs[n.surf as usize];
            let poly: Vec<bevy::math::Vec3> = m.node_polygon(n).map(bevy::math::Vec3::from).collect();
            let c = poly.iter().copied().sum::<bevy::math::Vec3>() / poly.len() as f32;
            let (lo, hi) = poly.iter().fold((poly[0], poly[0]), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
            println!("poly zones {:?} centre {c:.0} size {:.0} flags {:#x} {}", n.zone, hi - lo, s.flags, map.full_path(s.material));
        }
        return;
    }
    let pat = a.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    for i in 0..map.exports.len() {
        let Ok((_, p)) = begin_object(&map, i) else { continue };
        let Some(l) = p.vector("Location") else { continue };
        let name = map.name(map.exports[i].name).to_string();
        let mut refs = Vec::new();
        for q in &p.0 {
            if let Value::Object(r) = &q.value {
                refs.push(format!("{}={}", q.name, map.full_path(*r)));
            }
        }
        // `@X,Y,Z,R`: everything within R of a point instead of a name pattern.
        let hit = if let Some(s) = pat.strip_prefix('@') {
            let v: Vec<f32> = s.split(',').map(|s| s.parse().unwrap()).collect();
            (bevy::math::Vec3::from(l) - bevy::math::Vec3::new(v[0], v[1], v[2])).length() < v[3]
        } else {
            name.to_ascii_lowercase().contains(&pat) || refs.iter().any(|r| r.to_ascii_lowercase().contains(&pat))
        };
        if !hit {
            continue;
        }
        let z = bsp.point_zone(bevy::math::Vec3::from(l));
        let refs: Vec<&String> = refs.iter().filter(|r| !r.starts_with("Level=") && !r.starts_with("Region") && !r.starts_with("PhysicsVolume")).collect();
        println!("{name} ({}) at {:?} zone {z} {refs:?}", map.export_class(i), l);
    }
}
