//! Verificación (§10): Pass / Fail / Unverifiable; `run()` devuelve `Ok` con
//! `SinVerificar`, no `Err`; y una tool que escribe no declara «listo» antes de
//! pasar el verificador.

use hatboo_brain::api::request::{BrainRequest, ProjectContext, ToolInfo, VerifyCommands};
use hatboo_brain::api::response::OutputStatus;
use hatboo_brain::api::vocab::{
    ApprovalLevel, ExecutionPolicy, Level, OutputContract, VerificationMode,
};
use hatboo_brain::brain::{Brain, EjecutarTool, Montaje};
use hatboo_brain::config::loader::Cargada;
use hatboo_brain::models::{ModelInfo, ModelKind, Registry};
use hatboo_brain::prompt::Estimador;
use hatboo_brain::providers::{GenerationResult, MockProvider};
use hatboo_brain::resources::SondaFija;
use hatboo_brain::verification::execution::{Ejecutor, Salida, SinEjecutor};
use hatboo_brain::verification::{verificar, Candidato, Entorno, Unverifiable, VerificationResult};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn modelo() -> ModelInfo {
    let mut m = BTreeMap::new();
    m.insert(2048u32, 900u64);
    m.insert(4096u32, 1100u64);
    m.insert(8192u32, 1700u64);
    ModelInfo {
        id: "gemma3:1b".into(),
        provider: "ollama".into(),
        local: true,
        kind: ModelKind::Generativo,
        profile: hatboo_brain::api::vocab::Profile::Nano,
        tier: 1,
        ram_mb_by_ctx: m,
        max_ctx: 32768,
        strengths: vec![],
        supports_tools: true,
        supports_thinking: false,
        supports_vision: false,
        structured_output: false,
        disco_mb: Some(815),
    }
}

/// El parche que cuadra con `original`: contexto igual, una línea cambiada.
const PARCHE_BIEN: &str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"a\");\n+    println!(\"b\");\n }\n";

fn cand(texto: &str, contrato: OutputContract) -> Candidato<'_> {
    Candidato {
        texto,
        contrato,
        root: Some("C:/p"),
        archivo_objetivo: None,
    }
}

/// La verificación de forma tiene que medir la longitud contra el tope que firmó
/// EL Plan. `Entorno` no tenía ese campo y `text::forma` recibía un 2048 fijo, así
/// que un N1 de 512 tokens pasaba con ~12.000 caracteres: nadie comprobaba el
/// contrato de salida, y en esta máquina cada uno de esos caracteres se paga en
/// decodificación (~17 tok/s).
#[test]
fn la_forma_mide_contra_el_tope_del_plan() {
    let largo = "ta ".repeat(1300); // ~3.900 caracteres
    let c = cand(&largo, OutputContract::Texto);
    let holgado = Entorno {
        max_output_tokens: 4096,
        ..Default::default()
    };
    assert!(
        matches!(
            verificar(VerificationMode::Formato, &c, &holgado),
            VerificationResult::Pass
        ),
        "un tope de N2 tiene que dejar una salida de ese tamaño"
    );
    match verificar(
        VerificationMode::Formato,
        &c,
        &Entorno {
            max_output_tokens: 512,
            ..Default::default()
        },
    ) {
        VerificationResult::Fail { motivo, .. } => {
            assert!(motivo.contains("512"), "{motivo}");
        }
        otro => panic!("debía fallar contra el tope del plan: {otro:?}"),
    }
}

#[test]
fn los_tres_veredictos_salen_como_estan_en_el_canon() {
    let e = Entorno::default();
    assert_eq!(
        verificar(VerificationMode::Ninguna, &cand("", OutputContract::Texto), &e),
        VerificationResult::NoRequerida
    );
    assert_eq!(
        verificar(
            VerificationMode::Formato,
            &cand("una respuesta normal", OutputContract::Texto),
            &Entorno {
                idioma_pedido: Some("es"),
                ..Default::default()
            }
        ),
        VerificationResult::Pass
    );
    match verificar(
        VerificationMode::Formato,
        &cand("   ", OutputContract::Texto),
        &e,
    ) {
        VerificationResult::Fail { clase, motivo } => {
            assert_eq!(clase, hatboo_brain::api::vocab::FailureClass::Formato);
            assert!(motivo.contains("vacía"), "{motivo}");
        }
        otro => panic!("{otro:?}"),
    }
    // Sin ejecutor, un comando no se corrió: Unverifiable, jamás Pass.
    let r = verificar(
        VerificationMode::Determinista,
        &cand("arreglado", OutputContract::Texto),
        &Entorno {
            comando: Some("cargo check"),
            ejecutor: Some(&SinEjecutor),
            ..Default::default()
        },
    );
    assert!(r.es_unverifiable(), "{r:?}");
    assert_eq!(r.etiqueta(), "no verificable");
}

