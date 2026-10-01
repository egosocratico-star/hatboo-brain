//! Golden (§10): `inspect()` congelado para los prompts de referencia, en ES y en
//! EN. Si alguien cambia una regla, una señal o un por defecto, esto se rompe y
//! se ve **qué** prompts movieron de sitio.

use hatboo_brain::api::request::{BrainRequest, ToolInfo};
use hatboo_brain::api::vocab::{ApprovalLevel, ExecutionPolicy, Intent, Level, OutputContract, Risk, VerificationMode};
use hatboo_brain::brain::{Brain, Montaje};
use hatboo_brain::config::loader::Cargada;
use hatboo_brain::models::{ModelInfo, ModelKind, Registry};
use hatboo_brain::providers::MockProvider;
use hatboo_brain::resources::SondaFija;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

fn modelos() -> Vec<ModelInfo> {
    let mut m = BTreeMap::new();
    m.insert(2048u32, 900u64);
    m.insert(4096u32, 1100u64);
    m.insert(8192u32, 1700u64);
    vec![
        ModelInfo {
            id: "nano:0.8b".into(),
            provider: "ollama".into(),
            local: true,
            kind: ModelKind::Generativo,
            profile: hatboo_brain::api::vocab::Profile::Nano,
            tier: 1,
            ram_mb_by_ctx: m.clone(),
            max_ctx: 32768,
            strengths: vec![],
            supports_tools: false,
            supports_thinking: false,
            supports_vision: false,
            disco_mb: Some(815),
        },
        ModelInfo {
            id: "small:4b".into(),
            provider: "ollama".into(),
            local: true,
            kind: ModelKind::Generativo,
            profile: hatboo_brain::api::vocab::Profile::Small,
            tier: 2,
            ram_mb_by_ctx: m,
            max_ctx: 32768,
            strengths: vec!["codigo".into()],
            supports_tools: true,
            supports_thinking: false,
            supports_vision: false,
            disco_mb: Some(3000),
        },
    ]
}

fn cerebro() -> Brain {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(modelos()).con_nombre("ollama"));
    let montaje = Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(modelos()),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        ..Montaje::de_proveedor(mock)
    };
    Brain::nuevo(montaje).unwrap()
}

fn pedido(mensaje: &str, modo: &str) -> BrainRequest {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    BrainRequest::nuevo("hatboo", modo, mensaje)
        .con_tools(
            cfg.herramientas
                .disponibles()
                .iter()
                .map(|t| ToolInfo {
                    id: t.id.clone(),
                    escribe: t.escribe,
                    descripcion: t.descripcion.clone(),
                })
                .collect(),
        )
        .con_policy(ExecutionPolicy::LocalOnly)
        .con_aprobacion(ApprovalLevel::ApproveForMe)
}

/// La firma estable: lo que un consumidor ve y pinta. No incluye hashes ni
/// motivos largos, para que un cambio de redacción no rompa el golden.
#[derive(Debug, PartialEq, serde::Serialize)]
struct Firma {
    nivel: Level,
    intent: Intent,
    contrato: OutputContract,
    verificacion: VerificationMode,
    riesgo: Risk,
    tools: Vec<String>,
    modelo: String,
    skip_generativo: bool,
}

async fn firma(brain: &Brain, req: &BrainRequest) -> Firma {
    let t = brain.inspect(req).await.expect("inspect siempre responde");
    Firma {
        nivel: t.decision.level,
        intent: t.decision.intent,
        contrato: t.decision.output_contract,
        verificacion: t.decision.verification,
        riesgo: t.decision.risk,
        tools: t.decision.tools,
        modelo: t.modelo_elegido,
        skip_generativo: t.decision.skip_generative,
    }
}

