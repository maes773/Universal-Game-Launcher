//! Integraciones (proveedores de juegos). Cada una es independiente y solo entrega
//! datos planos (`DiscoveredGame`) al núcleo; ninguna conoce SQLite.

pub mod binary_kv;
pub mod manual;
pub mod steam;
pub mod steam_achievements;
pub mod vdf;
