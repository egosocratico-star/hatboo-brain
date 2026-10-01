//! Recuperación por clase de fallo. §XIV: un reintento por clase, dentro de
//! `max_retries`, y solo `model_capability` sube de tier.

pub mod classifier;
pub mod escalator;

pub use classifier::{clasificar, Origen};
pub use escalator::{Accion, Escalador};
