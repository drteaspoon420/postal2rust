//! Research: lists animation notifies across .ukx packages (notify object classes, their
//! properties, and the range of notify times).
//! Usage: notify_scan <Animations dir> [max_examples]

use std::collections::BTreeMap;
use upkg::skeletal::read_mesh_animation;
use upkg::{ObjRef, Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let max: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let mut by_class = BTreeMap::<String, usize>::new();
    let mut examples = BTreeMap::<String, Vec<String>>::new();
    let (mut total, mut tmin, mut tmax) = (0usize, f32::MAX, f32::MIN);
    for e in std::fs::read_dir(&a[0]).unwrap().flatten() {
        let Ok(pkg) = Package::open(e.path()) else { continue };
        for i in pkg.exports_of_class("MeshAnimation").collect::<Vec<_>>() {
            let Ok(anim) = read_mesh_animation(&pkg, i) else { continue };
            for s in &anim.sequences {
                for n in &s.notifies {
                    total += 1;
                    tmin = tmin.min(n.time);
                    tmax = tmax.max(n.time);
                    let class = match n.notify {
                        ObjRef::Null => "<function>".to_string(),
                        r => pkg.class_of(r).to_string(),
                    };
                    *by_class.entry(class.clone()).or_default() += 1;
                    let ex = examples.entry(class).or_default();
                    if ex.len() < max {
                        let props = match n.notify {
                            ObjRef::Export(x) => begin_object(&pkg, x)
                                .map(|(_, p)| p.0.iter().map(|p| format!("{}={:?}", p.name, p.value)).collect::<Vec<_>>().join(" "))
                                .unwrap_or_default(),
                            _ => String::new(),
                        };
                        ex.push(format!("{}.{} seq={} frames={} t={} fn={} {}", pkg.name, anim.name, s.name, s.num_frames, n.time, n.function, props));
                    }
                }
            }
        }
    }
    println!("NOTIFIES total={total} time range {tmin}..{tmax}");
    for (c, n) in &by_class {
        println!("{n:6} {c}");
        for e in &examples[c] {
            println!("        {e}");
        }
    }
}
