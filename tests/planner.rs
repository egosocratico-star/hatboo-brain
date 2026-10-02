//! Planner (§10): plan válido, inválido, tool prohibida, modelo inexistente,
//! presupuesto roto, `thinking` sobre el techo y `num_ctx` que no cabe.

use hatboo_brain::api::request::BrainRequest;
use hatboo_brain::api::vocab::{
    ApprovalLevel, ExecutionPolicy, ExecutionTarget, Intent, Level, OutputContract, Profile,
    ThinkingLevel, VerificationMode,
};
use hatboo_brain::models::{ModelInfo, ModelKind, Registry};
use hatboo_brain::planner::plan::{default_context_budget, default_max_output, Plan};
use hatboo_brain::planner::validation::{validate_plan, PlanContext, PlanViolation};
use std::collections::BTreeMap;

fn modelo(id: &str, razona: bool) -> ModelInfo {
    let mut ram = BTreeMap::new();
    ram.insert(2048u32, 900u64);
    ram.insert(4096u32, 1100u64);
    ram.insert(8192u32, 1700u64);
    ModelInfo {
        id: id.into(),
        provider: "ollama".into(),
        local: true,
        kind: ModelKind::Generativo,
        profile: Profile::Nano,
        tier: 1,
        ram_mb_by_ctx: ram,
        max_ctx: 32768,
        strengths: vec![],
        supports_tools: true,
        supports_thinking: razona,
        supports_vision: false,
        disco_mb: Some(800),
    }
}

fn contexto<'a>(tools: &'a [String], modelos: &'a [ModelInfo]) -> PlanContext<'a> {
    PlanContext {
        approval: ApprovalLevel::ApproveForMe,
        policy: ExecutionPolicy::LocalPreferred,
        tools_del_producto: tools,
        modelos,
        ceiling: Some(ThinkingLevel::Low),
        ..Default::default()
    }
}

fn plan_de_prueba() -> Plan {
    let mut p = Plan::firmar(
        Level::N2,
        Intent::Modify,
        "gemma3:1b".into(),
        "ollama".into(),
        ExecutionTarget::Local,
        4096,
        ThinkingLevel::Off,
        vec!["read_file".into(), "write_file".into()],
        OutputContract::Patch,
        VerificationMode::Determinista,
        "prueba".into(),
    );
    p.system_tokens = 200;
    p
}

const DOS_TOOLS: [&str; 2] = ["read_file", "write_file"];

fn herramientas() -> Vec<String> {
    DOS_TOOLS.iter().map(|s| s.to_string()).collect()
}

#[test]
fn un_plan_correcto_pasa_las_siete() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let p = plan_de_prueba();
    validate_plan(&p, &contexto(&herramientas(), &modelos)).expect("plan válido");
    assert!(p.cabe_en_ctx(), "{:?}", p.presupuesto_tokens());
    assert_eq!(p.schema_version, hatboo_brain::planner::SCHEMA_VERSION);
}

#[test]
fn una_tool_que_el_producto_no_ofrece_se_rechaza_en_codigo() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let solo_lectura = vec!["read_file".to_string()];
    let mut p = plan_de_prueba();
    p.tools = vec!["read_file".into(), "run_command".into()];
    let e = validate_plan(&p, &contexto(&solo_lectura, &modelos)).unwrap_err();
    assert!(
        matches!(&e, PlanViolation::ToolNotAllowed(t) if t == "run_command"),
        "{e:?}"
    );
    assert!(e.mensaje().contains("run_command"));
}

#[test]
fn n1_no_puede_llevar_tools_aunque_existan() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mut p = plan_de_prueba();
    p.level = Level::N1;
    p.num_ctx = 2048;
    p.verification = Level::N1.verificacion_minima();
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(e, PlanViolation::ToolNotAllowed(_)), "{e:?}");
}

