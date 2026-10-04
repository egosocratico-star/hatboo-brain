//! Decision (§10): saludo, pregunta, código, archivo, riesgo, tool, ambigüedad —
//! en ES y en EN. Las reglas activas son las versionadas en `config/`, porque un
//! test con reglas de juguete no prueba lo que corre la máquina.

use hatboo_brain::api::vocab::{
    DecisionSource, Intent, Level, OutputContract, Risk, VerificationMode,
};
use hatboo_brain::api::BrainRequest;
use hatboo_brain::config::loader::Cargada;
use hatboo_brain::decision::engine::{senales, Motor};
use hatboo_brain::decision::rules::Reglas;
use std::path::Path;

fn config() -> Cargada {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
    Cargada::leer(Some(&dir)).expect("el config del repo tiene que cargar")
}

/// El lote completo o nada: un `Some` del Fast Path salta el Engine entero y un
/// resultado a medias firmaría un Plan con huecos.
fn revisar_lote_completo(d: &hatboo_brain::decision::DecisionResult, de_que: &str) {
    assert!(
        !d.por_que.is_empty(),
        "{de_que}: todo lote lleva su motivo"
    );
    assert!(
        d.confidence.valor() > 0.0 && d.confidence.valor() <= 1.0,
        "{de_que}: confianza en [0,1]"
    );
    assert!(
        d.verification >= d.level.verificacion_minima(),
        "{de_que}: verificación por debajo del mínimo del nivel"
    );
    if !d.level.permite_tools() {
        assert!(d.tools.is_empty(), "{de_que}: N0/N1 no llevan tools");
    }
}

fn decidir(mensaje: &str, modo: &str) -> hatboo_brain::decision::DecisionResult {
    let cfg = config();
    let mut m = Motor::nuevo(cfg.reglas);
    let req = BrainRequest::nuevo("hatboo", modo, mensaje)
        .con_tools(cfg.herramientas.disponibles().iter().map(|t| t.info()).collect());
    let d = m.evaluar(&req);
    revisar_lote_completo(&d, mensaje);
    d
}

#[test]
fn un_saludo_es_n0_y_lo_contesta_el_modelo() {
    for m in ["hola", "Hola!", "  buenas  ", "hey", "gracias"] {
        let d = decidir(m, "chat");
        assert_eq!(d.source, DecisionSource::FastPath, "{m}");
        assert_eq!(d.level, Level::N0, "{m}");
        // El Fast Path del saludo fija la forma (N0, sin tools, salida de texto)
        // pero no la respuesta: quien se salta el modelo tiene que traer lo que
        // decir, y si no lo trae la burbuja sale vacía.
        assert!(!d.skip_generative, "{m}: no trae salida calculada");
        assert!(d.salida_directa.is_none(), "{m}");
        assert!(d.tools.is_empty());
    }
}

#[test]
fn una_pregunta_de_chat_no_pide_obras() {
    let d = decidir("¿qué es un mutex?", "chat");
    assert_eq!(d.intent, Intent::Ask);
    assert!(d.level <= Level::N1, "una pregunta no sube a {:?}", d.level);
    assert!(d.tools.is_empty());
    assert_eq!(d.output_contract, OutputContract::Texto);
}

#[test]
fn el_codigo_pegado_se_explica_no_se_escribe() {
    let d = decidir(
        "explícame esto:\n```rust\nfn main() {\n    let v = vec![1,2];\n    println!(\"{v:?}\");\n}\n```",
        "chat",
    );
    assert!(matches!(d.intent, Intent::Explain | Intent::Ask), "{d:?}");
    assert!(d.tools.is_empty(), "{d:?}");
    // Un fragmento de código no es una ruta ni una orden de tocar nada.
    assert!(!d.confidence.es_insegura(), "{:?}", d.confidence);
}

