//! Explores TerrainInfo native data (after the tagged properties): prints the first bytes and
//! tries to find arrays whose element count matches the terrain's vertex count (baked lighting).
//! Usage: terrain_native <map path>

use upkg::{Package, begin_object};

fn main() {
    let path = std::env::args().nth(1).expect("map");
    let pkg = Package::open(&path).unwrap();
    for i in pkg.exports_of_class("TerrainInfo").collect::<Vec<_>>() {
        let (r, _props) = begin_object(&pkg, i).unwrap();
        let start = r.pos();
        let end = pkg.exports[i].offset + pkg.exports[i].size;
        println!("== {} native {} bytes at {start}", pkg.name(pkg.exports[i].name), end - start);
        let d = &pkg.data[start..end];
        for (k, c) in d.chunks(16).take(24).enumerate() {
            let hex: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
            println!("{:6}: {}", k * 16, hex.join(" "));
        }
        // Scan for int32 counts followed by that many 4-byte items fitting before the end.
        let mut found = 0;
        for off in (0..d.len().saturating_sub(4)).step_by(1) {
            let n = i32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]]);
            if [16641, 16384, 66049, 65536, 4225, 4096].contains(&n) {
                println!("  candidate count {n} at +{off}");
                found += 1;
                if found > 20 {
                    break;
                }
            }
        }
    }
}
