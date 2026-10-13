//! `tex_dump <package.utx> <TextureName> <out.tga> [mip]`: decodes one texture mip to a TGA file
//! so a texture's decoded pixels can be eyeballed (e.g. a TV screen that renders white).
use upkg::Package;
use upkg::texture::read_texture;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("usage: tex_dump <package> <name> <out.tga> [mip]");
        std::process::exit(2);
    }
    let pkg = Package::open(&a[0]).expect("open package");
    let mip: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let i = (0..pkg.exports.len())
        .find(|&i| pkg.export_class(i) == "Texture" && pkg.name(pkg.exports[i].name).eq_ignore_ascii_case(&a[1]))
        .expect("texture not found");
    let t = read_texture(&pkg, i).expect("read texture");
    println!("{} format {:?} {}x{} mips {:?}", t.name, t.format, t.width, t.height, t.mips.iter().map(|m| (m.width, m.height, m.data.len())).collect::<Vec<_>>());
    let (w, h, px) = t.decode_rgba(mip).expect("decode");
    let (mut sum, mut n) = ([0u64; 4], 0u64);
    for p in px.chunks(4) {
        for k in 0..4 {
            sum[k] += p[k] as u64;
        }
        n += 1;
    }
    println!("mean rgba {:?}", sum.map(|s| s / n.max(1)));
    let mut out = vec![0u8, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(&[32, 0x28]);
    for p in px.chunks(4) {
        out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }
    std::fs::write(&a[2], out).expect("write");
}
