//! `validate_plan()` — las seis comprobaciones de §1 del plan. Ninguna es
//! opcional: cada una cierra una puerta por la que un modelo (o un prompt
//! inyectado) puede salirse del presupuesto o de los permisos.

use super::plan::Plan;
use crate::api::vocab::{
    ApprovalLevel, ExecutionPolicy, ExecutionTarget, Level, ModelId, ProviderId, ThinkingLevel,
    ToolId, VerificationMode,
};
use crate::models::ModelInfo;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanViolation {
    /// Tool fuera del Plan, o dentro del Plan pero fuera de lo que el approval o
    /// el producto permiten.
    ToolNotAllowed(ToolId),
    UnknownModel(ModelId),
    ProviderNotAllowed(ProviderId),
    BudgetExceedsCtx,
    ThinkingAboveCeiling,
    WritesExceedCalls,
    /// El nivel exige más verificación de la que pide el Plan.
    VerificacionBaja,
    /// `num_ctx` fuera del conjunto permitido (config lo puede ampliar por modelo).
    NumCtxFueraDeConjunto(u32),
    /// El plan apunta a una API pero la policy no la deja salir del equipo.
    LocalOnlyConApi,
}

impl PlanViolation {
    pub fn mensaje(&self) -> String {
        match self {
            PlanViolation::ToolNotAllowed(t) => format!("la tool «{t}» no está permitida"),
            PlanViolation::UnknownModel(m) => format!("el modelo «{m}» no está en el registry"),
            PlanViolation::ProviderNotAllowed(p) => {
                format!("el proveedor «{p}» no lo permite la política")
            }
            PlanViolation::BudgetExceedsCtx => {
                "system + turno + historial + contexto + salida no caben en num_ctx".into()
            }
            PlanViolation::ThinkingAboveCeiling => {
                "el techo de razonamiento del producto se superó".into()
            }
            PlanViolation::WritesExceedCalls => {
                "pide más escrituras que llamadas a tools".into()
            }
            PlanViolation::VerificacionBaja => {
                "el nivel exige una verificación mayor que la pedida".into()
            }
            PlanViolation::NumCtxFueraDeConjunto(n) => {
                format!("num_ctx {n} no está en el conjunto permitido")
            }
            PlanViolation::LocalOnlyConApi => {
                "local_only no puede llamar a una API".into()
            }
        }
    }
}

impl std::fmt::Display for PlanViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.mensaje())
    }
}

/// Lo que hace falta saber para validar: el approval del producto, la policy, las
/// tools que ese modo ofrece, el registry y el techo de `thinking`.
pub struct PlanContext<'a> {
    pub approval: ApprovalLevel,
    pub policy: ExecutionPolicy,
    /// Tools que el **producto** ofrece en este modo. No es lo mismo que las del
    /// Plan: el Plan firma una parte de estas.
    pub tools_del_producto: &'a [ToolId],
    pub modelos: &'a [ModelInfo],
    pub ceiling: Option<ThinkingLevel>,
    /// Conjunto de `num_ctx` válidos. Sale de config; el default es el del Canon.
    pub ctx_permitidos: &'a [u32],
}

impl<'a> Default for PlanContext<'a> {
    fn default() -> Self {
        static CTX: [u32; 3] = [2048, 4096, 8192];
        PlanContext {
            approval: ApprovalLevel::AskAlways,
            policy: ExecutionPolicy::LocalPreferred,
            tools_del_producto: &[],
            modelos: &[],
            ceiling: None,
            ctx_permitidos: &CTX,
        }
    }
}

/// La puerta de la policy (§15.2 del plan): qué destino de ejecución permite.
pub fn target_permitido(policy: ExecutionPolicy, target: ExecutionTarget) -> bool {
    match policy {
        ExecutionPolicy::LocalOnly => matches!(target, ExecutionTarget::Local),
        ExecutionPolicy::LocalPreferred | ExecutionPolicy::CloudAllowed => true,
        ExecutionPolicy::Balanced => true,
        ExecutionPolicy::CloudOnly => matches!(target, ExecutionTarget::Api | ExecutionTarget::Fallback),
    }
}

