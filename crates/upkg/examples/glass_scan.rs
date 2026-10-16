//! `glass_scan [game dir]`: every Shader in the content packages whose Opacity is its Diffuse,
//! with the alpha classification material_texture uses (translucent glass -> blended, else
//! alpha-tested). Audit for FIDELITY #56 (AW door glass drawn as solid white panes).
use upkg::library::{CONTENT_DIRS, Library, translucent_alpha};
use upkg::{ObjRef, begin_object};

fn main() {
    let game = std::env::args().nth(1).unwrap_or_else(|| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let lib = Library::new(&game).unwrap();
    let (mut n, mut blended) = (0, 0);
    for dir in CONTENT_DIRS.iter().filter(|d| **d != "Maps" && **d != "System" && **d != "Sounds") {
        let Ok(rd) = std::fs::read_dir(std::path::Path::new(&game).join(dir)) else { continue };
        for e in rd.flatten() {
            let stem = e.path().file_stem().unwrap().to_string_lossy().to_string();
            let Some(pkg) = lib.package(&stem) else { continue };
            for i in 0..pkg.exports.len() {
                if !pkg.export_class(i).eq_ignore_ascii_case("Shader") {
                    continue;
                }
                let Ok((_, p)) = begin_object(&pkg, i) else { continue };
                let op = p.object("Opacity").filter(|o| *o != ObjRef::Null);
                if op.is_none() || op != p.object("Diffuse") || p.byte("OutputBlending").is_some_and(|b| b != 0) {
                    continue;
                }
                let tex = op.and_then(|o| lib.resolve(&pkg, o)).and_then(|o| lib.material_texture(&o)).and_then(|m| m.texture);
                let t = tex.as_ref().is_some_and(translucent_alpha);
                n += 1;
                blended += t as usize;
                println!("{} {}.{} tex {:?}", if t { "BLEND" } else { "mask " }, pkg.name, pkg.name(pkg.exports[i].name), tex);
            }
        }
    }
    println!("{n} self-opacity shaders, {blended} now blended");
}
