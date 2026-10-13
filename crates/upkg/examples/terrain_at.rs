//! Terrain height vs BSP zone around a point: shows where terrain pokes above an indoor floor.
//! Usage: terrain_at <map> X Y Z [radius]
//! Prints, on a grid of heightmap vertices within radius, the terrain z, its height above Z and
//! the BSP zone (with bTerrainZone) the terrain vertex lies in.

use upkg::library::Library;
use upkg::model::{find_world_model, read_model};
use upkg::{ObjRef, Package, begin_object};

fn zone_of(m: &upkg::model::Model, p: [f32; 3]) -> Option<usize> {
    let mut i = 0usize;
    for _ in 0..4096 {
        let n = m.nodes.get(i)?;
        let d = n.plane[0] * p[0] + n.plane[1] * p[1] + n.plane[2] * p[2] - n.plane[3];
        let front = d >= 0.0;
        let child = if front { n.front } else { n.back };
        if child < 0 {
            let leaf = n.leaf[front as usize];
            return (leaf >= 0).then(|| m.leaves.get(leaf as usize).map(|l| l.zone as usize)).flatten();
        }
        i = child as usize;
    }
    None
}

fn leaf_of(m: &upkg::model::Model, p: [f32; 3]) -> Option<usize> {
    let mut i = 0usize;
    for _ in 0..4096 {
        let n = m.nodes.get(i)?;
        let d = n.plane[0] * p[0] + n.plane[1] * p[1] + n.plane[2] * p[2] - n.plane[3];
        let front = d >= 0.0;
        let child = if front { n.front } else { n.back };
        if child < 0 {
            let leaf = n.leaf[front as usize];
            return (leaf >= 0).then_some(leaf as usize);
        }
        i = child as usize;
    }
    None
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = std::path::Path::new(&a[0]);
    let p: Vec<f32> = a[1..4].iter().map(|s| s.parse().unwrap()).collect();
    let r: f32 = a.get(4).map(|s| s.parse().unwrap()).unwrap_or(400.0);
    let pkg = std::sync::Arc::new(Package::open(map).unwrap());
    let lib = Library::new(map.parent().unwrap().parent().unwrap()).unwrap();
    lib.insert(pkg.clone());
    let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
    let zname = |z: Option<usize>| -> String {
        let Some(z) = z else { return "-".into() };
        match m.zones.get(z) {
            Some(&o) if o != ObjRef::Null => {
                let tz = match o {
                    ObjRef::Export(i) => begin_object(&pkg, i).ok().and_then(|(_, q)| q.bool("bTerrainZone")).unwrap_or(false),
                    _ => false,
                };
                format!("{z}:{}{}", pkg.full_path(o), if tz { "(T)" } else { "" })
            }
            _ => format!("{z}:None"),
        }
    };
    println!("point zone {}", zname(zone_of(&m, [p[0], p[1], p[2]])));
    if let Some(l) = leaf_of(&m, [p[0], p[1], p[2]]) {
        let lf = &m.leaves[l];
        let vis: Vec<String> = (0..64).filter(|z| lf.visible_zones & (1u64 << z) != 0).map(|z| zname(Some(z))).collect();
        println!("leaf {l} zone {} visible_zones {:#x}: {}", lf.zone, lf.visible_zones, vis.join(" "));
    }
    if std::env::var("SCAN").is_ok() {
        // Visible quads whose centre (just above the surface) lies in a zone without bTerrainZone.
        let tz: Vec<bool> = m.zones.iter().map(|o| match o {
            ObjRef::Export(i) => begin_object(&pkg, *i).ok().and_then(|(_, q)| q.bool("bTerrainZone")).unwrap_or(false),
            _ => false,
        }).collect();
        for ti in upkg::terrain::find_terrains(&pkg) {
            let Ok(t) = upkg::terrain::read_terrain(&lib, &pkg, ti) else { continue };
            let mut per: std::collections::BTreeMap<usize, (usize, [f32; 3])> = Default::default();
            let mut total = 0;
            for y in 0..t.height - 1 {
                for x in 0..t.width - 1 {
                    if !t.quad_visible(x, y) {
                        continue;
                    }
                    total += 1;
                    let (a, b, c, d) = (t.vertex(x, y), t.vertex(x + 1, y), t.vertex(x, y + 1), t.vertex(x + 1, y + 1));
                    let cz = [(a[0] + d[0]) / 2.0, (a[1] + d[1]) / 2.0, (a[2] + b[2] + c[2] + d[2]) / 4.0 + 4.0];
                    let samples = [cz, [a[0], a[1], a[2] + 4.0], [b[0], b[1], b[2] + 4.0], [c[0], c[1], c[2] + 4.0], [d[0], d[1], d[2] + 4.0]];
                    let zs: Vec<Option<usize>> = samples.iter().map(|s| zone_of(&m, *s)).collect();
                    if zs.iter().any(|z| z.is_some_and(|z| tz.get(z).copied().unwrap_or(false))) {
                        continue;
                    }
                    if let Some(z) = zs.iter().flatten().next().copied() {
                        let e = per.entry(z).or_insert((0, cz));
                        e.0 += 1;
                    }
                }
            }
            println!("terrain {}: {total} visible quads", t.name);
            for (z, (n, at)) in per {
                println!("  {n:6} quads in zone {} e.g. at {:.0},{:.0},{:.0}", zname(Some(z)), at[0], at[1], at[2]);
            }
        }
        return;
    }
    for ti in upkg::terrain::find_terrains(&pkg) {
        let Ok(t) = upkg::terrain::read_terrain(&lib, &pkg, ti) else { continue };
        println!("terrain {} at {:?} scale {:?}", t.name, t.location, t.scale);
        for y in 0..t.height {
            for x in 0..t.width {
                let v = t.vertex(x, y);
                if (v[0] - p[0]).abs() > r || (v[1] - p[1]).abs() > r {
                    continue;
                }
                let vis = t.quad_visible(x.min(t.width - 2), y.min(t.height - 2));
                println!("  v({x},{y}) {:.0},{:.0} z {:.0} dz {:+.0} quad {} zone {}", v[0], v[1], v[2], v[2] - p[2], if vis { "vis" } else { "hole" }, zname(zone_of(&m, [v[0], v[1], v[2] + 1.0])));
            }
        }
    }
}
