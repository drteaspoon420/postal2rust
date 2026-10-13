//! Lists the static mesh actors the scene builder assigns to the sky zone, plus main-world
//! instances whose bounds overlap the sky zone box. Research aid for sky rendering.
//! Usage: sky_probe <map name> [game dir]

use bevy::math::Vec3;
use p2view::scene::{Options, build_scene};
use std::sync::Arc;
use upkg::library::Library;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let game = a.get(2).cloned().unwrap_or_else(|| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let lib = Library::new(&game).unwrap();
    let path = lib.map_path(&a[1]).expect("map");
    let map = Arc::new(upkg::Package::open(&path).unwrap());
    let data = build_scene(&lib, &map, &Options { gpu_bc: false, projectors: true }).unwrap();
    let Some(sky) = &data.sky else {
        println!("no sky");
        return;
    };
    println!("sky origin {:?}, {} bsp batches, {} instances", sky.origin, sky.world.len(), sky.instances.len());
    if let Some(s) = &data.sun {
        println!("sun dir {:?} color {:?} brightness {}", s.direction, s.color, s.brightness);
    }
    if let Some(z) = &data.zone {
        println!("zone ambient {:?} x{} dyn {:?} x{} fog {:?}", z.ambient, z.ambient_brightness, z.dynamic_ambient, z.dynamic_ambient_brightness, z.fog);
    }
    println!("spawn {:?}", data.spawn);
    for (i, t) in data.terrain.iter().enumerate() {
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &t.mesh.positions {
            lo = lo.min((*p).into());
            hi = hi.max((*p).into());
        }
        println!("terrain layer {i} order {} bounds {lo:.0?}..{hi:.0?}", t.order);
    }
    // Static mesh instances near the spawn with their baked light statistics.
    for inst in &data.instances {
        let p = inst.transform.w_axis.truncate();
        if p.distance(data.spawn.0) < 25.0 {
            let e = inst.export;
            let stats = inst.light.as_ref().map(|c| {
                let n = c.len().max(1) as f32;
                let s = c.iter().fold([0f32; 4], |a, v| [a[0] + v[0] as f32, a[1] + v[1] as f32, a[2] + v[2] as f32, a[3] + v[3] as f32]);
                [s[0] / n, s[1] / n, s[2] / n, s[3] / n]
            });
            let tex = data.materials[inst.material as usize].texture.map(|t| data.textures[t as usize].name.clone());
            println!("  near: {} {} d={:.1} light={:?} tex={:?}", e, map.name(map.exports[e].name), p.distance(data.spawn.0), stats, tex);
        }
    }
    for inst in &sky.instances {
        let m0 = &data.meshes[inst.mesh as usize];
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &m0.positions {
            let w = inst.transform.transform_point3((*p).into());
            lo = lo.min(w);
            hi = hi.max(w);
        }
        println!("    bounds {lo:.0?}..{hi:.0?} (sky origin {:.0?})", sky.origin);
        let e = inst.export;
        let m = &data.materials[inst.material as usize];
        let tex = m.texture.map(|t| (data.textures[t as usize].name.clone(), data.textures[t as usize].format));
        println!("  sky actor {} {} ({}) at {:?} blend={:?} two_sided={} tex={:?}", e, map.name(map.exports[e].name), map.export_class(e), inst.transform.w_axis, m.blend, m.two_sided, tex);
    }
    for m in &sky.world {
        let mat = &data.materials[m.material as usize];
        let tex = mat.texture.map(|t| data.textures[t as usize].name.clone());
        println!("  sky bsp batch tex={tex:?} blend={:?} tris={} lightmap={:?}", mat.blend, m.indices.len() / 3, m.lightmap);
    }
    // Terrain decoration materials (first instance of each).
    let mut seen = std::collections::HashSet::new();
    for d in &data.deco {
        if seen.insert(d.material) {
            let m = &data.materials[d.material as usize];
            let tex = m.texture.map(|t| (data.textures[t as usize].name.clone(), data.textures[t as usize].format));
            println!("  deco material {} blend={:?} two_sided={} tex={:?} mesh colors={}", d.material, m.blend, m.two_sided, tex, data.meshes[d.mesh as usize].colors.len());
        }
    }
    // Main-world instances whose bounds overlap the sky zone's BSP bounds.
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for m in &sky.world {
        for p in &m.positions {
            lo = lo.min((*p).into());
            hi = hi.max((*p).into());
        }
    }
    println!("sky bsp bounds {lo:?} .. {hi:?}");
    for inst in &data.instances {
        let m = &data.meshes[inst.mesh as usize];
        let (mut a, mut b) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &m.positions {
            let w = inst.transform.transform_point3((*p).into());
            a = a.min(w);
            b = b.max(w);
        }
        let far = a.length() > 1500.0 || b.length() > 1500.0;
        if (a.cmple(hi).all() && b.cmpge(lo).all()) || far {
            let e = inst.export;
            println!("  {}: {} {} bounds {a:.0?}..{b:.0?} mat={}", if far { "far" } else { "overlaps sky" }, e, map.name(map.exports[e].name), inst.material);
        }
    }
}
