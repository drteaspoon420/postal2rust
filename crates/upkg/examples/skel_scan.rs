//! Decodes every SkeletalMesh and MeshAnimation in the given directories (.ukx and maps) and
//! reports counts and failures.
//! Usage: skel_scan <dir>... [--list]

use upkg::Package;
use upkg::skeletal::{read_mesh_animation, read_skeletal_mesh};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let list = args.iter().any(|a| a == "--list");
    let dirs: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let t0 = std::time::Instant::now();
    let (mut m_ok, mut m_bad, mut a_ok, mut a_bad) = (0usize, 0usize, 0usize, 0usize);
    let (mut tris, mut bones, mut seqs, mut frames, mut keys) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut shown = 0;
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let Ok(pkg) = Package::open(e.path()) else { continue };
            for i in pkg.exports_of_class("SkeletalMesh").collect::<Vec<_>>() {
                match read_skeletal_mesh(&pkg, i) {
                    Ok(m) => {
                        m_ok += 1;
                        bones += m.bones.len();
                        tris += m.lods.first().map_or(0, |l| l.faces.len());
                        if list {
                            let l = m.lods.first();
                            println!(
                                "MESH {}.{} bones={} lods={} verts={} faces={} textures={} anim={}",
                                pkg.name,
                                m.name,
                                m.bones.len(),
                                m.lods.len(),
                                l.map_or(0, |l| l.wedges.len()),
                                l.map_or(0, |l| l.faces.len()),
                                m.textures.len(),
                                pkg.full_path(m.animation)
                            );
                        }
                    }
                    Err(err) => {
                        m_bad += 1;
                        if shown < 20 {
                            shown += 1;
                            println!("FAILED mesh {}.{}: {err}", pkg.name, pkg.name(pkg.exports[i].name));
                        }
                    }
                }
            }
            for i in pkg.exports_of_class("MeshAnimation").collect::<Vec<_>>() {
                match read_mesh_animation(&pkg, i) {
                    Ok(a) => {
                        a_ok += 1;
                        seqs += a.sequences.len();
                        frames += a.sequences.iter().map(|s| s.num_frames.max(0) as usize).sum::<usize>();
                        keys += a.moves.iter().flat_map(|m| &m.tracks).map(|t| t.rotations.len()).sum::<usize>();
                        if list {
                            println!("ANIM {}.{} bones={} seqs={}", pkg.name, a.name, a.bones.len(), a.sequences.len());
                        }
                    }
                    Err(err) => {
                        a_bad += 1;
                        if shown < 20 {
                            shown += 1;
                            println!("FAILED anim {}.{}: {err}", pkg.name, pkg.name(pkg.exports[i].name));
                        }
                    }
                }
            }
        }
    }
    println!(
        "SKEL SCAN: meshes ok={m_ok} failed={m_bad} (bones={bones} lod0_tris={tris}); animations ok={a_ok} failed={a_bad} (sequences={seqs} frames={frames} rot_keys={keys}) in {:.2}s",
        t0.elapsed().as_secs_f64()
    );
}
