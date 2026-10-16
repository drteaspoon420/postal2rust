//! Validator for upkg::lightmap: rebuilds every lightmap of a map from its saved light
//! visibility and compares against the saved atlas texels (maps whose atlases have data).
//! Usage: lm_rebuild_probe <map path> [light_scale] [ambient_scale]

use std::collections::HashMap;
use upkg::lighting::{light_from_props, read_zones};
use upkg::lightmap::{build_lightmap, zone_ambient};
use upkg::model::{find_world_model, read_model};
use upkg::texture::{TexFormat, decode_raw};
use upkg::{ObjRef, Package, begin_object};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pkg = Package::open(&a[1]).unwrap();
    let ls: f32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(upkg::lightmap::LIGHT_SCALE);
    let amb_s: f32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(upkg::lightmap::AMBIENT_SCALE);
    let m = read_model(&pkg, find_world_model(&pkg).unwrap()).unwrap();
    let zones = read_zones(&pkg);
    let amb = |zi: i32| -> [f32; 3] {
        let r = m.zones.get(zi.max(0) as usize).copied().unwrap_or(ObjRef::Null);
        let z = match r {
            ObjRef::Export(e) => zones.iter().find(|z| z.export == e),
            _ => None,
        }
        .or_else(|| zones.iter().find(|z| z.class.eq_ignore_ascii_case("LevelInfo")));
        z.map(|z| zone_ambient(z.ambient_hue, z.ambient_saturation, z.ambient_brightness, amb_s)).unwrap_or([0.0; 3])
    };
    let mut cache: HashMap<ObjRef, Option<upkg::lighting::MapLight>> = HashMap::new();
    let mut light = |r: ObjRef| -> Option<upkg::lighting::MapLight> {
        cache
            .entry(r)
            .or_insert_with(|| match r {
                ObjRef::Export(e) => begin_object(&pkg, e).ok().and_then(|(_, p)| light_from_props(pkg.export_class(e), e, &p)),
                _ => None,
            })
            .clone()
    };
    let (mut so, mut sr, mut soo, mut srr, mut sor, mut n) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    let mut hist = [0u32; 8];
    let mut cls = [(0f64, 0f64, 0f64); 3];
    for (ti, t) in m.light_map_textures.iter().enumerate() {
        let Some(d) = t.mips.first() else { continue };
        let Ok(saved) = decode_raw(TexFormat::from_byte(t.format), d, t.width, t.height) else { continue };
        let mut mine = vec![0u8; saved.len()];
        for lm in m.light_maps.iter().filter(|l| l.texture == ti as i32) {
            // LM_PROBE_SPOT=1: only lightmaps lit by a spotlight (and nothing but spotlights for =2).
            if let Ok(s) = std::env::var("LM_PROBE_SPOT") {
                let kinds: Vec<bool> = lm.lights.iter().filter_map(|b| light(b.actor)).map(|l| l.kind == upkg::lighting::LightKind::Spot).collect();
                if !kinds.iter().any(|k| *k) || (s == "2" && !kinds.iter().all(|k| *k)) {
                    continue;
                }
            }
            build_lightmap(&m, lm, &mut mine, t.width, &mut light, &amb, ls);
            // Per-lightmap mean luminance.
            let (mut a, mut b, mut c) = (0f64, 0f64, 0f64);
            for y in 0..lm.size[1] {
                for x in 0..lm.size[0] {
                    let o = (((lm.offset[1] + y) as u32 * t.width + (lm.offset[0] + x) as u32) * 4) as usize;
                    if o + 3 >= saved.len() {
                        continue;
                    }
                    let (sv, mv) = ((saved[o] as f64 + saved[o + 1] as f64 + saved[o + 2] as f64) / 3.0, (mine[o] as f64 + mine[o + 1] as f64 + mine[o + 2] as f64) / 3.0);
                    a += sv;
                    b += mv;
                    // Texel classes by the saved value: shadow (< 12), mid, lit (> 60).
                    let k = if sv < 12.0 { 0 } else if sv <= 60.0 { 1 } else { 2 };
                    cls[k].0 += sv;
                    cls[k].1 += mv;
                    cls[k].2 += 1.0;
                    c += 1.0;
                }
            }
            if c == 0.0 {
                continue;
            }
            let (o, r) = (a / c, b / c);
            so += o;
            sr += r;
            soo += o * o;
            srr += r * r;
            sor += o * r;
            n += 1.0;
            let ratio = if o > 2.0 { r / o } else { 1.0 };
            hist[((ratio * 4.0) as usize).min(7)] += 1;
        }
        if ti == 0 {
            let sa: f64 = saved.chunks_exact(4).map(|p| p[2] as f64).sum::<f64>() / (saved.len() / 4) as f64;
            let ma: f64 = mine.chunks_exact(4).map(|p| p[2] as f64).sum::<f64>() / (mine.len() / 4) as f64;
            println!("atlas0 mean B saved {sa:.1} rebuilt {ma:.1}; saved first texel {:?}", &saved[..4]);
        }
    }
    for (k, name) in ["shadow<12", "mid", "lit>60"].iter().enumerate() {
        let (s, r, c) = cls[k];
        println!("  texels {name}: {c:.0} saved {:.1} rebuilt {:.1}", s / c.max(1.0), r / c.max(1.0));
    }
    let cov = sor / n - (so / n) * (sr / n);
    let corr = cov / ((soo / n - (so / n).powi(2)).sqrt() * (srr / n - (sr / n).powi(2)).sqrt());
    println!("{} lightmaps: saved mean {:.1} rebuilt {:.1} corr {corr:.3} ratio hist (x0.25 bins) {hist:?}", n, so / n, sr / n);
}
