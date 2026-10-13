//! Decodes every TerrainInfo, light and zone in every map and reports totals.
//!
//! Usage: terrain_scan <game dir>

use upkg::Package;
use upkg::library::Library;
use upkg::lighting::{LightKind, read_lights, read_zones};
use upkg::terrain::{find_terrains, read_alpha_map, read_terrain};
use std::sync::Arc;

fn main() {
    let root = std::env::args().nth(1).expect("game dir");
    let lib = Library::new(&root).expect("library");
    let mut entries: Vec<_> = std::fs::read_dir(std::path::Path::new(&root).join("Maps")).unwrap().flatten().map(|e| e.path()).collect();
    entries.retain(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk")));
    entries.sort();
    let t0 = std::time::Instant::now();
    let (mut maps, mut maps_with_terrain, mut ok, mut bad) = (0, 0, 0, 0);
    let (mut layers, mut layers_ok, mut decos, mut verts) = (0usize, 0usize, 0usize, 0usize);
    let (mut lights, mut spots, mut suns, mut zones) = (0usize, 0usize, 0usize, 0usize);
    for p in entries {
        let Ok(pkg) = Package::open(&p) else { continue };
        let pkg = Arc::new(pkg);
        maps += 1;
        lib.insert(pkg.clone());
        let ls = read_lights(&pkg);
        lights += ls.len();
        spots += ls.iter().filter(|l| l.kind == LightKind::Spot).count();
        suns += ls.iter().filter(|l| l.kind == LightKind::Sun).count();
        zones += read_zones(&pkg).len();
        let ts = find_terrains(&pkg);
        if ts.is_empty() {
            continue;
        }
        maps_with_terrain += 1;
        let mut desc = Vec::new();
        for i in ts {
            match read_terrain(&lib, &pkg, i) {
                Ok(t) => {
                    ok += 1;
                    verts += (t.width * t.height) as usize;
                    decos += t.deco_layers.len();
                    for l in &t.layers {
                        layers += 1;
                        let tex_ok = lib.resolve(&pkg, l.texture).and_then(|m| lib.material_texture(&m)).and_then(|m| m.texture).is_some();
                        let alpha_ok = match lib.resolve(&pkg, l.alpha_map) {
                            Some(a) => read_alpha_map(&a).is_ok(),
                            None => l.alpha_map == upkg::ObjRef::Null,
                        };
                        if tex_ok && alpha_ok {
                            layers_ok += 1;
                        }
                    }
                    desc.push(format!("{}x{} L{} D{}", t.width, t.height, t.layers.len(), t.deco_layers.len()));
                }
                Err(e) => {
                    bad += 1;
                    desc.push(format!("FAILED {e}"));
                }
            }
        }
        println!("{:<24} {}  lights={}", pkg.name, desc.join(", "), ls.len());
    }
    println!(
        "TERRAIN SCAN: maps={maps} with_terrain={maps_with_terrain} terrains ok={ok} failed={bad} vertices={verts} layers={layers} layers_resolved={layers_ok} deco_layers={decos}"
    );
    println!("LIGHT SCAN: lights={lights} (spot={spots} sun={suns}) zones={zones} in {:.2}s", t0.elapsed().as_secs_f64());
}
