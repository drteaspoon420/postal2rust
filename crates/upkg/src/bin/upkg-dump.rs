//! `upkg-dump <package> [--class C] [--props] [--limit N]` : lists exports and optionally their properties.
//! `upkg-dump --scan <dir>...` : parses every package and every export's property list, reporting failures.

use std::path::Path;
use upkg::{Package, begin_object};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("--scan") {
        scan(&args[1..]);
        return;
    }
    let Some(path) = args.first() else {
        eprintln!("usage: upkg-dump <package> [--class C] [--name N] [--props] [--limit N] | --scan <dir>...");
        std::process::exit(2);
    };
    let class = flag(&args, "--class");
    let name = flag(&args, "--name");
    let props = args.iter().any(|a| a == "--props");
    let limit: usize = flag(&args, "--limit").and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let pkg = Package::open(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    println!(
        "{}: version={} licensee={} names={} imports={} exports={}",
        pkg.name,
        pkg.version,
        pkg.licensee,
        pkg.names.len(),
        pkg.imports.len(),
        pkg.exports.len()
    );
    let mut shown = 0;
    if args.iter().any(|a| a == "--names") {
        for (i, n) in pkg.names.iter().enumerate() {
            println!("name {i:#x} {n}");
        }
    }
    for i in 0..pkg.exports.len() {
        if class.as_deref().is_some_and(|c| !pkg.export_class(i).eq_ignore_ascii_case(c)) {
            continue;
        }
        if name.as_deref().is_some_and(|n| !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(n)) {
            continue;
        }
        if shown >= limit {
            break;
        }
        shown += 1;
        let e = &pkg.exports[i];
        println!(
            "#{i:<5} {:<24} {:<48} off={:<9} size={:<8} flags={:#010x}",
            pkg.export_class(i),
            pkg.full_path(upkg::ObjRef::Export(i)),
            e.offset,
            e.size,
            e.flags
        );
        if args.iter().any(|a| a == "--hex") {
            for (row, chunk) in pkg.export_data(i).chunks(16).enumerate() {
                let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                let asc: String = chunk.iter().map(|&b| if (32..127).contains(&b) { b as char } else { '.' }).collect();
                println!("        {:06x}  {:<48} {asc}", row * 16, hex.join(" "));
            }
        }
        if args.iter().any(|a| a == "--mesh") && pkg.export_class(i) == "StaticMesh" {
            match upkg::staticmesh::read_static_mesh(&pkg, i) {
                Ok(m) => {
                    println!("        verts={} idx={} uvs={} colors={} bounds {:?}..{:?}", m.positions.len(), m.indices.len(), m.uvs.len(), m.colors.len(), m.bounds_min, m.bounds_max);
                    for (s, sec) in m.sections.iter().enumerate() {
                        let mat = m.materials.get(s).map(|o| format!("{} ({})", pkg.full_path(*o), pkg.class_of(*o))).unwrap_or_default();
                        let ids = m.section_indices(s);
                        let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                        for &ix in ids {
                            if let Some(uv) = m.uvs.get(ix as usize) {
                                umin = umin.min(uv[0]); umax = umax.max(uv[0]); vmin = vmin.min(uv[1]); vmax = vmax.max(uv[1]);
                            }
                        }
                        println!("        section {s}: first={} tris={} verts {}..{} uv u[{umin:.3},{umax:.3}] v[{vmin:.3},{vmax:.3}] mat={mat}", sec.first_index, sec.num_triangles, sec.min_vertex, sec.max_vertex);
                    }
                    if args.iter().any(|a| a == "--verts") {
                        for (v, p) in m.positions.iter().enumerate() {
                            println!("        v{v}: pos {:?} n {:?} uv {:?}", p, m.normals[v], m.uvs.get(v));
                        }
                        println!("        idx {:?}", m.indices);
                    }
                }
                Err(e) => println!("        !! mesh parse failed: {e}"),
            }
        }
        if props {
            match begin_object(&pkg, i) {
                Ok((r, p)) => {
                    for prop in &p.0 {
                        let v = match &prop.value {
                            upkg::Value::Object(o) => format!("{} ({})", pkg.full_path(*o), pkg.class_of(*o)),
                            upkg::Value::Struct { name, raw } => format!("struct {name} [{} bytes]", raw.len()),
                            upkg::Value::Array { raw } => {
                                // Best effort: a compact count followed by compact object refs.
                                let mut r = upkg::Reader::new(raw);
                                let mut objs = Vec::new();
                                if let Ok(n) = r.count() {
                                    for _ in 0..n {
                                        match r.compact() {
                                            Ok(v) => objs.push(pkg.full_path(upkg::ObjRef::from_raw(v))),
                                            Err(_) => break,
                                        }
                                    }
                                    if r.pos() != raw.len() {
                                        objs.clear();
                                    }
                                }
                                if objs.is_empty() { format!("array [{} bytes]", raw.len()) } else { format!("array [{} bytes] objs? {:?}", raw.len(), objs) }
                            }
                            v => format!("{v:?}"),
                        };
                        println!("        {}[{}] = {}", prop.name, prop.index, v);
                    }
                    println!("        -- native data: {} bytes at {}", e.offset + e.size - r.pos(), r.pos());
                }
                Err(err) => println!("        !! property parse failed: {err}"),
            }
        }
    }
}

fn flag(args: &[String], f: &str) -> Option<String> {
    args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned()
}

fn scan(dirs: &[String]) {
    let (mut pkgs, mut pkg_fail, mut objs, mut obj_fail) = (0, 0, 0usize, 0usize);
    for d in dirs {
        let mut entries: Vec<_> = std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        entries.sort();
        for p in entries {
            if !p.is_file() {
                continue;
            }
            match Package::open(&p) {
                Ok(pkg) => {
                    pkgs += 1;
                    let mut bad = 0;
                    let mut first = None;
                    for i in 0..pkg.exports.len() {
                        // Class/function/struct objects use a different serialization; skip them here.
                        let c = pkg.export_class(i);
                        if matches!(
                            c,
                            "Class" | "Function" | "State" | "Struct" | "Const" | "Enum" | "TextBuffer"
                        ) || c.ends_with("Property")
                            || pkg.exports[i].size == 0
                        {
                            continue;
                        }
                        objs += 1;
                        if let Err(e) = begin_object(&pkg, i) {
                            bad += 1;
                            first.get_or_insert_with(|| format!("{} ({c}): {e}", pkg.full_path(upkg::ObjRef::Export(i))));
                        }
                    }
                    obj_fail += bad;
                    if bad > 0 {
                        println!("{}: {bad} property failures, first: {}", show(&p), first.unwrap());
                    }
                }
                Err(e) => {
                    if !matches!(e, upkg::Error::NotAPackage(_)) {
                        pkg_fail += 1;
                        println!("{}: OPEN FAILED {e}", show(&p));
                    }
                }
            }
        }
    }
    println!("SCAN: packages ok={pkgs} failed={pkg_fail}; objects parsed={objs} property failures={obj_fail}");
}

fn show(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}
