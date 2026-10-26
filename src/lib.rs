//! assfonts library: ASS subtitle font indexing, matching, subsetting,
//! embedding, and MKS/MKV muxing.
//!
//! The `assfonts` binary (`src/main.rs`) is a thin CLI wrapper around this
//! library; `assfonts-gui` (the `gui/` workspace member) builds its UI on the
//! same modules.

pub mod ass;
pub mod cff_fix;
pub mod cli;
pub mod commands;
pub mod db;
pub mod embed;
pub mod error;
pub mod font;
pub mod mux;
pub mod rename;
pub mod subset;