#[test]
fn un_proyecto_sin_comando_lo_dice_como_proyecto_sin_verificacion() {
    let r = verificar(
        VerificationMode::Determinista,
        &cand("listo", OutputContract::Texto),
        &Entorno {
            ejecutor: Some(&SinEjecutor),
            ..Default::default()
        },
    );
    match r {
        VerificationResult::Unverifiable {
            motivo: Unverifiable::ProyectoSinVerificacion,
        } => {}
        otro => panic!("{otro:?}"),
    }
}

/// Un `Fail` de formato no gasta una ejecución; un `Unverifiable` de formato sí
/// deja correr el comando, que es el que sabe si el proyecto compila.
#[test]
fn la_forma_rota_bloquea_el_comando_y_la_forma_dudosa_no() {
    #[derive(Default)]
    struct Contador(AtomicUsize);
    impl Ejecutor for Contador {
        fn correr(&self, _: &str, _: &str, _: u32) -> Result<Salida, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Salida {
                codigo: 0,
                stdout: "ok".into(),
                stderr: String::new(),
                dur_ms: 1,
            })
        }
    }
    let rompio = Contador::default();
    let r = verificar(
        VerificationMode::Determinista,
        &cand("```rust\nsin cerrar", OutputContract::Markdown),
        &Entorno {
            comando: Some("npm test"),
            ejecutor: Some(&rompio),
            ..Default::default()
        },
    );
    assert!(matches!(r, VerificationResult::Fail { .. }), "{r:?}");
    assert_eq!(rompio.0.load(Ordering::SeqCst), 0, "con la forma rota no se ejecuta");

    // Texto normal sin comando de proyecto: el formato no puede afirmar nada y el
    // comando tampoco existe → Unverifiable con el motivo del proyecto.
    let dudoso = Contador::default();
    let r2 = verificar(
        VerificationMode::Determinista,
        &cand("una respuesta en prosa", OutputContract::Texto),
        &Entorno {
            ejecutor: Some(&dudoso),
            ..Default::default()
        },
    );
    assert!(r2.es_unverifiable(), "{r2:?}");
    assert_eq!(dudoso.0.load(Ordering::SeqCst), 0, "sin comando nada que correr");

    // Y con comando, el texto sin verificar de forma **sí** lo deja correr.
    deja_correr();
}

fn deja_correr() {
    #[derive(Default)]
    struct Ok0(AtomicUsize);
    impl Ejecutor for Ok0 {
        fn correr(&self, _: &str, _: &str, _: u32) -> Result<Salida, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Salida {
                codigo: 0,
                stdout: String::new(),
                stderr: String::new(),
                dur_ms: 1,
            })
        }
    }
    let e = Ok0::default();
    let r = verificar(
        VerificationMode::Determinista,
        &cand("listo", OutputContract::Texto),
        &Entorno {
            comando: Some("cargo check"),
            ejecutor: Some(&e),
            ..Default::default()
        },
    );
    assert_eq!(r, VerificationResult::Pass, "{r:?}");
    assert_eq!(e.0.load(Ordering::SeqCst), 1, "el comando se corrió una vez");
}

#[test]
fn un_parche_cuyo_contexto_no_cuadra_no_es_una_escritura() {
    let original = "fn main() {\n    println!(\"otra cosa\");\n}\n";
    let leer = |ruta: &str| -> Option<String> {
        (ruta == "src/main.rs" || ruta == "a/src/main.rs").then(|| original.to_string())
    };
    let parche = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"a\");\n+    println!(\"b\");\n }\n";
    let r = verificar(
        VerificationMode::Determinista,
        &cand(parche, OutputContract::Patch),
        &Entorno {
            leer: Some(&leer),
            ..Default::default()
        },
    );
    match r {
        VerificationResult::Fail { motivo, .. } => {
            assert!(motivo.contains("no cuadra"), "{motivo}")
        }
        otro => panic!("un parche mentira no puede pasar: {otro:?}"),
    }
}

