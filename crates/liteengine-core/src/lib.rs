//! LiteEngine core — the heart of the engine.
//!
//! This crate contains everything that makes LiteEngine *LiteEngine*:
//! the embedded Lua 5.4 VM and its Scratch-like API, the coroutine event
//! scheduler, the `.lemodule` module manager, the streaming asset store,
//! device capability detection, and the runtime that ties it all together.
//!
//! Rendering backends live in `liteengine-render`; the core only speaks to
//! them through the [`host::Host`] trait and the [`snapshot`] frame types.

pub mod assets;
pub mod device;
pub mod events;
pub mod host;
pub mod luaapi;
pub mod modules;
pub mod project;
pub mod runtime;
pub mod scheduler;
pub mod scene;
pub mod snapshot;

pub use device::{DeviceInfo, GpuPath};
pub use project::ProjectConfig;
pub use runtime::Engine;

/// Engine version reported to Lua (`engine.version()`) and the CLI.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// API surface version expected by `.lemodule` manifests (`engine_api = "1"`).
pub const ENGINE_API: u32 = 1;

/// Unified error type for engine startup, scripting and IO failures.
#[derive(Debug)]
pub enum EngineError {
    /// Project or module manifest problems.
    Config(String),
    /// Filesystem / archive failures.
    Io(String),
    /// Lua compile or runtime errors (already formatted with context).
    Script(String),
    /// Rendering / window failures.
    Render(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Config(s) => write!(f, "config error: {s}"),
            EngineError::Io(s) => write!(f, "io error: {s}"),
            EngineError::Script(s) => write!(f, "script error: {s}"),
            EngineError::Render(s) => write!(f, "render error: {s}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(e.to_string())
    }
}

impl From<mlua::Error> for EngineError {
    fn from(e: mlua::Error) -> Self {
        EngineError::Script(e.to_string())
    }
}

pub type EngineResult<T> = Result<T, EngineError>;
