//! Lo que sale del Brain.

use super::vocab::{FailureClass, Intent, Level, ModelId, ToolId};
use crate::planner::Plan;
use crate::verification::VerificationResult;
use serde::{Deserialize, Serialize};

/// Estado honesto de la salida. `Unverifiable` **no** es éxito (§XIV del Canon):
/// se reporta aparte y no suma en el numerador de Efficiency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStatus {
    /// Verificado y pasado.
    Verificado,
    /// N2/N3 con contrato `patch`/`tool_call`: mostrado como propuesta hasta Pass.
    Propuesto,
    /// Hubo salida pero no se pudo verificar (o no había qué verificar).
    SinVerificar,
    /// El stream se retractó por fallar la verificación de formato.
    Rechazado,
}

/// La salida, con lo que hace falta para pintarla sin volver al modelo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub texto: String,
    pub status: OutputStatus,
    /// Llamadas a tool que propuso el modelo y **el producto** ejecutó. El Brain
    /// no ejecuta: las administra (§V, «Quién decide qué»).
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Razonamiento del modelo si el producto lo pidió y el nivel lo permitía.
    #[serde(default)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool: ToolId,
    pub args: serde_json::Value,
    /// `Ok` del producto tras ejecutarla. `None` si quedó pendiente de aprobación.
    #[serde(default)]
    pub resultado: Option<String>,
    #[serde(default)]
    pub ok: bool,
}

/// Lo que el Canon y el panel quieren medir. Todo medido en esta corrida; si un
/// dato no existe, es `None`, nunca un número inventado.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskMetrics {
    /// Segundos de reloj desde `RequestReceived` hasta el final, carga del modelo
    /// incluida (§15 del plan: `s` es wall-clock).
    pub duracion_ms: u64,
    pub ttft_ms: Option<u64>,
    pub tokens_entrada: Option<u64>,
    pub tokens_salida: Option<u64>,
    pub tok_s: Option<f32>,
    /// RAM que el modelo ocupó residente, si se pudo leer.
    pub ram_mb: Option<u64>,
    pub recargas: u32,
    pub reintentos: u8,
    pub contexto_rechazado: u32,
    pub clase_fallo: Option<FailureClass>,
    /// Coste normalizado: 1,00 = lo que cuesta hoy, en este modelo, sin Brain.
    pub coste: Option<f32>,
}

/// El resultado completo de un `run()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainResult {
    pub output: Output,
    pub plan: Plan,
    pub verification: VerificationResult,
    pub metrics: TaskMetrics,
}

impl BrainResult {
    pub fn es_exito(&self) -> bool {
        self.verification.es_pass()
    }
}

/// Traza de `inspect()`: cómo se llegó aquí. **Nunca** entra al prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTrace {
    pub senales: crate::api::vocab::Signals,
    /// `None` si el Fast Path no decidió nada (entonces hablaron las reglas).
    pub fast_path: Option<String>,
    pub decision: crate::decision::DecisionResult,
    pub modelo_elegido: ModelId,
    pub porque_este: String,
    pub descartados: Vec<(ModelId, String)>,
    pub contexto_rechazado: Vec<String>,
    pub recuperacion: Vec<String>,
}

/// La evidencia interna de una corrida: señales, restricciones, por qué ese
/// modelo y qué se descartó. Vive en el log y en `inspect()`, jamás en el prompt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub senales: crate::api::vocab::Signals,
    pub restricciones: Vec<String>,
    pub seleccionado_por: Option<String>,
    pub descartados: Vec<(ModelId, String)>,
    pub nivel_final: Option<Level>,
    pub intent_final: Option<Intent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_status_de_la_salida_viaja_en_json() {
        let o = Output {
            texto: "hola".into(),
            status: OutputStatus::SinVerificar,
            tool_calls: vec![],
            reasoning: None,
        };
        let s = serde_json::to_string(&o).unwrap();
        assert!(s.contains("\"sin_verificar\""));
        assert!(s.contains("\"toolCalls\""));
    }

    #[test]
    fn metricas_sin_dato_no_inventan_cero_falso() {
        let m = TaskMetrics::default();
        assert_eq!(m.ttft_ms, None);
        assert_eq!(m.tokens_salida, None);
        assert_eq!(m.coste, None);
        // El tiempo sí existe siempre: es lo que medimos nosotros.
        assert_eq!(m.duracion_ms, 0);
    }
}
