//! Exploration helper: parses a Model's vectors/points/nodes with the layout inferred from data,
//! checks index ranges, then hex-dumps what follows so the next array can be identified.
//! Run: cargo run --release -p upkg --example model_probe -- <map> <ModelName>

use upkg::{Package, Reader, begin_object};

#[derive(Debug)]
#[allow(dead_code)]
struct Node {
    plane: [f32; 4],
    vert_pool: i32,
    surf: i32,
    back: i32,
    front: i32,
    num_verts: u8,
    first_vertex: i32,
}

fn node(r: &mut Reader) -> upkg::Result<Node> {
    let plane = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    let _zone_mask = r.u64()?;
    let _flags = r.u8()?;
    r.skip(28)?;
    let vert_pool = r.compact()?;
    let surf = r.compact()?;
    let back = r.compact()?;
    let front = r.compact()?;
    let _iplane = r.compact()?;
    let _coll = r.compact()?;
    let _render = r.compact()?;
    r.skip(16)?; // sphere
    r.skip(16)?; // unknown
    let _z0 = r.u8()?;
    let _z1 = r.u8()?;
    let num_verts = r.u8()?;
    let _leaf0 = r.i32()?;
    let _leaf1 = r.i32()?;
    let _section = r.i32()?;
    let first_vertex = r.i32()?;
    let _lightmap = r.i32()?;
    Ok(Node { plane, vert_pool, surf, back, front, num_verts, first_vertex })
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let i = pkg.find_export(&a[1], Some("Model")).expect("model not found");
    let (mut r, _props) = begin_object(&pkg, i).unwrap();
    r.skip(25 + 16).unwrap(); // bounding box + sphere
    let vectors = r.array(|r| r.vec3()).unwrap();
    let points = r.array(|r| r.vec3()).unwrap();
    let nodes = r.array(node).unwrap();
    let max_surf = nodes.iter().map(|n| n.surf).max().unwrap_or(-1);
    let max_pool = nodes.iter().map(|n| n.vert_pool + n.num_verts as i32).max().unwrap_or(0);
    let bad_links = nodes.iter().filter(|n| n.back >= nodes.len() as i32 || n.front >= nodes.len() as i32).count();
    let unit_planes = nodes.iter().filter(|n| {
        let l = (n.plane[0].powi(2) + n.plane[1].powi(2) + n.plane[2].powi(2)).sqrt();
        (l - 1.0).abs() < 0.01
    }).count();
    println!(
        "vectors={} points={} nodes={} unit-normal planes={} bad child links={bad_links} max surf={max_surf} max vertpool end={max_pool}",
        vectors.len(),
        points.len(),
        nodes.len(),
        unit_planes
    );
    let surfs = r
        .array(|r| {
            let mat = r.compact()?;
            let flags = r.u32()?;
            let base = r.compact()?;
            let normal = r.compact()?;
            let tu = r.compact()?;
            let tv = r.compact()?;
            let brush_poly = r.compact()?;
            let actor = r.compact()?;
            let plane = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
            let lm_scale = r.f32()?;
            Ok((mat, flags, base, normal, tu, tv, brush_poly, actor, plane, lm_scale))
        })
        .unwrap();
    let bad = surfs
        .iter()
        .filter(|s| s.2 as usize >= points.len() || s.3 as usize >= vectors.len() || s.4 as usize >= vectors.len() || s.5 as usize >= vectors.len())
        .count();
    println!("surfs={} with out-of-range point/vector refs={bad}", surfs.len());
    for s in surfs.iter().take(3) {
        println!("  surf mat={} ({}) flags={:#x} base={} n={} u={} v={} lm={}", s.0, pkg.full_path(upkg::ObjRef::from_raw(s.0)), s.1, s.2, s.3, s.4, s.5, s.9);
    }
    // Try candidate vert record layouts; the right one keeps every point index in range.
    let vert_start = r.clone();
    {
        let mut h = vert_start.clone();
        let raw = h.bytes(96.min(h.remaining())).unwrap();
        for (row, c) in raw.chunks(16).enumerate() {
            let x: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
            println!("  vert-start +{:04x} {}", row * 16, x.join(" "));
        }
    }
    for (label, extra) in [("compact,compact", 0usize), ("compact,compact,+8", 8), ("compact,compact,+16", 16)] {
        let mut t = vert_start.clone();
        let res = t.array(|r| {
            let p = r.compact()?;
            let _side = r.compact()?;
            r.skip(extra)?;
            Ok(p)
        });
        match res {
            Ok(v) => {
                let bad_v = v.iter().filter(|&&p| p < 0 || p as usize >= points.len()).count();
                println!("verts[{label}]={} out-of-range={bad_v} (node vertpool end {max_pool}) next={}", v.len(), t.pos());
                if bad_v == 0 {
                    r = t;
                }
            }
            Err(e) => println!("verts[{label}] failed: {e}"),
        }
    }
    println!("next at {}", r.pos());
    let b = r.bytes(64.min(r.remaining())).unwrap();
    for (row, c) in b.chunks(16).enumerate() {
        let h: Vec<String> = c.iter().map(|x| format!("{x:02x}")).collect();
        println!("  +{:04x} {}", row * 16, h.join(" "));
    }
}
