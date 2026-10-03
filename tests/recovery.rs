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
        structured_output: false,
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
            // El stream se cortó: es el entorno, no el modelo. Ver el test de
            // `capacidad_es_lo_que_no_sabe_hacerse` en el lib.
            ProviderError::RespuestaInvalida("json roto".into())
        ))),
        F::Entorno
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
    assert!(
        r.recuperacion.is_empty(),
        "una corrida limpia no tiene nada que contar: {:?}",
        r.recuperacion
    );
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

/// La escalada tiene que ser visible y tener linaje: `BrainEvent::Escalada` (que
/// §7 lista y que no se emitía nunca) y `parent_plan_hash` del plan que falló.
/// Y subir de tier NO es subir de permisos: el plan nuevo sigue con las tools que
/// decidió el Engine, no con todas las que el producto ofrece.
#[tokio::test]
async fn escalar_emite_el_evento_deja_el_linaje_y_no_regala_tools() {
    let cfg = hatboo_brain::config::loader::Cargada::leer(Some(&std::path::Path::new(
        env!("CARGO_MANIFEST_DIR"),
    )
    .join("config")))
    .unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama"));
    // El contrato del plan es `json` y el modelo contesta en prosa: `Fail` de
    // forma. La segunda vez que se repite la clase, §1 dice que es capacidad y hay
    // que subir de tier.
    for _ in 0..6 {
        mock.responde_texto("esto no es json");
    }
    // Reglas de un solo tiro: se fija N2 con contrato JSON sin depender del
    // vocabulario versionado, que daría otro nivel según el mensaje. Hace falta N2
    // porque con los dos reintentos del nivel es como una clase repetida llega a la
    // escalada de tier (un N1 con un solo reintento aborta antes).
    let reglas = hatboo_brain::decision::rules::Reglas::desde_json(
        r#"{"version":1,"reglas":[{"id":"json-a-n2",
             "cuando":[{"senal":"message_length","op":">","valor":10}],
             "entonces":{"intent":"ask","level":"N2","contrato":"json"}}]}"#,
    )
    .unwrap();
    let brain = Brain::nuevo(Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo("nano:0.8b", 1)]),
        reglas,
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(hatboo_brain::resources::SondaFija {
            libre: Some(8000),
            ..Default::default()
        }),
        ..Montaje::de_proveedor(mock.clone())
    })
    .unwrap();
    let eventos = Arc::new(RecogeEventos::default());
    let r = brain
        .run_with(
            &pedido_json(),
            OpcionesDeCorrida::nueva().con_eventos(eventos.clone()),
        )
        .await
        .expect("hay salida que entregar aunque salga rechazada");
    assert_eq!(r.plan.level, hatboo_brain::api::vocab::Level::N3, "{:?}", r.plan);
    assert!(
        eventos.tiene("escalada"),
        "no se emitió Escalada: {:?}",
        eventos.0.lock().unwrap()
    );
    assert!(
        r.plan.parent_plan_hash.is_some(),
        "el plan escalado no dice de cuál viene: {:?}",
        r.plan
    );
    assert!(
        r.plan.escalate_to.is_none() || r.plan.escalate_to.as_deref() != Some(&r.plan.model),
        "la sugerencia de escalada no puede ser el propio modelo"
    );
    // Las tools: la decisión era N1 sin tools; al subir a N2 no se le regalaron las
    // dos del producto.
    assert!(
        r.plan.tools.is_empty(),
        "subir de tier no da permisos: {:?}",
        r.plan.tools
    );
}

