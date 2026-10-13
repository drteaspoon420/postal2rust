//! Hex-dumps a StaticMeshActor's StaticMeshInstance object: the colour array header, then the
//! bytes after it, to find extra lighting data (per-light streams). Usage:
//! smi_dump <map path> <actor name>

use upkg::{ObjRef, Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let i = pkg.find_export(&a[1], None).expect("actor");
    let (_, p) = begin_object(&pkg, i).unwrap();
    let Some(ObjRef::Export(smi)) = p.object("StaticMeshInstance") else { panic!("no smi") };
    let (mut r, props) = begin_object(&pkg, smi).unwrap();
    println!("smi export {smi} size {} props {:?}", pkg.exports[smi].size, props.0.iter().map(|q| &q.name).collect::<Vec<_>>());
    let start = r.pos();
    let n = r.count().unwrap();
    println!("colour count {n} (header at {start})");
    r.skip(n * 4).unwrap();
    let after = r.pos();
    let end = pkg.exports[smi].offset + pkg.exports[smi].size;
    println!("bytes after colours: {}", end - after);
    let data = &pkg.data[after..end.min(after + 512)];
    for (k, chunk) in data.chunks(16).enumerate().take(6) {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        println!("{:5}: {}", k * 16, hex.join(" "));
    }
    // Hypothesis: int32, then TArray<{compact light actor, TArray<u8> visibility bits, int32 applied}>.
    let mut r2 = upkg::Reader::at(&pkg.data, after);
    let x = r2.i32().unwrap();
    let nl = r2.count().unwrap();
    println!("int32 {x}, {nl} light entries");
    for _ in 0..nl {
        let actor = ObjRef::from_raw(r2.compact().unwrap());
        let nb = r2.count().unwrap();
        let bits = r2.bytes(nb).unwrap();
        let lit = bits.iter().map(|b| b.count_ones()).sum::<u32>();
        let applied = r2.i32().unwrap();
        // Mean stream luminance of vertices this light sees vs does not see.
        let cols = upkg::staticmesh::read_instance_colors(&pkg, smi).unwrap();
        let (mut sv, mut nv, mut cs, mut cn) = (0f32, 0f32, 0u32, 0u32);
        for (v, c) in cols.iter().enumerate() {
            let l = (c[0] as f32 + c[1] as f32 + c[2] as f32) / 3.0;
            if bits.get(v / 8).is_some_and(|b| b & (1 << (v % 8)) != 0) {
                sv += l;
                cs += 1;
            } else {
                nv += l;
                cn += 1;
            }
        }
        let corr = format!("lum seen {:.1} unseen {:.1}", sv / cs.max(1) as f32, nv / cn.max(1) as f32);
        let (class, extra) = match actor {
            ObjRef::Export(e) => {
                let (_, lp) = begin_object(&pkg, e).unwrap();
                (pkg.export_class(e).to_string(), format!("bright={:?} effect={:?} hue={:?} sat={:?}", lp.get("LightBrightness"), lp.byte("LightEffect"), lp.byte("LightHue"), lp.byte("LightSaturation")))
            }
            _ => (String::new(), String::new()),
        };
        println!("  {} ({class}) bits={nb} visible_verts={lit} applied={applied} {extra} {corr}", pkg.ref_name(actor));
    }
    println!("end at {} of {end}", r2.pos());
    // First colours for reference.
    let d = &pkg.data[start..start + 64];
    println!("head: {:02x?}", d);
}
