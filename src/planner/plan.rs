//! El `Plan`: la única salida ejecutable del Brain (§VIII del Canon).
//!
//! Es inmutable después de firmarse. El Executor no inventa campos ni amplía
//! tools, escrituras o tiempo: si hace falta más, se emite un Plan **nuevo** con
//! `parent_plan_hash`.

use crate::api::vocab::{
    ExecutionTarget, Intent, KeepAlive, Level, ModelId, OutputContract, ProviderId, Risk,
    ThinkingLevel, ToolId, VerificationMode,
};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 7;

/// El plan firmado. `plan_hash` va fuera del hash: se calcula sobre el resto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub schema_version: u32,
    pub level: Level,
    pub intent: Intent,
    pub model: ModelId,
    pub provider: ProviderId,
    pub execution_target: ExecutionTarget,
    /// Siempre uno de {2048, 4096, 8192} salvo config explícita por modelo.
    pub num_ctx: u32,
    pub keep_alive: KeepAlive,
    /// Cuánto razona el modelo. `off` en N0/N1, y nunca por encima del techo del
    /// producto. El razonamiento cuenta como salida (invariante de presupuesto).
    pub thinking: ThinkingLevel,
    pub max_output_tokens: u32,
    pub context_budget_tokens: u32,
    pub tools: Vec<ToolId>,
    pub max_tool_calls: u32,
    pub max_write_actions: u32,
    pub timeout_s: u32,
    pub output_contract: OutputContract,
    pub verification: VerificationMode,
    pub max_retries: u8,
    /// Solo la sugerencia precomputada: al escalar, el Governor la revalida y se
    /// firma otro Plan.
    pub escalate_to: Option<ModelId>,
    /// Panel «por qué». Se muestra al usuario; nunca es un selector silencioso.
    pub reason: String,
    /// Identificador de linaje (FNV-1a-64). No es una firma: sirve para
    /// emparejar `parent_plan_hash` en el log sin llevar `sha2` en el crate.
    #[serde(skip)]
    pub plan_hash: Option<String>,
    #[serde(default, skip)]
    pub parent_plan_hash: Option<String>,
    /// Cuánto vale el system prompt estable de esta corrida. Entra en la
    /// invariante de presupuesto junto con `context_budget_tokens`.
    #[serde(default)]
    pub system_tokens: u32,
    /// El mensaje de este turno, medido con el contador del producto. §11 habla de
    /// `system + contexto + salida ≤ num_ctx`, y el turno del usuario es contexto:
    /// mientras no se contó, un plan firmado como «cabe» podía mandar 1.500 tokens
    /// que nadie presupuestó.
    #[serde(default)]
    pub mensaje_tokens: u32,
    /// Los turnos de historial que caben dentro del presupuesto, contados desde el
    /// más reciente hacia atrás, y lo que valen. Es lo que el Brain manda al
    /// proveedor por el canal nativo de `messages`: el resto del historial se queda
    /// en el producto.
    #[serde(default)]
    pub historial_turnos: u8,
    #[serde(default)]
    pub historial_tokens: u32,
    /// El riesgo con el que se firmó. `risk` vive en la decisión, pero el
    /// Executor lo necesita para el Tool Gate.
    #[serde(default)]
    pub risk: Risk,
}

impl Plan {
    /// El Plan no se construye a mano: se firma. Este es el único constructor
    /// razonable y obliga a pasar por `validate_plan()`.
    #[allow(clippy::too_many_arguments)]
    pub fn firmar(
        level: Level,
        intent: Intent,
        model: ModelId,
        provider: ProviderId,
        execution_target: ExecutionTarget,
        num_ctx: u32,
        thinking: ThinkingLevel,
        tools: Vec<ToolId>,
        output_contract: OutputContract,
        verification: VerificationMode,
        reason: String,
    ) -> Plan {
        Plan {
            schema_version: SCHEMA_VERSION,
            level,
            intent,
            model,
            provider,
            execution_target,
            num_ctx,
            keep_alive: KeepAlive::PorDefecto,
            thinking,
            max_output_tokens: default_max_output(level),
            context_budget_tokens: default_context_budget(level),
            tools,
            max_tool_calls: default_max_tool_calls(level),
            max_write_actions: default_max_writes(level),
            timeout_s: default_timeout_s(level),
            output_contract,
            verification,
            max_retries: level.reintentos(),
            escalate_to: None,
            reason,
            plan_hash: None,
            parent_plan_hash: None,
            system_tokens: 0,
            mensaje_tokens: 0,
            historial_turnos: 0,
            historial_tokens: 0,
            risk: Risk::Low,
        }
    }

    /// La invariante de §11 del Canon: lo que va al modelo (system, el turno, el
    /// historial admitido, el presupuesto de contexto y la salida, contando el
    /// razonamiento como salida) no puede pasar de `num_ctx`.
    pub fn presupuesto_tokens(&self) -> u32 {
        self.system_tokens
            + self.mensaje_tokens
            + self.historial_tokens
            + self.context_budget_tokens
            + self.max_output_tokens
            + thinking_extra_tokens(&self.thinking)
    }

    pub fn cabe_en_ctx(&self) -> bool {
        self.presupuesto_tokens() <= self.num_ctx
    }

    /// El hash se calcula sobre la forma canónica sin el propio hash. Como `Plan`
    /// es inmutable tras firmar, el hash de dos planes idénticos coincide, y eso
    /// es lo que quiere el log.
    pub fn calcular_hash(&mut self) {
        let cuerpo =
            serde_json::to_string(self).unwrap_or_else(|_| "{}".into());
        self.plan_hash = Some(crate::observability::hash::fnv1a64(&cuerpo));
    }

