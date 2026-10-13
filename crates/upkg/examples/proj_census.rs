//! `proj_census [--list]`: every level Projector in every map (Maps\*.fuk), with the properties
//! that place the projected image (FOV, DrawScale, DrawScale3D, Rotation roll, MaxTraceDistance,
//! bProjectBSP/StaticMesh/Actor/OnBackfaces, bGradient, ProjTexture class and size, clamp modes).
//! Prints a census of value combinations and, with --list, one line per projector.
use std::collections::BTreeMap;
use std::sync::Arc;
use upkg::library::Library;
use upkg::{ObjRef, Package, begin_object};

fn main() {
    let list = std::env::args().any(|a| a == "--list");
    let root = "G:\\SteamLibrary\\steamapps\\common\\POSTAL2Complete";
    let lib = Library::new(root).unwrap();
    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    let mut maps: Vec<_> = std::fs::read_dir(format!("{root}\\Maps")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("fuk"))).collect();
    maps.sort();
    let mut total = 0;
    for path in maps {
        let Ok(pkg) = Package::open(&path) else { continue };
        let map = Arc::new(pkg);
        lib.insert(map.clone());
        let mname = path.file_stem().unwrap().to_string_lossy().to_string();
        for i in map.exports_of_class("Projector") {
            let Ok((_, p)) = begin_object(&map, i) else { continue };
            total += 1;
            let mat = p.object("ProjTexture").filter(|o| *o != ObjRef::Null);
            let mo = mat.and_then(|m| lib.resolve(&map, m));
            let mclass = mo.as_ref().map(|o| o.class().to_string()).unwrap_or("-".into());
            let tex = mo.as_ref().and_then(|o| lib.material_texture(o)).and_then(|m| m.texture);
            let (us, vs, uclamp, vclamp) = tex
                .and_then(|t| begin_object(&t.pkg, t.export).ok().map(|(_, tp)| (tp.int("USize").unwrap_or(0), tp.int("VSize").unwrap_or(0), tp.byte("UClampMode").unwrap_or(0), tp.byte("VClampMode").unwrap_or(0))))
                .unwrap_or((0, 0, 0, 0));
            let ds3 = p.vector("DrawScale3D").map(|v| format!("{:.2},{:.2},{:.2}", v[0], v[1], v[2])).unwrap_or("1,1,1".into());
            let rot = p.rotator("Rotation").unwrap_or([0; 3]);
            let key = format!(
                "FOV={:?} DS3D={} roll0={} ProjBSP={:?} ProjSM={:?} ProjActor={:?} Backfaces={:?} Gradient={:?} Mat={} clamp={}/{} MatOp={:?} FbOp={:?}",
                p.byte("FOV"),
                if ds3 == "1.00,1.00,1.00" || ds3 == "1,1,1" { "1".to_string() } else { "non1".into() },
                rot[2] % 65536 == 0,
                p.bool("bProjectBSP"),
                p.bool("bProjectStaticMesh"),
                p.bool("bProjectActor"),
                p.bool("bProjectOnBackfaces"),
                p.bool("bGradient"),
                mclass,
                uclamp,
                vclamp,
                p.byte("MaterialBlendingOp"),
                p.byte("FrameBufferBlendingOp"),
            );
            *census.entry(key).or_default() += 1;
            if list {
                println!(
                    "{mname} {} loc {:?} rot {:?} FOV {:?} DS {:?} DS3D {ds3} MTD {:?} mat {} ({mclass}) tex {us}x{vs} clamp {uclamp}/{vclamp}",
                    map.name(map.exports[i].name),
                    p.vector("Location").map(|v| [v[0] as i32, v[1] as i32, v[2] as i32]),
                    rot,
                    p.byte("FOV"),
                    p.float("DrawScale"),
                    p.int("MaxTraceDistance"),
                    mo.as_ref().map(|o| o.name().to_string()).unwrap_or_default()
                );
            }
        }
    }
    println!("{total} projectors");
    let mut v: Vec<_> = census.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for (k, n) in v {
        println!("{n:>5}  {k}");
    }
}