// ─────────────────────── el bucle completo con el mock ───────────────────────

struct SinTools;

#[async_trait::async_trait]
impl EjecutarTool for SinTools {
    async fn ejecutar(&self, tool: &String, _: &serde_json::Value) -> Result<String, String> {
        Err(format!("el test no ejecuta «{tool}»"))
    }
}

fn cerebro(mock: Arc<MockProvider>, cfg: &Cargada) -> Brain {
    let mut montaje = Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo()]),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        contador: Arc::new(Estimador),
        ejecutor_tools: Some(Arc::new(SinTools)),
        ejecutor_comandos: Some(Arc::new(SinEjecutor)),
        lector: Some(Arc::new(|_: &str| None)),
        ..Montaje::de_proveedor(mock.clone())
    };
    // El flag se pone en el montaje, no después: el Motor y el Escalador toman su
    // copia en `Brain::nuevo`, así que mutar `brain.config` más tarde solo medio
    // funcionaba.
    montaje.config.flags.verificacion_ejecucion = true;
    Brain::nuevo(montaje).unwrap()
}

fn pedido(mensaje: &str, modo: &str, verificar: Option<&str>) -> BrainRequest {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mut req = BrainRequest::nuevo("hatboo", modo, mensaje)
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
        .con_aprobacion(ApprovalLevel::ApproveForMe);
    req.project = Some(ProjectContext {
        root: "C:/proyecto".into(),
        has_tests: false,
        has_lint: false,
        has_build: true,
        language: None,
        hatboo_md: None,
        trust_state: Default::default(),
        verify: VerifyCommands {
            check: verificar.map(|s| s.to_string()),
            lint: None,
            test: None,
        },
    });
    req
}

#[tokio::test]
async fn sin_verificacion_posible_la_salida_es_ok_y_se_dice_sin_verificar() {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    mock.responde_texto("Un mutex protege datos compartidos.");
    let brain = cerebro(mock.clone(), &cfg);
    let r = brain
        .run(&pedido("explícame con detalle qué es un mutex y cuándo evitarías uno en un programa asíncrono escrito en rust para no perder rendimiento", "work", None))
        .await
        .expect("hay salida que mostrar: run() no devuelve Err");
    assert!(!r.output.texto.is_empty());
    // Desde que las señales casan por palabra («pro**grama**» ya no es `rama`, y
    // con eso se iba a N2 sin comando), esta explicación se queda en N1/Formato:
    // la forma sí es comprobable, así que un Pass aquí es honesto. Lo que se
    // exige es que el estado y el veredicto no se contradigan.
    match r.verification {
        VerificationResult::Pass => assert_eq!(r.output.status, OutputStatus::Verificado),
        VerificationResult::Unverifiable { .. } => {
            assert_eq!(r.output.status, OutputStatus::SinVerificar)
        }
        VerificationResult::NoRequerida => assert_eq!(r.output.status, OutputStatus::Propuesto),
        VerificationResult::Fail { .. } => panic!("la forma de un texto no debería fallar: {:?}", r.verification),
    }
    assert_eq!(
        r.es_exito(),
        matches!(r.verification, VerificationResult::Pass),
        "el éxito tiene que ser exactamente el Pass"
    );
}

#[tokio::test]
async fn un_parche_propuesto_no_se_vende_como_verificado() {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    // Un parche con contexto que no cuadra con lo que el lector devuelve (nada).
    mock.responde(GenerationResult {
        texto: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"a\");\n+    println!(\"b\");\n }\n".into(),
        ..Default::default()
    });
    let brain = cerebro(mock.clone(), &cfg);
    let r = brain
        .run(&pedido(
            "Corrige el error de compilación de src/main.rs",
            "work",
            Some("cargo check"),
        ))
        .await
        .unwrap();
    assert_eq!(r.plan.level, Level::N2, "{:?}", r.plan);
    assert_eq!(r.plan.output_contract, OutputContract::Patch);
    // No se pudo ni leer el archivo: propuesta, jamás «verificado».
    assert_ne!(r.output.status, OutputStatus::Verificado, "{:?}", r.output);
    assert!(!r.es_exito());
    assert!(matches!(
        r.verification,
        VerificationResult::Unverifiable { .. } | VerificationResult::Fail { .. }
    ), "{:?}", r.verification);
}