    /// Un plan que **sí** se puede ejecutar cuando el pedido era inválido:
    /// N2 en trabajo, N1 en chat, tools mínimas del approval, y con aviso.
    pub fn seguro(mode: &str, model: ModelId, provider: ProviderId, tools: Vec<ToolId>) -> Plan {
        let trabajo = mode == crate::api::vocab::Mode::WORK;
        let level = if trabajo { Level::N2 } else { Level::N1 };
        let mut p = Plan::firmar(
            level,
            Intent::Ask,
            model,
            provider,
            ExecutionTarget::Local,
            level.num_ctx_minimo(),
            ThinkingLevel::Off,
            if level.permite_tools() { tools } else { vec![] },
            OutputContract::Texto,
            level.verificacion_minima(),
            "el plan pedido era inválido; se firmó el plan seguro".into(),
        );
        p.schema_version = SCHEMA_VERSION;
        p
    }
}

/// Techo de tokens de salida por nivel. **No es una garantía de longitud**: es el
/// tope que el proveedor recibe. Medido en Fase 0: sin tope, la mediana de
/// salida era 604 tokens a ~17 tok/s; con 512, el tiempo de la corrida caía un
/// 25 % y los tokens un 38 %.
pub fn default_max_output(level: Level) -> u32 {
    match level {
        Level::N0 => 192,
        Level::N1 => 512,
        Level::N2 => 1024,
        Level::N3 => 2048,
    }
}

/// Presupuesto de contexto dinámico. Los rangos de §XII del Canon, por perfil;
/// aquí va el nivel, que es lo que el Plan conoce antes de elegir modelo.
pub fn default_context_budget(level: Level) -> u32 {
    match level {
        Level::N0 => 128,
        Level::N1 => 384,
        Level::N2 => 1024,
        Level::N3 => 2048,
    }
}

pub fn default_max_tool_calls(level: Level) -> u32 {
    match level {
        Level::N0 | Level::N1 => 0,
        Level::N2 => 8,
        Level::N3 => 16,
    }
}

pub fn default_max_writes(level: Level) -> u32 {
    match level {
        Level::N0 | Level::N1 => 0,
        Level::N2 | Level::N3 => 3,
    }
}

/// Suelo de tiempo, no predicción. Deriva de los ~17 tok/s medidos en CPU: una
/// salida de 1024 tokens son ~60 s de generación, y eso sin contar la carga.
pub fn default_timeout_s(level: Level) -> u32 {
    match level {
        Level::N0 => 30,
        Level::N1 => 60,
        Level::N2 => 180,
        Level::N3 => 300,
    }
}

/// El razonamiento ocupa presupuesto de salida: si no se cuenta aquí, la
/// invariante de §11 se rompe justo en los modelos que piensan. La tabla vive en
/// `ThinkingLevel::presupuesto_tokens` para que sea **la misma** que la que el
/// proveedor manda por la línea.
pub fn thinking_extra_tokens(thinking: &ThinkingLevel) -> u32 {
    thinking.presupuesto_tokens()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(n: Level) -> Plan {
        Plan::firmar(
            n,
            Intent::Ask,
            "gemma3:1b".into(),
            "ollama".into(),
            ExecutionTarget::Local,
            n.num_ctx_minimo(),
            ThinkingLevel::Off,
            vec![],
            OutputContract::Texto,
            n.verificacion_minima(),
            "prueba".into(),
        )
    }

    #[test]
    fn un_plan_de_level_deja_espacio_al_prompt() {
        for n in [Level::N0, Level::N1, Level::N2, Level::N3] {
            let p = plan(n);
            assert!(
                p.cabe_en_ctx(),
                "{n:?} no cabe: {:?} > {}",
                p.presupuesto_tokens(),
                p.num_ctx
            );
        }
    }

    #[test]
    fn el_thinking_come_presupuesto() {
        let mut p = plan(Level::N3);
        p.system_tokens = 200;
        // N3 con salida de 2048 y contexto de 2048 ya va justo a 8192; el
        // razonamiento del modelo cuenta como salida, así que aquí se pasa.
        assert!(p.thinking == ThinkingLevel::Off, "el plan base va justo");
        p.thinking = ThinkingLevel::High;
        assert!(!p.cabe_en_ctx(), "{:?}", p.presupuesto_tokens());
    }

    #[test]
    fn el_hash_es_estable_y_no_depende_del_order_del_json() {
        let mut a = plan(Level::N1);
        let mut b = plan(Level::N1);
        a.calcular_hash();
        b.calcular_hash();
        assert_eq!(a.plan_hash, b.plan_hash);
        b.reason = "otro motivo".into();
        b.calcular_hash();
        assert_ne!(a.plan_hash, b.plan_hash);
    }

    #[test]
    fn el_plan_seguro_nunca_pide_tools_a_un_n1() {
        let p = Plan::seguro(
            crate::api::vocab::Mode::CHAT,
            "gemma3:1b".into(),
            "ollama".into(),
            vec!["write_file".into()],
        );
        assert_eq!(p.level, Level::N1);
        assert!(p.tools.is_empty());
        let w = Plan::seguro(
            crate::api::vocab::Mode::WORK,
            "gemma3:1b".into(),
            "ollama".into(),
            vec!["read_file".into()],
        );
        assert_eq!(w.level, Level::N2);
        assert_eq!(w.tools, vec!["read_file".to_string()]);
    }
}
