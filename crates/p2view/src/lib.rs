//! p2view library: builds renderable scenes from POSTAL 2 maps in the user's own install and
//! spawns them into a Bevy world. Used by the p2view viewer binary and other front ends.

pub mod baked;
pub mod outdoor;
pub mod projectors;
pub mod scene;
pub mod spawn;
pub mod terrain_clip;
pub mod uvanim;
pub mod zones;
