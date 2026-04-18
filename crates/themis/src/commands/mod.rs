//! Command implementations.
//!
//! Each module corresponds to one subcommand (or a closely related pair).
//! Every public `run` function takes resolved arguments and an optional
//! `ThemisClient` (local-only commands don't need one).

pub mod chaos;
pub mod completions;
pub mod define;
pub mod deploy;
pub mod destroy;
pub mod diagram;
pub mod estimate;
pub mod health;
pub mod init;
pub mod inspect;
pub mod list;
pub mod logs;
pub mod pause;
pub mod plan;
pub mod push_config;
pub mod shutdown;
pub mod validate;
pub mod version;
