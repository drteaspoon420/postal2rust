//! `decal_probe [map ...]`: builds every level Projector's decal (as p2fx does at runtime) with
//! the current placement and with the legacy one (P2FX_DECAL_LEGACY semantics: centre-ray depth,
//! every surface, DrawScale only) and prints per projector: tris new/old, the decal's projected
//! extent coverage (fraction of the box's width/height spanned by the generated polygons), and
//! flags (0 tris, coverage drop). No map args = every map in the install. Read-only.
use bevy::math::Vec3;
use p2fx::decal::DecalSpec;
use std::sync::Arc;
use upkg::{ObjRef, Package, begin_object};

/// Share (0..1) of the texture's visible texels (alpha > 16) that the decal polygons cover:
/// UV triangles rasterised on a 64x64 grid (union, so overlapping pieces count once).
fn coverage(spec: &DecalSpec, b: &p2fx::geom::Batch) -> (f32, f32) {
    const N: usize = 64;
    let mut hit = vec![false; N * N];
    for t in b.indices.chunks(3) {
        let p: Vec<[f32; 2]> = t.iter().map(|&i| b.uv0[i as usize]).collect();
        let (lo_u, hi_u) = (p.iter().map(|q| q[0]).fold(f32::MAX, f32::min), p.iter().map(|q| q[0]).fold(f32::MIN, f32::max));
        let (lo_v, hi_v) = (p.iter().map(|q| q[1]).fold(f32::MAX, f32::min), p.iter().map(|q| q[1]).fold(f32::MIN, f32::max));
        let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
        if area.abs() < 1e-9 {
            continue;
        }
        for y in ((lo_v * N as f32).floor().max(0.0) as usize)..((hi_v * N as f32).ceil().min(N as f32) as usize) {
            for x in ((lo_u * N as f32).floor().max(0.0) as usize)..((hi_u * N as f32).ceil().min(N as f32) as usize) {
                let q = [(x as f32 + 0.5) / N as f32, (y as f32 + 0.5) / N as f32];
                let e = |a: [f32; 2], c: [f32; 2]| (c[0] - a[0]) * (q[1] - a[1]) - (c[1] - a[1]) * (q[0] - a[0]);
                let (e0, e1, e2) = (e(p[0], p[1]), e(p[1], p[2]), e(p[2], p[0]));
                if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0) {
                    hit[y * N + x] = true;
                }
            }
        }
    }
    let Some(t) = spec.texture.as_ref() else { return (hit.iter().filter(|h| **h).count() as f32 / (N * N) as f32, 0.0) };
    let (mut vis, mut cov) = (0u32, 0u32);
    for y in 0..N {
        for x in 0..N {
            let (tx, ty) = (x * t.width as usize / N, y * t.height as usize / N);
            let a = t.rgba.get((ty * t.width as usize + tx) * 4 + 3).copied().unwrap_or(255);
            if a > 16 {
                vis += 1;
                if hit[y * N + x] {
                    cov += 1;
                }
            }
        }
    }
    (hit.iter().filter(|h| **h).count() as f32 / (N * N) as f32, if vis > 0 { cov as f32 / vis as f32 } else { 0.0 })
}

