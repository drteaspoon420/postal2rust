//! Research aid: finds lightmap atlas trailers (format u8, width i32, height i32 with a power of two
//! size) in a map's world model export and prints their offsets, to check where the light map
//! array parser lands. Usage: lm_atlas_scan <map path>

use upkg::Package;
use upkg::model::find_world_model;

fn main() {
    let path = std::env::args().nth(1).expect("map path");
    let pkg = Package::open(&path).unwrap();
    let mi = find_world_model(&pkg).unwrap();
    let e = &pkg.exports[mi];
    let d = &pkg.data[e.offset..e.offset + e.size];
    let m = upkg::model::read_model(&pkg, mi).unwrap();
    println!("model export {} at {} size {} tail {} (abs {})", mi, e.offset, e.size, m.tail - e.offset, m.tail);
    for i in 0..d.len().saturating_sub(13) {
        let f = d[i];
        let w = i32::from_le_bytes(d[i + 1..i + 5].try_into().unwrap());
        let h = i32::from_le_bytes(d[i + 5..i + 9].try_into().unwrap());
        if (1..=12).contains(&f) && [64, 128, 256, 512, 1024].contains(&w) && [64, 128, 256, 512, 1024].contains(&h) {
            println!("  atlas trailer at +{i}: format {f} {w}x{h}");
        }
    }
}
