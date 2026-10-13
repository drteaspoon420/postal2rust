//! Lists exports of a map whose properties mention a pattern (case-insensitive, in names, string
//! or name values) with all their properties. Usage: props_grep <map> <pattern> [class filter]
use upkg::{Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let pat = a[1].to_ascii_lowercase();
    let cls = a.get(2).map(|s| s.to_ascii_lowercase());
    for i in 0..pkg.exports.len() {
        let class = pkg.export_class(i).to_string();
        if class == "Class" || class.ends_with("Property") || class == "Function" || class == "State" {
            continue;
        }
        if cls.as_ref().is_some_and(|c| !class.to_ascii_lowercase().contains(c)) {
            continue;
        }
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let text: Vec<String> = p.0.iter().map(|q| format!("  {} [{}] = {:?}", q.name, q.index, q.value)).collect();
        let name = pkg.name(pkg.exports[i].name).to_string();
        if name.to_ascii_lowercase().contains(&pat) || text.iter().any(|t| t.to_ascii_lowercase().contains(&pat)) {
            println!("{name} ({class})");
            for t in text {
                if !["LastRenderTime", "bNetDirty", "Level ", "PhysicsVolume", "ColLocation", "bSelected", "Region ", "OldLocation", "PrePivot"].iter().any(|s| t.contains(s)) {
                    println!("{t}");
                }
            }
        }
    }
}
