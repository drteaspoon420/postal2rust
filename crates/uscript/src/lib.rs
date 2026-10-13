//! Clean-room UnrealScript support for POSTAL 2 (Unreal Engine 2, package version 118, licensee 2).
//!
//! - [`objects`]: parses the script object graph (classes, states, functions, structs, enums,
//!   constants and properties) from `.u` packages.
//! - [`bytecode`]: decodes function bytecode into an expression tree.
//! - [`disasm`]: renders decoded scripts as readable pseudo-code.
//! - [`vm`]: the interpreter (object model, frames, states and latent code, iterators,
//!   delegates, native registry). Class defaults: [`vm::Vm::class_default`],
//!   [`vm::Vm::find_class`], [`vm::Vm::default_value`].
//! - [`host`]: the [`host::Host`] trait an engine implements for world/actor natives, the
//!   actor life cycle (spawn, destroy, tick) and a headless host.
//!
//! Everything here was written from observation of the shipped binary data.

pub mod bytecode;
pub mod config;
pub mod defaults;
pub mod disasm;
pub mod graph;
pub mod host;
pub mod nav;
pub mod natives;
pub mod objects;
pub mod vm;

pub use bytecode::{Expr, Script, Stmt};
pub use objects::{ScriptObject, parse_export};

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error(transparent)]
    Pkg(#[from] upkg::Error),
    #[error("{0}")]
    Bad(String),
}

pub type Result<T> = std::result::Result<T, ScriptError>;
