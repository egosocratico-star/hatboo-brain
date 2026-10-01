//! Verificación (§10): Pass / Fail / Unverifiable; `run()` devuelve `Ok` con
//! `SinVerificar`, no `Err`; y una tool que escribe no declara «listo» antes de
//! pasar el verificador.

use hatboo_brain::api::request::{BrainRequest, ProjectContext, ToolInfo, VerifyCommands};
use hatboo_brain::api::response::OutputStatus;
use hatboo_brain::api::vocab::{
    ApprovalLevel, ExecutionPolicy, Intent, Level, OutputContract, VerificationMode,
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
    let montaje = Montaje {
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
    let mut brain = Brain::nuevo(montaje).unwrap();
    brain.config.flags.verificacion_ejecucion = true;
    brain
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
    assert!(
        matches!(
            r.verification,
            VerificationResult::Unverifiable { .. } | VerificationResult::NoRequerida
        ),
        "{:?}",
        r.verification
    );
    assert_eq!(r.output.status, OutputStatus::SinVerificar, "{:?}", r.output);
    assert!(!r.es_exito(), "Unverifiable no es éxito");
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
async fn un_command_pass_si_es_exito() {
    // Con un ejecutor que devuelve 0, el mismo pedido sale Verificado: el Pass no
    // lo inventa el modelo.
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
    assert_eq!(r.verification, VerificationResult::Pass, "{:?}", r.verification);
    assert!(r.es_exito());
    assert_eq!(r.output.status, OutputStatus::Verificado);
    assert_eq!(r.plan.intent, Intent::Modify);
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
