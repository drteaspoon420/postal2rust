//! Decodes every texture in the given .utx directories, reporting failures per format.
//! With `--png <outdir> <name>...`, also writes the named textures' top mips as PNG for visual checks.
//! Run: cargo run --release -p upkg --example tex_scan -- <dir>... [--png out name1 name2]

use std::collections::BTreeMap;
use upkg::Package;
use upkg::texture::read_texture;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let png_at = args.iter().position(|a| a == "--png");
    let dirs = &args[..png_at.unwrap_or(args.len())];
    let (png_dir, wanted): (Option<&str>, Vec<String>) = match png_at {
        Some(i) => (args.get(i + 1).map(|s| s.as_str()), args[i + 2..].iter().map(|s| s.to_lowercase()).collect()),
        None => (None, vec![]),
    };
    let mut ok: BTreeMap<String, usize> = BTreeMap::new();
    let mut bad: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut bytes = 0usize;
    let t0 = std::time::Instant::now();
    for d in dirs {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let Ok(pkg) = Package::open(e.path()) else { continue };
            for i in pkg.exports_of_class("Texture").collect::<Vec<_>>() {
                let res = read_texture(&pkg, i).and_then(|t| {
                    let fmt = format!("{:?}", t.format);
                    let (w, h, px) = t.decode_rgba(0).map_err(|err| (fmt.clone(), err)).map_err(|(f, err)| upkg::Error::Unsupported(format!("{f}: {err}")))?;
                    if let Some(dir) = png_dir {
                        if wanted.contains(&t.name.to_lowercase()) {
                            write_png(&format!("{dir}/{}_{}.png", pkg.name, t.name), w, h, &px);
                        }
                    }
                    Ok((fmt, px.len()))
                });
                match res {
                    Ok((fmt, n)) => {
                        *ok.entry(fmt).or_default() += 1;
                        bytes += n;
                    }
                    Err(err) => {
                        let ent = bad.entry(err.to_string().split(':').next().unwrap_or("").to_string()).or_insert((0, String::new()));
                        ent.0 += 1;
                        if ent.1.is_empty() {
                            ent.1 = format!("{}.{} : {err}", pkg.name, pkg.name(pkg.exports[i].name));
                        }
                    }
                }
            }
        }
    }
    for (f, n) in &ok {
        println!("decoded {n:6} {f}");
    }
    for (k, (n, ex)) in &bad {
        println!("FAILED  {n:6} {k}  e.g. {ex}");
    }
    let total_ok: usize = ok.values().sum();
    let total_bad: usize = bad.values().map(|v| v.0).sum();
    println!(
        "TEX SCAN: ok={total_ok} failed={total_bad} decoded={:.1} MiB in {:.2}s",
        bytes as f64 / 1048576.0,
        t0.elapsed().as_secs_f64()
    );
}

fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) {
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}
