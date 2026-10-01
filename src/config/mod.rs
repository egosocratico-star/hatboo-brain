//! Configuración del Brain: lo que el producto fija al construirlo, más los
//! archivos de política (`brain-rules.json`, `tools.json`, `models.json`).

pub mod loader;
pub mod schema;

pub use schema::{BrainConfig, ConfigError, Ids};
