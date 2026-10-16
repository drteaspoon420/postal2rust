//! Terrain layers of one map: texture, alpha map, scale and alpha coverage; writes each alpha map
//! as out_dir/layer<N>.png (grey) when out_dir is given.
//! Usage: terrain_layers <map path> [out_dir]
use std::sync::Arc;
use upkg::Package;
use upkg::library::Library;
use upkg::terrain::{find_terrains, read_alpha_map, read_terrain};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = std::path::Path::new(&a[0]);
    let out = a.get(1);
    let pkg = Arc::new(Package::open(map).unwrap());
    let lib = Library::new(map.parent().unwrap().parent().unwrap()).unwrap();
    lib.insert(pkg.clone());
    for ti in find_terrains(&pkg) {
        let t = read_terrain(&lib, &pkg, ti).unwrap();
        println!("terrain {} {}x{} layers {}", t.name, t.width, t.height, t.layers.len());
        for (k, l) in t.layers.iter().enumerate() {
            let tex = lib.resolve(&pkg, l.texture);
            let tname = tex.as_ref().map(|o| o.pkg.full_path(upkg::ObjRef::Export(o.export))).unwrap_or("-".into());
            let tinfo = tex.as_ref().and_then(|m| lib.material_texture(m));
            let alpha = lib.resolve(&pkg, l.alpha_map);
            let aname = alpha.as_ref().map(|o| o.pkg.full_path(upkg::ObjRef::Export(o.export))).unwrap_or("-".into());
            let stats = alpha.as_ref().map(|o| match read_alpha_map(o) {
                Ok((w, h, px)) => {
                    let n = px.len().max(1);
                    let mean = px.iter().map(|&v| v as u64).sum::<u64>() as f64 / n as f64;
                    let on = px.iter().filter(|&&v| v > 127).count() * 100 / n;
                    if let Some(d) = out {
                        let path = format!("{d}/layer{k}.pgm");
                        let mut f = format!("P5\n{w} {h}\n255\n").into_bytes();
                        f.extend_from_slice(&px);
                        std::fs::write(&path, f).ok();
                    }
                    format!("{w}x{h} mean {mean:.0} >127 {on}%")
                }
                Err(e) => format!("ERR {e}"),
            });
            println!(
                "  L{k} tex {tname} resolved_tex {} blended {} masked {} alpha {aname} {} scale {}x{} axis {} rot {}",
                tinfo.as_ref().and_then(|m| m.texture.as_ref()).is_some(),
                tinfo.as_ref().map(|m| m.blended).unwrap_or(false),
                tinfo.as_ref().map(|m| m.masked).unwrap_or(false),
                stats.unwrap_or("(none)".into()),
                l.u_scale,
                l.v_scale,
                l.map_axis,
                l.rotation
            );
        }
    }
}
