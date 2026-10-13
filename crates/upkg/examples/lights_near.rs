//! Research aid: lists light actors within a radius of a point (Unreal units) with their
//! properties. Usage: lights_near <map path> X Y Z RADIUS
use upkg::Package;
use upkg::lighting::read_lights;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pkg = Package::open(&a[1]).unwrap();
    let p: Vec<f32> = a[2..5].iter().map(|s| s.parse().unwrap()).collect();
    let r: f32 = a[5].parse().unwrap();
    for l in read_lights(&pkg) {
        let d = ((l.location[0] - p[0]).powi(2) + (l.location[1] - p[1]).powi(2) + (l.location[2] - p[2]).powi(2)).sqrt();
        if d <= r {
            println!(
                "{} {} {} at {:?} d {d:.0} type {:?} effect {} b {} hue {} sat {} radius {} ({:.0} uu) cone {} rot {:?} special {} dyn {}",
                l.export,
                pkg.name(pkg.exports[l.export].name),
                l.class,
                l.location,
                l.light_type,
                l.light_effect,
                l.brightness,
                l.hue,
                l.saturation,
                l.radius,
                l.world_radius(),
                l.cone,
                l.rotation,
                l.special_lit,
                l.dynamic
            );
        }
    }
}