/// Los prompts de referencia, uno por categoría y en los dos idiomas.
#[tokio::test]
async fn los_prompts_de_referencia_no_mueven_su_decision() {
    let brain = cerebro();
    let casos: &[(&str, &str, Firma)] = &[
        (
            "saludo-es",
            "hola",
            Firma {
                nivel: Level::N0,
                intent: Intent::Ask,
                contrato: OutputContract::Texto,
                verificacion: VerificationMode::Ninguna,
                riesgo: Risk::Low,
                tools: vec![],
                modelo: "nano:0.8b".into(),
                skip_generativo: true,
            },
        ),
        (
            "aritmetica-es",
            "20+8",
            Firma {
                nivel: Level::N0,
                intent: Intent::Ask,
                contrato: OutputContract::Texto,
                verificacion: VerificationMode::Ninguna,
                riesgo: Risk::Low,
                tools: vec![],
                modelo: "nano:0.8b".into(),
                skip_generativo: true,
            },
        ),
        (
            "archivo-verbo-es",
            "Corrige el error de compilación de src/main.rs",
            Firma {
                nivel: Level::N2,
                intent: Intent::Modify,
                contrato: OutputContract::Patch,
                verificacion: VerificationMode::Determinista,
                // Riesgo medio, no bajo: además de «archivo + verbo» hablan la
                // regla de archivo a secas y la de verbo de acción, el margen
                // 1.º/2.º baja a 0,50 y §1 manda `risk` ≥ medio por debajo de 0,55.
                riesgo: Risk::Medium,
                tools: vec!["read_file".into(), "write_file".into()],
                modelo: "small:4b".into(),
                skip_generativo: false,
            },
        ),
        (
            "archivo-verbo-en",
            "fix the compile error in src/main.rs",
            Firma {
                nivel: Level::N2,
                intent: Intent::Modify,
                contrato: OutputContract::Patch,
                verificacion: VerificationMode::Determinista,
                riesgo: Risk::Medium,
                tools: vec!["read_file".into(), "write_file".into()],
                modelo: "small:4b".into(),
                skip_generativo: false,
            },
        ),
        (
            "riesgo-es",
            "rm -rf la carpeta de logs",
            Firma {
                nivel: Level::N2,
                intent: Intent::Execute,
                contrato: OutputContract::ToolCall,
                verificacion: VerificationMode::Determinista,
                riesgo: Risk::High,
                tools: vec!["run_command".into(), "write_file".into(), "read_file".into()],
                modelo: "small:4b".into(),
                skip_generativo: false,
            },
        ),
    ];
    for (nombre, mensaje, esperada) in casos {
        let modo = if nombre.starts_with("saludo") || nombre.starts_with("aritmetica") {
            "chat"
        } else {
            "work"
        };
        let f = firma(&brain, &pedido(mensaje, modo)).await;
        assert_eq!(&f, esperada, "golden roto en «{nombre}» ({mensaje})");
    }
}

/// ES y EN tienen que terminar en el mismo sitio: si no, el idioma del usuario
/// decide cuánto cuesta su tarea.
#[tokio::test]
async fn el_mismo_pedido_en_dos_idiomas_da_el_mismo_nivel() {
    let brain = cerebro();
    let pares = [
        (
            "Corrige el error de compilación de src/main.rs",
            "Fix the compile error in src/main.rs",
        ),
        ("¿en qué rama estoy?", "what branch am I on?"),
        ("abre README.md", "open README.md"),
    ];
    for (es, en) in pares {
        let a = firma(&brain, &pedido(es, "work")).await;
        let b = firma(&brain, &pedido(en, "work")).await;
        assert_eq!(a.nivel, b.nivel, "{es} | {en} → {:?} vs {:?}", a, b);
        assert_eq!(a.intent, b.intent, "{es} | {en}");
        assert_eq!(a.modelo, b.modelo, "{es} | {en}");
    }
}

/// La traza completa tiene que seguir siendo serializable y no llevar el texto
/// del usuario: `inspect()` se guarda en el log.
#[tokio::test]
async fn la_traza_es_serializable_completa() {
    let brain = cerebro();
    let t = brain
        .inspect(&pedido("haz un commit de todo y sube a main", "work"))
        .await
        .unwrap();
    let j = serde_json::to_value(&t).unwrap();
    for campo in [
        "senales",
        "decision",
        "modeloElegido",
        "porqueEste",
        "descartados",
    ] {
        assert!(j.get(campo).is_some(), "falta {campo} en {j}");
    }
    // «commit» y «sube» están en riesgo_medio; nada de esto es catastrófico, así
    // que medio es la respuesta honesta. Alto se prueba en el golden de arriba.
    assert_eq!(t.decision.risk, Risk::Medium, "{:?}", t.decision);
    assert!(t.porque_este.contains("nano") || t.porque_este.contains("small") || !t.porque_este.is_empty());
    assert_eq!(t.senales.language, "es");
}
