//! Research: prints the nested tagged contents of struct/array properties of particle emitter
//! sub-objects. Usage: emitter_probe <package> [class] [limit]

use upkg::{Package, Reader, Value, begin_object, read_properties};

fn show(pkg: &Package, v: &Value, depth: usize) -> String {
    match v {
        Value::Struct { name, raw } => {
            let mut r = Reader::new(raw);
            match read_properties(&mut r, pkg) {
                Ok(p) => format!(
                    "{name}{{{}}}",
                    p.0.iter().map(|q| format!("{}={}", q.name, show(pkg, &q.value, depth + 1))).collect::<Vec<_>>().join(", ")
                ),
                Err(_) => format!("{name}<raw {} bytes>", raw.len()),
            }
        }
        Value::Array { raw } => {
            let mut r = Reader::new(raw);
            let n = r.count().unwrap_or(0);
            let mut items = Vec::new();
            for _ in 0..n {
                let save = r.pos();
                match read_properties(&mut r, pkg) {
                    Ok(p) => items.push(format!("{{{}}}", p.0.iter().map(|q| format!("{}={}", q.name, show(pkg, &q.value, depth + 1))).collect::<Vec<_>>().join(", "))),
                    Err(_) => {
                        r.seek(save);
                        items.push(format!("<{} raw bytes>", raw.len() - save));
                        break;
                    }
                }
            }
            format!("[{}]", items.join(", "))
        }
        Value::Object(o) => format!("Object({})", pkg.full_path(*o)),
        v => format!("{v:?}"),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let class = a.get(1).cloned().unwrap_or_else(|| "SpriteEmitter".into());
    let limit: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    // Optional 4th arg: only exports whose full path contains this text.
    let filter = a.get(3).cloned().unwrap_or_default();
    for i in pkg.exports_of_class(&class).filter(|&i| pkg.full_path(upkg::ObjRef::Export(i)).contains(filter.as_str())).take(limit).collect::<Vec<_>>() {
        println!("#{i} {}", pkg.full_path(upkg::ObjRef::Export(i)));
        let (_, props) = begin_object(&pkg, i).unwrap();
        for p in &props.0 {
            println!("    {}[{}] = {}", p.name, p.index, show(&pkg, &p.value, 0));
        }
    }
}