/// §5.1 del Canon: la subclase `truncated`. Un JSON cortado por el techo **no**
/// es un fallo de formato: reintentar con el mismo Plan reproduce el corte, y eso
/// son dos llamadas al modelo que no podían salir bien. Aquí se cobra la primera
/// y se para, diciendo por qué.
#[tokio::test]
async fn una_salida_cortada_por_el_techo_no_se_reintenta_igual() {
    let cfg = hatboo_brain::config::loader::Cargada::leer(Some(&std::path::Path::new(
        env!("CARGO_MANIFEST_DIR"),
    )
    .join("config")))
    .unwrap();
    let mock = Arc::new(MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama"));
    // Cuatro salidas encoladas a propósito: si el Brain reintentara, se vería en
    // `n_peticiones`.
    for _ in 0..4 {
        mock.responde(hatboo_brain::providers::GenerationResult {
            texto: r#"{"resumen":"un mutex"#.into(),
            modelo: "nano:0.8b".into(),
            truncado: true,
            ..Default::default()
        });
    }
    let reglas = hatboo_brain::decision::rules::Reglas::desde_json(
        r#"{"version":1,"reglas":[{"id":"json-a-n2",
             "cuando":[{"senal":"message_length","op":">","valor":10}],
             "entonces":{"intent":"ask","level":"N2","contrato":"json"}}]}"#,
    )
    .unwrap();
    let brain = Brain::nuevo(Montaje {
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(vec![modelo("nano:0.8b", 1)]),
        reglas,
        herramientas: cfg.herramientas.clone(),
        sonda: Arc::new(hatboo_brain::resources::SondaFija {
            libre: Some(8000),
            ..Default::default()
        }),
        ..Montaje::de_proveedor(mock.clone())
    })
    .unwrap();
    let r = brain
        .run_with(&pedido_json(), OpcionesDeCorrida::nueva())
        .await
        .expect("hay salida que entregar, aunque esté cortada");
    assert_eq!(
        mock.n_peticiones(),
        1,
        "el techo cortó la salida y aun así se volvió al modelo"
    );
    assert_eq!(r.metrics.clase_fallo, Some(FailureClass::Truncado));
    assert!(!r.es_exito(), "una salida cortada no es un éxito");
    // `Rechazado`, no `SinVerificar`: el contrato `json` se verificó y **falló**
    // porque el texto está cortado. `SinVerificar` es para cuando no había con
    // qué comprobar.
    assert_eq!(
        r.output.status,
        hatboo_brain::api::response::OutputStatus::Rechazado,
        "{:?}",
        r.output.status
    );
}

/// Un pedido de chat con contrato JSON y dos tools ofrecidas por el producto.
fn pedido_json() -> BrainRequest {
    BrainRequest::nuevo(
        "hatboo",
        "chat",
        "devuélveme en json el resumen de qué es un mutex y cuándo evitar uno",
    )
    .con_tools(vec![
        hatboo_brain::api::request::ToolInfo {
            id: "read_file".into(),
            escribe: false,
            descripcion: String::new(),
        },
        hatboo_brain::api::request::ToolInfo {
            id: "write_file".into(),
            escribe: true,
            descripcion: String::new(),
        },
    ])
}

/// La pieza que faltaba para que un producto pueda hacer aprobación interactiva:
/// el crate deja la llamada pendiente y termina, así que lo que pasó después
/// (aprobada, ejecutada, qué salió) tiene que volver a entrar en el prompt. Sin
/// `OpcionesDeCorrida::observaciones` no había por dónde.
#[tokio::test]
async fn una_observacion_del_producto_llega_al_prompt() {
    let mock = Arc::new(MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama"));
    mock.responde_texto("entonces el README ya está escrito");
    let brain = cerebro(mock.clone());
    let opts = OpcionesDeCorrida::nueva().con_observaciones(vec![
        "write_file aprobada por el usuario: 12 líneas en README.md".into(),
    ]);
    let r = brain
        .run_with(
            &BrainRequest::nuevo("hatboo", "work", "escribe un README con el arranque"),
            opts,
        )
        .await;
    assert!(r.is_ok(), "{r:?}");
    let peticion = mock.ultima_peticion().expect("el mock recibió una petición");
    assert!(
        peticion.prompt.contains("write_file aprobada"),
        "la observación no llegó al prompt: {:?}",
        peticion.prompt
    );
}

/// Y al revés: si no se pasan observaciones, no se cuela ninguna.
#[tokio::test]
async fn sin_observaciones_el_prompt_no_lleva_observaciones() {
    let mock = Arc::new(MockProvider::nuevo(vec![modelo("nano:0.8b", 1)]).con_nombre("ollama"));
    mock.responde_texto("va");
    let brain = cerebro(mock.clone());
    let r = brain
        .run(&BrainRequest::nuevo("hatboo", "chat", "qué es un mutex"))
        .await;
    assert!(r.is_ok(), "{r:?}");
    let peticion = mock.ultima_peticion().expect("una petición");
    assert!(!peticion.prompt.contains("observación"), "{:?}", peticion.prompt);
}
