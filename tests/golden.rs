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
            structured_output: false,
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
            structured_output: false,
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
                // `false` desde el arreglo del saludo vacío: el Fast Path decide
                // la forma (N0, sin tools, texto corto) pero no trae la respuesta,
                // y quien no trae qué decir no puede saltarse el modelo. La
                // aritmética de abajo sí lo salta, porque sí trae el número.
                skip_generativo: false,
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

/// §II.10: un flag apaga una pieza que existe. `logprobs` y `backend_decision` son
/// las Fases 6 y 7, que no están construidas: un config que las enciende se
/// rechaza en vez de arrancar diciendo «los flags están puestos» sin que nada los
/// lea.
#[test]
fn encender_una_fase_que_no_esta_construida_no_arranca_en_silencio() {
    use hatboo_brain::config::schema::BrainConfig;
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(modelos()).con_nombre("ollama"));
    assert!(
        !(BrainConfig::default().flags.logprobs || BrainConfig::default().flags.backend_decision),
        "las Fases 6 y 7 nacen apagadas"
    );
    // El nombre que se comprueba es el que el producto escribe en el JSON
    // (`camelCase`), no el del campo en Rust.
    let mut config = BrainConfig::default();
    config.flags.backend_decision = true;
    let s = match Brain::nuevo(Montaje {
        config,
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(modelos()),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        ..Montaje::de_proveedor(mock.clone())
    }) {
        Err(e) => e.mensaje(),
        Ok(_) => panic!("`backendDecision` es la Fase 7: aquí no hay nada que encender"),
    };
    assert!(s.contains("backendDecision"), "el campo no se nombró: {s}");
    assert!(s.contains("Fase 7"), "el error tiene que decir de qué fase es: {s}");
    assert!(s.contains("no está construida"), "{s}");

    // La Fase 6 (`logprobs`) se construyó el 04-10: se pide al proveedor, se cose
    // y se reporta. Encenderla ya no puede negarse, y si alguien le vuelve a
    // quitar la pieza, esto rompe.
    let mut con_logprobs = BrainConfig::default();
    con_logprobs.flags.logprobs = true;
    assert!(
        Brain::nuevo(Montaje {
            config: con_logprobs,
            proveedores: vec![mock.clone()],
            registry: Registry::nuevo(modelos()),
            reglas: cfg.reglas.clone(),
            herramientas: cfg.herramientas.clone(),
            sonda: Arc::new(SondaFija::default()),
            ..Montaje::de_proveedor(mock.clone())
        })
        .is_ok(),
        "la Fase 6 está construida: encender `logprobs` tiene que montar"
    );
}

/// §1 del Plan: una decisión cerrada solo se mueve con una entrada nueva en
/// `docs/decision-log.md`. Esta prueba es lo que hace que esa regla sea de
/// verdad: si alguien sube el `SCHEMA_VERSION` del Plan sin escribir la
/// entrada, o escribe una entrada con otro número, esto se rompe.
#[test]
fn el_registro_de_decisiones_dice_el_schema_que_tiene_el_codigo() {
    let ruta = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/decision-log.md");
    let log = std::fs::read_to_string(ruta).expect("el registro viaja en el repo");
    let ultimo = log
        .lines()
        .filter(|l| l.starts_with('|'))
        .filter_map(|l| {
            let i = l.find("schema ")? + "schema ".len();
            l[i..].chars().next()?.to_digit(10)
        })
        .next_back()
        .expect("ninguna entrada del registro habla del schema del Plan");
    assert_eq!(
        ultimo,
        hatboo_brain::planner::plan::SCHEMA_VERSION,
        "el código está en schema {} y el registro dice otra cosa",
        hatboo_brain::planner::plan::SCHEMA_VERSION
    );
}
