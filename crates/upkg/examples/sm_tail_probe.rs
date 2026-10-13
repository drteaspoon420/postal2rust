//! Exploration helper: decodes a StaticMesh's render data with the known layout, prints its
//! tagged properties, then hex-dumps the bytes after the index buffer (collision data).
//! Run: cargo run --release -p upkg --example sm_tail_probe -- <package.usx> [mesh name]

use upkg::{Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let list: Vec<usize> = match a.get(1) {
        Some(n) => vec![pkg.find_export(n, Some("StaticMesh")).expect("mesh")],
        None => pkg.exports_of_class("StaticMesh").take(3).collect(),
    };
    for i in list {
        let (mut r, props) = begin_object(&pkg, i).unwrap();
        println!("== {} props:", pkg.name(pkg.exports[i].name));
        for p in &props.0 {
            println!("   {} [{}] = {:?}", p.name, p.index, p.value);
        }
        r.skip(25 + 16).unwrap();
        let ns = r.count().unwrap();
        r.skip(ns * 14).unwrap();
        r.skip(25).unwrap();
        let nv = r.count().unwrap();
        r.skip(nv * 24 + 4).unwrap();
        for _ in 0..2 {
            let n = r.count().unwrap();
            r.skip(n * 4 + 4).unwrap();
        }
        let nuv = r.count().unwrap();
        for _ in 0..nuv {
            let n = r.count().unwrap();
            r.skip(n * 8 + 8).unwrap();
        }
        let ni = r.count().unwrap();
        r.skip(ni * 2 + 4).unwrap();
        let nw = r.count().unwrap();
        r.skip(nw * 2 + 4).unwrap();
        let cm = r.compact().unwrap();
        println!("   wire={nw} collision model ref={cm} ({})", pkg.full_path(upkg::ObjRef::from_raw(cm)));
        println!("   verts={nv} indices={ni} tail at {} remaining {}", r.pos(), r.remaining());
        let nt = r.count().unwrap();
        let mut maxv = 0;
        for _ in 0..nt {
            r.skip(64).unwrap();
            for k in 0..4 {
                let v = r.compact().unwrap();
                if k < 3 {
                    maxv = maxv.max(v);
                }
            }
        }
        println!("   collision tris={nt} max vertex={maxv} (render verts {nv}) now remaining {}", r.remaining());
        let mut h = r.clone();
        let raw = h.bytes(256.min(h.remaining())).unwrap();
        for (row, c) in raw.chunks(16).enumerate() {
            let x: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
            println!("  +{:04x} {}", row * 16, x.join(" "));
        }
    }
}
