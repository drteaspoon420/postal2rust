//! Lists every sunlight in a map: export, name, brightness, hue/saturation, rotation, zone.
//! Usage: sun_list <map path>

fn main() {
    let path = std::env::args().nth(1).expect("map");
    let pkg = upkg::Package::open(&path).unwrap();
    let mut total = 0.0;
    for l in upkg::lighting::read_lights(&pkg).iter().filter(|l| l.kind == upkg::lighting::LightKind::Sun) {
        total += l.brightness;
        println!("{} {} b={} hue={} sat={} rot={:?} dir={:?}", l.export, pkg.name(pkg.exports[l.export].name), l.brightness, l.hue, l.saturation, l.rotation, l.direction());
    }
    println!("total {total}");
}
