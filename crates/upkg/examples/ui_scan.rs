//! Decodes every Font and every material-graph object (Shader, Combiner, FinalBlend, Tex* modifiers,
//! ConstantColor, ...) in the given directories, and checks that their references resolve.
//! Run: cargo run --release -p upkg --example ui_scan -- <game root>
//! With `--fonts`, also lists every font with its glyph count, pages and height.

use std::collections::BTreeMap;
use upkg::font::read_font;
use upkg::library::Library;
use upkg::material::{Material, read_material};
use upkg::{ObjRef, Package};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = args.first().cloned().unwrap_or_else(|| std::env::var("POSTAL2_DIR").expect("pass the game root"));
    let list_fonts = args.iter().any(|a| a == "--fonts");
    let lib = std::sync::Arc::new(Library::new(&root).expect("game root"));
    let t0 = std::time::Instant::now();
    let (mut pkgs, mut fonts_ok, mut fonts_bad, mut glyphs, mut pages, mut pages_unresolved) = (0, 0, 0, 0usize, 0usize, 0usize);
    let mut remapped = 0;
    let mut kern_hist: BTreeMap<i32, usize> = BTreeMap::new();
    let mut mats: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let (mut refs, mut refs_unresolved) = (0usize, 0usize);
    let mut first_err: BTreeMap<String, String> = BTreeMap::new();
    for dir in upkg::library::CONTENT_DIRS {
        let Ok(rd) = std::fs::read_dir(std::path::Path::new(&root).join(dir)) else { continue };
        let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let Ok(pkg) = Package::open(&p) else { continue };
            pkgs += 1;
            let pkg = std::sync::Arc::new(pkg);
            for i in 0..pkg.exports.len() {
                let class = pkg.export_class(i).to_string();
                if class == "Font" {
                    match read_font(&pkg, i) {
                        Ok(f) => {
                            fonts_ok += 1;
                            glyphs += f.chars.iter().filter(|c| c.u_size > 0).count();
                            pages += f.pages.len();
                            remapped += f.is_remapped as usize;
                            *kern_hist.entry(f.kerning).or_default() += 1;
                            for pg in &f.pages {
                                if lib.resolve(&pkg, *pg).is_none_or(|o| o.class() != "Texture") {
                                    pages_unresolved += 1;
                                }
                            }
                            if list_fonts {
                                let printable = (32u8..127).filter(|c| f.glyph(*c as char).is_some_and(|g| g.u_size > 0)).count();
                                println!(
                                    "FONT {} chars={} printable={printable}/95 pages={} height={} kerning={} remapped={}",
                                    pkg.full_path(ObjRef::Export(i)),
                                    f.chars.len(),
                                    f.pages.len(),
                                    f.max_height(),
                                    f.kerning,
                                    f.is_remapped
                                );
                            }
                        }
                        Err(e) => {
                            fonts_bad += 1;
                            first_err.entry("Font".into()).or_insert(format!("{}: {e}", pkg.full_path(ObjRef::Export(i))));
                        }
                    }
                } else if Material::CLASSES.contains(&class.as_str()) {
                    let ent = mats.entry(class.clone()).or_default();
                    match read_material(&pkg, i) {
                        Ok(m) => {
                            ent.0 += 1;
                            for r in m.inputs() {
                                if r == ObjRef::Null {
                                    continue;
                                }
                                refs += 1;
                                if lib.resolve(&pkg, r).is_none() {
                                    refs_unresolved += 1;
                                    first_err.entry(format!("{class} ref")).or_insert(format!("{} -> {}", pkg.full_path(ObjRef::Export(i)), pkg.full_path(r)));
                                }
                            }
                        }
                        Err(e) => {
                            ent.1 += 1;
                            first_err.entry(class.clone()).or_insert(format!("{}: {e}", pkg.full_path(ObjRef::Export(i))));
                        }
                    }
                }
            }
        }
    }
    for (c, (ok, bad)) in &mats {
        println!("material {c:<16} ok={ok:6} failed={bad}");
    }
    println!("font kerning values: {kern_hist:?}");
    for (k, v) in &first_err {
        println!("first problem [{k}]: {v}");
    }
    let mat_ok: usize = mats.values().map(|v| v.0).sum();
    let mat_bad: usize = mats.values().map(|v| v.1).sum();
    println!(
        "UI SCAN: packages={pkgs} fonts ok={fonts_ok} failed={fonts_bad} (glyphs={glyphs} pages={pages} unresolved_pages={pages_unresolved} remapped={remapped}); materials ok={mat_ok} failed={mat_bad}; material refs={refs} unresolved={refs_unresolved}; {:.2}s",
        t0.elapsed().as_secs_f64()
    );
}
