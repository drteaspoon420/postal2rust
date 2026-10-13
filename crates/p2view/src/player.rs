//! Walk mode: the camera becomes the eyes of a UE2-sized player pawn that collides with the map
//! (BSP, blocking static meshes, terrain) through `p2collide`.
//!
//! Enable with `--walk` or toggle with V. In walk mode WASD moves along the ground, Space jumps,
//! C or Ctrl crouches, Shift walks slowly; the mouse look stays on the right mouse button.
//! `--walk-auto <seconds>` holds W for that long (automated checks) and prints a `WALK_AUTO`
//! line about half a second later. Pick a duration that ends before any screenshot capture
//! closes the app (about 4 s of wall time).
//!
//! The plugin only touches the main camera's translation (in PostUpdate, after the fly camera),
//! so the viewer's own camera code keeps working unchanged.

use bevy::prelude::*;
use p2collide::{MoveInput, Physics, Walker, World};
use p2view::scene::UU_PER_M;
use std::path::PathBuf;
use std::sync::Arc;
use upkg::library::Library;

pub struct PlayerPlugin {
    pub game: PathBuf,
    pub map: String,
}

#[derive(Resource)]
struct PlayerState {
    world: Option<Arc<World>>,
    walker: Option<Walker>,
    walking: bool,
    auto: Option<f32>,
    elapsed: f32,
    reported: bool,
    start: Vec3,
    accum: f32,
}

#[derive(Resource, Clone)]
struct PlayerConfig {
    game: PathBuf,
    map: String,
}

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        let a: Vec<String> = std::env::args().collect();
        let auto = a.iter().position(|x| x == "--walk-auto").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok());
        let walking = a.iter().any(|x| x == "--walk") || auto.is_some();
        app.insert_resource(PlayerConfig { game: self.game.clone(), map: self.map.clone() })
            .insert_resource(PlayerState { world: None, walker: None, walking, auto, elapsed: 0.0, reported: false, start: Vec3::ZERO, accum: 0.0 })
            .add_systems(Startup, build_world)
            .add_systems(PostUpdate, walk.before(bevy::transform::TransformSystems::Propagate));
    }
}

/// Unreal (X fwd, Y right, Z up, units) to Bevy (Y up, metres), as in `scene::to_bevy`.
fn to_bevy(p: glam_compat::V) -> Vec3 {
    Vec3::new(p.x, p.z, p.y) / UU_PER_M
}

fn to_unreal(p: Vec3) -> glam_compat::V {
    glam_compat::V::new(p.x, p.z, p.y) * UU_PER_M
}

/// p2collide uses glam directly; Bevy re-exports the same glam version, so the types match.
mod glam_compat {
    pub type V = bevy::math::Vec3;
}

fn build_world(cfg: Res<PlayerConfig>, mut st: ResMut<PlayerState>) {
    // Built eagerly only when walk mode starts on; otherwise on the first toggle.
    if st.walking {
        load(&cfg, &mut st);
    }
}

fn load(cfg: &PlayerConfig, st: &mut PlayerState) {
    if st.world.is_some() {
        return;
    }
    let t0 = std::time::Instant::now();
    let Ok(lib) = Library::new(&cfg.game) else { return };
    let Some(path) = lib.map_path(&cfg.map) else { return };
    let Ok(pkg) = upkg::Package::open(&path) else { return };
    match World::build(&lib, &Arc::new(pkg)) {
        Ok(w) => {
            println!(
                "COLLISION {}: tris={} (bsp {}, actors {} from {} blocking / {} non-blocking) terrains={} player_starts={} pawn r={} h={} ({}) built in {} ms",
                cfg.map,
                w.triangle_count(),
                w.stats.bsp_tris,
                w.stats.actor_tris,
                w.stats.blocking_actors,
                w.stats.nonblocking_actors,
                w.stats.terrains,
                w.player_starts.len(),
                w.pawn.collision_radius,
                w.pawn.collision_height,
                w.pawn.source,
                t0.elapsed().as_millis()
            );
            st.world = Some(Arc::new(w));
        }
        Err(e) => eprintln!("collision world for {} failed: {e}", cfg.map),
    }
}

fn walk(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    cfg: Res<PlayerConfig>,
    mut st: ResMut<PlayerState>,
    mut cam: Query<&mut Transform, With<Camera3d>>,
) {
    let Some(mut tf) = cam.iter_mut().next() else { return };
    if keys.just_pressed(KeyCode::KeyV) {
        st.walking = !st.walking;
        if !st.walking {
            st.walker = None;
            println!("WALK off (fly camera)");
        }
    }
    if !st.walking {
        return;
    }
    load(&cfg, &mut st);
    let Some(world) = st.world.clone() else {
        st.walking = false;
        return;
    };
    if st.walker.is_none() {
        // Spawn at the first PlayerStart (or below the current camera when the map has none,
        // or when toggled on mid-flight).
        let toggled = keys.just_pressed(KeyCode::KeyV);
        let at = match world.player_starts.first() {
            Some(&(p, _)) if !toggled => p,
            _ => to_unreal(tf.translation),
        };
        let mut w = Walker::new(at, world.pawn);
        let on_floor = w.place_on_floor(&world, at, 4000.0);
        println!("WALK on at {:?} (on floor: {on_floor}, eye {:.0} uu above feet)", w.location, w.eye().z - w.feet().z);
        st.start = w.location;
        st.walker = Some(w);
    }
    // Movement relative to the camera heading (Bevy forward -> Unreal XY).
    let fwd = *tf.forward();
    let right = *tf.right();
    let f2 = Vec2::new(fwd.x, fwd.z).normalize_or_zero();
    let r2 = Vec2::new(right.x, right.z).normalize_or_zero();
    let mut wish = Vec2::ZERO;
    let auto_on = st.auto.is_some_and(|a| st.elapsed < a);
    if keys.pressed(KeyCode::KeyW) || auto_on {
        wish += f2;
    }
    if keys.pressed(KeyCode::KeyS) {
        wish -= f2;
    }
    if keys.pressed(KeyCode::KeyD) {
        wish += r2;
    }
    if keys.pressed(KeyCode::KeyA) {
        wish -= r2;
    }
    let input = MoveInput {
        wish: wish.normalize_or_zero(),
        jump: keys.pressed(KeyCode::Space),
        crouch: keys.pressed(KeyCode::KeyC) || keys.pressed(KeyCode::ControlLeft),
        walk: keys.pressed(KeyCode::ShiftLeft),
    };
    // Fixed 120 Hz substeps keep the physics independent of the frame rate.
    const STEP: f32 = 1.0 / 120.0;
    let dt = time.delta_secs().min(0.25);
    st.elapsed += dt;
    st.accum += dt;
    let mut walker = st.walker.take().expect("walker");
    while st.accum >= STEP {
        walker.tick(&world, &input, STEP);
        st.accum -= STEP;
    }
    tf.translation = to_bevy(walker.eye());
    if let Some(a) = st.auto {
        if st.elapsed >= a + 0.5 && !st.reported {
            st.reported = true;
            let moved = (walker.location - st.start).truncate().length();
            let under = world.line(walker.location, walker.location - glam_compat::V::Z * 5000.0).is_some();
            println!(
                "WALK_AUTO {:.1}s: moved {moved:.0} uu, feet z {:.0} (start {:.0}), physics {:?}, floor below {under}",
                a,
                walker.feet().z,
                st.start.z - walker.half_height(),
                walker.physics
            );
            if walker.physics != Physics::Walking || !under {
                println!("WALK_AUTO FAILED");
            }
        }
    }
    st.walker = Some(walker);
}
