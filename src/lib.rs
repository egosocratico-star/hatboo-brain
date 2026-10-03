//! HATBOO BRAIN — el cerebro que administra el trabajo alrededor de un modelo.
//!
//! Crate Rust embebible, no un servidor ni un chatbot. El producto elige el
//! modelo y ejecuta las tools; el Brain decide **cómo, cuándo y con qué recursos**
//! trabaja ese modelo, y explica cada desvío con el `reason` del Plan.
//!
//! ```text
//! BrainRequest
//!   → Fast Path ─ Some ─────────────┐
//!        └ None → Motor de reglas ───┤
//!                                    ↓
//!   Governor + Selector → Planner → PLAN firmado (plan_hash)
//!   → Contexto + Tool Gate + Prompt → Proveedor → Ejecutor del producto
//!   → Verificación ─ Pass → BrainResult
//!                   └ Fail → Recuperación por clase de fallo
//!   → Log
//! ```
//!
//! ## Cómo se usa
//!
//! ```no_run
//! use hatboo_brain::prelude::*;
//! use std::sync::Arc;
//!
//! # async fn demo(proveedor: Arc<dyn ModelProvider>) -> Result<(), BrainError> {
//! let brain = Brain::nuevo(Montaje::de_proveedor(proveedor))?;
//! let req = BrainRequest::nuevo("mi-producto", Mode::CHAT, "¿qué hace src/main.rs?");
//! let r = brain.run(&req).await?;
//! // `output.status` dice si está verificado, propuesto o sin verificar.
//! println!("{} — {}", r.output.texto, r.plan.reason);
//! # Ok(())
//! # }
//! ```
//!
//! ## Las piezas, por si vienes de otro producto
//!
//! - [`api`] · los cinco contratos que no se rompen: Request, Decision, Plan,
//!   Provider, Verification.
//! - [`decision`] · el motor: fast path → reglas de JSON → heurística. Cero LLM.
//! - [`planner`] · el Plan inmutable, sus siete comprobaciones y el plan seguro.
//! - [`resources`] · el Governor y su sonda de hardware, que **inyecta el
//!   producto**: este crate no trae `sysinfo`.
//! - [`providers`] · `ModelProvider` con Ollama, OpenAI-compatible, Anthropic y
//!   cualquier puerta genérica. Cada una detrás de su feature.
//! - [`verification`] · Pass / Fail / Unverifiable, con `Unverifiable` escrito
//!   para que no se lea como éxito.
//! - [`brain`] · el bucle.
//!
//! ## Lo que este crate **no** hace
//!
//! No recuerda conversaciones anteriores. No relaja el sandbox ni los cuatro
//! niveles de aprobación. No ejecuta tools ni comandos por su cuenta: pide que
//! los ejecuten. No habla con la UI. Y no afirma nada que no haya medido: si no
//! hay línea base medida para un modelo, [`observability::metrics::Referencia`]
//! devuelve coste `None` en vez de un número bonito.

pub mod api;
pub mod brain;
pub mod config;
pub mod context;
pub mod decision;
pub mod models;
pub mod observability;
pub mod planner;
pub mod project;
pub mod prompt;
pub mod providers;
pub mod recovery;
pub mod resources;
pub mod security;
pub mod tools;
pub mod verification;

pub use api::error::BrainError;
pub use api::request::BrainRequest;
pub use api::response::BrainResult;
pub use brain::{Brain, EjecutarTool, Montaje, OpcionesDeCorrida};

/// Lo mínimo para empezar.
pub mod prelude {
    pub use crate::api::request::{BrainRequest, Message, ProjectContext, ToolInfo, ToolSet};
    pub use crate::api::response::{BrainResult, OutputStatus};
    pub use crate::api::vocab::{
        ApprovalLevel, ExecutionPolicy, Level, Mode, ModelId, ToolId,
    };
    pub use crate::api::BrainError;
    pub use crate::brain::{Brain, EjecutarTool, Montaje, OpcionesDeCorrida};
    pub use crate::config::schema::BrainConfig;
    pub use crate::models::{ModelInfo, Registry};
    pub use crate::providers::ModelProvider;
    pub use crate::resources::{Governor, ResourceProbe};
}

/// Versión del crate, para el log (`brain_version` en cada registro).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::prelude::*;
    use std::sync::Arc;

    fn registro() -> Vec<ModelInfo> {
        use crate::api::vocab::Profile;
        use crate::models::ModelKind;
        let mut m = std::collections::BTreeMap::new();
        m.insert(2048u32, 878u64);
        m.insert(4096u32, 1037u64);
        m.insert(8192u32, 1652u64);
        vec![ModelInfo {
            id: "gemma3:1b".into(),
            provider: "mock".into(),
            local: true,
            kind: ModelKind::Generativo,
            profile: Profile::Nano,
            tier: 1,
            ram_mb_by_ctx: m,
            max_ctx: 32768,
            strengths: vec![],
            supports_tools: true,
            supports_thinking: false,
            supports_vision: false,
            structured_output: false,
            disco_mb: Some(815),
        }]
    }

    fn cerebro(texto: &str) -> (Arc<crate::providers::MockProvider>, Brain) {
        let mock = Arc::new(crate::providers::MockProvider::nuevo(registro()));
        mock.responde_texto(texto);
        let montaje = Montaje {
            config: BrainConfig::default(),
            proveedores: vec![mock.clone()],
            registry: Registry::nuevo(registro()),
            reglas: crate::decision::rules::Reglas::default(),
            herramientas: crate::tools::Herramientas::default(),
            sonda: Arc::new(crate::resources::SondaFija {
                libre: Some(6000),
                ..Default::default()
            }),
            contador: Arc::new(crate::prompt::Estimador),
            identidad: crate::prompt::Identidad::default(),
            ejecutor_tools: None,
            ejecutor_comandos: None,
            lector: None,
            decisiones: None,
            consentimiento_api: false,
        };
        (mock, Brain::nuevo(montaje).unwrap())
    }

    #[tokio::test]
    async fn un_pedido_normal_llega_a_un_plan_firmado() {
        let (_m, brain) = cerebro("hola, qué tal");
        let req = BrainRequest::nuevo("hatboo", Mode::CHAT, "explícame qué es un mutex");
        let r = brain.run(&req).await.expect("corrió");
        assert_eq!(r.output.texto, "hola, qué tal");
        assert!(r.plan.plan_hash.is_some(), "el plan sale firmado");
        assert_eq!(r.plan.model, "gemma3:1b");
        // Un modelo que no razona no promete razonamiento.
        assert_eq!(
            r.plan.thinking,
            crate::api::vocab::ThinkingLevel::Off
        );
    }

    #[tokio::test]
    async fn el_fast_path_no_toca_al_proveedor() {
        let (m, brain) = cerebro("no debería leerse");
        let r = brain
            .run(&BrainRequest::nuevo("hatboo", Mode::CHAT, "20+8"))
            .await
            .unwrap();
        assert_eq!(r.output.texto, "28");
        assert_eq!(m.n_peticiones(), 0, "sin generativo no hay HTTP");
        assert_eq!(
            r.verification,
            crate::verification::VerificationResult::NoRequerida
        );
    }

    #[tokio::test]
    async fn sin_proveedores_no_hay_cerebro() {
        let montaje = Montaje {
            proveedores: vec![],
            ..Montaje::de_proveedor(Arc::new(crate::providers::MockProvider::default()))
        };
        assert!(matches!(
            Brain::nuevo(montaje),
            Err(BrainError::Config(crate::config::ConfigError::SinBackend))
        ));
    }
}
