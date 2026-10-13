//! Counts textures that reference a detail texture (Texture.Detail / DetailScale) per package.
//! Run: cargo run --release -p upkg --example detail_scan -- <dir>...

use std::collections::BTreeMap;
use upkg::{ObjRef, Package, begin_object};

fn main() {
    let mut per_detail: BTreeMap<String, usize> = BTreeMap::new();
    let (mut total, mut with) = (0usize, 0usize);
    for d in std::env::args().skip(1) {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let Ok(pkg) = Package::open(e.path()) else { continue };
            for i in pkg.exports_of_class("Texture").collect::<Vec<_>>() {
                total += 1;
                let Ok((_, p)) = begin_object(&pkg, i) else { continue };
                if let Some(r) = p.object("Detail").filter(|r| *r != ObjRef::Null) {
                    with += 1;
                    let name = format!("{} x{}", pkg.full_path(r), p.float("DetailScale").unwrap_or(8.0));
                    *per_detail.entry(name).or_default() += 1;
                }
            }
        }
    }
    println!("textures {total}, with Detail {with}");
    for (k, v) in &per_detail {
        println!("  {v:5} {k}");
    }
}
