//! Developer overlay in the top-left corner of the window: frame rate, map and game class, script
//! world counters, the player's pawn and controller state, and the camera. F3 toggles it.
//! `--no-debug` starts with it hidden.
//!
//! It is a plain Bevy UI text node drawn above the scene and the script HUD. It reads the VM and
//! does not change anything, so it can't affect gameplay.

use crate::window::{FlyCam, Sim};
use bevy::prelude::*;
use uscript::vm::{ObjId, Value};

pub(crate) struct DebugOverlayPlugin;

impl Plugin for DebugOverlayPlugin {
    fn build(&self, app: &mut App) {
        let visible = !std::env::args().any(|a| a == "--no-debug");
        app.insert_resource(DebugOverlay { visible, ..default() }).add_systems(Startup, spawn_overlay);
    }
}

#[derive(Resource, Default)]
pub(crate) struct DebugOverlay {
    pub(crate) visible: bool,
    /// Seconds since the text was last rebuilt (refreshed a few times a second, not every frame).
    since_refresh: f32,
    /// Exponentially smoothed frame time in seconds.
    smooth_dt: f32,
    worst_dt: f32,
}

#[derive(Component)]
pub(crate) struct DebugText;

fn spawn_overlay(mut commands: Commands, overlay: Res<DebugOverlay>) {
    commands.spawn((
        DebugText,
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(14.0), ..default() },
        TextColor(Color::srgb(0.85, 1.0, 0.85)),
        TextShadow::default(),
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
        Node { position_type: PositionType::Absolute, top: px(6.0), left: px(6.0), padding: UiRect::all(px(6.0)), ..default() },
        GlobalZIndex(i32::MAX - 1),
        if overlay.visible { Visibility::Inherited } else { Visibility::Hidden },
    ));
}

/// UE2 EPhysics names, by enum value.
const PHYSICS: [&str; 15] = [
    "None", "Walking", "Falling", "Swimming", "Flying", "Rotating", "Projectile", "Interpolating", "MovingBrush", "Spider", "Trailer", "Ladder", "RootMotion", "Karma", "KarmaRagDoll",
];

fn obj_prop(vm: &uscript::vm::Vm, o: ObjId, name: &str) -> Option<ObjId> {
    vm.get_prop(o, name).and_then(Value::as_obj).filter(|&p| live(vm, p))
}

fn live(vm: &uscript::vm::Vm, o: ObjId) -> bool {
    vm.objects.get(o).is_some_and(|x| !x.deleted)
}

