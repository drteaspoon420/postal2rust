//! Foot placement probe: boots a map, runs a few seconds, and for every walking pawn compares
//! the skinned mesh's lowest vertex (current pose, actor_world x mesh_xform x skinning) and the
//! foot/toe bone heights against a floor trace under the pawn. Also prints Location,
//! CollisionHeight, PrePivot and the gap between the cylinder bottom and the floor.
//! `feet_probe [Map] [--secs N] [--all]`
use glam::{Mat4, Vec3};
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::collections::HashMap;
use std::sync::Arc;
use uscript::host::{Host, TraceRequest};
use uscript::vm::Value;

fn main() {
    std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let secs: f32 = a.iter().position(|x| x == "--secs").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let all = a.iter().any(|x| x == "--all");
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
    let mut meshes: HashMap<String, Option<upkg::skeletal::SkeletalMesh>> = HashMap::new();
    let pawns: Vec<_> = vm.actors.iter().copied().filter(|&p| !vm.objects[p].deleted && vm.class_is_a(vm.objects[p].class, "Pawn")).collect();
    let mut by_class: std::collections::BTreeMap<String, (usize, f32, f32, f32)> = Default::default();
    for p in pawns {
        let phys = vm.get_prop(p, "Physics").map(|v| v.as_int()).unwrap_or(0);
        if phys != 1 {
            continue;
        }
        let loc = Vec3::from(vm.actor_loc(p));
        let ch = vm.get_prop(p, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0);
        let pre = vm.get_prop(p, "PrePivot").map(|v| v.as_vec()).unwrap_or_default();
        let req = TraceRequest { start: loc.to_array(), end: (loc - Vec3::Z * (ch + 200.0)).to_array(), extent: [0.0; 3], trace_actors: false };
        if all {
            let r2 = TraceRequest { start: loc.to_array(), end: (loc - Vec3::Z * (ch + 200.0)).to_array(), extent: [0.0; 3], trace_actors: true };
            let r3 = TraceRequest { start: loc.to_array(), end: (loc - Vec3::Z * 200.0).to_array(), extent: [20.0, 20.0, ch], trace_actors: true };
            let h2 = host.trace(vm, Some(p), &r2).map(|h| (h.location[2], h.actor.map(|a| vm.object_name(a).to_string())));
            let h3 = host.trace(vm, Some(p), &r3).map(|h| (h.location[2], h.actor.map(|a| vm.object_name(a).to_string())));
            println!("TRACES {}: line+actors {h2:?} extent-cyl {h3:?} (extent hit z is the cylinder centre)", vm.object_name(p));
        }
        let Some(hit) = host.trace(vm, Some(p), &req) else { continue };
        let floor = hit.location[2];
        let Some(info) = host.anims.borrow().get(&p).map(|x| (x.mesh.obj.clone(), x.mesh.path.clone())) else { continue };
        let mesh = meshes.entry(info.1.clone()).or_insert_with(|| upkg::skeletal::read_skeletal_mesh(&info.0.pkg, info.0.export).ok());
        let Some(mesh) = mesh.as_ref() else { continue };
        let world = host.anims.borrow().get(&p).map(|x| p2game::host::skel_actor_world(vm, p, x.inst.skeleton.mesh_xform)).unwrap_or_else(|| p2game::host::actor_world(vm, p));
        let (minz, feet) = host
            .with_inst(vm, p, |i| {
                let skel = i.skeleton.clone();
                // Bind pose (mesh space) for inverse bind matrices.
                let mut bind: Vec<Mat4> = Vec::with_capacity(skel.len());
                for b in 0..skel.len() {
                    let l = skel.ref_pose[b].mat();
                    bind.push(if b == 0 || skel.parents[b] == b { l } else { bind[skel.parents[b]] * l });
                }
                let cur = i.bone_matrices().to_vec();
                if std::env::var_os("FEET_ROOT").is_some() {
                    for (b, n) in skel.names.iter().enumerate() {
                        let l = n.to_ascii_lowercase();
                        if l.contains("foot") || l.contains("toe") || l.contains("calf") {
                            let bw = skel.mesh_xform * bind[b];
                            let cw = skel.mesh_xform * cur[b];
                            println!("  {n}: bind z {:.2} anim z {:.2} (actor space; cylinder bottom = -CollisionHeight)", bw.w_axis.z, cw.w_axis.z);
                        }
                    }
                    let loc = i.local_pose().to_vec();
                    for b in 0..3.min(skel.len()) {
                        println!("  bone {b} {} parent {} ref pos {:?} rot {:?} | cur local pos {:?} rot {:?}", skel.names[b], skel.parents[b], skel.ref_pose[b].pos, skel.ref_pose[b].rot, loc[b].pos, loc[b].rot);
                    }
                }
                let skin: Vec<Mat4> = (0..skel.len()).map(|b| world * skel.mesh_xform * cur[b] * bind[b].inverse()).collect();
                let lod = &mesh.lods[0];
                let mut acc = vec![(Vec3::ZERO, 0f32); lod.points.len()];
                for inf in &lod.influences {
                    let pt = inf.point as usize;
                    acc[pt].0 += skin[inf.bone as usize].transform_point3(Vec3::from(lod.points[pt])) * inf.weight;
                    acc[pt].1 += inf.weight;
                }
                let minz = acc.iter().filter(|x| x.1 > 0.0).map(|x| x.0.z / x.1).fold(f32::MAX, f32::min);
                let feet: Vec<(String, f32)> = skel
                    .names
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| {
                        let n = n.to_ascii_lowercase();
                        n.contains("foot") || n.contains("toe")
                    })
                    .map(|(b, n)| (n.clone(), (world * skel.mesh_xform * cur[b]).w_axis.z - floor))
                    .collect();
                (minz, feet)
            })
            .unwrap_or((f32::MAX, vec![]));
        let class = vm.classes[vm.objects[p].class].name.clone();
        let anim = host.anim_channel(vm, p, 0).map(|c| c.sequence).unwrap_or_default();
        let e = by_class.entry(class.clone()).or_insert((0, 0.0, f32::MAX, f32::MIN));
        let d = minz - floor;
        e.0 += 1;
        e.1 += d;
        e.2 = e.2.min(d);
        e.3 = e.3.max(d);
        if all {
            let bind_min = mesh.lods[0].points.iter().map(|p| p2anim::runtime::Skeleton::from_mesh(mesh).mesh_xform.transform_point3(Vec3::from(*p)).z).fold(f32::MAX, f32::min);
            println!("MESH {} origin {:?} scale {:?} rot {:?} bind-min(actor space) {bind_min:.2} props {:?}", info.1, mesh.origin, mesh.scale, mesh.rotation, mesh.props.0.iter().map(|p| p.name.clone()).collect::<Vec<_>>());
            let fs: Vec<String> = feet.iter().map(|(n, z)| format!("{n}={z:.1}")).collect();
            println!(
                "FEET {} ({class}) loc {:.2?} floor {floor:.2} mesh {} anim {anim} crouch {:?} loc.z-floor {:.2} ch {ch:.1} gap(cyl bottom-floor) {:.2} PrePivot {:?} DrawScale {:?} meshmin-floor {d:.2} bones [{}]",
                vm.object_name(p),
                loc,
                info.1,
                vm.get_prop(p, "bIsCrouched").map(|v| v.as_bool()),
                loc.z - floor,
                loc.z - ch - floor,
                pre,
                vm.get_prop(p, "DrawScale"),
                fs.join(" ")
            );
        }
        let _ = Value::Bool(false);
    }
    for (c, (n, s, lo, hi)) in by_class {
        println!("CLASS {c}: n {n} meshmin-floor mean {:.2} min {lo:.2} max {hi:.2}", s / n as f32);
    }
}
