//! Recovery (§10): las seis clases, un reintento por clase, la repetición de la
//! misma clase sube de tier, timeout/OOM/proveedor caído son `environment`, y la
//! cancelación no deja un modelo cargado de más.

use hatboo_brain::api::error::BrainError;
use hatboo_brain::api::request::BrainRequest;
use hatboo_brain::api::vocab::FailureClass;
use hatboo_brain::brain::{Brain, Montaje, OpcionesDeCorrida};
use hatboo_brain::models::{ModelInfo, ModelKind, Registry};
use hatboo_brain::observability::events::{BrainEvent, CancelToken, Emitidor};
use hatboo_brain::providers::{MockProvider, ProviderError};
use hatboo_brain::recovery::classifier::{clasificar, motivo, Origen};
use hatboo_brain::recovery::escalator::{Accion, Escalador};
use hatboo_brain::resources::SondaFija;
use hatboo_brain::verification::VerificationResult;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn modelo(id: &str, tier: u8) -> ModelInfo {
    let mut m = BTreeMap::new();
    m.insert(2048u32, 900u64);
    m.insert(4096u32, 1100u64);
    m.insert(8192u32, 1700u64);
    ModelInfo {
        id: id.into(),
        provider: "ollama".into(),
        local: true,
        kind: ModelKind::Generativo,
        profile: hatboo_brain::api::vocab::Profile::Nano,
        tier,
        ram_mb_by_ctx: m,
        max_ctx: 32768,
        strengths: vec![],
        supports_tools: true,
        supports_thinking: false,
        supports_vision: false,
        disco_mb: Some(815),
    }
}

#[test]
fn cada_clase_sale_de_su_origen() {
    use FailureClass as F;
    assert_eq!(clasificar(&Origen::Error(&BrainError::Timeout)), F::Entorno);
    assert_eq!(
        clasificar(&Origen::Error(&BrainError::ResourceExhausted {
            needed_mb: 6000,
            free_mb: 300
        })),
        F::Entorno
    );
    assert_eq!(
        clasificar(&Origen::Error(&BrainError::Provider(
            ProviderError::Transporte("connection refused".into())
        ))),
        F::Entorno
    );
    assert_eq!(
        clasificar(&Origen::Error(&BrainError::Provider(
            ProviderError::Status { codigo: 413, cuerpo: "too large".into() }
        ))),
        F::Contexto
    );
    assert_eq!(
        clasificar(&Origen::Error(&BrainError::Provider(
            ProviderError::RespuestaInvalida("json roto".into())
        ))),
        F::ModelCapability
    );
    assert_eq!(
        clasificar(&Origen::Error(&BrainError::NoEligibleModel)),
        F::ModelCapability
    );
    assert_eq!(clasificar(&Origen::ToolRechazada), F::Tool);
    assert_eq!(clasificar(&Origen::SinLlamadaDeTool), F::Tool);
    assert_eq!(clasificar(&Origen::PresupuestoAgotado), F::Tool);
    let v = VerificationResult::Unverifiable {
        motivo: hatboo_brain::verification::Unverifiable::ProyectoSinVerificacion,
    };
    assert_eq!(clasificar(&Origen::Verificacion(&v)), F::Verificacion);
    let f = VerificationResult::Fail {
        clase: F::Formato,
        motivo: "vacía".into(),
    };
    assert_eq!(clasificar(&Origen::Verificacion(&f)), F::Formato);
    assert!(!motivo(&Origen::ToolRechazada).is_empty(), "todo fallo dice algo");
}

#[test]
fn un_reintento_por_clase_y_nada_mas() {
    let mut e = Escalador::nuevo(4);
    let primera = e.decidir(FailureClass::Formato);
    assert!(matches!(primera, Accion::Reintentar { intento: 1, .. }), "{primera:?}");
    // La misma clase por segunda vez ya no es un reintento: es capacidad.
    let segunda = e.decidir(FailureClass::Formato);
    match segunda {
        Accion::NuevoPlan { subir_tier: true, .. } => {}
        Accion::Reintentar { .. } => panic!("repitió la clase y sigue reintentando"),
        otro => panic!("{otro:?}"),
    }
}

#[test]
fn environment_cambia_config_y_no_suba_de_tier_a_ciegas() {
    let mut e = Escalador::nuevo(2);
    match e.decidir(FailureClass::Entorno) {
        Accion::NuevoPlan { subir_tier, .. } => assert!(!subir_tier, "lo primero es bajar el ctx"),
        otro => panic!("{otro:?}"),
    }
}

#[test]
fn verificacion_que_no_pasa_no_se_reintenta_infinitamente() {
    let mut e = Escalador::nuevo(3);
    match e.decidir(FailureClass::Verificacion) {
        Accion::Abortar { porque } => assert!(!porque.is_empty()),
        otro => panic!("un fallo de verificación no se reintenta a ciegas: {otro:?}"),
    }
}

#[test]
fn agotado_el_presupuesto_de_reintentos_se_aborta_con_motivo() {
    // `max_reintentos` es el techo de la corrida entera, no por clase: con uno
    // concedido, la segunda clase que sea ya se aborta.
    let mut e = Escalador::nuevo(1);
    assert!(matches!(e.decidir(FailureClass::Formato), Accion::Reintentar { .. }));
    match e.decidir(FailureClass::Contexto) {
        Accion::Abortar { porque } => assert!(!porque.is_empty(), "se dice por qué se para"),
        otro => panic!("se pasó del techo de reintentos: {otro:?}"),
    }
    assert_eq!(e.usados(), 1);
    assert_eq!(e.restantes(), 0);

    // Con dos, la segunda clase aún tiene su reintento y la tercera se aborta.
    let mut dos = Escalador::nuevo(2);
    assert!(matches!(dos.decidir(FailureClass::Tool), Accion::Reintentar { .. }));
    assert!(matches!(dos.decidir(FailureClass::Formato), Accion::Reintentar { .. }));
    assert!(matches!(dos.decidir(FailureClass::Tool), Accion::Abortar { .. }));
}