#[tokio::test]
async fn el_comando_de_verificacion_manda_sobre_lo_que_diga_el_modelo() {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    // El plan es N2 con reintento: cada pasada gasta una respuesta. Se encolan
    // varias a propósito; lo que se comprueba es que la última palabra la tiene
    // el comando, no lo que el modelo afirme.
    for _ in 0..4 {
        mock.responde_texto("ya compila, te lo juro");
    }
    let brain = cerebro(mock.clone(), &cfg);
    let r = brain
        .run(&pedido("Corrige el error de compilación de src/main.rs", "work", Some("cargo check")))
        .await
        .unwrap();
    assert_eq!(
        r.verification,
        VerificationResult::Fail {
            clase: hatboo_brain::api::vocab::FailureClass::Formato,
            motivo: "no tiene cabeceras de archivo, no es un parche".into()
        },
        "{:?}",
        r.verification
    );
    assert_ne!(r.output.status, OutputStatus::Verificado);
}

#[tokio::test]
async fn el_comando_sobre_lo_no_parcheado_no_es_un_pass() {
    // Antes este caso salía `Verificado` con solo devolver 0 el comando, y el
    // comando había corrido sobre el archivo SIN parchear: decía «compilaba
    // antes», no «tu parche compila». Ahora eso es `Unverifiable` declarado.
    struct Cero;
    impl Ejecutor for Cero {
        fn correr(&self, _: &str, _: &str, _: u32) -> Result<Salida, String> {
            Ok(Salida {
                codigo: 0,
                stdout: "Finished".into(),
                stderr: String::new(),
                dur_ms: 40,
            })
        }
    }
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    // El contrato del plan es `patch`: para que el Pass signifique algo, la
    // salida tiene que ser un parche cuyo contexto cuadre con el archivo real.
    let original = "fn main() {\n    println!(\"a\");\n}\n";
    for _ in 0..4 {
        mock.responde_texto(PARCHE_BIEN);
    }
    let o = original.to_string();
    let mock2: Arc<dyn hatboo_brain::providers::ModelProvider> = mock.clone();
    let montaje = Montaje {
        proveedores: vec![mock2],
        registry: Registry::nuevo(vec![modelo()]),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        contador: Arc::new(Estimador),
        ejecutor_comandos: Some(Arc::new(Cero)),
        lector: Some(Arc::new(move |ruta: &str| {
            ruta.ends_with("src/main.rs").then(|| o.clone())
        })),
        ..Montaje::de_proveedor(mock.clone())
    };
    let brain = Brain::nuevo(montaje).unwrap();
    let r = brain
        .run(&pedido(
            "Corrige el error de compilación de src/main.rs",
            "work",
            Some("cargo check"),
        ))
        .await
        .unwrap();
    assert!(
        matches!(r.verification, VerificationResult::Unverifiable { .. }),
        "{:?}",
        r.verification
    );
    assert!(!r.es_exito(), "sin el parche aplicado no puede ser éxito");
    assert_ne!(r.output.status, OutputStatus::Verificado);
    // Y el Pass sí llega cuando el producto declara que aplicó el parche antes
    // de correr el comando: no era un capricho del verificador.
    let e = Entorno {
        comando: Some("cargo check"),
        ejecutor: Some(&Cero),
        parche_aplicado: true,
        ..Default::default()
    };
    let c = Candidato {
        texto: PARCHE_BIEN,
        contrato: OutputContract::Patch,
        root: Some("C:/p"),
        archivo_objetivo: None,
    };
    assert_eq!(
        verificar(hatboo_brain::api::vocab::VerificationMode::Determinista, &c, &e),
        VerificationResult::Pass,
        "con el parche aplicado, el 0 del comando sí es un Pass"
    );
}

#[tokio::test]
async fn la_traza_de_un_turno_no_lleva_el_texto_del_usuario() {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    mock.responde_texto("vale");
    let brain = cerebro(mock, &cfg);
    let t = brain
        .inspect(&BrainRequest::nuevo(
            "hatboo",
            "chat",
            "mi contraseña es raton313 y quiero borrar la carpeta de logs",
        ))
        .await
        .unwrap();
    let j = serde_json::to_string(&t).unwrap();
    assert!(!j.contains("raton313"), "la traza no filtra el secreto: {j}");
    assert!(j.contains("riskHint") || j.contains("risk_hint"), "{j}");
    assert_eq!(t.senales.risk_hint, hatboo_brain::api::vocab::Risk::High);
}

