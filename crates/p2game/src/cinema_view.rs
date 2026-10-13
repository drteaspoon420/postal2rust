//! Matinee cutscenes in the window: while a scene runs, the 3D camera shows the scene's camera
//! (location, rotation, FOV from [`p2game::cinema`]) instead of the player's eyes, and the scene's
//! screen fade is drawn over the view. The letterbox bars come from script (`HUD.bCinematicView`
//! drawn in `HUD.PostRender`), so they need nothing here.
//!
//! Runs after `ui::follow_player` so it wins while a scene is active. When the scene ends the FOV
//! returns to the default and `follow_player` takes the camera back.

use crate::window::{FlyCam, Sim};
use bevy::prelude::*;

/// Default horizontal-ish FOV the window camera is created with (window.rs `load_scene`).
const DEFAULT_FOV_DEG: f32 = 75.0;

pub(crate) struct CinemaViewPlugin;

impl Plugin for CinemaViewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CinemaViewState>()
            .add_systems(Startup, spawn_fade);
        // `apply_cinema_view` is chained into window.rs's frame right after `ui::follow_player`.
    }
}

#[derive(Resource, Default)]
pub(crate) struct CinemaViewState {
    /// A scene drove the camera last frame (so the FOV must be restored when it stops).
    active: bool,
}

#[derive(Component)]
pub(crate) struct FadeOverlay;

fn spawn_fade(mut commands: Commands) {
    commands.spawn((
        FadeOverlay,
        Node { position_type: PositionType::Absolute, left: px(0.0), top: px(0.0), width: percent(100.0), height: percent(100.0), ..default() },
        BackgroundColor(Color::NONE),
        // Above the scene and the script HUD, below the dev overlay.
        GlobalZIndex(i32::MAX - 2),
        Pickable::IGNORE,
    ));
}

pub(crate) fn apply_cinema_view(
    // Also pins this system to the main thread, where `p2game::cinema`'s thread-local lives (on a
    // worker thread `cinema()` is always None).
    sim: NonSend<Sim>,
    mut state: ResMut<CinemaViewState>,
    mut cams: Query<(&mut Transform, &mut Projection), With<FlyCam>>,
    mut fade: Query<&mut BackgroundColor, With<FadeOverlay>>,
    windows: Query<&Window>,
) {
    let view = p2game::cinema::cinema().and_then(|c| c.view.borrow().clone());
    if std::env::var_os("P2GAME_CINEMA_TRACE").is_some() && (view.is_some() || state.active) {
        println!("CINEMA_VIEW window cam gets {:?} cams {}", view.as_ref().map(|v| (v.location, v.rotation)), cams.iter().count());
    }
    let Ok((mut tf, mut proj)) = cams.single_mut() else { return };
    let mut fade_color = Color::NONE;
    match view {
        Some(v) => {
            state.active = true;
            let l = v.location;
            tf.translation = p2view::scene::to_bevy([l.x, l.y, l.z]);
            // Same Unreal -> Bevy rotation convention as ui::follow_player, built from the angles
            // (not look_to: scenes may look straight up or down, e.g. MilitaryBase's opening shot
            // at pitch 16384, where a look_to with up = Y is degenerate and breaks the camera).
            const U: f32 = std::f32::consts::TAU / 65536.0;
            let (yaw, pitch) = (v.rotation[1] as f32 * U, ((v.rotation[0] as i16) as f32) * U);
            tf.rotation = Quat::from_rotation_y(-yaw - std::f32::consts::FRAC_PI_2) * Quat::from_rotation_x(pitch);
            // Roll (camera shake / tilted shots).
            let roll = ((v.rotation[2] as i16) as f32) * U;
            if roll != 0.0 {
                tf.rotate_local_z(-roll);
            }
            if let Projection::Perspective(p) = &mut *proj {
                p.fov = v.fov.unwrap_or(DEFAULT_FOV_DEG).clamp(1.0, 170.0).to_radians();
            }
            if let Some((rgba, cover)) = v.fade {
                let a = cover.clamp(0.0, 1.0);
                if a > 0.0 {
                    fade_color = Color::srgba_u8(rgba[0], rgba[1], rgba[2], (a * 255.0) as u8);
                }
            }
        }
        None => {
            state.active = false;
            // Player view: UE2's FOVAngle is the horizontal angle (PlayerController.FOV already ran
            // it through ApplyAspectRatio: 85 at 4:3 -> ~101 at 16:9). Bevy's fov is vertical.
            let fov_h = sim
                .game
                .player_controller
                .and_then(|pc| sim.game.world.vm.get_prop(pc, "FOVAngle"))
                .map(|v| v.as_float())
                .filter(|f| f.is_finite() && *f > 1.0)
                .map(|f| f.clamp(1.0, 170.0));
            if let (Some(fov_h), Projection::Perspective(p)) = (fov_h, &mut *proj) {
                let aspect = windows.single().map(|w| w.width() / w.height().max(1.0)).unwrap_or(16.0 / 9.0);
                let v = 2.0 * ((fov_h.to_radians() * 0.5).tan() / aspect).atan();
                if (p.fov - v).abs() > 1e-4 {
                    p.fov = v;
                }
            } else if let Projection::Perspective(p) = &mut *proj {
                p.fov = DEFAULT_FOV_DEG.to_radians();
            }
        }
    }
    if let Ok(mut bg) = fade.single_mut() {
        if bg.0 != fade_color {
            bg.0 = fade_color;
        }
    }
}
