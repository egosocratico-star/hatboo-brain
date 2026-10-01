//! Observabilidad: eventos hacia fuera, métricas medidas, log con rotación y sin
//! texto completo por defecto. Nada de esto conoce la UI.

pub mod events;
pub mod hash;
pub mod logger;
pub mod metrics;

pub use events::{BrainEvent, CancelToken, Emitidor};
pub use logger::{Bitacora, BitacoraConfig};
pub use metrics::{Coste, Pesos, Referencia};