#[test]
fn un_modelo_no_descrito_no_se_firma() {
    let modelos = vec![modelo("otra:1b", false)];
    let p = plan_de_prueba();
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(&e, PlanViolation::UnknownModel(id) if id == "gemma3:1b"), "{e:?}");
}

#[test]
fn el_presupuesto_roto_se_detecta() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mut p = plan_de_prueba();
    p.context_budget_tokens = 4000;
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(e, PlanViolation::BudgetExceedsCtx), "{e:?}");
    assert!(!p.cabe_en_ctx());
}

#[test]
fn thinking_por_encima_del_techo_del_producto_no_pasa() {
    let modelos = vec![modelo("gemma3:1b", true)];
    let mut p = plan_de_prueba();
    // El techo del producto es `low` en este contexto, así que `medium` es una
    // violación de política: se comprueba antes que la resta de presupuesto, que
    // con `medium` también daría pero diría el sitio equivocado.
    p.thinking = ThinkingLevel::Medium;
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(e, PlanViolation::ThinkingAboveCeiling), "{e:?}");
}

#[test]
fn un_n0_con_thinking_es_invalido_otros_ocho_no_mandan() {
    let modelos = vec![modelo("gemma3:1b", true)];
    let mut p = plan_de_prueba();
    p.level = Level::N0;
    p.num_ctx = 2048;
    p.tools = vec![];
    p.max_output_tokens = 192;
    p.context_budget_tokens = 128;
    p.verification = Level::N0.verificacion_minima();
    p.thinking = ThinkingLevel::Low;
    let ctx = PlanContext {
        ceiling: Some(ThinkingLevel::High),
        ..contexto(&[], &modelos)
    };
    let e = validate_plan(&p, &ctx).unwrap_err();
    assert!(matches!(e, PlanViolation::ThinkingAboveCeiling), "{e:?}");
}

#[test]
fn un_modelo_que_no_razona_no_puede_prometer_razonamiento() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mut p = plan_de_prueba();
    p.thinking = ThinkingLevel::Low; // dentro del techo, pero el modelo no puede
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(e, PlanViolation::ThinkingAboveCeiling), "{e:?}");
}

#[test]
fn las_escrituras_no_pueden_superar_las_llamadas() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mut p = plan_de_prueba();
    p.max_tool_calls = 2;
    p.max_write_actions = 5;
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(e, PlanViolation::WritesExceedCalls), "{e:?}");
}

#[test]
fn un_num_ctx_fuera_del_conjunto_no_es_plan() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mut p = plan_de_prueba();
    p.num_ctx = 6000;
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(e, PlanViolation::NumCtxFueraDeConjunto(6000)), "{e:?}");
}

#[test]
fn local_only_nunca_firma_un_destino_api() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let tools = herramientas();
    let mut p = plan_de_prueba();
    p.execution_target = ExecutionTarget::Api;
    let ctx = PlanContext {
        policy: ExecutionPolicy::LocalOnly,
        ..contexto(&tools, &modelos)
    };
    let e = validate_plan(&p, &ctx).unwrap_err();
    assert!(matches!(e, PlanViolation::LocalOnlyConApi), "{e:?}");
}

#[test]
fn bajar_la_verificacion_por_debajo_del_nivel_es_invalido() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mut p = plan_de_prueba();
    p.verification = VerificationMode::Ninguna;
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(matches!(e, PlanViolation::VerificacionBaja), "{e:?}");
}

#[test]
fn el_plan_seguro_pasa_las_mismas_siete() {
    // La salida de emergencia de §5 tiene que ser firmable con el producto más
    // cerrado que hay: sin tools y con approval de preguntar siempre.
    let modelos = vec![modelo("gemma3:1b", false)];
    let solo_lectura = vec!["read_file".to_string()];
    let p = Plan::seguro("work", "gemma3:1b".into(), "ollama".into(), solo_lectura.clone());
    let ctx = PlanContext {
        approval: ApprovalLevel::AskAlways,
        ceiling: None,
        ..contexto(&solo_lectura, &modelos)
    };
    validate_plan(&p, &ctx).expect("el plan seguro es válido");
    assert!(p.level <= Level::N2, "{:?}", p.level);
    assert!(!p.tools.iter().any(|t| t == "write_file"), "{:?}", p.tools);
    assert_eq!(p.thinking, ThinkingLevel::Off);
    assert!(p.cabe_en_ctx());
}

