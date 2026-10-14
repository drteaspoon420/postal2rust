//! Clean-room reader for Unreal Engine 2 packages as shipped with POSTAL 2 (package version 118).
//!
//! Written from observation of the on-disk data; contains no code from the original game.
//! Game data is never bundled: callers point this crate at the user's own installation.

pub mod ci;
pub mod font;
pub mod emitter;
pub mod library;
pub mod lighting;
pub mod lightmap;
pub mod material;
pub mod model;
pub mod package;
pub mod props;
pub mod reader;
pub mod skeletal;
pub mod sound;
pub mod staticmesh;
pub mod terrain;
pub mod texture;
pub mod writer;

pub use package::{Export, Import, ObjRef, Package};
pub use props::{Properties, Property, Value, begin_object, read_properties};
pub use reader::Reader;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error on {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("not an Unreal package (tag {0:#010x})")]
    NotAPackage(u32),
    #[error("unexpected end of data at offset {0}")]
    Eof(usize),
    #[error("malformed compact index at offset {0}")]
    BadCompact(usize),
    #[error("implausible element count {1} at offset {0}")]
    BadCount(usize, i64),
    #[error("corrupt package: {0}")]
    Corrupt(&'static str),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;
