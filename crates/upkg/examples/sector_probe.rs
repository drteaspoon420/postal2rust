//! Dumps TerrainSector exports of a map: header ints and the size of the trailing native data.
//! Research aid for saved terrain lighting (FIDELITY #10).
//! Usage: sector_probe <map path> [sector index to hex dump]

use upkg::{Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pkg = Package::open(&a[1]).unwrap();
    let want: Option<usize> = a.get(2).and_then(|s| s.parse().ok());
    let secs: Vec<usize> = pkg.exports_of_class("TerrainSector").collect();
    println!("{} TerrainSector exports", secs.len());
    for (k, &i) in secs.iter().enumerate() {
        let (r, _props) = begin_object(&pkg, i).unwrap();
        let start = r.pos();
        let end = pkg.exports[i].offset + pkg.exports[i].size;
        let d = &pkg.data[start..end];
        let int = |o: usize| i32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
        if k < 4 || want == Some(k) || k % 97 == 0 {
            let mut r = upkg::Reader::new(d);
            let _terrain = r.compact().unwrap();
            let h: Vec<i32> = (0..4).map(|_| r.i32().unwrap()).collect();
            for _ in 0..25 {
                r.u8().unwrap();
            }
            let n = r.count().unwrap();
            let mut ls = Vec::new();
            for _ in 0..n {
                let o = upkg::ObjRef::from_raw(r.compact().unwrap());
                let bytes = r.array(|r| r.u8()).unwrap();
                let set: u32 = bytes.iter().map(|b| b.count_ones()).sum();
                ls.push(format!("{}({}) {}/{}", pkg.full_path(o), pkg.class_of(o), set, bytes.len() * 8));
            }
            println!("sector {k} export {i} hdr {h:?} rest {} lights {ls:?}", d.len() - r.pos());
            println!("  first ints: {:?}", (0..d.len() / 4).take(12).map(|j| int(j * 4)).collect::<Vec<_>>());
        }
        if want == Some(k) {
            for (n, c) in d.chunks(32).take(40).enumerate() {
                let hex: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
                println!("{:6}: {}", n * 32, hex.join(" "));
            }
        }
    }
}
