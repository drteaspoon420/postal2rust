//! Dumps zone ambient settings and the terrain's saved zone: `cargo run -p upkg --example zone_dump -- MAP...`
use upkg::lighting::read_zones;
use upkg::Package;

fn main() {
    let dir = std::env::var("P2_MAPS").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete\Maps".into());
    for m in std::env::args().skip(1) {
        let path = format!(r"{dir}\{m}.fuk");
        let Ok(pkg) = Package::open(&path) else {
            println!("{m}: cannot open");
            continue;
        };
        println!("== {m}");
        for z in read_zones(&pkg) {
            println!(
                "  {:<14} exp {:>5} tag {:<16} amb b{:>3} h{:>3} s{:>3} terrain {} sky {} fog {} loc {:?}",
                z.class,
                z.export,
                z.tag.as_deref().unwrap_or("-"),
                z.ambient_brightness,
                z.ambient_hue,
                z.ambient_saturation,
                z.terrain_zone as u8,
                z.sky_zone as u8,
                z.distance_fog as u8,
                z.location.map(|v| v.round())
            );
        }
        for i in 0..pkg.exports.len() {
            if pkg.export_class(i).eq_ignore_ascii_case("TerrainInfo") {
                if let Ok((_, p)) = upkg::props::begin_object(&pkg, i) {
                    println!("  TerrainInfo exp {i} region {:?}", p.get("Region"));
                }
            }
        }
    }
}