#[test]
fn la_firma_del_plan_es_estable_y_el_motivo_cuenta() {
    let mut a = plan_de_prueba();
    let b = plan_de_prueba();
    a.calcular_hash();
    let mut c = a.clone();
    assert_eq!(a.plan_hash, c.plan_hash, "recalcular no cambia nada");
    c.reason = "otro motivo".into();
    c.calcular_hash();
    assert_ne!(a.plan_hash, c.plan_hash, "el motivo es parte del plan");
    assert_eq!(b.plan_hash, None, "sin firmar no hay hash");
}

#[test]
fn la_invariante_de_presupuesto_se_cumple_a_todos_los_niveles() {
    let modelos = vec![modelo("gemma3:1b", true)];
    let herramientas = herramientas();
    for nivel in [Level::N0, Level::N1, Level::N2, Level::N3] {
        let mut p = plan_de_prueba();
        p.level = nivel;
        p.num_ctx = nivel.num_ctx_minimo();
        p.max_output_tokens = default_max_output(nivel);
        p.context_budget_tokens = default_context_budget(nivel);
        p.verification = nivel.verificacion_minima();
        p.tools = if nivel.permite_tools() {
            herramientas.clone()
        } else {
            vec![]
        };
        assert!(p.cabe_en_ctx(), "{nivel:?}: {:?}", p.presupuesto_tokens());
        let ctx = PlanContext {
            ceiling: Some(ThinkingLevel::High),
            ..contexto(&herramientas, &modelos)
        };
        validate_plan(&p, &ctx).unwrap_or_else(|e| panic!("{nivel:?}: {e}"));
    }
}

#[tokio::test]
async fn un_pedido_que_pide_thinking_sin_soporte_baja_a_off_y_llega_ok() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mock = std::sync::Arc::new(
        hatboo_brain::providers::MockProvider::nuevo(modelos.clone()).con_nombre("ollama"),
    );
    mock.responde_texto("listo");
    // Reglas de un solo tiro: este test mira al Armador, no al vocabulario. Con
    // las versionadas, un «instala» dentro de «instalados» sube o baja el nivel
    // por azar y el test dejaría de decir lo que dice.
    let reglas = hatboo_brain::decision::rules::Reglas::desde_json(
        r#"{"version":1,"reglas":[{"id":"largo-a-n2",
             "cuando":[{"senal":"message_length","op":">","valor":200}],
             "entonces":{"intent":"explain","level":"N2","contrato":"markdown"}}]}"#,
    )
    .unwrap();
    let montaje = hatboo_brain::brain::Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(modelos.clone()),
        reglas,
        sonda: std::sync::Arc::new(hatboo_brain::resources::SondaFija::default()),
        ..hatboo_brain::brain::Montaje::de_proveedor(mock.clone())
    };
    let brain = hatboo_brain::brain::Brain::nuevo(montaje).unwrap();
    // A N2 es donde el techo del producto pediría `thinking`; el modelo no lo
    // soporta, así que §1 dice que se ignora **y se registra**, no que se calle.
    let mut req = BrainRequest::nuevo(
        "hatboo",
        "work",
        "explícame con detalle, ejemplos y contraejemplos cómo administraría este crate la memoria de una máquina sin GPU cuando hay varios modelos instalados y el usuario elige uno que no cabe, qué haría el Governor y por qué",
    );
    req.thinking_ceiling = Some(ThinkingLevel::High);
    let r = brain.run(&req).await.unwrap();
    assert_eq!(r.plan.level, Level::N2, "{:?}", r.plan);
    assert_eq!(r.plan.thinking, ThinkingLevel::Off);
    assert!(r.plan.plan_hash.is_some());
    assert!(r.plan.reason.contains("no razona"), "{}", r.plan.reason);
}