/// Ejecuta de verdad, con una salida reconocible: es lo que tiene que volver al
/// modelo en la ronda siguiente.
struct LectorQueResponde;

#[async_trait::async_trait]
impl EjecutarTool for LectorQueResponde {
    async fn ejecutar(&self, _: &String, _: &serde_json::Value) -> Result<String, String> {
        Ok("CONTENIDO-LEIDO-42".into())
    }
}

#[tokio::test]
async fn el_resultado_de_la_tool_vuelve_al_modelo() {
    // El bucle ejecutaba la tool, tiraba el resultado y pedía otra vez lo mismo:
    // con temperature 0 salían ocho generaciones idénticas y un `Timeout`.
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    mock.responde(GenerationResult {
        tool_calls: vec![hatboo_brain::providers::LlamadaTool {
            tool: "read_file".into(),
            args: serde_json::json!({ "path": "src/main.rs" }),
        }],
        ..Default::default()
    });
    // Suficientes por si la verificación pide su reintento: aquí se comprueba la
    // segunda petición, no cómo termina la corrida.
    for _ in 0..3 {
        mock.responde_texto("ya lo leí");
    }
    let montaje = Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo()]),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        contador: Arc::new(Estimador),
        ejecutor_tools: Some(Arc::new(LectorQueResponde)),
        ..Montaje::de_proveedor(mock.clone())
    };
    let brain = Brain::nuevo(montaje).unwrap();
    let r = brain
        .run(&pedido(
            "Corrige el error de compilación de src/main.rs",
            "work",
            None,
        ))
        .await
        .unwrap();
    let peticiones = mock.peticiones();
    assert!(
        peticiones.len() >= 2,
        "no hubo una segunda vuelta con el resultado: {}",
        peticiones.len()
    );
    assert!(
        peticiones[1].prompt.contains("CONTENIDO-LEIDO-42"),
        "la tool se ejecutó pero su resultado no volvió al modelo: {:?}",
        peticiones[1].prompt
    );
    assert!(!r.output.texto.is_empty(), "el turno terminó sin salida");
}

/// La puerta dijo que no: el modelo tiene que enterarse, la corrida tiene que
/// clasificarlo como `tool` y el escalador tiene que ponerle un techo. Antes la
/// llamada rechazada se tiraba a la basura, así que el modelo repetía la misma
/// llamada hasta `MAX_RONDAS` y `FailureClass::Tool` no la producía nadie.
#[tokio::test]
async fn una_tool_fuera_del_plan_se_le_dice_al_modelo_y_cuenta_como_fallo() {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    for _ in 0..8 {
        mock.responde(GenerationResult {
            tool_calls: vec![hatboo_brain::providers::LlamadaTool {
                tool: "rm_todo".into(),
                args: serde_json::json!({ "path": "." }),
            }],
            ..Default::default()
        });
    }
    let brain = cerebro(mock.clone(), &cfg);
    let r = brain
        .run(&pedido(
            "Corrige el error de compilación de src/main.rs",
            "work",
            None,
        ))
        .await
        .unwrap();

    // 1 · el motivo llega al turno siguiente, mezclado con las observaciones.
    let peticiones = mock.peticiones();
    assert!(
        peticiones.len() >= 2,
        "no hubo una vuelta con el rechazo: {}",
        peticiones.len()
    );
    assert!(
        peticiones[1].prompt.contains("rm_todo"),
        "el modelo no se enteró de qué se rechazó: {:?}",
        peticiones[1].prompt
    );
    assert!(
        peticiones[1].prompt.contains("no está en el Plan"),
        "sin motivo no hay nada que corregir: {:?}",
        peticiones[1].prompt
    );

    // 2 · el bucle se corta con el presupuesto de reintentos, no con las rondas.
    assert!(
        peticiones.len() < 8,
        "repitió hasta agotar MAX_RONDAS: {}",
        peticiones.len()
    );

    // 3 · la métrica lo dice: `Tool` es alcanzable y ya no es un `None` fijo.
    assert_eq!(
        r.metrics.clase_fallo,
        Some(hatboo_brain::api::vocab::FailureClass::Tool),
        "{:?}",
        r.metrics
    );
    // 4 · y el rastro de la recuperación viaja con el resultado. Antes se acumulaba
    // dentro del bucle y se tiraba al devolver: el panel no podía contar qué costó
    // el turno.
    assert!(
        r.recuperacion
            .iter()
            .any(|x| x.contains("no está en el Plan")),
        "{:?}",
        r.recuperacion
    );
    // Y la llamada rechazada se ve en el resultado, con su motivo, no borrada.
    let rechazada = r
        .output
        .tool_calls
        .iter()
        .find(|c| c.tool == "rm_todo")
        .expect("la llamada rechazada tiene que quedar en el resultado");
    assert!(!rechazada.ok);
    assert!(
        rechazada.resultado.as_deref().unwrap_or("").contains("Plan"),
        "{:?}",
        rechazada.resultado
    );
}

