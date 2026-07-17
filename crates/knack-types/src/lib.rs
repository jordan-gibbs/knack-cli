//! Shared wire-format types and the Backend trait that both the cloud and
//! GitHub-backed implementations satisfy.

mod backend;
pub mod bom;
mod run;
mod skill;
pub mod tls;

pub use backend::*;
pub use bom::{strip_utf8_bom, strip_utf8_bom_str};
pub use run::*;
pub use skill::*;