pub fn validate_plan(plan: &Plan, ctx: &PlanContext) -> Result<(), PlanViolation> {
    // 1 · tools ⊆ approval ∩ tools del producto. El approval no lo comprueba este
    // crate: lo aplica el producto al ejecutar. Aquí se corta lo que el producto
    // no ofrece y lo que el nivel no permite tocar.
    for t in &plan.tools {
        if !ctx.tools_del_producto.iter().any(|x| x == t) {
            return Err(PlanViolation::ToolNotAllowed(t.clone()));
        }
    }
    if !plan.level.permite_tools() && !plan.tools.is_empty() {
        // N0/N1 no ven tools. La lectura directa de un `skip_generative` no vive
        // aquí: la ejecuta el producto, no es una tool que llame el modelo.
        return Err(PlanViolation::ToolNotAllowed(plan.tools[0].clone()));
    }

    // 2 · el modelo existe y el proveedor lo deja la policy.
    let info = ctx
        .modelos
        .iter()
        .find(|m| m.id == plan.model)
        .ok_or_else(|| PlanViolation::UnknownModel(plan.model.clone()))?;
    if info.provider != plan.provider {
        return Err(PlanViolation::ProviderNotAllowed(plan.provider.clone()));
    }
    if !target_permitido(ctx.policy, plan.execution_target) {
        if matches!(ctx.policy, ExecutionPolicy::LocalOnly) {
            return Err(PlanViolation::LocalOnlyConApi);
        }
        return Err(PlanViolation::ProviderNotAllowed(plan.provider.clone()));
    }

    // 3 · presupuesto dentro de num_ctx (el razonamiento cuenta como salida).
    if !plan.cabe_en_ctx() {
        return Err(PlanViolation::BudgetExceedsCtx);
    }

    // 4 · escrituras ≤ llamadas.
    if plan.max_write_actions > plan.max_tool_calls {
        return Err(PlanViolation::WritesExceedCalls);
    }

    // 5 · thinking ≤ techo del producto, y off en N0/N1.
    match ctx.ceiling {
        None => {
            if plan.thinking != ThinkingLevel::Off {
                return Err(PlanViolation::ThinkingAboveCeiling);
            }
        }
        Some(techo) => {
            if plan.thinking > techo {
                return Err(PlanViolation::ThinkingAboveCeiling);
            }
        }
    }
    if matches!(plan.level, Level::N0 | Level::N1) && plan.thinking != ThinkingLevel::Off {
        return Err(PlanViolation::ThinkingAboveCeiling);
    }
    // Un modelo que no razona no puede prometer razonamiento: se ignora y se
    // registra, no se calla.
    if plan.thinking != ThinkingLevel::Off && !info.supports_thinking {
        return Err(PlanViolation::ThinkingAboveCeiling);
    }

    // 6 · verificación no menor que el mínimo del nivel.
    if plan.verification < plan.level.verificacion_minima() {
        return Err(PlanViolation::VerificacionBaja);
    }

    // 7 · el conjunto de num_ctx.
    if !ctx.ctx_permitidos.contains(&plan.num_ctx) {
        return Err(PlanViolation::NumCtxFueraDeConjunto(plan.num_ctx));
    }

    Ok(())
}

