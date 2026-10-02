//! Verificación. Un `Pass` tiene que valer algo, y un `Unverifiable` no es éxito:
//! se reporta aparte (§XIV del Canon).

pub mod engine;
pub mod execution;
pub mod json;
pub mod patch;
pub mod text;

use crate::api::vocab::{FailureClass, OutputContract};
use serde::{Deserialize, Serialize};

/// Lo que pone el producto para leer un archivo del proyecto: ruta → contenido.
/// Es una sola definición porque la piden los dos lados: `Entorno.leer` la usa
/// prestada y el `Montaje` del runtime la guarda en un `Arc` (de ahí `Send + Sync`).
pub type Lector = dyn Fn(&str) -> Option<String> + Send + Sync;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail(Fallo),
    /// No se pudo comprobar. Se dice, no se disfraza.
    Unverifiable { motivo: Unverifiable },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Fallo {
    pub clase: FailureClass,
    pub motivo: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unverifiable {
    /// El contrato no es comprobable en código (prosa).
    SinContratoVerificable,
    /// El proyecto no tiene tests ni comando de verificación.
    ProyectoSinVerificacion,
    /// El comando no se pudo correr (sin permiso, sin sandbox, no existe).
    ComandoNoEjecutable(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "estado", rename_all = "snake_case")]
pub enum VerificationResult {
    /// Ninguna verificación pedida (N0).
    NoRequerida,
    Pass,
    Fail { clase: FailureClass, motivo: String },
    Unverifiable { motivo: Unverifiable },
}

impl VerificationResult {
    pub fn es_pass(&self) -> bool {
        matches!(self, VerificationResult::Pass)
    }

    pub fn es_unverifiable(&self) -> bool {
        matches!(self, VerificationResult::Unverifiable { .. })
    }

    pub fn etiqueta(&self) -> &'static str {
        match self {
            VerificationResult::NoRequerida => "sin verificar",
            VerificationResult::Pass => "verificado",
            VerificationResult::Fail { .. } => "falló",
            VerificationResult::Unverifiable { .. } => "no verificable",
        }
    }

    pub fn al_resultado(&self) -> Verdict {
        match self {
            VerificationResult::Pass => Verdict::Pass,
            VerificationResult::Fail { clase, motivo } => Verdict::Fail(Fallo {
                clase: *clase,
                motivo: motivo.clone(),
            }),
            VerificationResult::NoRequerida => Verdict::Pass,
            VerificationResult::Unverifiable { motivo } => Verdict::Unverifiable {
                motivo: motivo.clone(),
            },
        }
    }
}

pub use engine::{Entorno, verificar};

/// Lo que cada verificador necesita: la salida, su contrato y el entorno del
/// proyecto (para el `patch` y para correr comandos).
#[derive(Debug, Clone)]
pub struct Candidato<'a> {
    pub texto: &'a str,
    pub contrato: OutputContract,
    pub root: Option<&'a str>,
    /// El diff o el contenido que el modelo propone, si el contrato es `patch`.
    pub archivo_objetivo: Option<&'a str>,
}
