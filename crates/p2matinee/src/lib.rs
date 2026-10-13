//! Matinee (UE2 `SceneManager`) cutscenes for POSTAL 2 maps.
//!
//! * [`scene`]: decodes `SceneManager`, `InterpolationPoint`, `ActionMoveCamera` /
//!   `ActionPause` and the `SubAction*` objects from a map package.
//! * [`runtime`]: [`runtime::ScenePlayer`], a Bevy-independent playback runtime implementing
//!   `GetTotalSceneTime`, `Trigger`, the `SceneStarted` / `SceneEnded` events, path
//!   interpolation, camera orientation modes and sub-action effects.

pub mod runtime;
pub mod scene;

/// Pitch, yaw, roll in Unreal angle units (65536 = full turn).
pub type Rotator = [i32; 3];

pub use runtime::{LookTarget, NoActors, SceneEvent, SceneHost, ScenePlayer, ViewState, total_scene_time};
pub use scene::{Action, ActionKind, Scene, SubAction, SubActionKind, parse_scene, scenes_in};
