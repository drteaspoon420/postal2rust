//! Physics on the shipped maps (skipped when the game is not installed).
//! Set POSTAL2_DIR to the install; defaults to the developer's Steam path.

use glam::Vec3;
use p2physics::{Actor, Collider, Event, LoadedMap, PawnInfo, Physics};
use std::path::PathBuf;
use std::sync::Arc;
use upkg::library::Library;

fn game() -> Option<PathBuf> {
    let p: PathBuf = std::env::var_os("POSTAL2_DIR").map(PathBuf::from).unwrap_or_else(|| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    p.join("Maps").is_dir().then_some(p)
}

fn load(map: &str) -> Option<LoadedMap> {
    let g = game()?;
    let lib = Library::new(&g).ok()?;
    let pkg = Arc::new(upkg::Package::open(lib.map_path(map)?).ok()?);
    let m = LoadedMap::load(&lib, &pkg).expect("map loads");
    eprintln!("{map}: {:?} level tris {} (movers skipped {})", m.stats, m.world.level.triangle_count(), m.world.level.stats.movers_skipped);
    Some(m)
}

fn dude() -> PawnInfo {
    PawnInfo { is_player: true, ..Default::default() }
}

/// police: a DoorMover swings open (key 0 to key 1, a quarter turn about its hinge) into a pawn
/// standing in its sweep; the pawn is pushed out of the way, ends clear of the door and still walking.
#[test]
fn police_door_opens_and_pushes_pawn() {
    let Some(mut m) = load("police") else { return };
    assert!(m.stats.movers >= 50, "movers {}", m.stats.movers);
    // DoorMover0 (export 837): hinge at BasePos, closed at yaw -16384.
    let door = m.by_export[&837];
    assert_eq!(m.world.get(door).unwrap().class, "DoorMover");
    let w = &mut m.world;
    let (hinge, bounds) = {
        let d = w.get(door).unwrap();
        (d.location, d.bounds())
    };
    eprintln!("door at {hinge} rot {:?} bounds {:?}..{:?}", w.get(door).unwrap().rotation, bounds.min, bounds.max);
    let Collider::Mesh { placed, .. } = &w.get(door).unwrap().collider else { panic!("door has a mesh collider") };
    assert!(!placed.is_empty());
    // Where does the door leaf point? The farthest collision vertex from the hinge, horizontally.
    let far = placed.triangles().iter().flatten().copied().max_by(|a, b| (*a - hinge).truncate().length().total_cmp(&(*b - hinge).truncate().length())).unwrap();
    let leaf = (far - hinge).truncate();
    let width = leaf.length();
    eprintln!("leaf direction {leaf} width {width}");
    assert!(width > 30.0 && width < 120.0, "door width {width}");

    // Floor below the hinge.
    let floor = w.level.line(hinge + Vec3::Z * 10.0, hinge - Vec3::Z * 400.0).expect("floor under the door").location;
    // The door opens from key 0 to key 1 (yaw BaseRot + KeyRot[1]); the leaf sweeps the sector
    // between its closed direction and that direction rotated by the yaw change.
    let mi = w.get(door).unwrap().mover.clone().unwrap();
    let open_yaw = mi.base_rot[1] + mi.key_rot[1][1];
    let closed_yaw = w.get(door).unwrap().rotation[1];
    let dyaw = (open_yaw - closed_yaw) as f32 * std::f32::consts::TAU / 65536.0;
    // Stand the pawn inside the door's swing sector: the first spot (by angle from the closed
    // leaf, then distance from the hinge) where its cylinder fits on the floor.
    let p = w.add(Actor::pawn("Dude", hinge + Vec3::Z * 500.0, 28.0, 72.0, dude()));
    let mut stand = None;
    'search: for ta in [0.5f32, 0.4, 0.6, 0.3, 0.7, 0.8] {
        let (s, c) = (dyaw * ta).sin_cos();
        let dir = glam::Vec2::new(leaf.x * c - leaf.y * s, leaf.x * s + leaf.y * c).normalize();
        for fr in [0.6f32, 0.7, 0.5, 0.8] {
            let q = Vec3::new(hinge.x + dir.x * width * fr, hinge.y + dir.y * width * fr, floor.z + 73.0);
            if w.fits(p, q) {
                stand = Some(q);
                break 'search;
            }
        }
    }
    let stand = stand.expect("a free spot in the door's swing");
    assert!(w.place_on_floor(p, stand, 200.0), "pawn on the floor next to the door");
    w.drain_events();
    let p0 = w.get(p).unwrap().location;
    eprintln!("pawn at {p0}, opening to yaw {open_yaw} (closed {closed_yaw})");

    w.mover_interpolate_to(door, 1, mi.move_time.max(0.5));
    let mut events = Vec::new();
    for _ in 0..(60.0 * (mi.move_time.max(0.5) + 0.5)) as usize {
        w.tick(1.0 / 60.0);
        events.extend(w.drain_events());
    }
    let d = w.get(door).unwrap();
    eprintln!("door rot {:?} key {} pushes {} blocked {}", d.rotation, d.mover.as_ref().unwrap().key_num, w.stats.encroach_pushes, w.stats.encroach_blocked);
    let p1 = w.get(p).unwrap().location;
    eprintln!("pawn {p0} -> {p1}");
    let reached = events.contains(&Event::KeyFrameReached { actor: door });
    let pushed = w.get(p).unwrap().pushed > 0;
    assert!(pushed, "the opening door pushed the pawn");
    assert!((p1 - p0).truncate().length() > 5.0, "pawn moved");
    assert!(!w.overlaps(door, p), "pawn no longer inside the door");
    assert!(reached || events.iter().any(|e| matches!(e, Event::EncroachingOn { actor, .. } if *actor == door)), "door finished or reported the encroachment");
    if reached {
        assert_eq!(w.get(door).unwrap().rotation[1], open_yaw);
    }
    assert_eq!(w.get(p).unwrap().physics, Physics::Walking);
}

