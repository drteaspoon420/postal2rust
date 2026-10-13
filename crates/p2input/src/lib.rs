//! UE2 input layer for the POSTAL 2 port.
//!
//! - [`keys`]: `EInputKey` / `EInputAction` codes and names.
//! - [`bindings`]: User.ini `[Engine.Input]` key bindings and the 40 command aliases.
//! - [`input`]: press/release filtering, binding execution (`Button`, `Toggle`, `Axis`, `Count`,
//!   `Pulse`, aliases, `|` chains, `OnRelease`), end-of-frame axis scaling, and the input console
//!   queries (`KEYNAME`, `BINDING2KEYSTR`, `ISKEYBIND`, ...). Targets implement [`InputTarget`],
//!   which the game host backs with the PlayerController's `input` variables and exec functions.
//! - `bevy_map` (feature `bevy`): Bevy keyboard/mouse/gamepad to `EInputKey` events.
//!
//! Event flow per frame, as in UE2: each raw event goes `Input::pre_process` (drop repeats), then
//! to the interactions (`InteractionMaster.Process_KeyEvent`, see p2canvas::natives); if no
//! interaction consumes it, `Input::process` runs the binding. Then `Input::finish_frame` sends
//! `IST_Hold` for held keys and scales the axes for `PlayerInput`.

pub mod bindings;
pub mod input;
pub mod keys;

#[cfg(feature = "bevy")]
pub mod bevy_map;

pub use bindings::{Alias, Bindings};
pub use input::{Input, InputTarget};
pub use keys::{Action, Key};