/// §II.10 probado con las dos mitades: encendida, la pieza corre y se le cobra el
/// comando al ejecutor; apagada, no se ejecuta nada en el proyecto y el veredicto
/// sigue diciendo la verdad. Hasta aquí `verificacion_ejecucion` no lo leía nadie,
/// así que apagar la puerta de seguridad de la Fase 5 no apagaba nada.
#[tokio::test]
async fn el_flag_de_verificacion_ejecucion_cambia_de_verdad_la_conducta() {
    #[derive(Default)]
    struct CeroQueCuenta(AtomicUsize);
    impl Ejecutor for CeroQueCuenta {
        fn correr(&self, _: &str, _: &str, _: u32) -> Result<Salida, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Salida {
                codigo: 0,
                stdout: "Finished".into(),
                stderr: String::new(),
                dur_ms: 40,
            })
        }
    }

    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();

    async fn corrido(
        cfg: &Cargada,
        ejecuta: bool,
    ) -> (usize, hatboo_brain::api::response::BrainResult) {
        let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
        // Un `search` en N2 con contrato markdown: la forma no exige parche, así
        // que lo único que puede impedir correr el comando es el flag.
        for _ in 0..4 {
            mock.responde_texto("En la rama hay dos archivos modificados sin commitear.");
        }
        let contador = Arc::new(CeroQueCuenta::default());
        let mut montaje = Montaje {
            proveedores: vec![mock.clone()],
            registry: Registry::nuevo(vec![modelo()]),
            reglas: cfg.reglas.clone(),
            herramientas: cfg.herramientas.clone(),
            sonda: Arc::new(SondaFija::default()),
            contador: Arc::new(Estimador),
            ejecutor_comandos: Some(contador.clone()),
            lector: Some(Arc::new(|_: &str| None)),
            ..Montaje::de_proveedor(mock.clone())
        };
        montaje.config.flags.verificacion_ejecucion = ejecuta;
        let brain = Brain::nuevo(montaje).unwrap();
        let r = brain
            .run(&pedido(
                "¿qué hay sin commitear en la rama?",
                "work",
                Some("cargo check"),
            ))
            .await
            .unwrap();
        (contador.0.load(Ordering::SeqCst), r)
    }

    let (con_flag, r_con) = corrido(&cfg, true).await;
    assert!(
        con_flag >= 1,
        "encendida la pieza, el comando se corrió: {:?}",
        r_con.verification
    );
    assert_eq!(r_con.verification, VerificationResult::Pass, "el 0 del comando, con la pieza encendida, es un Pass");
    assert_eq!(r_con.output.status, OutputStatus::Verificado);

    let (sin_flag, r_sin) = corrido(&cfg, false).await;
    assert_eq!(
        sin_flag, 0,
        "apagada la pieza no se corre nada en el proyecto"
    );
    assert!(
        matches!(
            r_sin.verification,
            VerificationResult::Unverifiable { .. }
        ),
        "sin la pieza el veredicto tiene que decirlo: {:?}",
        r_sin.verification
    );
    assert_ne!(r_sin.output.status, OutputStatus::Verificado);
    assert!(!r_sin.es_exito());
}