pub(crate) fn update_overlay(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut overlay: ResMut<DebugOverlay>,
    sim: NonSend<Sim>,
    cam: Query<&Transform, With<FlyCam>>,
    mut text: Query<(&mut Text, &mut Visibility), With<DebugText>>,
) {
    let Ok((mut text, mut vis)) = text.single_mut() else { return };
    if keys.just_pressed(KeyCode::F3) {
        overlay.visible = !overlay.visible;
    }
    // Also toggled from the dev menu (View tab).
    let want = if overlay.visible { Visibility::Inherited } else { Visibility::Hidden };
    if *vis != want {
        *vis = want;
    }
    let dt = time.delta_secs();
    overlay.smooth_dt = if overlay.smooth_dt == 0.0 { dt } else { overlay.smooth_dt * 0.9 + dt * 0.1 };
    overlay.worst_dt = overlay.worst_dt.max(dt);
    overlay.since_refresh += dt;
    if !overlay.visible || overlay.since_refresh < 0.25 {
        return;
    }
    overlay.since_refresh = 0.0;
    let worst = std::mem::take(&mut overlay.worst_dt);

    let game = &sim.game;
    let vm = &game.world.vm;
    let mut s = String::with_capacity(512);
    use std::fmt::Write;
    let ms = overlay.smooth_dt * 1000.0;
    let _ = writeln!(s, "p2game {}  |  {:.0} fps  {:.1} ms (worst {:.1})", env!("CARGO_PKG_VERSION"), 1000.0 / ms.max(0.001), ms, worst * 1000.0);
    // After a save load the level package is the save; the host keeps the original map's name.
    let _ = writeln!(s, "map {}  game {}", game.host.map_name.borrow(), game.game_class);
    let _ = writeln!(
        s,
        "script frame {}{}  live actors {}  rigs {}",
        sim.frames,
        if sim.paused { " (PAUSED, P)" } else { "" },
        game.live_actors(),
        sim.rig_count()
    );
    let _ = writeln!(
        s,
        "missing natives {}  script errors {}",
        vm.missing_natives.values().sum::<u64>(),
        vm.script_errors.values().sum::<u64>()
    );

    match game.player_controller.filter(|&pc| live(vm, pc)) {
        Some(pc) => {
            let _ = writeln!(s, "controller {}  state {}", vm.object_name(pc), vm.state_name(pc));
            match obj_prop(vm, pc, "Pawn") {
                Some(p) => {
                    let loc = vm.get_prop(p, "Location").map(Value::as_vec).unwrap_or_default();
                    let vel = vm.get_prop(p, "Velocity").map(Value::as_vec).unwrap_or_default();
                    let rot = vm.get_prop(pc, "Rotation").map(Value::as_rot).unwrap_or_default();
                    let phys = vm.get_prop(p, "Physics").map(Value::as_int).unwrap_or(0) as usize;
                    let health = vm.get_prop(p, "Health").map(Value::as_int).unwrap_or(0);
                    let speed = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
                    let _ = writeln!(s, "pawn {}  state {}  health {}", vm.object_name(p), vm.state_name(p), health);
                    let _ = writeln!(s, "  loc {:.0} {:.0} {:.0}  rot p{} y{} r{}", loc[0], loc[1], loc[2], rot[0] & 0xFFFF, rot[1] & 0xFFFF, rot[2] & 0xFFFF);
                    let _ = writeln!(s, "  phys {}  speed {:.0} uu/s  vz {:.0}", PHYSICS.get(phys).copied().unwrap_or("?"), speed, vel[2]);
                    if let Some(w) = obj_prop(vm, p, "Weapon") {
                        let _ = writeln!(s, "  weapon {}  state {}", vm.object_name(w), vm.state_name(w));
                    }
                }
                None => {
                    let _ = writeln!(s, "no pawn");
                }
            }
        }
        None => {
            let _ = writeln!(s, "no player controller");
        }
    }
    if let Ok(t) = cam.single() {
        // Back to Unreal units for comparison with actor locations (Bevy metres, Y up).
        let u = t.translation * p2view::scene::UU_PER_M;
        let _ = writeln!(s, "camera {:.0} {:.0} {:.0}", u.x, u.z, u.y);
    }
    if let Some(c) = p2game::cinema::cinema() {
        let st = c.stats.borrow().clone();
        match c.view.borrow().as_ref() {
            Some(v) => {
                let _ = writeln!(
                    s,
                    "cutscene {:.0}%  cam {:.0} {:.0} {:.0} yaw {}  fov {}  fade {}  started {} ended {} skipped {}",
                    v.pct * 100.0,
                    v.location.x,
                    v.location.y,
                    v.location.z,
                    v.rotation[1] & 0xFFFF,
                    v.fov.map(|f| format!("{f:.0}")).unwrap_or_else(|| "-".into()),
                    v.fade.map(|f| format!("{:.2}", f.1)).unwrap_or_else(|| "-".into()),
                    st.scenes_started,
                    st.scenes_ended,
                    st.scenes_skipped
                );
            }
            None if st.scenes > 0 => {
                let _ = writeln!(s, "scenes {}  started {} ended {}", st.scenes, st.scenes_started, st.scenes_ended);
            }
            None => {}
        }
    }
    s.push_str("F3 overlay  F1 log stats  F10 dev menu  F11 editor");
    text.0 = s;
}
