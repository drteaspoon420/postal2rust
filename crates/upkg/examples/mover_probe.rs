//! Counts mover actors per map, split into static-mesh movers and brush (Model) movers.
//!
//! Usage: mover_probe <map path>...

use upkg::{ObjRef, Package, begin_object};

fn main() {
    let (mut sm, mut brush, mut other) = (0, 0, 0);
    for path in std::env::args().skip(1) {
        let Ok(pkg) = Package::open(&path) else { continue };
        let (mut s, mut b, mut o) = (0, 0, 0);
        for i in 0..pkg.exports.len() {
            let c = pkg.export_class(i).to_ascii_lowercase();
            if !c.contains("mover") {
                continue;
            }
            let Ok((_, p)) = begin_object(&pkg, i) else { continue };
            let draw = p.byte("DrawType");
            if p.object("StaticMesh").is_some_and(|r| r != ObjRef::Null) && draw.is_none_or(|d| d == 8) {
                s += 1;
            } else if p.object("Brush").is_some_and(|r| r != ObjRef::Null) {
                b += 1;
                if b == 1 {
                    let br = p.object("Brush").unwrap();
                    println!("  {} brush mover {} -> {} ({}) draw={:?}", pkg.name, pkg.ref_name(ObjRef::Export(i)), pkg.ref_name(br), pkg.class_of(br), draw);
                }
            } else {
                o += 1;
            }
        }
        if s + b + o > 0 {
            println!("{:<24} sm={s} brush={b} other={o}", pkg.name);
        }
        sm += s;
        brush += b;
        other += o;
    }
    println!("MOVERS sm={sm} brush={brush} other={other}");
}