/// TrainingCamp-part2: the pawn dropped into the WaterVolume (export 342) enters PHYS_Swimming
/// with PhysicsVolumeChange and PawnEnteredVolume.
#[test]
fn training_camp_water_volume_swimming() {
    let Some(mut m) = load("TrainingCamp-part2") else { return };
    assert!(m.stats.water_volumes >= 1);
    let water = m.by_export[&342];
    let w = &mut m.world;
    assert!(w.get(water).unwrap().volume.as_ref().unwrap().water, "WaterVolume class default bWaterVolume");
    let b = w.get(water).unwrap().bounds();
    eprintln!("water bounds {:?}..{:?}", b.min, b.max);
    // A point inside the water near the top of the volume, above the pool floor.
    let c = b.center();
    let mut inside = None;
    for k in 0..40 {
        let p = Vec3::new(c.x, c.y, b.max.z - 10.0 - k as f32 * 5.0);
        if w.volume_at(p) == Some(water) {
            inside = Some(p);
            break;
        }
    }
    let inside = inside.expect("a point inside the water volume");
    // Start the pawn above the water surface and let it fall in.
    let start = Vec3::new(inside.x, inside.y, b.max.z + 100.0);
    let p = w.add(Actor::pawn("Dude", start, 28.0, 72.0, dude()));
    let start_volume = w.get(p).unwrap().physics_volume;
    assert_ne!(start_volume, Some(water));
    let mut events = Vec::new();
    for _ in 0..240 {
        w.tick(1.0 / 60.0);
        events.extend(w.drain_events());
        if w.get(p).unwrap().physics == Physics::Swimming {
            break;
        }
    }
    let a = w.get(p).unwrap();
    eprintln!("pawn at {} physics {:?} volume {:?}", a.location, a.physics, a.physics_volume);
    assert_eq!(a.physics, Physics::Swimming);
    assert_eq!(a.physics_volume, Some(water));
    assert!(events.contains(&Event::PhysicsVolumeChange { actor: p, volume: Some(water) }));
    assert!(events.contains(&Event::PawnEnteredVolume { actor: water, other: p }));
    // Swim a little: stays in the water, swimming.
    w.get_mut(p).unwrap().acceleration = Vec3::new(1.0, 0.0, 0.0) * 2048.0;
    for _ in 0..30 {
        w.tick(1.0 / 60.0);
    }
    assert_eq!(w.get(p).unwrap().physics, Physics::Swimming);
}

/// Every volume brush loads on a few maps, and a pawn at each PlayerStart is in the level's
/// default physics volume with a zone.
#[test]
fn maps_load_volumes_and_movers() {
    for map in ["police", "suburbs-3", "Estates", "Junkyard"] {
        let Some(mut m) = load(map) else { return };
        assert_eq!(m.stats.brush_errors, 0, "{map}");
        assert!(m.stats.volumes > 10, "{map}: volumes {}", m.stats.volumes);
        assert!(m.stats.movers > 0, "{map}");
        let w = &mut m.world;
        let starts = w.level.player_starts.clone();
        let mut walking = 0;
        for &(s, _) in &starts {
            let p = w.add(Actor::pawn("Dude", s, 28.0, 72.0, dude()));
            for _ in 0..60 {
                w.tick(1.0 / 60.0);
            }
            if w.get(p).unwrap().physics == Physics::Walking {
                walking += 1;
            }
            w.remove(p);
        }
        w.drain_events();
        eprintln!("{map}: {walking}/{} starts land walking", starts.len());
        assert!(walking * 2 >= starts.len());
    }
}
