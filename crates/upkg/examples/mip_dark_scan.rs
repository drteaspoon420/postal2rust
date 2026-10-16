//! Finds textures whose smaller mips decode much darker (or more transparent) than mip 0, the
//! "sign looks blacked out from a distance" symptom: the renderer samples the small mips far away.
//! Usage: mip_dark_scan <dir_or_utx>... [--ratio 0.6]
use upkg::Package;
use upkg::texture::read_texture;

fn mean(rgba: &[u8]) -> [f32; 4] {
    let n = (rgba.len() / 4).max(1) as f32;
    let mut m = [0f32; 4];
    for p in rgba.chunks_exact(4) {
        for k in 0..4 {
            m[k] += p[k] as f32;
        }
    }
    m.map(|v| v / n)
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let ratio: f32 = a.iter().position(|x| x == "--ratio").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(0.6);
    let mut files = Vec::new();
    for p in a.iter().filter(|x| !x.starts_with("--") && x.parse::<f32>().is_err()) {
        let path = std::path::Path::new(p);
        if path.is_dir() {
            for e in std::fs::read_dir(path).unwrap().flatten() {
                let f = e.path();
                if f.extension().is_some_and(|x| x.eq_ignore_ascii_case("utx")) {
                    files.push(f);
                }
            }
        } else {
            files.push(path.to_path_buf());
        }
    }
    let (mut checked, mut flagged) = (0, 0);
    for f in files {
        let Ok(pkg) = Package::open(&f) else { continue };
        for i in 0..pkg.exports.len() {
            if pkg.export_class(i) != "Texture" {
                continue;
            }
            let Ok(t) = read_texture(&pkg, i) else { continue };
            if t.mips.len() < 3 {
                continue;
            }
            checked += 1;
            let m0 = mean(&t.decode_rgba(0).map(|x| x.2).unwrap_or_default());
            let l0 = m0[0] + m0[1] + m0[2];
            if l0 < 60.0 {
                continue;
            }
            let mut worst: Option<(usize, f32, f32)> = None;
            for k in 1..t.mips.len() {
                let (w, h) = (t.mips[k].width, t.mips[k].height);
                if w < 4 || h < 4 {
                    break;
                }
                let Ok((_, _, px)) = t.decode_rgba(k) else { continue };
                let mk = mean(&px);
                let lr = (mk[0] + mk[1] + mk[2]) / l0;
                let ar = if m0[3] > 1.0 { mk[3] / m0[3] } else { 1.0 };
                let r = lr.min(ar);
                if worst.is_none_or(|w| r < w.1) {
                    worst = Some((k, r, lr));
                }
            }
            if let Some((k, r, lr)) = worst {
                if r < ratio {
                    flagged += 1;
                    println!("DARK {} {} {:?} {}x{} mip{} ratio {:.2} (lum {:.2}) mip0 mean {:?}", f.file_name().unwrap().to_string_lossy(), t.name, t.format, t.width, t.height, k, r, lr, m0.map(|v| v as u8));
                }
            }
        }
    }
    println!("checked {checked} textures, flagged {flagged}");
}
