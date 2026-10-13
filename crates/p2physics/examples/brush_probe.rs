//! Research: checks the brush-to-world transform of volume brushes. For each volume in a map,
//! samples points inside its brush BSP (local space) and reports how many land in empty world
//! space under two conventions: world = Location + R * (p - PrePivot) and world = Location + R * p.
//! Usage: brush_probe <map name> [class filter]

use glam::Vec3;
use p2collide::bsp::Bsp;
use p2collide::world::rotator_matrix;
use std::sync::Arc;
use upkg::library::Library;
use upkg::{ObjRef, Package, Value, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let lib = Library::new(r"G:\SteamLibrary\steamapps\common\POSTAL2Complete").unwrap();
    let pkg = Arc::new(Package::open(lib.map_path(&a[0]).unwrap()).unwrap());
    let filter = a.get(1).map(|s| s.to_lowercase()).unwrap_or("volume".into());
    let wm = upkg::model::read_model(&pkg, upkg::model::find_world_model(&pkg).unwrap()).unwrap();
    let world = Bsp::new(&wm);
    let (mut tot_a, mut tot_b, mut tot_in) = (0usize, 0usize, 0usize);
    for i in 0..pkg.exports.len() {
        let c = pkg.export_class(i).to_lowercase();
        if !c.contains(&filter) {
            continue;
        }
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let Some(ObjRef::Export(b)) = p.object("Brush") else { continue };
        let m = match upkg::model::read_model(&pkg, b) {
            Ok(m) => m,
            Err(e) => {
                println!("#{i} model err {e}");
                continue;
            }
        };
        let bsp = Bsp::new(&m);
        let pts: Vec<Vec3> = m.points.iter().map(|&q| Vec3::from(q)).collect();
        let (mn, mx) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), &q| (a.min(q), b.max(q)));
        let loc = Vec3::from(p.vector("Location").unwrap_or_default());
        let pre = Vec3::from(p.vector("PrePivot").unwrap_or_default());
        let rot = rotator_matrix(p.rotator("Rotation").unwrap_or_default());
        let scale = match (p.get("MainScale"), p.get("PostScale")) {
            (Some(Value::Struct { raw: a, .. }), Some(Value::Struct { raw: b, .. })) => format!("{:02x?} {:02x?}", a, b),
            _ => "-".into(),
        };
        // Convexity: every point behind (or on) every node plane.
        let convex = m.nodes.iter().all(|n| pts.iter().all(|q| Vec3::new(n.plane[0], n.plane[1], n.plane[2]).dot(*q) - n.plane[3] <= 0.5));
        if !convex && a.len() > 3 {
            println!("#{i} NOT CONVEX nodes {}", m.nodes.len());
        }
        if a.len() > 3 {
            println!("#{i} root_outside {} points {} centroid-solid {}", m.root_outside, m.points.len(), bsp.point_solid((mn + mx) * 0.5));
            for n in &m.nodes {
                let poly: Vec<[f32; 3]> = m.node_polygon(n).collect();
                println!("   plane {:?} f{} b{} fr{} flags {} poly {:?}", n.plane, n.front, n.back, n.coplanar, n.flags, poly);
            }
        }
        let (mut n_in, mut ea, mut eb) = (0, 0, 0);
        for k in 0..2000u32 {
            let f = |s: u32| ((s.wrapping_mul(2654435761) >> 8) & 0xffff) as f32 / 65535.0;
            let q = mn + (mx - mn) * Vec3::new(f(k * 3 + 1), f(k * 3 + 2), f(k * 3 + 3));
            if !bsp.point_solid(q) {
                continue;
            }
            n_in += 1;
            if !world.point_solid(loc + rot * (q - pre)) {
                ea += 1;
            }
            if !world.point_solid(loc + rot * q) {
                eb += 1;
            }
        }
        tot_a += ea;
        tot_b += eb;
        tot_in += n_in;
        if a.len() > 2 {
            println!("#{i} {} nodes {} box {mn}..{mx} pre {pre} rot {:?} inside {n_in}/2000 empty(minus pre) {ea} empty(no pre) {eb} scale {scale}", pkg.export_class(i), m.nodes.len(), p.rotator("Rotation"));
        }
    }
    println!("{}: inside samples {tot_in}, empty with -PrePivot {tot_a}, empty without {tot_b}", a[0]);
}
