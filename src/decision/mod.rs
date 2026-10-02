//! El Decision Engine: convierte estado en decisiones tipadas **antes** de que
//! hable el modelo generativo. No genera texto.
//!
//! Un lote por turno, una sola evaluación (fast path → caché → reglas → scoring).
//! Está prohibido gastar un LLM por intent, otro por level y otro por tools.

pub mod cache;
pub mod confidence;
pub mod engine;
pub mod fast_path;
pub mod rules;

use crate::api::vocab::{
    Confidence, DecisionSource, ExecutionTarget, Intent, Level, ModelTarget, OutputContract,
    Risk, ToolId, VerificationMode,
};
use serde::{Deserialize, Serialize};

/// El lote completo de un turno. `needs_tools` **no** existe: se deriva de
/// `tools`, y guardarlo aparte sería un sitio donde mentir.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionResult {
    pub intent: Intent,
    pub level: Level,
    pub risk: Risk,
    /// `true` = este pedido no necesita que un modelo genere nada (aritmética,
    /// lectura directa de un archivo nombrado).
    pub skip_generative: bool,
    pub execution_target: ExecutionTarget,
    pub model_target: ModelTarget,
    pub tools: Vec<ToolId>,
    pub output_contract: OutputContract,
    pub verification: VerificationMode,
    pub confidence: Confidence,
    pub source: DecisionSource,
    /// Qué regla o atajo decidió, para el panel «por qué» y para el golden test.
    #[serde(default)]
    pub por_que: String,
    /// La respuesta ya calculada cuando `skip_generative` es `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salida_directa: Option<String>,
    /// Ruta que el **producto** debe leer sin preguntar al modelo. Es la
    /// excepción documentada a «N0 no tiene tools»: una lectura directa acotada
    /// por el approval, que no es una tool que llame el modelo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lectura_directa: Option<String>,
}

impl DecisionResult {
    pub fn necesita_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    /// Nivel y verificación que exige el riesgo: un `high` nunca se queda bajo.
    pub fn con_suelo_de_riesgo(mut self) -> Self {
        if self.risk == Risk::High && self.level < Level::N2 {
            self.level = Level::N2;
            self.por_que.push_str(" · riesgo alto sube a N2");
        }
        self.nivelar_verificacion();
        self
    }

    /// Después de subir el nivel por cualquier suelo (el modo, el riesgo), la
    /// verificación tiene que subir con él. Un Plan que dice N2 y trae
    /// `Ninguna` miente: firma un nivel que no comprueba nada, y el mínimo del
    /// nivel es justo lo que §4 deja exigir.
    pub fn nivelar_verificacion(&mut self) {
        let minima =
            crate::decision::rules::nivelacion_verificacion(self.level, &self.output_contract);
        self.verification = self.verification.max(minima);
    }
}

/// Lo que devuelve el Engine. `Some` del Fast Path va directo al Planner.
#[derive(Debug, Clone, PartialEq)]
pub enum Decidido {
    Rapido(DecisionResult),
    Reglas(DecisionResult),
    Ninguno,
}
