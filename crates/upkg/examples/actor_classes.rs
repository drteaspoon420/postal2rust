//! Research: lists actor classes placed in a map with their counts and the package each class
//! comes from. Usage: actor_classes <map.fuk> [filter]

use std::collections::BTreeMap;
use upkg::{ObjRef, Package};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let filter = a.get(1).map(|s| s.to_lowercase());
    let mut counts = BTreeMap::<String, usize>::new();
    for e in &pkg.exports {
        if let ObjRef::Import(i) = e.class {
            let imp = &pkg.imports[i];
            let outer = pkg.ref_name(imp.outer);
            let key = format!("{}.{}", outer, pkg.name(imp.name));
            if filter.as_ref().is_none_or(|f| key.to_lowercase().contains(f)) {
                *counts.entry(key).or_default() += 1;
            }
        }
    }
    for (k, v) in counts {
        println!("{v:5} {k}");
    }
}