/// El registry dice qué proveedor sirve cada modelo; si el montaje empareja un
/// proveedor con otro nombre, el Plan no es firmable. Es el error de config más
/// fácil de cometer al integrar un producto nuevo.
#[test]
fn un_registry_que_no_cuadra_con_el_proveedor_no_se_firma() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mut p = plan_de_prueba();
    p.provider = "openai".into();
    let e = validate_plan(&p, &contexto(&herramientas(), &modelos)).unwrap_err();
    assert!(
        matches!(&e, PlanViolation::ProviderNotAllowed(pr) if pr == "openai"),
        "{e:?}"
    );
}

/// El plan promete una ventana; la petición tiene que ser **esa** ventana. Antes el
/// Brain mandaba `req.history` entero al proveedor y además lo metía recortado dentro
/// del texto del turno: dos copias del mismo historial, y la que no entraba en
/// ninguna cuenta era la que desbordaba el `num_ctx` firmado.
#[tokio::test]
async fn lo_que_el_plan_admitio_es_lo_que_llega_al_proveedor() {
    let modelos = vec![modelo("gemma3:1b", false)];
    let mock = std::sync::Arc::new(
        hatboo_brain::providers::MockProvider::nuevo(modelos.clone()).con_nombre("ollama"),
    );
    for _ in 0..3 {
        mock.responde_texto("vale");
    }
    let reglas = hatboo_brain::decision::rules::Reglas::desde_json(
        r#"{"version":1,"reglas":[{"id":"corto-a-n1",
             "cuando":[{"senal":"message_length","op":"<=","valor":60}],
             "entonces":{"intent":"ask","level":"N1","contrato":"texto"}}]}"#,
    )
    .unwrap();
    let montaje = hatboo_brain::brain::Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(modelos.clone()),
        reglas,
        sonda: std::sync::Arc::new(hatboo_brain::resources::SondaFija::default()),
        ..hatboo_brain::brain::Montaje::de_proveedor(mock.clone())
    };
    let brain = hatboo_brain::brain::Brain::nuevo(montaje).unwrap();

    let mut req = BrainRequest::nuevo("hatboo", "chat", "¿y ahora qué?");
    req.history = (0..12)
        .map(|i| {
            hatboo_brain::api::request::Message::usuario(format!("turno {i}: {}", "k".repeat(1200)))
        })
        .collect();

    let plan = brain.plan(&req).await.unwrap();
    assert!(
        plan.mensaje_tokens > 0,
        "el turno del usuario tiene que entrar en la cuenta: {plan:?}"
    );
    assert!(plan.historial_turnos > 0, "algo de historial cabe: {plan:?}");
    assert!(
        usize::from(plan.historial_turnos) < req.history.len(),
        "pero no los doce: {:?}",
        plan.historial_turnos
    );
    assert!(
        plan.presupuesto_tokens() <= plan.num_ctx,
        "{:?} > {}",
        plan.presupuesto_tokens(),
        plan.num_ctx
    );
    assert!(
        plan.reason.contains("historial recortado"),
        "lo que se quedó fuera se dice: {}",
        plan.reason
    );

    brain.run(&req).await.ok();
    let peticion = mock.ultima_peticion().expect("hubo una petición");
    assert_eq!(
        peticion.history.len(),
        usize::from(plan.historial_turnos),
        "al proveedor llegan justo los turnos que el plan admitió"
    );
    let esperados = req.history[req.history.len() - usize::from(plan.historial_turnos)..].to_vec();
    assert_eq!(
        peticion.history, esperados,
        "llegan los más recientes, en el mismo orden"
    );
    assert!(
        !peticion.prompt.contains("kkkk"),
        "el historial no se cuela también en el texto del turno: {}",
        peticion.prompt
    );
}
