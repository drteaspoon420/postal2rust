//! Research: lists a package's imports (class, full path), optionally filtered by class name.
//! Usage: import_list <package> [class filter]
use upkg::Package;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let filter = a.get(1).map(|s| s.to_lowercase());
    for (i, imp) in pkg.imports.iter().enumerate() {
        let class = pkg.name(imp.class_name).to_string();
        if filter.as_ref().is_some_and(|f| !class.to_lowercase().contains(f)) {
            continue;
        }
        println!("{i:5} {class:<16} {}", pkg.full_path(upkg::ObjRef::Import(i)));
    }
}
