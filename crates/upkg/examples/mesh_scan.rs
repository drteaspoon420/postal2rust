//! Decodes every StaticMesh in the given directories and validates the buffers.
//! With `--obj <outdir> <name>...`, writes the named meshes as Wavefront OBJ for visual checks.

use upkg::Package;
use upkg::staticmesh::read_static_mesh;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let obj_at = args.iter().position(|a| a == "--obj");
    let dirs = &args[..obj_at.unwrap_or(args.len())];
    let (obj_dir, wanted): (Option<&str>, Vec<String>) = match obj_at {
        Some(i) => (args.get(i + 1).map(|s| s.as_str()), args[i + 2..].iter().map(|s| s.to_lowercase()).collect()),
        None => (None, vec![]),
    };
    let (mut ok, mut bad, mut tris, mut verts, mut with_mat) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut with_coll, mut coll_tris, mut with_model, mut no_coll_sections) = (0usize, 0usize, 0usize, 0usize);
    let mut shown = 0;
    let t0 = std::time::Instant::now();
    for d in dirs {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let Ok(pkg) = Package::open(e.path()) else { continue };
            for i in pkg.exports_of_class("StaticMesh").collect::<Vec<_>>() {
                match read_static_mesh(&pkg, i) {
                    Ok(m) => {
                        ok += 1;
                        tris += m.indices.len() / 3;
                        verts += m.positions.len();
                        if m.materials.len() == m.sections.len() {
                            with_mat += 1;
                        }
                        if !m.collision_tris.is_empty() {
                            with_coll += 1;
                            coll_tris += m.collision_tris.len();
                        }
                        if m.collision_model != upkg::ObjRef::Null {
                            with_model += 1;
                        }
                        no_coll_sections += m.material_collision.iter().filter(|c| !**c).count();
                        if let Some(dir) = obj_dir {
                            if wanted.contains(&m.name.to_lowercase()) {
                                write_obj(&format!("{dir}/{}_{}.obj", pkg.name, m.name), &m);
                                for (s, mat) in m.materials.iter().enumerate() {
                                    println!("  {} section {s}: material {} ({})", m.name, pkg.full_path(*mat), pkg.class_of(*mat));
                                }
                            }
                        }
                    }
                    Err(err) => {
                        bad += 1;
                        if shown < 10 {
                            shown += 1;
                            println!("FAILED {}.{}: {err}", pkg.name, pkg.name(pkg.exports[i].name));
                        }
                    }
                }
            }
        }
    }
    println!(
        "MESH SCAN: ok={ok} failed={bad} materials-match-sections={with_mat} verts={verts} tris={tris} in {:.2}s",
        t0.elapsed().as_secs_f64()
    );
    println!("MESH COLLISION: meshes with collision tris={with_coll} collision tris={coll_tris} with collision model={with_model} sections with EnableCollision=false: {no_coll_sections}");
}

fn write_obj(path: &str, m: &upkg::staticmesh::StaticMesh) {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for p in &m.positions {
        writeln!(f, "v {} {} {}", p[0], p[2], -p[1]).unwrap();
    }
    for uv in &m.uvs {
        writeln!(f, "vt {} {}", uv[0], 1.0 - uv[1]).unwrap();
    }
    for t in m.indices.chunks(3) {
        let (a, b, c) = (t[0] + 1, t[1] + 1, t[2] + 1);
        writeln!(f, "f {a}/{a} {b}/{b} {c}/{c}").unwrap();
    }
}
