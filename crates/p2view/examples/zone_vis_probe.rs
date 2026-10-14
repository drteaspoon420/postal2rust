//! `zone_vis_probe <map> X,Y,Z`: the BSP zone of an Unreal-space point and the zones its leaf
//! can see (Leaf::visible_zones), plus how many static mesh actors sit in each zone.
use std::collections::BTreeMap;
use std::sync::Arc;
use upkg::library::Library;
use upkg::{Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let lib = Library::new("G:\\SteamLibrary\\steamapps\\common\\POSTAL2Complete").unwrap();
    let map = Arc::new(Package::open(lib.map_path(&a[0]).unwrap()).unwrap());
    let m = upkg::model::read_model(&map, upkg::model::find_world_model(&map).unwrap()).unwrap();
    let bsp = p2collide::bsp::Bsp::new(&m);
    let p: Vec<f32> = a[1].split(',').map(|s| s.parse().unwrap()).collect();
    let leaf = bsp.point_leaf(bevy::math::Vec3::new(p[0], p[1], p[2]));
    let zone = bsp.point_zone(bevy::math::Vec3::new(p[0], p[1], p[2]));
    let vis = leaf.and_then(|l| m.leaves.get(l)).map(|l| l.visible_zones).unwrap_or(0);
    println!("leaf {leaf:?} zone {zone} visible_zones {vis:#066b} ({} zones of {})", vis.count_ones(), m.zones.len());
    let mut per: BTreeMap<usize, usize> = BTreeMap::new();
    for i in map.exports_of_class("StaticMeshActor") {
        let Ok((_, pr)) = begin_object(&map, i) else { continue };
        let Some(l) = pr.vector("Location") else { continue };
        per.entry(bsp.point_zone(bevy::math::Vec3::from(l))).and_modify(|c| *c += 1).or_insert(1);
    }
    for (z, n) in per {
        println!("zone {z:2} statics {n:4} visible {}", vis & (1u64 << z) != 0);
    }
    // Zone graph from portal surfaces (PF_PORTAL nodes join node.zone[0] and node.zone[1]).
    let mut adj: BTreeMap<usize, std::collections::BTreeSet<usize>> = BTreeMap::new();
    for n in &m.nodes {
        if n.num_verts >= 3 && m.surfs.get(n.surf as usize).is_some_and(|s| s.flags & upkg::model::pf::PORTAL != 0) {
            let (a, b) = (n.zone[0] as usize, n.zone[1] as usize);
            if a != b {
                adj.entry(a).or_default().insert(b);
                adj.entry(b).or_default().insert(a);
            }
        }
    }
    for (z, s) in &adj {
        println!("portal adj {z}: {s:?}");
    }
    let mut seen = std::collections::BTreeSet::from([zone]);
    let mut todo = vec![zone];
    while let Some(z) = todo.pop() {
        for &o in adj.get(&z).into_iter().flatten() {
            if o != 0 && seen.insert(o) {
                todo.push(o);
            }
        }
    }
    println!("reachable from {zone} (not via zone 0): {seen:?}");
    // Optional yaw,pitch (Unreal units): BSP surfaces near that view ray, 30000..60000 UU away.
    if let Some(r) = a.get(2) {
        let v: Vec<f32> = r.split(',').map(|s| s.parse().unwrap()).collect();
        let k = std::f32::consts::TAU / 65536.0;
        let (yaw, pitch) = (v[0] * k, v[1] * k);
        let dir = bevy::math::Vec3::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin());
        let o = bevy::math::Vec3::new(p[0], p[1], p[2]);
        let mut hits: BTreeMap<(u8, u8, String), usize> = BTreeMap::new();
        for n in &m.nodes {
            if n.num_verts < 3 {
                continue;
            }
            let poly: Vec<bevy::math::Vec3> = m.node_polygon(n).map(bevy::math::Vec3::from).collect();
            let c = poly.iter().copied().sum::<bevy::math::Vec3>() / poly.len() as f32;
            let d = c - o;
            let dist = d.length();
            if !(20000.0..80000.0).contains(&dist) || d.normalize().dot(dir) < 0.9 {
                continue;
            }
            let s = &m.surfs[n.surf as usize];
            let mat = map.full_path(s.material);
            *hits.entry((n.zone[0], n.zone[1], format!("{mat} flags {:#x}", s.flags))).or_default() += 1;
        }
        for ((z0, z1, mat), c) in hits {
            println!("view poly zones ({z0},{z1}) x{c} {mat}");
        }
        for i in map.exports_of_class("StaticMeshActor") {
            let Ok((_, pr)) = begin_object(&map, i) else { continue };
            let Some(l) = pr.vector("Location") else { continue };
            let d = bevy::math::Vec3::from(l) - o;
            if bsp.point_zone(bevy::math::Vec3::from(l)) == 0 && d.length() > 15000.0 && d.normalize().dot(dir) > 0.9 {
                println!("zone0 static {} at {l:?} dist {:.0} mesh {:?}", map.name(map.exports[i].name), d.length(), pr.object("StaticMesh").map(|r| map.full_path(r)));
            }
        }
    }
}
