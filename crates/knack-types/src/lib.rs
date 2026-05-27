//! Shared wire-format types and the Backend trait that both the cloud and
//! GitHub-backed implementations satisfy.

mod backend;
mod skill;
mod run;

pub use backend::*;
pub use skill::*;
pub use run::*;
