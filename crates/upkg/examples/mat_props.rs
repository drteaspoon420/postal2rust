//! Research probe: properties and texture alpha stats of named material exports.
//! Usage: mat_props <Package> <Name>...
use upkg::library::Library;
use upkg::{ObjRef, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let lib = Library::new("G:\\SteamLibrary\\steamapps\\common\\POSTAL2Complete").unwrap();
    let pkg = lib.package(&a[0]).expect("package");
    for want in &a[1..] {
        for i in 0..pkg.exports.len() {
            if !pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(want) {
                continue;
            }
            let (_, p) = begin_object(&pkg, i).unwrap();
            println!("{want} ({})", pkg.export_class(i));
            for q in &p.0 {
                let s = format!("{:?}", q.value);
                println!("  {} = {}", q.name, &s[..s.len().min(80)]);
                if let Some(r) = p.object(&q.name).filter(|r| *r != ObjRef::Null) {
                    if let Some(o) = lib.resolve(&pkg, r) {
                        let mut extra = String::new();
                        if o.class() == "Texture" {
                            if let Ok(t) = upkg::texture::read_texture(&o.pkg, o.export) {
                                if let Ok((_, _, px)) = t.decode_rgba(0) {
                                    let n = (px.len() / 4).max(1) as f32;
                                    let lo = px.chunks(4).filter(|c| c[3] < 128).count() as f32;
                                    extra = format!(" fmt {:?} alpha<128 {:.0}%", t.format, 100.0 * lo / n);
                                }
                            }
                        }
                        println!("    -> {}.{} ({}){extra}", o.pkg.name, o.name(), o.class());
                    }
                }
            }
        }
    }
}
