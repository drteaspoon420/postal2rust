//! Exploration helper: parses a map's world Model up to the vertex pool with the known layout,
//! then hex-dumps the following bytes so the zone / leaf / hull arrays can be identified.
//! Run: cargo run --release -p upkg --example model_tail_probe -- <map.fuk>

use upkg::{Package, Reader, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let i = upkg::model::find_world_model(&pkg).expect("no model");
    let (mut r, _props) = begin_object(&pkg, i).unwrap();
    r.skip(25 + 16).unwrap();
    let nv = r.count().unwrap();
    r.skip(nv * 12).unwrap();
    let np = r.count().unwrap();
    r.skip(np * 12).unwrap();
    let nn = r.count().unwrap();
    let mut flags_hist = [0usize; 256];
    let (mut max_coll, mut max_render, mut max_leaf) = (-1, -1, -1);
    for _ in 0..nn {
        r.skip(16 + 8).unwrap();
        let f = r.u8().unwrap();
        flags_hist[f as usize] += 1;
        r.skip(28).unwrap();
        let mut c = [0i32; 7];
        for v in &mut c {
            *v = r.compact().unwrap();
        }
        max_coll = max_coll.max(c[5]);
        max_render = max_render.max(c[6]);
        r.skip(32).unwrap();
        let z = (r.u8().unwrap(), r.u8().unwrap(), r.u8().unwrap());
        let leaf = (r.i32().unwrap(), r.i32().unwrap());
        max_leaf = max_leaf.max(leaf.0).max(leaf.1);
        r.skip(12).unwrap();
        if nn < 60 {
            println!("  node coll={} render={} zones={},{} nv={} leaves={:?} back={} front={} plane={}", c[5], c[6], z.0, z.1, z.2, leaf, c[2], c[3], c[4]);
        }
    }
    println!("max collision bound={max_coll} max render bound={max_render} max leaf={max_leaf}");
    println!("nodes={nn} flag histogram:");
    for (f, c) in flags_hist.iter().enumerate().filter(|(_, c)| **c > 0) {
        println!("  {f:#04x}: {c}");
    }
    let ns = r.count().unwrap();
    for _ in 0..ns {
        r.compact().unwrap();
        r.u32().unwrap();
        for _ in 0..6 {
            r.compact().unwrap();
        }
        r.skip(20).unwrap();
    }
    let nverts = r.count().unwrap();
    for _ in 0..nverts {
        r.compact().unwrap();
        r.compact().unwrap();
    }
    println!("surfs={ns} verts={nverts} tail at {} of {} (remaining {})", r.pos(), r.pos() + r.remaining(), r.remaining());
    let shared = r.i32().unwrap();
    let nz = r.i32().unwrap();
    println!("shared sides={shared} zones={nz}");
    for _ in 0..nz {
        let a = r.compact().unwrap();
        println!("  zone actor {}", pkg.full_path(upkg::ObjRef::from_raw(a)));
        r.skip(76).unwrap();
    }
    let polys = r.compact().unwrap();
    println!("polys {}", pkg.full_path(upkg::ObjRef::from_raw(polys)));
    let mode = a.get(1).map(|s| s.as_str()).unwrap_or("dump");
    let nb = r.count().unwrap();
    r.skip(nb * 25).unwrap();
    let hulls = r.array(|r| r.i32()).unwrap();
    println!("bounds={nb} leafhulls={}", hulls.len());
    for (i, h) in hulls.iter().take(40).enumerate() {
        println!("  hull[{i}] = {h} ({:#x}, f={})", h, f32::from_bits(*h as u32));
    }
    if mode == "dump" {
        dump(&r, 512);
    }
}

fn dump(r: &Reader, n: usize) {
    let mut h = r.clone();
    let raw = h.bytes(n.min(h.remaining())).unwrap();
    for (row, c) in raw.chunks(16).enumerate() {
        let x: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
        println!("  +{:04x} {}", row * 16, x.join(" "));
    }
}