fn main() {
    let game = "G:\\SteamLibrary\\steamapps\\common\\POSTAL2Complete";
    let fx = p2fx::FxLibrary::new(game).expect("library");
    let mut maps: Vec<String> = std::env::args().skip(1).collect();
    if maps.is_empty() {
        let mut v: Vec<String> = std::fs::read_dir(format!("{game}\\Maps")).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk"))).map(|p| p.file_stem().unwrap().to_string_lossy().to_string()).collect();
        v.sort();
        maps = v;
    }
    let (mut n_all, mut n_zero_new, mut n_zero_old, mut n_wider, mut n_narrower) = (0, 0, 0, 0, 0);
    for m in maps {
        let Some(path) = fx.lib.map_path(&m) else { continue };
        let Ok(map) = Package::open(&path) else { continue };
        let map = Arc::new(map);
        fx.lib.insert(map.clone());
        let projs: Vec<usize> = map.exports_of_class("Projector").collect();
        if projs.is_empty() {
            continue;
        }
        let Ok(world) = p2collide::World::build_with(&fx.lib, &map, p2collide::BuildOptions { skip_movers: true, skip_dynamic: true }) else { continue };
        for i in projs {
            let Ok((_, p)) = begin_object(&map, i) else { continue };
            let Some(mat) = p.object("ProjTexture").filter(|o| *o != ObjRef::Null) else { continue };
            let Some(mobj) = fx.lib.resolve(&map, mat) else { continue };
            let tex = fx.lib.material_texture(&mobj).and_then(|mi| mi.texture).and_then(|t| fx.texture(&t));
            let (w, h) = tex.as_ref().map(|t| (t.width as f32, t.height as f32)).unwrap_or((256.0, 256.0));
            let ds = p.float("DrawScale").unwrap_or(1.0);
            let ds3 = p.vector("DrawScale3D").map(Vec3::from).unwrap_or(Vec3::ONE);
            let flag = |n: &str, d: bool| p.bool(n).unwrap_or(d);
            let spec = DecalSpec {
                location: Vec3::from(p.vector("Location").unwrap_or([0.0; 3])),
                rotation: p.rotator("Rotation").unwrap_or([0; 3]),
                half_w: w * ds * ds3.y.abs() * 0.5,
                half_h: h * ds * ds3.z.abs() * 0.5,
                spread: p.int("FOV").map(|f| f as f32).filter(|f| *f > 1.0 && std::env::var_os("P2FX_PROJ_ORTHO").is_none()).map_or(0.0, |f| (f.min(179.0).to_radians() * 0.5).tan()),
                depth: p.int("MaxTraceDistance").map(|d| d as f32).filter(|d| *d > 0.0).unwrap_or(1000.0),
                texture: tex.clone(),
                style: upkg::emitter::DrawStyle::AlphaBlend,
                targets: p2collide::TraceFlags { world: flag("bProjectBSP", true), actors: flag("bProjectStaticMesh", true), terrain: flag("bProjectTerrain", true) },
            };
            let new = spec.build(&world);
            // P2FX_PROBE_GRID=Name: depth (along the projector axis) of the first surface each
            // source offers, on a 7x7 grid over the box.
            if std::env::var("P2FX_PROBE_GRID").is_ok_and(|g| g.eq_ignore_ascii_case(map.name(map.exports[i].name))) {
                let m3 = p2fx::sim::rotator_matrix(spec.rotation);
                for (label, fl) in [("terrain", p2collide::TraceFlags { world: false, actors: false, terrain: true }), ("bsp", p2collide::TraceFlags { world: true, actors: false, terrain: false }), ("mesh", p2collide::TraceFlags { world: false, actors: true, terrain: false })] {
                    println!("  grid {label} (rows = up axis +..-, depth uu, '-' = none within 300)");
                    for b in [1.0f32, 0.66, 0.33, 0.0, -0.33, -0.66, -1.0] {
                        let row: Vec<String> = [-1.0f32, -0.66, -0.33, 0.0, 0.33, 0.66, 1.0]
                            .iter()
                            .map(|&a| {
                                let o = spec.location + m3.y_axis * (a * spec.half_w) + m3.z_axis * (b * spec.half_h);
                                world.trace(o - m3.x_axis * 300.0, o + m3.x_axis * 300.0, &p2collide::Shape::POINT, fl).map_or("   -".into(), |h| format!("{:4.0}", (h.location - spec.location).dot(m3.x_axis)))
                            })
                            .collect();
                        println!("   {}", row.join(" "));
                    }
                }
            }
            let mut old_spec = spec.clone();
            old_spec.half_w = w * ds * 0.5;
            old_spec.half_h = h * ds * 0.5;
            // SAFETY: single-threaded probe; the env var only switches build() placement.
            unsafe { std::env::set_var("P2FX_DECAL_LEGACY", "1") };
            let old = old_spec.build(&world);
            unsafe { std::env::remove_var("P2FX_DECAL_LEGACY") };
            let tn = new.as_ref().map_or(0, |b| b.indices.len() / 3);
            let to = old.as_ref().map_or(0, |b| b.indices.len() / 3);
            let cn = new.as_ref().map_or((0.0, 0.0), |b| coverage(&spec, b));
            let co = old.as_ref().map_or((0.0, 0.0), |b| coverage(&old_spec, b));
            n_all += 1;
            let mut note = String::new();
            if tn == 0 {
                n_zero_new += 1;
                note += " ZERO-NEW";
            }
            if to == 0 {
                n_zero_old += 1;
            }
            let area = |c: (f32, f32)| c.1;
            if area(cn) > area(co) + 0.02 {
                n_wider += 1;
                note += " WIDER";
            }
            if area(cn) + 0.02 < area(co) {
                n_narrower += 1;
                note += " NARROWER";
            }
            println!(
                "{m} {} tex {} ds {ds} ds3 {:?} rot {:?} mtd {} bsp/sm/ter {}/{}/{} tris new {tn} old {to} box new {:.2} old {:.2} art new {:.2} old {:.2}{note}",
                map.name(map.exports[i].name),
                tex.as_ref().map_or("-".into(), |t| t.name.clone()),
                ds3.to_array(),
                spec.rotation,
                spec.depth,
                spec.targets.world as u8,
                spec.targets.actors as u8,
                spec.targets.terrain as u8,
                cn.0,
                co.0,
                cn.1,
                co.1
            );
        }
    }
    println!("SUMMARY projectors {n_all}, zero tris new {n_zero_new} (old {n_zero_old}), coverage wider {n_wider}, narrower {n_narrower}");
}
