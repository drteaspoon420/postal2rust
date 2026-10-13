//! Research probe: dumps the native (post-property) bytes of skeletal objects.
//! Usage: skel_probe <file.ukx> [class] [name|-] [bytes] [skip]

use upkg::{Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let class = a.get(1).cloned().unwrap_or_else(|| "SkeletalMesh".into());
    let want = a.get(2).cloned().filter(|s| !s.is_empty() && s != "-");
    let nbytes: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(512);
    let skip: usize = a.get(4).and_then(|s| usize::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
    let mut classes = std::collections::BTreeMap::<String, usize>::new();
    for i in 0..pkg.exports.len() {
        *classes.entry(pkg.export_class(i).to_string()).or_default() += 1;
    }
    println!("classes: {classes:?}");
    for i in pkg.exports_of_class(&class).collect::<Vec<_>>() {
        let name = pkg.name(pkg.exports[i].name);
        if want.as_deref().is_some_and(|w| !w.eq_ignore_ascii_case(name)) {
            continue;
        }
        let (r, props) = begin_object(&pkg, i).unwrap();
        let e = &pkg.exports[i];
        println!("== {name} size={} native_at=+{} native_len={}", e.size, r.pos() - e.offset, e.offset + e.size - r.pos());
        for p in &props.0 {
            println!("  prop {}[{}] = {}", p.name, p.index, short(&p.value));
        }
        let d = &pkg.data[r.pos()..e.offset + e.size];
        let d = &d[skip.min(d.len())..];
        for (k, c) in d[..d.len().min(nbytes)].chunks(16).enumerate() {
            let hex: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
            println!("  {:06x}: {}", skip + k * 16, hex.join(" "));
        }
        if want.is_none() {
            break;
        }
    }
}

fn short(v: &upkg::Value) -> String {
    let s = format!("{v:?}");
    if s.len() > 120 { format!("{}...", &s[..120]) } else { s }
}
