//! Shared character support for POSTAL 2 skeletal meshes:
//! - [`runtime`]: Bevy-independent UE2 animation runtime (channels, tweening, blending,
//!   notifies, bone controllers, root motion, attachments). This is what the game VM's actor
//!   animation natives call.
//! - [`notify`]: decoding of `AnimNotify_*` objects.
//! - [`character`]: appearance from class defaults.
//! - [`rig`]: GPU-skinned Bevy spawning driven by [`runtime::AnimInstance`].

pub mod character;
pub mod notify;
pub mod notify_host;
pub mod ragdoll;
pub mod rig;
pub mod runtime;
