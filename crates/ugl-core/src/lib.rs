//! UGL Core: todo lo que no es interfaz.
//!
//! La interfaz (crate `ugl-app`) solo habla con [`Core`]; nunca toca SQLite
//! ni las integraciones directamente.

pub mod achievement_engine;
pub mod application;
pub mod database;
pub mod diagnostics;
pub mod domain;
pub mod error;
pub mod integrations;
pub mod paths;
pub mod process_manager;
pub mod registry;
pub mod settings;

pub use application::Core;
pub use domain::{
    DiscoveredGame, Game, GameId, GameStatus, LaunchKind, RegisterSummary, SortOrder,
};
pub use error::{Result, UglError};
pub use paths::AppPaths;
