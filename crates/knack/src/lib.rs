//! Knack CLI — library entry. The binary in `main.rs` is a thin shell that
//! parses arguments and calls into here. Everything is `pub` so integration
//! tests in `tests/` can drive the CLI without forking a subprocess.

pub mod api;
pub mod auth_store;
pub mod commands;
pub mod config;
pub mod docs;
pub mod errors;
pub mod output;
pub mod skill_pack;
pub mod skill_validators;
pub mod slug;
pub mod update_check;
pub mod workspace;
