//! Dumps the properties of named exports in a package. Usage: props_dump <package> <name>...

use upkg::{Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    for name in &a[1..] {
        let Some(i) = name.parse::<usize>().ok().or_else(|| pkg.find_export(name, None)) else {
            println!("{name}: not found");
            continue;
        };
        let (_, p) = begin_object(&pkg, i).unwrap();
        println!("{name} ({})", pkg.export_class(i));
        for q in &p.0 {
            println!("  {} [{}] = {:?}", q.name, q.index, q.value);
        }
    }
}