/// Qué verificación corresponde a un contrato, para no dejar un `patch` sin gate.
pub fn verificacion_sugerida(contrato: &crate::api::vocab::OutputContract, level: Level) -> VerificationMode {
    let minimo = level.verificacion_minima();
    match contrato {
        crate::api::vocab::OutputContract::Json
        | crate::api::vocab::OutputContract::ToolCall
        | crate::api::vocab::OutputContract::Patch => {
            if minimo == VerificationMode::Ninguna {
                VerificationMode::Formato
            } else {
                minimo
            }
        }
        _ => minimo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{Intent, OutputContract, Profile};
    use crate::models::ModelInfo;

    fn modelo() -> ModelInfo {
        ModelInfo {
            id: "gemma3:1b".into(),
            provider: "ollama".into(),
            local: true,
            kind: crate::models::ModelKind::Generativo,
            profile: Profile::Nano,
            tier: 1,
            ram_mb_by_ctx: Default::default(),
            max_ctx: 32768,
            strengths: vec![],
            supports_tools: false,
            supports_thinking: false,
            supports_vision: false,
            disco_mb: None,
        }
    }

    fn plan_ok() -> Plan {
        Plan::firmar(
            Level::N1,
            Intent::Ask,
            "gemma3:1b".into(),
            "ollama".into(),
            ExecutionTarget::Local,
            2048,
            ThinkingLevel::Off,
            vec![],
            OutputContract::Texto,
            VerificationMode::Formato,
            "motivo".into(),
        )
    }

    #[test]
    fn un_plan_sano_pasa() {
        let ms = [modelo()];
        let ctx = PlanContext {
            modelos: &ms,
            ..Default::default()
        };
        assert_eq!(validate_plan(&plan_ok(), &ctx), Ok(()));
    }

    #[test]
    fn rechaza_lo_que_no_debe_pasar() {
        let ms = [modelo()];
        let base = PlanContext {
            modelos: &ms,
            ..Default::default()
        };

        // Modelo que no está en el registry.
        let mut p = plan_ok();
        p.model = "no-existe:1b".into();
        assert_eq!(
            validate_plan(&p, &base),
            Err(PlanViolation::UnknownModel("no-existe:1b".into()))
        );

        // Tool que el producto no ofrece.
        let mut p = plan_ok();
        p.level = Level::N2;
        p.num_ctx = 4096;
        p.verification = VerificationMode::Determinista;
        p.max_tool_calls = 8;
        p.max_write_actions = 3;
        p.tools = vec!["borra_todo".into()];
        assert_eq!(
            validate_plan(&p, &base),
            Err(PlanViolation::ToolNotAllowed("borra_todo".into()))
        );

        // ...y la ofrece, pero el nivel no admite tools.
        let ctx_con_tools = PlanContext {
            modelos: &ms,
            tools_del_producto: &["read_file".into()],
            ..Default::default()
        };
        let mut p = plan_ok();
        p.tools = vec!["read_file".into()];
        assert_eq!(
            validate_plan(&p, &ctx_con_tools),
            Err(PlanViolation::ToolNotAllowed("read_file".into()))
        );

        // local_only contra una API.
        let mut p = plan_ok();
        p.execution_target = ExecutionTarget::Api;
        p.provider = "openai".into();
        let ctx_local = PlanContext {
            modelos: &ms,
            policy: ExecutionPolicy::LocalOnly,
            ..Default::default()
        };
        assert_eq!(
            validate_plan(&p, &ctx_local),
            Err(PlanViolation::ProviderNotAllowed("openai".into()))
        );

        // Presupuesto roto.
        let mut p = plan_ok();
        p.context_budget_tokens = 4000;
        assert_eq!(validate_plan(&p, &base), Err(PlanViolation::BudgetExceedsCtx));

        // Escrituras por encima de llamadas.
        let mut p = plan_ok();
        p.max_tool_calls = 1;
        p.max_write_actions = 2;
        assert_eq!(validate_plan(&p, &base), Err(PlanViolation::WritesExceedCalls));

        // Thinking sin techo del producto.
        let mut p = plan_ok();
        p.thinking = ThinkingLevel::Low;
        assert_eq!(
            validate_plan(&p, &base),
            Err(PlanViolation::ThinkingAboveCeiling)
        );

        // Verificación por debajo del mínimo del nivel.
        let mut p = plan_ok();
        p.level = Level::N2;
        p.num_ctx = 4096;
        p.verification = VerificationMode::Formato;
        p.max_tool_calls = 8;
        assert_eq!(
            validate_plan(&p, &base),
            Err(PlanViolation::VerificacionBaja)
        );

        // num_ctx fuera del conjunto.
        let mut p = plan_ok();
        p.num_ctx = 3072;
        assert_eq!(
            validate_plan(&p, &base),
            Err(PlanViolation::NumCtxFueraDeConjunto(3072))
        );
    }

    #[test]
    fn modelo_que_no_razona_no_promete_razonamiento() {
        let ms = [modelo()];
        let ctx = PlanContext {
            modelos: &ms,
            ceiling: Some(ThinkingLevel::High),
            ..Default::default()
        };
        let mut p = plan_ok();
        p.level = Level::N3;
        p.num_ctx = 8192;
        p.verification = VerificationMode::Determinista;
        p.max_output_tokens = 2048;
        p.context_budget_tokens = 2048;
        p.thinking = ThinkingLevel::Low;
        assert_eq!(
            validate_plan(&p, &ctx),
            Err(PlanViolation::ThinkingAboveCeiling)
        );
    }
}
