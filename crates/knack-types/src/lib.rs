//! Shared wire-format types and the Backend trait that both the cloud and
//! GitHub-backed implementations satisfy.

mod backend;
mod run;
mod skill;

pub use backend::*;
pub use run::*;
pub use skill::*;