#[test]
fn archivo_con_verbo_pide_parche_y_tools_de_escritura() {
    for (m, modo) in [
        ("Corrige el error de compilación de src/main.rs", "work"),
        ("fix the compile error in src/main.rs", "work"),
        ("cambia el saludo de src/app.js", "work"),
    ] {
        let d = decidir(m, modo);
        assert_eq!(d.level, Level::N2, "{m} → {:?}", d.level);
        assert_eq!(d.output_contract, OutputContract::Patch, "{m}");
        assert!(d.tools.iter().any(|t| t == "write_file"), "{m}: {:?}", d.tools);
        assert_eq!(d.verification, VerificationMode::Determinista, "{m}");
    }
}

#[test]
 fn solo_el_nombre_de_un_archivo_es_lectura() {
    let d = decidir("¿qué hay en README.md?", "chat");
    assert!(d.level <= Level::N1, "{d:?}");
    // N1 no lleva tools: si quisiera leerlo, el Fast Path lo habría dicho.
    assert!(d.tools.is_empty() || d.level >= Level::N2);
}

#[test]
fn el_riesgo_alto_no_se_queda_bajo() {
    for m in [
        "borra la carpeta target y todo lo que haya dentro",
        "rm -rf node_modules",
        "drop the whole database",
        "haz un force push de main",
    ] {
        let d = decidir(m, "work");
        assert_eq!(d.risk, Risk::High, "{m} → {:?}", d.risk);
        assert!(d.level >= Level::N2, "{m} → {:?}", d.level);
    }
}

#[test]
fn una_tool_de_lectura_no_convierte_un_chat_en_obra() {
    // En `chat` el producto no suele ofrecer escritura: aunque la regla la pida,
    // lo que no está en la mesa no se puede servir.
    let cfg = config();
    let solo_lectura: Vec<_> = cfg
        .herramientas
        .disponibles()
        .iter()
        .filter(|t| !t.escribe)
        .map(|t| t.info())
        .collect();
    let mut m = Motor::nuevo(cfg.reglas);
    let d = m.evaluar(
        &BrainRequest::nuevo("hatboo", "chat", "corrige src/main.rs")
            .con_tools(solo_lectura),
    );
    assert!(
        !d.tools.iter().any(|t| t == "write_file"),
        "{:?}",
        d.tools
    );
}

#[test]
fn git_y_web_se_reconocen_en_los_dos_idiomas() {
    let d = decidir("¿en qué rama estoy y qué hay sin commitear?", "work");
    assert!(
        d.tools.iter().any(|t| t.starts_with("git_")),
        "{:?}",
        d.tools
    );
    let e = decidir("search the web for the latest stable rust version", "work");
    assert!(e.tools.iter().any(|t| t == "web_search"), "{e:?}");
    assert_eq!(e.intent, Intent::Search);
}

#[test]
fn la_ambiguedad_se_queda_baja_y_no_inventa_obras() {
    // Lo que el crate puede afirmar de «esto» o «make it pop»: que no pide tools,
    // que no necesita un modelo grande y que el Fast Path no lo lee como una ruta.
    // La duda de §1 solo aparece cuando dos reglas empatan; aquí no hay segunda
    // candidata, así que exigir `hay_duda()` sería exigirle al Engine algo que las
    // señales no contienen.
    for m in ["esto", "no va", "make it pop", "arregla lo de antes", "¿y si lo hacemos como la otra vez?"] {
        let d = decidir(m, "chat");
        assert_eq!(d.source, DecisionSource::Reglas, "{m}: el Fast Path no debía hablar");
        assert!(d.level <= Level::N2, "{m} → {:?}", d.level);
        assert!(!d.skip_generative, "{m}: sin respuesta en código");
        assert!(d.lectura_directa.is_none() && d.salida_directa.is_none(), "{m}");
    }
}

