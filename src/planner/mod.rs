//! Planner: toma la decisión, le pone modelo y presupuesto, la valida y la firma.

pub mod plan;
pub mod planner;
pub mod validation;

pub use plan::{SCHEMA_VERSION, Plan};
pub use planner::{Armador, Pie};
pub use validation::{PlanContext, PlanViolation, validate_plan, verificacion_sugerida};
