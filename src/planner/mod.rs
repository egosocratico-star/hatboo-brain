//! Planner: toma la decisión, le pone modelo y presupuesto, la valida y la firma.

pub mod plan;
pub mod validation;

/// El `Armador` vive aquí fuera del stutter: `planner::planner` era el mismo
/// nombre dos veces, y lo que hay dentro es el armador, no otro planner.
mod armador;

pub use armador::{Armador, Carga, Entrada, Pie};
pub use plan::{SCHEMA_VERSION, Plan};
pub use validation::{PlanContext, PlanViolation, validate_plan, verificacion_sugerida};