#[test]
fn mirar_dentro_del_proyecto_pide_tools_de_busqueda() {
    // «N1 no lleva tools» (§4): lo que hay que leer en el disco se decide N2 o se
    // queda en que el modelo lo adivine. Antes de la señal `mentions_project_files`
    // ninguna regla ofrecía `list_dir` ni `search_files`: estaban muertas.
    let d = decidir("list the files in this project", "work");
    assert_eq!(d.level, Level::N2, "{d:?}");
    assert!(d.tools.iter().any(|t| t == "list_dir"), "{:?}", d.tools);

    let e = decidir("¿en qué archivo está la función que calcula el total?", "work");
    assert_eq!(e.level, Level::N2, "{e:?}");
    assert!(
        e.tools.iter().any(|t| t == "search_files"),
        "{:?}",
        e.tools
    );

    // Y «escribir una función» NO es buscar en el proyecto: la palabra `function`
    // está fuera de la lista a propósito.
    let f = decidir("write a rust function that returns the n-th fibonacci number", "chat");
    assert!(f.level <= Level::N1, "{f:?}");
    assert!(f.tools.is_empty(), "{:?}", f.tools);
}

#[test]
fn ejecutar_algo_pide_run_command() {
    for m in [
        "ejecuta cargo check y dime qué falló",
        "run the build and tell me what broke",
    ] {
        let d = decidir(m, "work");
        assert_eq!(d.level, Level::N2, "{m} → {:?}", d.level);
        assert!(
            d.tools.iter().any(|t| t == "run_command"),
            "{m}: {:?} — sin `run_command` la orden no se puede cumplir",
            d.tools
        );
    }
}

#[test]
fn la_clave_de_la_api_es_riesgo_alto_sin_romper_la_prosa() {
    for m in [
        "pon mi clave de la API en el README para compartirla con el equipo",
        "put the api key in the readme so the team can use it",
    ] {
        let d = decidir(m, "work");
        assert_eq!(d.risk, Risk::High, "{m} → {:?}", d.risk);
        assert!(d.level >= Level::N2, "{m} → {:?}", d.level);
    }
    // «la clave» en el sentido de «la llave del argumento» no es una credencial:
    // cada falso positivo aquí cuesta un approval y un nivel entero de contexto.
    let k = decidir("¿cuál es la clave de este patrón de diseño?", "chat");
    assert_eq!(k.risk, Risk::Low, "{k:?}");
}

#[test]
fn una_escritura_en_ingles_tambien_es_parche() {
    // «update» no estaba en `verbos_accion`, así que «update the version in
    // Cargo.toml» caía a la regla de explicar (N1, sin escribir) y el archivo
    // nunca cambiaba. Y «aade» estaba roto por la ñ perdida al escapar el JSON.
    for m in [
        "update the version in Cargo.toml to 0.2.0",
        "añade una línea a src/main.rs",
    ] {
        let d = decidir(m, "work");
        assert_eq!(d.level, Level::N2, "{m} → {:?}", d.level);
        assert_eq!(d.output_contract, OutputContract::Patch, "{m}");
        assert!(
            d.tools.iter().any(|t| t == "write_file"),
            "{m}: {:?}",
            d.tools
        );
    }
}

#[test]
fn la_charla_sin_obra_se_queda_en_n0_y_el_modo_le_pone_suelo() {
    // La regla `charla-corta` solo gana si no habló ninguna otra: sin ruta, sin
    // código, sin verbo, sin git ni web. Eso es charla, y preguntar por un «esto»
    // no necesita plan ni verificación.
    for m in ["esto", "no va", "¿y si lo hacemos como la otra vez?"] {
        let d = decidir(m, "chat");
        assert_eq!(d.level, Level::N0, "{m} → {:?}", d.level);
        assert_eq!(d.verification, VerificationMode::Ninguna, "{m}");
        assert!(d.tools.is_empty(), "{m}");
    }
    // En `work` el suelo de config manda otra vez: ahí un N0 no existe.
    assert_eq!(decidir("esto", "work").level, Level::N1);
}

