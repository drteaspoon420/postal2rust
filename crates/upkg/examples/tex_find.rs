//! Lists Texture exports whose name contains a pattern, across every package in a directory.
//! Usage: tex_find <dir> <pattern> [out_dir]   (e.g. tex_find G:\...\Textures warn out\tex)
//! With `out_dir`, every matching Texture is also written there as `<path>.png`.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = a.first().expect("dir");
    let pat = a.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let out = a.get(2).map(std::path::PathBuf::from);
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("read dir").flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let Ok(pkg) = upkg::Package::open(&p) else { continue };
        for (i, e) in pkg.exports.iter().enumerate() {
            let n = pkg.name(e.name);
            if n.to_ascii_lowercase().contains(&pat) {
                let path = pkg.full_path(upkg::ObjRef::Export(i));
                let mut size = String::new();
                if let (Some(out), "Texture") = (&out, pkg.export_class(i)) {
                    if let Ok(t) = upkg::texture::read_texture(&pkg, i) {
                        if let Ok((w, h, px)) = t.decode_rgba(0) {
                            size = format!(" {w}x{h}");
                            std::fs::create_dir_all(out).ok();
                            let f = std::fs::File::create(out.join(format!("{path}.png"))).expect("png");
                            let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
                            enc.set_color(png::ColorType::Rgba);
                            enc.set_depth(png::BitDepth::Eight);
                            enc.write_header().and_then(|mut wr| wr.write_image_data(&px)).expect("png write");
                        }
                    }
                }
                println!("{path} {}{size} ({})", pkg.export_class(i), p.file_name().unwrap().to_string_lossy());
            }
        }
    }
}
