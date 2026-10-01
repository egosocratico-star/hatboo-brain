//! `BrainError` tipado: nada de `String` suelto en la API (§6 del plan).
//!
//! Dos reglas que vienen de §1 y que son fáciles de romper por accidente:
//! - `run()` devuelve `Ok` **siempre que haya salida que mostrar**. Un resultado
//!   no verificado es `Ok(BrainResult)` con `verification: Unverifiable` y
//!   `output.status: sin_verificar`; no es un error.
//! - `Err` solo cuando no hay nada que enseñar.

use crate::api::request::RequestIssue;
use crate::planner::PlanViolation;
use crate::providers::ProviderError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "tipo", content = "dato", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BrainError {
    #[error("pedido inválido: {0}")]
    InvalidRequest(RequestIssue),
    #[error("plan inválido: {0}")]
    InvalidPlan(PlanViolation),
    /// Ningún modelo cumple tools / contexto / RAM / policy.
    #[error("ningún modelo cumple lo que pide este pedido")]
    NoEligibleModel,
    /// El Governor dijo «no cabe» con cifras en la mano.
    #[error("no hay sitio: hacen falta {needed_mb} MB libres y quedan {free_mb}")]
    ResourceExhausted { needed_mb: u32, free_mb: u32 },
    #[error("proveedor: {0}")]
    Provider(ProviderError),
    #[error("se agotó el tiempo del plan")]
    Timeout,
    #[error("cancelado")]
    Cancelled,
    #[error("configuración: {0}")]
    Config(crate::config::ConfigError),
}

impl BrainError {
    /// Lo que ve el usuario. El Canon dice que la UI muestra el `reason` del Plan
    /// o este mensaje, y que ningún fallo se traga.
    pub fn mensaje(&self) -> String {
        match self {
            BrainError::InvalidRequest(i) => i.mensaje(),
            BrainError::InvalidPlan(v) => v.mensaje(),
            BrainError::NoEligibleModel => {
                "ningún modelo disponible cumple lo que pide este pedido".into()
            }
            BrainError::ResourceExhausted { needed_mb, free_mb } => format!(
                "no hay sitio: hacen falta {needed_mb} MB libres y quedan {free_mb}"
            ),
            BrainError::Provider(e) => match e {
                ProviderError::ModeloNoInstalado(m) => {
                    format!("el modelo «{m}» no está instalado")
                }
                ProviderError::SinCredencial(p) => {
                    format!("{p} necesita una clave y no la tiene")
                }
                ProviderError::TiempoFuera => "el proveedor tardó demasiado".into(),
                ProviderError::Cancelado => "cancelado".into(),
                otra => otra.to_string(),
            },
            BrainError::Timeout => "se agotó el tiempo del plan".into(),
            BrainError::Cancelled => "cancelado".into(),
            BrainError::Config(e) => e.mensaje(),
        }
    }

    /// Si el Brain puede recuperar con **otro** modelo. Un 401 no se arregla
    /// escalando de tier.
    pub fn recuperable(&self) -> bool {
        match self {
            BrainError::Provider(p) => !p.es_no_recuperable(),
            BrainError::Timeout | BrainError::ResourceExhausted { .. } => true,
            BrainError::NoEligibleModel => false,
            _ => false,
        }
    }
}

impl From<ProviderError> for BrainError {
    fn from(e: ProviderError) -> Self {
        BrainError::Provider(e)
    }
}

impl From<PlanViolation> for BrainError {
    fn from(v: PlanViolation) -> Self {
        BrainError::InvalidPlan(v)
    }
}

impl From<RequestIssue> for BrainError {
    fn from(i: RequestIssue) -> Self {
        BrainError::InvalidRequest(i)
    }
}
