//! Clean-room id RoQ movie playback for POSTAL 2 (`Movies\*.RoQ`).
//!
//! * [`roq`]: chunk parser and decoder (VQ codebooks, 4x4/2x2 blocks, motion compensation,
//!   DPCM audio).
//! * [`player`]: the UE2 `Movie` / HUD movie natives as a Bevy-independent state machine.
//! * `bevy_plugin` (feature "bevy"): fullscreen letterboxed playback with the soundtrack
//!   played through `p2sound`.

pub mod player;
pub mod roq;

#[cfg(feature = "bevy")]
pub mod bevy_plugin;

pub use player::{MoviePlayer, Placement, find_movie};
pub use roq::{Decoder, Event, RoqError};
