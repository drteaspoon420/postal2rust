//! Prints each MeshAnimation sequence's timing: frames, rate (fps), track_time and duration.
//! Usage: anim_rates <file.ukx> [name-filter]
use upkg::Package;
use upkg::skeletal::read_mesh_animation;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).unwrap();
    let filt = a.get(1).map(|s| s.to_ascii_lowercase());
    let ids: Vec<_> = pkg.exports_of_class("MeshAnimation").collect();
    eprintln!("{} exports, {} MeshAnimation", pkg.exports.len(), ids.len());
    for ai in ids {
        let anim = match read_mesh_animation(&pkg, ai) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("read failed: {e}");
                continue;
            }
        };
        for (s, seq) in anim.sequences.iter().enumerate() {
            if filt.as_ref().is_some_and(|f| !seq.name.to_ascii_lowercase().contains(f.as_str())) {
                continue;
            }
            let tt = anim.moves.get(s).map_or(0.0, |m| m.track_time);
            println!("{ai:?} {} frames={} rate={} track_time={} dur={:.3}s", seq.name, seq.num_frames, seq.rate, tt, seq.num_frames as f32 / seq.rate.max(1e-3));
        }
    }
}
