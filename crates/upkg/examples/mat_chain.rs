//! `mat_chain <map> [X,Y,Z R]`: prints the material wrapper chain (class, blend props, texture
//! format/alpha stats) of every Projector and static-mesh actor material in a map, optionally
//! only within R of X,Y,Z. Used to find why decal-like materials (graffiti, crosswalks) render
//! opaque.
use std::sync::Arc;
use upkg::library::{Library, Obj};
use upkg::{ObjRef, Package, begin_object};

fn chain(lib: &Library, o: &Obj, depth: usize) {
    let pad = "    ".repeat(depth + 1);
    let Ok((_, p)) = begin_object(&o.pkg, o.export) else { return };
    let mut props = Vec::new();
    for q in &p.0 {
        let s = format!("{:?}", q.value);
        if matches!(q.name.as_str(), "MipZero" | "MaxColor" | "Palette" | "InternalTime" | "PrevTime") {
            continue;
        }
        props.push(format!("{}={}", q.name, if s.len() > 40 { &s[..40] } else { &s }));
    }
    let mut extra = String::new();
    if o.class() == "Texture" {
        if let Ok(t) = upkg::texture::read_texture(&o.pkg, o.export) {
            if let Ok((_, _, px)) = t.decode_rgba(t.mips.len().saturating_sub(1).min(3)) {
                let n = (px.len() / 4).max(1);
                let a: Vec<u8> = px.chunks(4).map(|c| c[3]).collect();
                let lum: u64 = px.chunks(4).map(|c| (c[0] as u64 + c[1] as u64 + c[2] as u64) / 3).sum();
                let amean: u64 = a.iter().map(|&x| x as u64).sum::<u64>() / n as u64;
                extra = format!(" fmt {:?} alpha0 {:.0}% alpha255 {:.0}% mean_alpha {amean} mean_lum {}", t.format, 100.0 * a.iter().filter(|&&x| x < 8).count() as f32 / n as f32, 100.0 * a.iter().filter(|&&x| x > 247).count() as f32 / n as f32, lum / n as u64);
            }
        }
    }
    println!("{pad}{}.{} ({}){extra} {}", o.pkg.name, o.name(), o.class(), props.join(" "));
    if depth > 6 {
        return;
    }
    for k in ["Diffuse", "Opacity", "Material", "Material1", "Material2", "Mask", "SelfIllumination", "SelfIlluminationMask", "Specular", "SpecularityMask", "FallbackMaterial"] {
        if let Some(r) = p.object(k).filter(|r| *r != ObjRef::Null) {
            if let Some(c) = lib.resolve(&o.pkg, r) {
                println!("{pad}  [{k}]");
                chain(lib, &c, depth + 1);
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let lib = Library::new("G:\\SteamLibrary\\steamapps\\common\\POSTAL2Complete").unwrap();
    // `mat_chain sm:Package Name`: the section materials of one static mesh.
    if let Some(pk) = a[0].strip_prefix("sm:") {
        let pkg = lib.package(pk).expect("package");
        for i in 0..pkg.exports.len() {
            if pkg.export_class(i) == "StaticMesh" && pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(&a[1]) {
                let m = upkg::staticmesh::read_static_mesh(&pkg, i).unwrap();
                println!("{} bounds {:?}..{:?} sections {}", a[1], m.bounds_min, m.bounds_max, m.sections.len());
                for r in m.materials {
                    if let Some(o) = lib.resolve(&pkg, r) {
                        chain(&lib, &o, 0);
                    }
                }
            }
        }
        return;
    }
    let path = lib.map_path(&a[0]).unwrap();
    let map = Arc::new(Package::open(&path).unwrap());
    lib.insert(map.clone());
    let near: Option<([f32; 3], f32)> = a.get(1).map(|s| {
        let v: Vec<f32> = s.split(',').map(|x| x.parse().unwrap()).collect();
        ([v[0], v[1], v[2]], a.get(2).and_then(|r| r.parse().ok()).unwrap_or(500.0))
    });
    for i in 0..map.exports.len() {
        let class = map.export_class(i).to_string();
        let Ok((_, p)) = begin_object(&map, i) else { continue };
        let Some(loc) = p.vector("Location") else { continue };
        if let Some((c, r)) = near {
            let d = ((loc[0] - c[0]).powi(2) + (loc[1] - c[1]).powi(2) + (loc[2] - c[2]).powi(2)).sqrt();
            if d > r {
                continue;
            }
        }
        let mats: Vec<ObjRef> = if class == "Projector" {
            p.object("ProjTexture").into_iter().collect()
        } else if let Some(sm) = p.object("StaticMesh").filter(|o| *o != ObjRef::Null) {
            let mut v = Vec::new();
            if let Some(smo) = lib.resolve(&map, sm) {
                if let Ok(m) = upkg::staticmesh::read_static_mesh(&smo.pkg, smo.export) {
                    for r in m.materials {
                        if let Some(o) = lib.resolve(&smo.pkg, r) {
                            println!("{} ({class}) at {:?} sm {} skins? section", map.name(map.exports[i].name), loc, smo.name());
                            chain(&lib, &o, 0);
                        }
                    }
                }
            }
            v.clear();
            v
        } else {
            continue;
        };
        for m in mats {
            if let Some(o) = lib.resolve(&map, m) {
                println!("{} ({class}) at {:?} MatOp {:?} FbOp {:?} DrawScale {:?}", map.name(map.exports[i].name), loc, p.byte("MaterialBlendingOp"), p.byte("FrameBufferBlendingOp"), p.float("DrawScale"));
                chain(&lib, &o, 0);
            }
        }
    }
}
