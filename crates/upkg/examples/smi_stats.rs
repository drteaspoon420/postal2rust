//! Stats of StaticMeshInstance colour streams in a map: how many have zero alpha everywhere,
//! and their mean RGB, to decide whether alpha-0 streams carry usable lighting.
//! Usage: smi_stats <map path>

use upkg::{ObjRef, Package, begin_object};

fn main() {
    let path = std::env::args().nth(1).expect("map");
    let pkg = Package::open(&path).unwrap();
    let (mut total, mut zero_a, mut zero_a_nonblack) = (0, 0, 0);
    let mut shown = 0;
    for i in 0..pkg.exports.len() {
        let Ok((_, p)) = begin_object(&pkg, i) else { continue };
        let Some(ObjRef::Export(smi)) = p.object("StaticMeshInstance") else { continue };
        let Ok(c) = upkg::staticmesh::read_instance_colors(&pkg, smi) else { continue };
        if c.is_empty() {
            continue;
        }
        total += 1;
        let amax = c.iter().map(|x| x[3]).max().unwrap();
        let mean = c.iter().fold([0f32; 3], |a, x| [a[0] + x[0] as f32, a[1] + x[1] as f32, a[2] + x[2] as f32]).map(|v| v / c.len() as f32);
        if amax == 0 {
            zero_a += 1;
            if mean.iter().any(|&m| m > 1.0) {
                zero_a_nonblack += 1;
                if shown < 8 {
                    shown += 1;
                    println!("{} alpha0 mean={mean:.1?} n={}", pkg.name(pkg.exports[i].name), c.len());
                }
            }
        }
    }
    println!("{}: {total} streams, {zero_a} all-alpha-0 ({zero_a_nonblack} of them non-black)", pkg.name);
}
