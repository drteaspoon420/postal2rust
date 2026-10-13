//! Collision tests against the shipped maps (skipped when the game is not installed).
//! Set POSTAL2_DIR to the install; defaults to the developer's Steam path.

use glam::{Vec2, Vec3};
use p2collide::{MoveInput, Physics, Shape, TraceFlags, Walker, World};
use std::path::PathBuf;
use std::sync::Arc;
use upkg::library::Library;

fn game() -> Option<PathBuf> {
    let p: PathBuf = std::env::var_os("POSTAL2_DIR").map(PathBuf::from).unwrap_or_else(|| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    p.join("Maps").is_dir().then_some(p)
}

fn load(map: &str) -> Option<World> {
    let g = game()?;
    let lib = Library::new(&g).ok()?;
    let path = lib.map_path(map)?;
    let pkg = Arc::new(upkg::Package::open(&path).ok()?);
    let w = World::build(&lib, &pkg).expect("world builds");
    eprintln!(
        "{map}: tris={} (bsp {} actor {} from {} blocking actors, {} non-blocking) terrains={} starts={} pawn={:?} in {} ms",
        w.triangle_count(),
        w.stats.bsp_tris,
        w.stats.actor_tris,
        w.stats.blocking_actors,
        w.stats.nonblocking_actors,
        w.stats.terrains,
        w.player_starts.len(),
        w.pawn.source,
        w.stats.load_ms
    );
    Some(w)
}

const MAPS: &[&str] = &["Junkyard", "suburbs-3", "police", "Estates", "Highlands"];

#[test]
fn trace_down_from_player_start_hits_floor() {
    for &m in MAPS {
        let Some(w) = load(m) else { return };
        assert!(!w.player_starts.is_empty(), "{m}: no PlayerStart");
        let mut hits = 0;
        for &(p, _) in &w.player_starts {
            let h = w.line(p, p - Vec3::Z * 2000.0);
            let Some(h) = h else { continue };
            // A PlayerStart sits about CollisionHeight above the floor.
            let drop = p.z - h.location.z;
            assert!(h.normal.z > 0.7, "{m}: floor normal {:?}", h.normal);
            assert!((0.0..400.0).contains(&drop), "{m}: floor {drop} below start");
            hits += 1;
        }
        eprintln!("{m}: {hits}/{} player starts have floor below", w.player_starts.len());
        assert!(hits * 2 >= w.player_starts.len(), "{m}: most player starts must have floor below");
        // The player start itself is in empty space.
        let p = w.player_starts[0].0;
        assert!(!w.bsp.point_solid(p) || w.bsp.is_empty(), "{m}: player start inside BSP solid");
    }
}

#[test]
fn bsp_tree_agrees_with_polygon_trace() {
    // Vertical rays on a grid around the player start: whenever both the BSP tree (solid/empty
    // classification) and the BSP polygon trace find a surface, they should agree on where.
    for &m in &["police", "suburbs-3", "Estates"] {
        let Some(w) = load(m) else { return };
        let (s, _) = w.player_starts[0];
        let (mut agree, mut total) = (0, 0);
        for gy in -15..=15 {
            for gx in -15..=15 {
                let p = s + Vec3::new(gx as f32 * 97.0, gy as f32 * 97.0, 40.0);
                if w.bsp.point_solid(p) {
                    continue;
                }
                let end = p - Vec3::Z * 3000.0;
                let (Some((t, _)), Some(h)) = (w.bsp.line_check(p, end), w.trace(p, end, &Shape::POINT, TraceFlags { world: true, actors: false, terrain: false })) else {
                    continue;
                };
                total += 1;
                if (t - h.t).abs() * 3000.0 < 2.0 {
                    agree += 1;
                }
            }
        }
        eprintln!("{m}: bsp tree vs polygon trace agreement {agree}/{total}");
        assert!(total >= 50 && agree * 10 >= total * 9, "{m}: {agree}/{total}");
    }
}

/// Walks forward from each player start in several directions for a few seconds; the pawn must
/// never fall out of the world (end below every map geometry) and must keep a floor under it.
#[test]
fn walk_forward_without_falling_through() {
    for &m in MAPS {
        let Some(w) = load(m) else { return };
        let (start, yaw) = w.player_starts[0];
        let mut wk = Walker::new(start, w.pawn);
        assert!(wk.place_on_floor(&w, start, 1000.0), "{m}: no floor under start");
        let floor0 = wk.feet().z;
        let mut min_z = f32::MAX;
        let mut travelled = 0.0;
        let dt = 1.0 / 60.0;
        for (k, turn) in [0.0f32, 1.5, -1.5, 3.14].iter().enumerate() {
            let a = yaw + turn;
            let input = MoveInput { wish: Vec2::new(a.cos(), a.sin()), ..Default::default() };
            let mut last = wk.location;
            for i in 0..(5 * 60) {
                // A jump now and then exercises falling and landing.
                let jump = i % 90 == 45 && k == 0;
                wk.tick(&w, &MoveInput { jump, ..input }, dt);
                travelled += (wk.location - last).truncate().length();
                last = wk.location;
                min_z = min_z.min(wk.feet().z);
                assert!(wk.location.is_finite(), "{m}: NaN location");
            }
        }
        // Settle.
        for _ in 0..120 {
            wk.tick(&w, &MoveInput::default(), dt);
        }
        let under = w.line(wk.location, wk.location - Vec3::Z * 5000.0);
        eprintln!(
            "{m}: start feet z {floor0:.0}, end {:?} feet z {:.0}, min feet z {min_z:.0}, travelled {travelled:.0} uu, physics {:?}",
            wk.location,
            wk.feet().z,
            wk.physics
        );
        assert_eq!(wk.physics, Physics::Walking, "{m}: still falling after settling");
        assert!(under.is_some(), "{m}: no geometry under the pawn at the end");
        assert!(min_z > floor0 - 3000.0, "{m}: fell far below the start");
        assert!(travelled > 300.0, "{m}: pawn barely moved ({travelled})");
    }
}

#[test]
fn walls_block_long_walks() {
    // Walking in one direction for 20 s at full speed must be stopped by geometry long before
    // the pawn leaves the level bounds.
    let Some(w) = load("police") else { return };
    let (start, yaw) = w.player_starts[0];
    let mut wk = Walker::new(start, w.pawn);
    wk.place_on_floor(&w, start, 1000.0);
    let input = MoveInput { wish: Vec2::new(yaw.cos(), yaw.sin()), ..Default::default() };
    for _ in 0..(20 * 60) {
        wk.tick(&w, &input, 1.0 / 60.0);
    }
    let dist = (wk.location - start).truncate().length();
    eprintln!("police: 20 s walk ended {dist:.0} uu from start (max possible {:.0})", w.pawn.ground_speed * 20.0);
    assert!(dist < w.pawn.ground_speed * 20.0 * 0.95, "nothing blocked a 20 s straight walk");
    assert_eq!(wk.physics, Physics::Walking);
}