/// El `EstadoTarea` ya no se fabrica dentro de `preparar` para tirarlo después: lo
/// que hizo la ronda anterior entra en el prompt de la siguiente, y un bloqueo sin
/// motivo impediría el cierre de un N3 (§IX: determinista, no le pregunta al
/// modelo si «se acabó»).
#[tokio::test]
async fn el_estado_de_la_tarea_viaja_al_siguiente_prompt() {
    /// `read_file` se ejecuta; `write_file` espera aprobación del producto.
    struct UnaSiUnaNo;
    #[async_trait::async_trait]
    impl EjecutarTool for UnaSiUnaNo {
        async fn ejecutar(&self, _: &String, _: &serde_json::Value) -> Result<String, String> {
            Ok("CONTENIDO-42".into())
        }
        fn requiere_aprobacion(&self, tool: &String, _: &serde_json::Value) -> bool {
            tool == "write_file"
        }
    }

    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    mock.responde(GenerationResult {
        tool_calls: vec![
            hatboo_brain::providers::LlamadaTool {
                tool: "read_file".into(),
                args: serde_json::json!({ "path": "src/main.rs" }),
            },
            hatboo_brain::providers::LlamadaTool {
                tool: "write_file".into(),
                args: serde_json::json!({ "path": "src/main.rs" }),
            },
        ],
        ..Default::default()
    });
    for _ in 0..4 {
        mock.responde_texto("Un mutex protege datos compartidos.");
    }
    let montaje = Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo()]),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        contador: Arc::new(Estimador),
        ejecutor_tools: Some(Arc::new(UnaSiUnaNo)),
        lector: Some(Arc::new(|_: &str| None)),
        ..Montaje::de_proveedor(mock.clone())
    };
    let brain = Brain::nuevo(montaje).unwrap();
    let r = brain
        .run(&pedido(
            "Corrige el error de compilación de src/main.rs",
            "work",
            None,
        ))
        .await
        .expect("hay salida que mostrar");
    let peticiones = mock.peticiones();
    assert!(
        peticiones.len() >= 2,
        "no hubo una segunda vuelta: {}",
        peticiones.len()
    );
    let p2 = &peticiones[1].prompt;
    assert!(p2.contains("Objetivo:"), "{p2}");
    assert!(p2.contains("Hecho: read_file"), "el estado no viajó: {p2}");
    assert!(
        p2.contains("Pendiente: write_file"),
        "el estado no viajó: {p2}"
    );
    assert!(!r.output.texto.is_empty(), "el turno terminó sin salida");
}

/// §IX con la puerta de cierre: un N3 puede tener el comando en verde y aun así
/// dejar una llamada esperando aprobación. `Verificado` entonces sería mentira, y
/// lo decide el crate, no un modelo al que se le pregunta si «se acabó».
#[tokio::test]
async fn un_n3_con_pendientes_no_se_vende_como_verificado() {
    struct Aprobador;
    #[async_trait::async_trait]
    impl EjecutarTool for Aprobador {
        async fn ejecutar(&self, _: &String, _: &serde_json::Value) -> Result<String, String> {
            Ok("HECHO".into())
        }
        fn requiere_aprobacion(&self, tool: &String, _: &serde_json::Value) -> bool {
            tool == "write_file"
        }
    }
    struct Cero;
    impl Ejecutor for Cero {
        fn correr(&self, _: &str, _: &str, _: u32) -> Result<Salida, String> {
            Ok(Salida {
                codigo: 0,
                stdout: "Finished".into(),
                stderr: String::new(),
                dur_ms: 40,
            })
        }
    }

    let reglas = hatboo_brain::decision::rules::Reglas::desde_json(
        r#"{"version":1,"reglas":[{"id":"todo-a-n3",
             "cuando":[{"senal":"message_length","op":">","valor":10}],
             "entonces":{"intent":"explain","level":"N3","contrato":"markdown",
                         "tools":["read_file","write_file"]}}]}"#,
    )
    .unwrap();
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    mock.responde(GenerationResult {
        tool_calls: vec![
            hatboo_brain::providers::LlamadaTool {
                tool: "read_file".into(),
                args: serde_json::json!({ "path": "src/main.rs" }),
            },
            hatboo_brain::providers::LlamadaTool {
                tool: "write_file".into(),
                args: serde_json::json!({ "path": "src/main.rs" }),
            },
        ],
        ..Default::default()
    });
    for _ in 0..4 {
        mock.responde_texto("La corrección está explicada abajo.");
    }
    let montaje = Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo()]),
        reglas,
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        contador: Arc::new(Estimador),
        ejecutor_tools: Some(Arc::new(Aprobador)),
        ejecutor_comandos: Some(Arc::new(Cero)),
        lector: Some(Arc::new(|_: &str| None)),
        ..Montaje::de_proveedor(mock.clone())
    };
    let brain = Brain::nuevo(montaje).unwrap();
    let r = brain
        .run(&pedido(
            "Corrige el error de compilación de src/main.rs",
            "work",
            Some("cargo check"),
        ))
        .await
        .expect("hay salida que entregar");
    assert_eq!(r.plan.level, Level::N3, "{:?}", r.plan);
    assert_eq!(
        r.verification,
        VerificationResult::Pass,
        "el comando sí dio 0: lo que falta es el cierre"
    );
    assert_ne!(
        r.output.status,
        OutputStatus::Verificado,
        "con una llamada pendiente de aprobación no se cierra la tarea"
    );
    assert_eq!(r.output.status, OutputStatus::SinVerificar, "{:?}", r.output);
    assert!(
        !r.es_exito(),
        "el éxito tiene que ser exactamente el Pass cerrado"
    );
}