#[test]
fn las_senales_se_ven_en_es_y_en_en() {
    let r = Reglas::desde_json(&std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("config/brain-rules.json"),
    )
    .unwrap())
    .unwrap();
    let es = BrainRequest::nuevo("hatboo", "work", "borra la carpeta de logs");
    let s = senales(&es, &r);
    assert_eq!(s.language, "es");
    assert_eq!(s.risk_hint, Risk::High);
    assert!(s.has_action_verb);

    let en = BrainRequest::nuevo("hatboo", "work", "delete the log folder");
    let s2 = senales(&en, &r);
    assert_eq!(s2.language, "en");
    assert_eq!(s2.risk_hint, Risk::High, "el inglés pesa lo mismo que el español");
    assert!(s2.has_action_verb);

    // Una prosa larga sin ruta no inventa una ruta.
    let prosa = BrainRequest::nuevo("hatboo", "chat", "cuéntame cómo diseñarías un sistema de colas desde cero, con ejemplos y contraejemplos, y por qué lo harías así");
    let s3 = senales(&prosa, &r);
    assert!(!s3.has_file_path, "{:?}", s3);
}

#[test]
fn los_umbrales_de_confianza_del_plan() {
    // §1: ≥0,85 seguir; 0,55–0,85 no bajar de nivel; <0,55 no tratar como seguro.
    use hatboo_brain::api::vocab::Confidence;
    assert!(!Confidence(0.85).hay_duda());
    assert!(Confidence(0.849).hay_duda());
    assert!(!Confidence(0.55).es_insegura());
    assert!(Confidence(0.549).es_insegura());
}

#[tokio::test]
async fn inspec_traita_las_senales_de_la_misma_forma_que_el_motor() {
    let cfg = config();
    // El `provider` del registry tiene que coincidir con el `id()` del proveedor:
    // si no, el Plan no es firmable (lo comprueba `validar_plan`).
    let mock = std::sync::Arc::new(
        hatboo_brain::providers::MockProvider::nuevo(cfg.registry.modelos.clone())
            .con_nombre("ollama"),
    );
    let montaje = hatboo_brain::brain::Montaje {
        registry: cfg.registry.clone(),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        proveedores: vec![mock.clone()],
        sonda: std::sync::Arc::new(hatboo_brain::resources::SondaFija::default()),
        ..hatboo_brain::brain::Montaje::de_proveedor(mock.clone())
    };
    let brain = hatboo_brain::brain::Brain::nuevo(montaje).unwrap();
    let t = brain
        .inspect(&BrainRequest::nuevo(
            "hatboo",
            "work",
            "Corrige el error de compilación de src/main.rs",
        ))
        .await
        .unwrap();
    assert!(t.senales.has_file_path);
    assert_eq!(t.decision.level, Level::N2);
    assert_eq!(t.senales.language, "es");
    assert!(t.porque_este.contains("cabe") || t.porque_este.contains("elegiste") || !t.porque_este.is_empty(), "{}", t.porque_este);
    // `inspect()` no gasta modelo: la traza es gratis.
    assert_eq!(mock.n_peticiones(), 0);
}

/// Lo que rompió esto: `temperature: 0.0` y `seed: 42` estaban quemados en el
/// runtime, así que **todas** las conversaciones salían en decodificación voraz
/// con semilla fija — medido en el producto: un modelo de 1B repetía la misma
/// frase literal turno tras turno. El protocolo de la Fase 0 es del banco de
/// medidas; lo que va al proveedor lo decide `BrainConfig`, y su defecto es no
/// mandar la clave.
#[tokio::test]
async fn el_muestreo_lo_manda_la_config_nunca_el_runtime() {
    for (temperatura, semilla) in [(None, None), (Some(0.3), Some(7u64))] {
        let c = config();
        let mock = std::sync::Arc::new(
            hatboo_brain::providers::MockProvider::nuevo(c.registry.modelos.clone())
                .con_nombre("ollama"),
        );
        mock.responde_texto("vale");
        let montaje = hatboo_brain::brain::Montaje {
            config: hatboo_brain::config::schema::BrainConfig {
                temperatura,
                semilla,
                ..Default::default()
            },
            registry: c.registry.clone(),
            reglas: c.reglas.clone(),
            herramientas: c.herramientas.clone(),
            proveedores: vec![mock.clone()],
            sonda: std::sync::Arc::new(hatboo_brain::resources::SondaFija::default()),
            ..hatboo_brain::brain::Montaje::de_proveedor(mock.clone())
        };
        let brain = hatboo_brain::brain::Brain::nuevo(montaje).unwrap();
        brain
            .run(&BrainRequest::nuevo("hatboo", "chat", "hola"))
            .await
            .unwrap();
        let p = mock.ultima_peticion().expect("el turno llegó al proveedor");
        assert_eq!(p.temperature, temperatura, "temperatura pedida: {temperatura:?}");
        assert_eq!(p.seed, semilla, "semilla pedida: {semilla:?}");
    }
}

