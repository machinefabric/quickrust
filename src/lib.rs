//! Haul: a drop-in Cargo replacement that owns the Rust environment.
//!
//! Haul installs and selects toolchains, manages the build state of every project on the
//! machine, and runs each command with the toolchain's Cargo and rustc as its engine, around
//! which it applies policy, observation, caching and its own output.

pub mod analyze;
pub mod archive;
pub mod artifact_cache;
pub mod bench;
pub mod cache;
pub mod cli;
pub mod compare;
pub mod config;
pub mod discover;
pub mod driver;
pub mod engine;
pub mod fast;
pub mod inventory;
pub mod net;
pub mod prebuilt;
pub mod proc;
pub mod recommend;
pub mod remedy;
pub mod render;
pub mod resolve;
pub mod rustc_info;
pub mod session;
pub mod setup;
pub mod store;
pub mod toolchain;
pub mod ui;
pub mod util;
pub mod view;
pub mod waste;
pub mod wrapper;

pub const SCHEMA_VERSION: &str = "1.0";

pub const ENV_SESSION: &str = "HAUL_SESSION_ID";
pub const ENV_EVENT_DIR: &str = "HAUL_EVENT_DIR";
pub const ENV_INNER_WRAPPER: &str = "HAUL_INNER_WRAPPER";
pub const ENV_WRAPPER_FAIL: &str = "HAUL_WRAPPER_FAIL";
pub const ENV_EXEC_GUARD: &str = "HAUL_EXEC_GUARD";
pub const ENV_HOME: &str = "HAUL_HOME";

/// Exit status used when Haul itself (not Cargo) fails before delegation.
pub const EXIT_HAUL_ERROR: i32 = 2;
/// Exit status for a blocking budget/policy failure on an otherwise successful command.
pub const EXIT_POLICY_FAIL: i32 = 3;
/// Exit status for an incomparable result under a policy that blocks on it.
pub const EXIT_INCOMPARABLE: i32 = 4;