/// El almacén del producto, en miniatura: guarda lo que le entregan.
#[derive(Clone, Default)]
struct Coleccion(Arc<std::sync::Mutex<Vec<hatboo_brain::observability::RegistroDecision>>>);

impl hatboo_brain::observability::SinkDecisiones for Coleccion {
    fn guarda(&self, r: &hatboo_brain::observability::RegistroDecision) {
        self.0.lock().unwrap().push(r.clone());
    }
}

/// §15.9: el dataset de decisiones es **opt-in**, y lo que guarda sale redactado.
/// La prueba no es que exista la línea: es que la clave que escribió el usuario
/// no esté en ella, y que con la bandera apagada no se guarde nada.
#[tokio::test]
async fn el_dataset_de_decisiones_es_opt_in_y_sale_redactado() {
    let cfg = Cargada::leer(Some(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))).unwrap();
    let clave = "sk-ant-0123456789ABCDEFGHIJKLMN";
    let pedido_clave = BrainRequest::nuevo(
        "hatboo",
        "chat",
        format!("usa la clave {clave} y dime si está bien formada"),
    );

    // Apagado por defecto, con almacén puesto: no se guarda nada.
    let sin_bandera = Coleccion::default();
    // `con_nombre("ollama")`: el registry dice `provider: "ollama"` y el `id()`
    // del proveedor tiene que coincidir, si no `validate_plan` corta con
    // `ProviderNotAllowed`.
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    mock.responde_texto("está bien formada");
    let brain = Brain::nuevo(Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo()]),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        decisiones: Some(Arc::new(sin_bandera.clone())),
        ..Montaje::de_proveedor(mock.clone())
    })
    .unwrap();
    brain.run_with(&pedido_clave, hatboo_brain::brain::OpcionesDeCorrida::nueva()).await.unwrap();
    assert!(sin_bandera.0.lock().unwrap().is_empty(), "el default es no guardar nada");

    // Encendido: una línea por turno, con la clave fuera del texto.
    let con_bandera = Coleccion::default();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo()]).con_nombre("ollama"));
    mock.responde_texto("está bien formada");
    let mut montaje = Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo()]),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija::default()),
        decisiones: Some(Arc::new(con_bandera.clone())),
        ..Montaje::de_proveedor(mock.clone())
    };
    montaje.config.flags.record_decisiones = true;
    let brain = Brain::nuevo(montaje).unwrap();
    let r = brain
        .run_with(&pedido_clave, hatboo_brain::brain::OpcionesDeCorrida::nueva())
        .await
        .unwrap();
    let guardadas = con_bandera.0.lock().unwrap().clone();
    assert_eq!(
        guardadas.len(),
        1,
        "un turno, una decisión registrada (aunque hubiera reintentos)"
    );
    let reg = &guardadas[0];
    assert!(!reg.mensaje.contains(clave), "se coló la clave: {}", reg.mensaje);
    // `redact::texto` deja el prefijo de la clave y oscurece el valor: así el
    // dataset sirve para afinar sin entregar la clave.
    assert!(reg.mensaje.contains("sk-ant-***"), "{}", reg.mensaje);
    assert_eq!(reg.nivel, format!("{:?}", r.plan.level));
    assert_eq!(reg.modelo, r.plan.model);
    assert_eq!(reg.resultado, format!("{:?}", r.output.status));
    // Y la línea es JSON, que es la forma en que se anexa a un dataset.
    let linea = reg.to_linea();
    let j: serde_json::Value = serde_json::from_str(&linea).expect("una línea JSON por turno");
    assert_eq!(j["nivel"], reg.nivel);
}