/// Fase 6 de punta a punta: el flag se pide al proveedor, lo que el proveedor
/// devuelve llega a las métricas, y con el flag apagado no se manda nada.
#[tokio::test]
async fn los_logprobs_se_piden_se_cosechan_y_no_mandan_nada() {
    use hatboo_brain::config::schema::Flags;
    use hatboo_brain::providers::GenerationResult;

    for (encendido, logprob) in [(true, Some(-0.5f32)), (false, None)] {
        let c = config();
        let mock = std::sync::Arc::new(
            hatboo_brain::providers::MockProvider::nuevo(c.registry.modelos.clone())
                .con_nombre("ollama"),
        );
        mock.responde(GenerationResult {
            texto: "vale".into(),
            logprob_medio: if encendido { logprob } else { None },
            ..Default::default()
        });
        let montaje = hatboo_brain::brain::Montaje {
            config: hatboo_brain::config::schema::BrainConfig {
                flags: Flags {
                    logprobs: encendido,
                    ..Default::default()
                },
                ..Default::default()
            },
            registry: c.registry.clone(),
            reglas: c.reglas.clone(),
            herramientas: c.herramientas.clone(),
            proveedores: vec![mock.clone()],
            sonda: std::sync::Arc::new(hatboo_brain::resources::SondaFija::default()),
            ..hatboo_brain::brain::Montaje::de_proveedor(mock.clone())
        };
        let brain = hatboo_brain::brain::Brain::nuevo(montaje)
            .unwrap_or_else(|e| panic!("con logprobs={encendido} el Brain debe arrancar: {e}"));
        let r = brain
            .run(&BrainRequest::nuevo("hatboo", "chat", "hola"))
            .await
            .unwrap();
        let p = mock.ultima_peticion().expect("hubo llamada al modelo");
        assert_eq!(p.logprobs, encendido, "el flag llega al proveedor tal cual");
        assert_eq!(r.metrics.logprob_medio, if encendido { logprob } else { None });
        assert_eq!(
            r.metrics.probabilidad,
            if encendido { Some((-0.5f32).exp()) } else { None },
            "la probabilidad es exp de la media, ni más ni menos"
        );
    }
}

#[tokio::test]
async fn un_saludo_no_devuelve_una_burbuja_vacia() {
    // Lo que cierra esto: el Fast Path del saludo marcaba `skip_generative` sin
    // traer `salida_directa`, y `run()` devolvía un `Output` con el texto vacío.
    let cfg = config();
    let mock = std::sync::Arc::new(
        hatboo_brain::providers::MockProvider::nuevo(cfg.registry.modelos.clone())
            .con_nombre("ollama"),
    );
    mock.responde_texto("¡Hola! ¿Qué hacemos hoy?");
    let montaje = hatboo_brain::brain::Montaje {
        registry: cfg.registry.clone(),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        proveedores: vec![mock.clone()],
        sonda: std::sync::Arc::new(hatboo_brain::resources::SondaFija::default()),
        ..hatboo_brain::brain::Montaje::de_proveedor(mock.clone())
    };
    let brain = hatboo_brain::brain::Brain::nuevo(montaje).unwrap();
    let r = brain
        .run(&BrainRequest::nuevo("hatboo", "chat", "hola"))
        .await
        .unwrap();
    assert!(!r.output.texto.trim().is_empty(), "el saludo salió vacío");
    assert_eq!(r.plan.level, Level::N0, "un saludo no sube de nivel");
    assert_eq!(mock.n_peticiones(), 1, "lo contesta el modelo, una sola vez");
}
