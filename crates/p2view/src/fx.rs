//! Particle effects in p2view: every emitter actor placed in the map (Emitter, P2Emitter,
//! FireSizeableEmitter, ...) is resolved by `p2fx` and spawned at its saved transform. Emitters
//! with `UseCollision` collide with the map through a `p2collide` world. `--no-fx` disables this.

use bevy::prelude::*;
use p2fx::collide::FxCollision;
use p2fx::{FxLibrary, FxPlugin, actor_transform, spawn_emitter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct FxMapPlugin {
    pub game: PathBuf,
    pub map: String,
}

#[derive(Resource, Clone)]
struct FxMapCfg {
    game: PathBuf,
    map: String,
}

impl Plugin for FxMapPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FxPlugin);
        if std::env::args().any(|a| a == "--no-fx") {
            return;
        }
        app.insert_resource(FxMapCfg { game: self.game.clone(), map: self.map.clone() }).add_systems(Startup, spawn_map_emitters).add_systems(Update, apply_focus);
    }
}

fn focus_arg() -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == "--fx-focus").and_then(|i| a.get(i + 1)).cloned()
}

/// `--fx-focus <index|name>`: camera placement (eye, target) in Bevy space for one map emitter.
#[derive(Resource)]
struct FxFocus {
    target: (Vec3, Vec3),
    name: String,
    done: bool,
}

/// Picks an unobstructed eye point around an emitter (Unreal space in, Bevy space out): the
/// direction with the longest clear sightline, up to 380 uu away and 90 uu above it.
fn best_view(w: &p2collide::World, loc: Vec3) -> (Vec3, Vec3) {
    let target = loc + Vec3::Z * 40.0;
    let mut best = (0.0f32, target + Vec3::new(300.0, 0.0, 90.0));
    for k in 0..16 {
        let a = k as f32 * std::f32::consts::TAU / 16.0;
        let want = target + Vec3::new(a.cos() * 380.0, a.sin() * 380.0, 90.0);
        let eye = match w.line(target, want) {
            Some(h) => target + (h.location - target) * 0.85,
            None => want,
        };
        let d = (eye - target).length();
        if d > best.0 + 1.0 {
            best = (d, eye);
        }
    }
    (p2fx::geom::to_bevy(best.1), p2fx::geom::to_bevy(target))
}

fn apply_focus(focus: Option<ResMut<FxFocus>>, mut cams: Query<&mut Transform, With<Camera3d>>) {
    let Some(mut f) = focus else { return };
    if f.done {
        return;
    }
    let (eye, target) = f.target;
    let mut n = 0;
    for mut tf in &mut cams {
        *tf = Transform::from_translation(eye).looking_at(target, Vec3::Y);
        n += 1;
    }
    if n == 0 {
        return;
    }
    f.done = true;
    println!("FX focus: camera on {} from {eye:.1} toward {target:.1}", f.name);
}

fn spawn_map_emitters(mut commands: Commands, cfg: Res<FxMapCfg>) {
    let t0 = std::time::Instant::now();
    let Ok(fx) = FxLibrary::new(&cfg.game) else { return };
    let Some(path) = fx.lib.map_path(&cfg.map) else { return };
    let Ok(map) = upkg::Package::open(&path).map(Arc::new) else { return };
    fx.lib.insert(map.clone());
    let (mut n, mut failed, mut hidden, mut particles, mut colliding) = (0, 0, 0, 0, false);
    for (export, _class) in FxLibrary::map_emitter_actors(&map) {
        match fx.load_actor(&map, export) {
            Ok(sys) if !sys.emitters.is_empty() => {
                if sys.actor.hidden {
                    hidden += 1;
                    continue;
                }
                colliding |= sys.emitters.iter().any(|e| e.data.use_collision);
                particles += sys.particle_capacity();
                let tf = actor_transform(&sys.actor);
                spawn_emitter(&mut commands, Arc::new(sys), tf);
                n += 1;
            }
            Ok(_) => {}
            Err(_) => failed += 1,
        }
    }
    let focus = focus_arg();
    let world = if colliding || focus.is_some() { p2collide::World::build(&fx.lib, &map).ok().map(Arc::new) } else { None };
    if let (Some(w), true) = (&world, colliding) {
        commands.insert_resource(FxCollision(w.clone()));
    }
    if let (Some(f), Some(w)) = (focus, &world) {
        // Emitters by name or index (in map order).
        let list: Vec<(String, Vec3)> = FxLibrary::map_emitter_actors(&map)
            .into_iter()
            .filter_map(|(e, _)| fx.load_actor(&map, e).ok())
            .filter(|s| !s.emitters.is_empty() && !s.actor.hidden)
            .map(|s| (s.actor.object_name.clone(), Vec3::from(s.actor.location)))
            .collect();
        let pick = f.parse::<usize>().ok().and_then(|i| list.get(i)).or_else(|| list.iter().find(|(n, _)| n.eq_ignore_ascii_case(&f)));
        if let Some((name, loc)) = pick {
            commands.insert_resource(FxFocus { target: best_view(w, *loc), name: name.clone(), done: false });
        } else {
            println!("FX focus: no emitter '{f}' (have {})", list.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "));
        }
    }
    println!("FX {}: {n} emitters spawned ({particles} particle slots, {hidden} hidden, {failed} failed, collision={colliding}) in {} ms", cfg.map, t0.elapsed().as_millis());
}
