//! Research probe: per-LOD and raw-import data counts of skeletal meshes.
//! Usage: skel_lods <file.ukx> [name...]
//!        skel_lods --verify <dir>   decodes every LOD's packed skin stream and compares it with
//!        the plain wedge / influence lists where the file ships both; lists rebuilt meshes.
use upkg::Package;
use upkg::skeletal::unpack_skin_stream;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some("--verify") {
        return verify(&a[1]);
    }
    let pkg = Package::open(&a[0]).unwrap();
    let want = &a[1..];
    for i in pkg.exports_of_class("SkeletalMesh").collect::<Vec<_>>() {
        let name = pkg.name(pkg.exports[i].name).to_string();
        if !want.is_empty() && !want.iter().any(|w| w.eq_ignore_ascii_case(&name)) {
            continue;
        }
        println!("== {}.{name}", pkg.name);
        if let Err(e) = upkg::skeletal::trace(&pkg, i) {
            println!("  error {e}");
        }
    }
}

fn verify(dir: &str) {
    let (mut checked, mut bad, mut empty) = (0, 0, 0);
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let Ok(pkg) = Package::open(e.path()) else { continue };
        for i in pkg.exports_of_class("SkeletalMesh").collect::<Vec<_>>() {
            let name = pkg.name(pkg.exports[i].name).to_string();
            let Ok(m) = upkg::skeletal::read_skeletal_mesh(&pkg, i) else { continue };
            for (li, l) in m.lods.iter().enumerate() {
                if l.faces.is_empty() {
                    empty += 1;
                    println!("EMPTY {}.{name} lod{li} (stream {} words, sections {})", pkg.name, l.skin_stream.len(), l.smooth_sections.len() + l.rigid_sections.len());
                    continue;
                }
                if l.skin_stream.is_empty() || !l.rigid_sections.is_empty() {
                    continue;
                }
                let Some(sw) = unpack_skin_stream(&l.skin_stream) else {
                    bad += 1;
                    println!("BAD {}.{name} lod{li}: stream does not decode", pkg.name);
                    continue;
                };
                checked += 1;
                let mut mism = 0;
                if sw.len() != l.wedges.len() {
                    mism += 1;
                }
                for (s, w) in sw.iter().zip(&l.wedges) {
                    if s.point != w.point || s.uv != w.uv {
                        mism += 1;
                    }
                }
                // Influences: the bones of each new point.
                let mut by_point: Vec<Vec<u16>> = vec![Vec::new(); l.points.len()];
                for i in &l.influences {
                    by_point[i.point as usize].push(i.bone);
                }
                for s in sw.iter().filter(|s| !s.influences.is_empty()) {
                    let mut a: Vec<u16> = s.influences.iter().map(|x| x.0).collect();
                    a.sort();
                    a.dedup();
                    let mut b = by_point.get(s.point as usize).cloned().unwrap_or_default();
                    b.sort();
                    b.dedup();
                    if a != b {
                        mism += 1;
                    }
                }
                if mism > 0 {
                    bad += 1;
                    println!("MISMATCH {}.{name} lod{li}: {mism} (stream wedges {} vs {})", pkg.name, sw.len(), l.wedges.len());
                }
            }
        }
    }
    println!("VERIFY checked={checked} bad={bad} empty_lods={empty}");
}