#[derive(Default)]
struct RecogeEventos(Mutex<Vec<String>>);

impl Emitidor for RecogeEventos {
    fn emitir(&self, e: BrainEvent) {
        let nombre = match &e {
            BrainEvent::PlanCreated(_) => "plan",
            BrainEvent::ModeloElegido { .. } => "modelo",
            BrainEvent::Token(_) => "token",
            BrainEvent::Reasoning(_) => "razonamiento",
            BrainEvent::StreamRetracted { .. } => "retractado",
            BrainEvent::ToolLlamada { .. } => "tool",
            BrainEvent::ToolDenegada { .. } => "denegada",
            BrainEvent::Verificacion { .. } => "verificacion",
            BrainEvent::Reintento { .. } => "reintento",
            BrainEvent::Escalada { .. } => "escalada",
            BrainEvent::Cancelado => "cancelado",
            BrainEvent::Completado(_) => "completado",
        };
        self.0.lock().unwrap().push(nombre.into());
    }
}

impl RecogeEventos {
    fn tiene(&self, nombre: &str) -> bool {
        self.0.lock().unwrap().iter().any(|x| x == nombre)
    }
}

fn cerebro(mock: Arc<MockProvider>) -> Brain {
    // Con las reglas y el catálogo versionados: sin ellas «corrige src/main.rs»
    // cae a N1 por heurística y el test dejaría de mirar lo que mira.
    let cfg = hatboo_brain::config::loader::Cargada::leer(Some(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config"),
    ))
    .unwrap();
    Brain::nuevo(Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo("nano:0.8b", 1), modelo("grande:9b", 3)]),
        reglas: cfg.reglas.clone(),
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(SondaFija {
            libre: Some(8000),
            ..Default::default()
        }),
        ..Montaje::de_proveedor(mock)
    })
    .unwrap()
}

#[tokio::test]
async fn una_falla_del_proveedor_se_recupera_o_se_dice_como_es() {
    let mut crudo = MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama");
    // Primero cae el entorno; el reintento necesitaría una segunda respuesta.
    crudo.falla_con(ProviderError::Transporte("socket closed".into()));
    let mock = Arc::new(crudo);
    let brain = cerebro(mock.clone());
    let r = brain
        .run(&BrainRequest::nuevo("hatboo", "chat", "cuéntame qué es un mutex"))
        .await;
    match r {
        // `Err` solo si no hay salida que mostrar.
        Err(e) => {
            let s = format!("{e:?}");
            assert!(s.contains("Transporte") || s.contains("Entorno"), "{s}");
        }
        Ok(res) => {
            assert!(
                res.metrics.clase_fallo == Some(FailureClass::Entorno)
                    || !res.output.texto.is_empty(),
                "{:?}",
                res.metrics
            );
        }
    }
}

#[tokio::test]
async fn cancelar_a_mitad_devuelve_cancelled_y_su_evento() {
    let mock = Arc::new(MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama"));
    mock.responde_texto("hola que tal");
    let brain = cerebro(mock.clone());
    let token = CancelToken::nuevo();
    token.cancelar();
    let eventos = Arc::new(RecogeEventos::default());
    let opts = OpcionesDeCorrida::nueva()
        .con_cancelacion(token.clone())
        .con_eventos(eventos.clone());
    let e = brain
        .run_with(
            &BrainRequest::nuevo("hatboo", "chat", "explícame un mutex"),
            opts,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, BrainError::Cancelled), "{e:?}");
    assert!(!e.recuperable(), "cancelar no es un fallo del que reintentar");
    assert!(eventos.tiene("cancelado"), "{:?}", eventos.0.lock().unwrap());
    // Cancelado antes de salir, no se llamó al proveedor.
    assert_eq!(mock.n_peticiones(), 0);
}

#[tokio::test]
async fn la_corrida_normal_emite_plan_verificacion_y_completado() {
    let mock = Arc::new(MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama"));
    mock.responde_texto("un mutex protege datos compartidos");
    let brain = cerebro(mock.clone());
    let eventos = Arc::new(RecogeEventos::default());
    let r = brain
        .run_with(
            &BrainRequest::nuevo("hatboo", "chat", "explícame con detalle qué es un mutex"),
            OpcionesDeCorrida::nueva().con_eventos(eventos.clone()),
        )
        .await
        .unwrap();
    assert!(eventos.tiene("plan"), "{:?}", eventos.0.lock().unwrap());
    assert!(eventos.tiene("completado"));
    assert!(!eventos.tiene("escalada"), "sin causa no se escala");
    assert_eq!(r.plan.model, "nano:0.8b");
}

#[tokio::test]
async fn escalar_por_capacidad_cambia_de_plan_no_de_ilusion() {
    // El modelo no devolvió la tool que el contrato pedía: clase `tool`; si se
    // repite, toca plan nuevo con otro tier.
    let mock = Arc::new(MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama"));
    mock.responde_texto("te lo cuento en prosa en vez de llamar a la tool");
    let brain = cerebro(mock.clone());
    let req = BrainRequest::nuevo(
        "hatboo",
        "work",
        "Corrige el error de compilación de src/main.rs",
    );
    let r = brain.run(&req).await;
    // Que salga o no, el resultado dice qué pasó: sin «listo» falso.
    if let Ok(res) = r {
        assert_ne!(
            res.output.status,
            hatboo_brain::api::response::OutputStatus::Verificado,
            "{:?}",
            res.output
        );
    }
}
