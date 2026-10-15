//! Bolton / head attachment probe: boots a map, runs a few seconds and prints every bolton
//! (PeoplePart) with its parent, bone, RelativeLocation/Rotation, DrawScale(3D) and PrePivot,
//! where it sits in its parent's actor space and, for parts on a skeletal parent (head boltons:
//! glasses, hats, earrings), how many of its vertices end up inside the parent's skinned mesh
//! in the current pose (`INSIDE` lines: ray parity, depth to the surface along head forward).
//! `bolton_probe [Map] [--secs N] [--heads]` (`--heads` also prints each head actor once;
//! `BOLTON_BONES=1` dumps head bones bind vs current).
use glam::{Mat4, Vec3};
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;

fn main() {
    std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
}

/// Moller-Trumbore: distance along d from o to triangle t (either face), if hit.
fn ray_tri(o: Vec3, d: Vec3, t: &[Vec3; 3]) -> Option<f32> {
    let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist > 1e-4).then_some(dist)
}

fn run() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let secs: f32 = a.iter().position(|x| x == "--secs").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let heads = a.iter().any(|x| x == "--heads");
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::ai::install();
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("");
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    for _ in 0..(secs * 30.0) as usize {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
    }
    let host = game.host.clone();
    let vm = &mut game.world.vm;
    let parts: Vec<_> = vm.actors.iter().copied().filter(|&p| !vm.objects[p].deleted && vm.class_is_a(vm.objects[p].class, "PeoplePart")).collect();
    let mut seen = std::collections::HashSet::new();
    let mut summary: std::collections::BTreeMap<String, (usize, usize, usize, f32)> = Default::default();
    for part in parts {
        let base = vm.get_prop(part, "Base").and_then(|v| v.as_obj());
        let bone = vm.get_prop(part, "AttachmentBone").map(|v| v.as_name()).unwrap_or_default();
        let sm = vm.get_prop(part, "StaticMesh").and_then(|v| v.as_resource().map(str::to_string)).unwrap_or_default();
        let mesh = vm.get_prop(part, "Mesh").and_then(|v| v.as_resource().map(str::to_string)).unwrap_or_default();
        let class = vm.classes[vm.objects[part].class].name.clone();
        let is_head = vm.class_is_a(vm.objects[part].class, "Head");
        if is_head && (!heads || !seen.insert(format!("{mesh}|{:?}", vm.get_prop(part, "DrawScale3D")))) {
            continue;
        }
        let g = |n: &str| vm.get_prop(part, n).cloned();
        let base_name = base.map(|b| format!("{} ({})", vm.object_name(b), vm.classes[vm.objects[b].class].name)).unwrap_or_default();
        println!(
            "PART {} ({class}) base {base_name} bone '{bone}' sm {sm} mesh {mesh} RelLoc {:?} RelRot {:?} DrawScale {:?} DrawScale3D {:?} PrePivot {:?}",
            vm.object_name(part),
            g("RelativeLocation"),
            g("RelativeRotation"),
            g("DrawScale"),
            g("DrawScale3D"),
            g("PrePivot"),
        );
        let Some(b) = base else { continue };
        let pawn = if vm.class_is_a(vm.objects[b].class, "Head") { vm.get_prop(b, "Owner").and_then(|v| v.as_obj()).unwrap_or(b) } else { b };
        let gp = |n: &str| vm.get_prop(pawn, n).cloned();
        println!(
            "    pawn {} Mesh {:?} Skins0 {:?} MyBody {:?} MyGender {:?} bIsFat {:?} HeadMesh {:?} HeadScale {:?} bChameleon {:?}",
            vm.object_name(pawn),
            gp("Mesh"),
            vm.get_prop(pawn, "Skins").map(|v| format!("{v:?}").chars().take(80).collect::<String>()),
            gp("MyBody"),
            gp("MyGender"),
            gp("bIsFat"),
            gp("HeadMesh"),
            gp("HeadScale"),
            gp("bChameleon")
        );
        let bw = host.anims.borrow().get(&b).map(|x| p2game::host::skel_actor_world(vm, b, x.inst.skeleton.mesh_xform)).unwrap_or_else(|| p2game::host::actor_world(vm, b));
        let pw = p2game::host::actor_world(vm, part);
        let rel = bw.inverse() * pw;
        let (s, r, t) = rel.to_scale_rotation_translation();
        let (rz, ry, rx) = r.to_euler(glam::EulerRot::ZYX);
        println!("    in base actor space: t {t:.2} rot zyx deg [{:.1} {:.1} {:.1}] scale {s:.3}; base DrawScale3D {:?}", rz.to_degrees(), ry.to_degrees(), rx.to_degrees(), vm.get_prop(b, "DrawScale3D"));
        if std::env::var_os("BOLTON_BONES").is_some() && vm.class_is_a(vm.objects[b].class, "Head") {
            let rows = host.with_inst(vm, b, |i| {
                let sk = i.skeleton.clone();
                let cur = i.bone_matrices().to_vec();
                let mut bind: Vec<Mat4> = Vec::new();
                for k in 0..sk.len() {
                    let l = sk.ref_pose[k].mat();
                    bind.push(if k == 0 || sk.parents[k] == k { l } else { bind[sk.parents[k]] * l });
                }
                (0..sk.len()).map(|k| format!("      bone {k} {} bind {:.2} cur {:.2}", sk.names[k], (sk.mesh_xform * bind[k]).w_axis.truncate(), (sk.mesh_xform * cur[k]).w_axis.truncate())).collect::<Vec<_>>()
            });
            for r in rows.unwrap_or_default() {
                println!("{r}");
            }
        }
        if sm.is_empty() {
            continue;
        }
        let Some(o) = p2game::host::resolve_path(&host.lib, &sm, Some("StaticMesh")) else { continue };
        let Ok(m) = upkg::staticmesh::read_static_mesh(&o.pkg, o.export) else { continue };
        let (lo, hi) = m.positions.iter().map(|p| rel.transform_point3(Vec3::from(*p))).fold((Vec3::splat(1e9), Vec3::splat(-1e9)), |(l, h), p| (l.min(p), h.max(p)));
        println!("    part mesh raw bounds {:?} .. {:?}; in base actor space {lo:.1} .. {hi:.1}", m.bounds_min, m.bounds_max);
        // The base's skinned triangles in world space (current pose).
        let base_mesh = vm.get_prop(b, "Mesh").and_then(|v| v.as_resource().map(str::to_string)).unwrap_or_default();
        let Some(info) = host.mesh(&base_mesh) else { continue };
        let Ok(sk_mesh) = upkg::skeletal::read_skeletal_mesh(&info.obj.pkg, info.obj.export) else { continue };
        let tris = host
            .with_inst(vm, b, |i| {
                let sk = i.skeleton.clone();
                let cur = i.bone_matrices().to_vec();
                let mut bind: Vec<Mat4> = Vec::new();
                for k in 0..sk.len() {
                    let l = sk.ref_pose[k].mat();
                    bind.push(if k == 0 || sk.parents[k] == k { l } else { bind[sk.parents[k]] * l });
                }
                let lod = &sk_mesh.lods[0];
                let mut acc = vec![(Vec3::ZERO, 0f32); lod.points.len()];
                for inf in &lod.influences {
                    let pt = inf.point as usize;
                    let k = inf.bone as usize;
                    let skin = bw * sk.mesh_xform * cur[k] * bind[k].inverse();
                    acc[pt].0 += skin.transform_point3(Vec3::from(lod.points[pt])) * inf.weight;
                    acc[pt].1 += inf.weight;
                }
                let pts: Vec<Vec3> = acc.iter().map(|(p, w)| if *w > 0.0 { *p / *w } else { *p }).collect();
                lod.faces.iter().map(|f| f.wedges.map(|w| pts[lod.wedges[w as usize].point as usize])).collect::<Vec<[Vec3; 3]>>()
            })
            .unwrap_or_default();
        if tris.is_empty() {
            continue;
        }
        let fwd = bw.x_axis.truncate().normalize();
        let (mut inside, mut depth) = (0, 0f32);
        for p in &m.positions {
            let wp = pw.transform_point3(Vec3::from(*p));
            let hits: Vec<f32> = tris.iter().filter_map(|t| ray_tri(wp, fwd, t)).collect();
            if hits.len() % 2 == 1 {
                inside += 1;
                depth = depth.max(hits.iter().cloned().fold(f32::MAX, f32::min));
            }
        }
        println!("    INSIDE {sm} on {}: {inside}/{} vertices inside the parent mesh (max depth along parent forward {depth:.2} uu)", info.path, m.positions.len());
        let e = summary.entry(format!("{sm} on {}", info.path)).or_default();
        e.0 += 1;
        e.1 += inside;
        e.2 += m.positions.len();
        e.3 = e.3.max(depth);
    }
    for (k, (n, inside, total, depth)) in summary {
        println!("SUMMARY {k}: {n} parts, {:.1}% vertices inside, max depth {depth:.2}", 100.0 * inside as f32 / total.max(1) as f32);
    }
}
