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
    /// El Governor dijo «no cabe» con cifras en la mano. `needed_mb` incluye el
    /// margen que hay que dejar libre, no solo lo que pesa el modelo.
    #[error("no hay sitio: hacen falta {needed_mb} MB libres (modelo + margen) y quedan {free_mb}")]
    ResourceExhausted { needed_mb: u32, free_mb: u32 },
    /// `respetarModelo` encendido y el modelo que eligió el producto no puede
    /// correr: **no se le sustituye por otro**. `motivo` es la frase del Governor
    /// (`no cabe: «x» necesita 1063 MB a 2048 y quedan 1302 libres (margen 1056)`)
    /// o, si el modelo cabe pero no cumple lo que pide el Plan, el motivo concreto
    /// de eso; siempre nombra al modelo pedido, que es lo que el usuario tiene que
    /// poder corregir en Ajustes. Los números van también sueltos para que el
    /// producto no tenga que leer una frase para pintar una tabla. Mismo criterio de
    /// `needed_mb` que en `ResourceExhausted`: RAM del modelo + el margen con el que
    /// se comprobó, no el de la spec.
    #[error("{motivo}")]
    ModeloPedidoNoCorre {
        modelo: crate::api::vocab::ModelId,
        motivo: String,
        needed_mb: u32,
        margen_mb: u32,
        free_mb: u32,
    },
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
            // La misma frase que da `Display`: duplicarla aquí es tener dos
            // mensajes que se separan en cuanto alguien cambia uno.
            BrainError::ResourceExhausted { .. } => self.to_string(),
            BrainError::ModeloPedidoNoCorre { .. } => self.to_string(),
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
            // Ninguna de las dos se arregla con otro modelo: una es que no hay
            // nada que quepa, y la otra es que el producto dijo explícitamente que
            // el suyo es el que tiene que correr.
            BrainError::NoEligibleModel | BrainError::ModeloPedidoNoCorre { .. } => false,
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
