//! UE2 Canvas (HUD and menu 2D drawing) on Bevy.
//!
//! - [`canvas::Canvas`]: the script-visible Canvas state and the native drawing operations
//!   (DrawTile, DrawText, StrLen, TextSize, ...), queueing textured quads. No Bevy dependency in
//!   its logic, so VM natives and tests can drive it directly.
//! - [`font::CanvasFont`]: bitmap fonts loaded from the user's packages.
//! - [`eval::MaterialDb`]: flattens material graphs (Shader, Combiner, FinalBlend, ColorModifier,
//!   TexPanner/Rotator/Scaler/Oscillator, ConstantColor, FadeColor, MaterialSwitch, animated
//!   textures) into something one shader can draw, animated by time.
//! - [`render::CanvasPlugin`]: batches the queue into 2D meshes every frame.

pub mod canvas;
pub mod eval;
pub mod font;
pub mod natives;
pub mod render;
pub mod shell;

pub use canvas::{Canvas, DrawItem, MaterialSizer, Rgba, style};
pub use font::CanvasFont;
pub use render::{CanvasAssets, CanvasCamera, CanvasPlugin, CanvasStats, HudCanvas};
