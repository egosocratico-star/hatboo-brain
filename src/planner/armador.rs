//! El `Armador`: `DecisionResult` + modelo elegido + consejo del Governor → `Plan`
//! validado y firmado. Si el plan sale inválido, se firma el **plan seguro** y se
//! avisa: §VIII del Canon.

use super::plan::{
    default_max_output, default_timeout_s, thinking_extra_tokens, Plan,
};
use super::validation::{validate_plan, PlanContext, PlanViolation};
use super::SCHEMA_VERSION;
use crate::api::request::BrainRequest;
use crate::api::vocab::{
    ExecutionTarget, KeepAlive, Level, ModelId, Risk, ThinkingLevel, ToolId,
};
use crate::decision::DecisionResult;
use crate::models::ModelInfo;
use crate::resources::Consejo;

/// Qué hizo el armador: el plan y por qué ese modelo.
#[derive(Debug, Clone, PartialEq)]
pub struct Pie {
    pub plan: Plan,
    pub porque_este_modelo: String,
    pub descartados: Vec<(ModelId, String)>,
    /// `true` si hubo que firmar el plan seguro porque el pedido era inválido.
    pub degradado: bool,
    pub violacion: Option<PlanViolation>,
}

pub struct Armador;

impl Armador {
    /// Une todo. `system_tokens` llega del Prompt Engine (ya medido con el
    /// contador del producto), porque la invariante de presupuesto lo necesita.
    pub fn armar(
        req: &BrainRequest,
        decision: &DecisionResult,
        modelo: &ModelInfo,
        consejo: &Consejo,
        system_tokens: u32,
        ctx: &PlanContext,
    ) -> Pie {
        let nivel = decision.level;
        let num_ctx = if consejo.cabe {
            consejo.num_ctx
        } else {
            nivel.num_ctx_minimo()
        };

        // thinking: techo del producto, mínimo del nivel, y lo que el modelo sabe
        // hacer. Si el modelo no razona, se ignora y queda dicho.
        let thinking = elegir_thinking(req, decision, modelo, consejo);
        let mut razon_ignorado = String::new();
        let thinking = if thinking != ThinkingLevel::Off && !modelo.supports_thinking {
            razon_ignorado = format!(" · «{}» no razona: thinking vuelve a off", modelo.id);
            ThinkingLevel::Off
        } else {
            thinking
        };

        let mut max_output = default_max_output(nivel);
        let mut contexto = decision_a_contexto(decision);
        // Invariante de §11: system + contexto + salida (+ razonamiento) ≤ num_ctx.
        // Se aprieta primero la salida y después el contexto: la seguridad y el
        // objetivo del usuario caben siempre antes que un archivo grande.
        let extra = thinking_extra_tokens(&thinking);
        while system_tokens + contexto + max_output + extra > num_ctx && max_output > 64 {
            max_output -= 64;
        }
        while system_tokens + contexto + max_output + extra > num_ctx && contexto > 64 {
            contexto -= 64;
        }

        let tools = tools_del_plan(req, decision);
        let mut plan = Plan {
            schema_version: SCHEMA_VERSION,
            level: nivel,
            intent: decision.intent,
            model: modelo.id.clone(),
            provider: modelo.provider.clone(),
            execution_target: decision_target(req, decision, modelo),
            num_ctx,
            keep_alive: KeepAlive::PorDefecto,
            thinking,
            max_output_tokens: max_output,
            context_budget_tokens: contexto,
            tools,
            max_tool_calls: max_tool_calls(nivel, decision),
            max_write_actions: max_writes(nivel, decision),
            timeout_s: default_timeout_s(nivel),
            output_contract: decision.output_contract,
            verification: decision.verification,
            max_retries: nivel.reintentos(),
            escalate_to: None,
            reason: format!(
                "{} · {} · ctx {num_ctx} · salida {max_output}{}",
                decision.por_que, decision.level.etiqueta(), razon_ignorado
            ),
            plan_hash: None,
            parent_plan_hash: None,
            system_tokens,
            risk: decision.risk,
        };

        match validate_plan(&plan, ctx) {
            Ok(()) => {
                plan.calcular_hash();
                Pie {
                    porque_este_modelo: consejo.porque.clone(),
                    plan,
                    descartados: vec![],
                    degradado: false,
                    violacion: None,
                }
            }
            Err(v) => {
                let mut seguro = Plan::seguro(
                    &req.mode,
                    plan.model.clone(),
                    plan.provider.clone(),
                    tools_del_plan(req, decision),
                );
                seguro.system_tokens = system_tokens;
                // El plan seguro también pasa por el Governor: si no cabe, no cabe.
                if consejo.cabe {
                    seguro.num_ctx = consejo.num_ctx;
                }
                seguro.reason = format!("plan pedido inválido ({}); plan seguro", v.mensaje());
                let degradado = validate_plan(&seguro, ctx).is_err();
                seguro.calcular_hash();
                Pie {
                    plan: seguro,
                    porque_este_modelo: consejo.porque.clone(),
                    descartados: vec![],
                    degradado: !degradado,
                    violacion: Some(v),
                }
            }
        }
    }
}

fn decision_a_contexto(d: &DecisionResult) -> u32 {
    match d.level {
        Level::N0 => 128,
        Level::N1 => 384,
        Level::N2 => 1024,
        Level::N3 => 2048,
    }
}

/// El techo del producto manda; si el nivel lo apaga, se apaga.
fn elegir_thinking(
    req: &BrainRequest,
    d: &DecisionResult,
    _m: &ModelInfo,
    _c: &Consejo,
) -> ThinkingLevel {
    if matches!(d.level, Level::N0 | Level::N1) {
        return ThinkingLevel::Off;
    }
    let techo = req.thinking_ceiling.unwrap_or(ThinkingLevel::Off);
    if techo == ThinkingLevel::Off {
        return ThinkingLevel::Off;
    }
    // N2: off o low. N3: lo que pida el producto, sin pasar de su techo.
    match d.level {
        Level::N2 => techo.min(ThinkingLevel::Low),
        Level::N3 => techo,
        _ => ThinkingLevel::Off,
    }
}

fn decision_target(req: &BrainRequest, d: &DecisionResult, m: &ModelInfo) -> ExecutionTarget {
    if req.policies == crate::api::vocab::ExecutionPolicy::LocalOnly {
        return ExecutionTarget::Local;
    }
    if d.execution_target == ExecutionTarget::Api && !m.local {
        return ExecutionTarget::Api;
    }
    if m.local {
        ExecutionTarget::Local
    } else {
        ExecutionTarget::Api
    }
}

fn tools_del_plan(req: &BrainRequest, d: &DecisionResult) -> Vec<ToolId> {
    if !d.level.permite_tools() {
        return vec![];
    }
    // La intersección con lo que el producto ofrece: la decisión ya candidatesó
    // sobre esa lista, pero el approval puede haber cambiado entre los dos sitios.
    let disponibles: Vec<ToolId> = req.tools.ids();
    d.tools
        .iter()
        .filter(|t| disponibles.iter().any(|x| x == *t))
        .cloned()
        .collect()
}

fn max_tool_calls(nivel: Level, d: &DecisionResult) -> u32 {
    let base = super::plan::default_max_tool_calls(nivel);
    if d.risk == Risk::High && base > 4 {
        4
    } else {
        base
    }
}

fn max_writes(nivel: Level, d: &DecisionResult) -> u32 {
    let base = super::plan::default_max_writes(nivel);
    match d.risk {
        Risk::High => 1,
        Risk::Medium => base.min(2),
        Risk::Low => base,
    }
}
