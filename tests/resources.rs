//! Recursos (§10): no cargar nada sin margen, y el fallback con `reason` a la
//! vista. La RAM que manda es la libre del sistema, medida por la sonda que
//! inyecta el producto — Fase 0 dejó demostrado que el `size` de Ollama se queda
//! corto (878 MB declarados contra 1,92 GB que se fueron de la máquina).

use hatboo_brain::api::error::BrainError;
use hatboo_brain::api::request::BrainRequest;
use hatboo_brain::api::vocab::{
    Confidence, ExecutionPolicy, ExecutionTarget, Intent, Level, ModelTarget, OutputContract, Risk,
};
use hatboo_brain::decision::DecisionResult;
use hatboo_brain::decision::DecisionResult as Lote;
use hatboo_brain::models::{selector::elegir, ModelInfo, ModelKind, ModeloCargado, Registry};
use hatboo_brain::resources::{Governor, GovernorConfig, SondaFija};
use std::collections::BTreeMap;

fn modelo(id: &str, tier: u8, ram: u64, local: bool, tools: bool) -> ModelInfo {
    let mut m = BTreeMap::new();
    m.insert(2048u32, ram);
    m.insert(4096u32, ram + 200);
    m.insert(8192u32, ram + 800);
    ModelInfo {
        id: id.into(),
        provider: if local { "ollama".into() } else { "openai".into() },
        local,
        kind: ModelKind::Generativo,
        profile: if ram > 3000 { hatboo_brain::api::vocab::Profile::Small } else { hatboo_brain::api::vocab::Profile::Nano },
        tier,
        ram_mb_by_ctx: m,
        max_ctx: 32768,
        strengths: vec![],
        supports_tools: tools,
        supports_thinking: false,
        supports_vision: false,
        structured_output: false,
        disco_mb: Some(ram),
    }
}

fn lote(nivel: Level, tools: Vec<String>) -> Lote {
    DecisionResult {
        intent: Intent::Ask,
        level: nivel,
        risk: Risk::Low,
        skip_generative: false,
        execution_target: ExecutionTarget::Local,
        model_target: ModelTarget::Indistinto,
        tools,
        output_contract: OutputContract::Texto,
        verification: nivel.verificacion_minima(),
        confidence: Confidence(1.0),
        source: hatboo_brain::api::vocab::DecisionSource::Reglas,
        por_que: "prueba".into(),
        salida_directa: None,
        lectura_directa: None,
    }
}

fn gov() -> Governor {
    Governor::nuevo(GovernorConfig::default())
}

#[test]
fn sin_medicion_no_se_afirma_que_cabe() {
    let mut sin_datos = modelo("nuevo:1b", 1, 900, true, false);
    sin_datos.ram_mb_by_ctx.clear();
    sin_datos.disco_mb = None;
    let consejo = gov().aconsejar(&SondaFija::default(), &sin_datos, Level::N1);
    assert!(!consejo.cabe, "no se puede afirmar que cabe algo sin medirlo");
    assert!(consejo.porque.contains("no se midió") || consejo.porque.contains("medida"), "{}", consejo.porque);
}

#[test]
fn sin_margen_no_se_carga_nada() {
    // 1.200 MB libres contra un modelo de 900 + margen de 1.500: no cabe.
    let sonda = SondaFija {
        libre: Some(1200),
        ..Default::default()
    };
    let consejo = gov().aconsejar(&sonda, &modelo("nano:1b", 1, 900, true, false), Level::N1);
    assert!(!consejo.cabe, "{:?}", consejo);
    assert!(consejo.ram_mb.is_some());
}

#[test]
fn el_ctx_mas_pequeno_que_suficiente_es_el_que_se_pide() {
    let consejo = gov().aconsejar(&SondaFija::default(), &modelo("nano:1b", 1, 900, true, false), Level::N1);
    assert!(consejo.cabe, "{:?}", consejo);
    assert_eq!(consejo.num_ctx, 2048, "N1 no pide 8192: §4");
    let n3 = gov().aconsejar(&SondaFija::default(), &modelo("nano:1b", 1, 900, true, false), Level::N3);
    assert_eq!(n3.num_ctx, 8192, "N3 sí: {:?}", n3);
}

#[test]
fn cambiar_num_ctx_cuesta_una_recarga_y_queda_dicho() {
    let m = modelo("nano:1b", 1, 900, true, false);
    // Residente a 2048: no hay recarga.
    let residente = SondaFija {
        cargados: vec![ModeloCargado { id: "nano:1b".into(), ram_mb: 900, num_ctx: 2048 }],
        ..Default::default()
    };
    let igual = gov().aconsejar(&residente, &m, Level::N1);
    assert_eq!(igual.recarga_ms, Some(0), "{:?}", igual);
    // Y a 4096 hay que recargar: se dice, no se esconde.
    let arriba = gov().aconsejar(&residente, &m, Level::N2);
    assert!(
        arriba.recarga_ms.map(|t| t > 0).unwrap_or(false),
        "{:?}",
        arriba
    );
}

#[test]
fn el_pedido_se_respeta_si_cabe_y_se_dice_por_que() {
    let reg = Registry::nuevo(vec![
        modelo("grande:9b", 3, 6000, true, true),
        modelo("nano:0.8b", 1, 900, true, false),
        modelo("medio:4b", 2, 3000, true, true),
    ]);
    let req = BrainRequest::nuevo("hatboo", "work", "usa las tools").con_modelo("medio:4b");
    let e = elegir(
        &req,
        &lote(Level::N2, vec!["read_file".into()]),
        &reg,
        &gov(),
        &SondaFija::default(),
        false,
    )
    .unwrap();
    assert_eq!(e.modelo.id, "medio:4b");
    assert!(e.porque.contains("el que elegiste"), "{}", e.porque);
}

#[test]
fn si_no_cabe_el_pedido_baja_al_siguiente_y_lo_explica() {
    let reg = Registry::nuevo(vec![
        modelo("grande:9b", 3, 6000, true, true),
        modelo("medio:4b", 2, 3000, true, true),
    ]);
    let req = BrainRequest::nuevo("hatboo", "work", "usa las tools").con_modelo("grande:9b");
    let sonda = SondaFija {
        // 5.000 libres: al de 9B (6.000 + 200) le falta margen y al de 4B
        // (3.000 + 200 + 1.500 de margen) sí le sobra.
        libre: Some(5000),
        ..Default::default()
    };
    let e = elegir(
        &req,
        &lote(Level::N2, vec!["read_file".into()]),
        &reg,
        &gov(),
        &sonda,
        false,
    )
    .unwrap();
    assert_eq!(e.modelo.id, "medio:4b", "nunca un selector silencioso");
    assert!(e.porque.contains("no cabe"), "{}", e.porque);
    assert!(e.descartados.iter().any(|(id, _)| id == "grande:9b"));
}

#[test]
fn nada_cabe_dan_las_cifras_y_es_recuperable() {
    let reg = Registry::nuevo(vec![modelo("nano:0.8b", 1, 900, true, false)]);
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("nano:0.8b");
    let sonda = SondaFija {
        libre: Some(300),
        ..Default::default()
    };
    let err = elegir(&req, &lote(Level::N0, vec![]), &reg, &gov(), &sonda, false).unwrap_err();
    match err {
        BrainError::ResourceExhausted { needed_mb, free_mb } => {
            // 900 del modelo + 1500 de margen del Governor: lo que se dice es lo
            // que hizo falta, no lo que pesa el modelo a secas.
            assert_eq!(needed_mb, 2400);
            assert_eq!(free_mb, 300);
        }
        otro => panic!("{otro:?}"),
    }
    assert!(err.recuperable());
}

/// El margen de la spec son 1500 MB, pero el comprobador lo estrecha a t/8 cuando
/// conoce el total. El aviso de «no cabe» tiene que decir el número con el que
/// rechazó, no el de la spec: si no, explica un rechazo que no hizo.
#[test]
fn el_aviso_de_no_cabe_dice_el_margen_con_el_que_rechazo() {
    let reg = Registry::nuevo(vec![modelo("nano:0.8b", 1, 900, true, false)]);
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("nano:0.8b");
    let sonda = SondaFija {
        libre: Some(1800),
        total: Some(8450),
        ..Default::default()
    };
    // Con el margen estrechado (8450/8 = 1056) 900 + 1056 = 1956 > 1800: no cabe,
    // y el motivo tiene que hablar de 1056.
    let err = elegir(&req, &lote(Level::N0, vec![]), &reg, &gov(), &sonda, false).unwrap_err();
    let s = err.mensaje();
    assert!(s.contains("1056") || s.contains("1956"), "{s}");
    let consejo = gov().aconsejar(&sonda, &modelo("nano:0.8b", 1, 900, true, false), Level::N0);
    assert!(!consejo.cabe, "{:?}", consejo);
    assert!(
        consejo.porque.contains("1056"),
        "el consejo dice el margen de la spec, no el que usó: {}",
        consejo.porque
    );
    assert!(
        consejo.porque.contains("octava parte"),
        "{}",
        consejo.porque
    );
}

#[test]
fn local_only_nunca_propone_una_api() {
    let reg = Registry::nuevo(vec![
        modelo("nano:0.8b", 1, 900, true, false),
        modelo("nube:gpt", 2, 0, false, true),
    ]);
    let req = BrainRequest::nuevo("hatboo", "work", "usa tools")
        .con_policy(ExecutionPolicy::LocalOnly);
    // El único con tools es la nube: con local_only tiene que quedarse sin nada.
    let err = elegir(
        &req,
        &lote(Level::N2, vec!["read_file".into()]),
        &reg,
        &gov(),
        &SondaFija::default(),
        false,
    )
    .unwrap_err();
    assert!(matches!(err, BrainError::NoEligibleModel), "{err:?}");
    assert!(!format!("{err:?}").contains("nube"), "local_only no nombra la nube");
}

#[test]
fn cloud_only_no_usa_los_locales() {
    let reg = Registry::nuevo(vec![
        modelo("nano:0.8b", 1, 900, true, true),
        modelo("nube:gpt", 2, 0, false, true),
    ]);
    let req =
        BrainRequest::nuevo("hatboo", "work", "usa tools").con_policy(ExecutionPolicy::CloudOnly);
    let e = elegir(
        &req,
        &lote(Level::N2, vec!["read_file".into()]),
        &reg,
        &gov(),
        &SondaFija::default(),
        false,
    );
    match e {
        Ok(el) => assert_eq!(el.modelo.id, "nube:gpt"),
        // Si la policy y el Governor no cuadran, se dice con un error tipado.
        Err(BrainError::NoEligibleModel) => {}
        Err(otro) => panic!("{otro:?}"),
    }
}

#[test]
fn la_presion_de_bateria_recorta_un_escalon() {
    let m = modelo("nano:1b", 1, 900, true, false);
    let normal = gov().aconsejar(&SondaFija::default(), &m, Level::N3);
    let con_bateria = SondaFija {
        bateria: Some(0.15),
        ..Default::default()
    };
    let flojo = Governor::nuevo(GovernorConfig {
        perfil_ligero_presion: true,
        ..Default::default()
    })
    .aconsejar(&con_bateria, &m, Level::N3);
    assert!(normal.num_ctx >= flojo.num_ctx, "{normal:?} vs {flojo:?}");
    assert!(flojo.presion, "{flojo:?}");
}

/// Los dos de la máquina de referencia (8,45 GB) y las cifras medidas el 03-10:
/// `qwen3.5:0.8b` residente 1063 MB a 2048 y `gemma3:1b` 878 MB. Con 2000 MB
/// libres y el margen estrechado a la octava parte del total (1056) el primero no
/// cabe y el segundo sí: es exactamente el caso en el que el crate decidía solo.
fn dos_modelos() -> Registry {
    Registry::nuevo(vec![
        modelo("qwen3.5:0.8b", 1, 1063, true, false),
        modelo("gemma3:1b", 1, 878, true, false),
    ])
}

fn sonda_de_dos_mil() -> SondaFija {
    SondaFija {
        libre: Some(2000),
        total: Some(8450),
        ..Default::default()
    }
}

/// La mitad apagada del interruptor: sin `respetarModelo` la conducta medida de
/// siempre no cambia para nadie, ni el aviso de desvío.
#[test]
fn sin_respetar_modelo_el_pedido_no_cabe_y_se_desvia_a_otro() {
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("qwen3.5:0.8b");
    let e = elegir(
        &req,
        &lote(Level::N0, vec![]),
        &dos_modelos(),
        &gov(),
        &sonda_de_dos_mil(),
        false,
    )
    .unwrap();
    assert_eq!(e.modelo.id, "gemma3:1b", "sin la opción se sigue desviando");
    assert!(
        e.porque.contains("«qwen3.5:0.8b» no cabe ahora; sigue «gemma3:1b»"),
        "{}",
        e.porque
    );
    assert!(e.descartados.iter().any(|(id, _)| id == "qwen3.5:0.8b"));
}

/// La mitad encendida: nada de firmar otro modelo. El error tiene que nombrar el
/// modelo del producto y traer las tres cifras con las que el Governor lo rechazó
/// (lo que pesa, el margen con el que comprobó y lo que quedaba libre), porque sin
/// números el aviso no se puede ni comprobar ni actuar.
#[test]
fn respetar_modelo_niega_la_sustitucion_y_dice_las_cifras() {
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("qwen3.5:0.8b");
    let err = elegir(
        &req,
        &lote(Level::N0, vec![]),
        &dos_modelos(),
        &gov(),
        &sonda_de_dos_mil(),
        true,
    )
    .unwrap_err();
    match &err {
        BrainError::ModeloPedidoNoCorre {
            modelo,
            needed_mb,
            margen_mb,
            free_mb,
            ..
        } => {
            assert_eq!(modelo, "qwen3.5:0.8b");
            // 1063 del modelo + 1056 de margen sobre 2000 libres: la suma tiene
            // que cuadrar, es la aritmética del rechazo.
            assert_eq!(*needed_mb, 2119, "{err:?}");
            assert_eq!(*margen_mb, 1056, "{err:?}");
            assert_eq!(*free_mb, 2000, "{err:?}");
        }
        otro => panic!("tenía que negarse el pedido, salió {otro:?}"),
    }
    let s = err.mensaje();
    assert!(s.contains("qwen3.5:0.8b"), "{s}");
    assert!(s.contains("1063") && s.contains("1056") && s.contains("2000"), "{s}");
    assert!(
        !s.contains("gemma3"),
        "no se propone el modelo que nadie pidió: {s}"
    );
    // Y no es recuperable: «recuperable» es poder continuar con **otro** modelo.
    assert!(!err.recuperable(), "{err:?}");
}

/// Un modelo que cabe pero no cumple lo que pide el Plan también se niega, y se dice
/// por qué: con `respetarModelo` no hay sustitución ni por capacidad.
#[test]
fn respetar_modelo_también_niega_el_que_no_cumple_lo_del_plan() {
    let reg = Registry::nuevo(vec![
        modelo("gemma3:1b", 1, 878, true, false),
        modelo("qwen3:1.7b", 2, 1645, true, true),
    ]);
    // Se pide `gemma3:1b` para un N2 con tools: cabe (878 + 1056 ≤ 8000) pero no
    // las declara, así que antes el Selector lo saltaba a `qwen3:1.7b`.
    let req = BrainRequest::nuevo("hatboo", "work", "usa las tools").con_modelo("gemma3:1b");
    let err = elegir(
        &req,
        &lote(Level::N2, vec!["read_file".into()]),
        &reg,
        &gov(),
        &SondaFija::default(),
        true,
    )
    .unwrap_err();
    match &err {
        BrainError::ModeloPedidoNoCorre { modelo, motivo, .. } => {
            assert_eq!(modelo, "gemma3:1b");
            assert!(motivo.contains("tools"), "{motivo}");
        }
        otro => panic!("tenía que negarse el pedido, salió {otro:?}"),
    }
    // Apagada la opción, la misma corrida firma el de tools: es la conducta de hoy.
    let e = elegir(
        &req,
        &lote(Level::N2, vec!["read_file".into()]),
        &reg,
        &gov(),
        &SondaFija::default(),
        false,
    )
    .unwrap();
    assert_eq!(e.modelo.id, "qwen3:1.7b");
}

/// Lo que de verdad le importa al usuario: con la opción encendida **no se llama a
/// ningún modelo**. Medido en Hatboo el 04-10, desviarse costaba 34,7 s y 3 tokens
/// en un «Hola.» porque había que cargar el modelo que nadie pidió; contar las
/// peticiones al proveedor es la única prueba de que eso ya no pasa.
#[tokio::test]
async fn respetar_modelo_no_carga_ningun_modelo_en_la_corrida() {
    let modelos = vec![
        modelo("qwen3.5:0.8b", 1, 1063, true, false),
        modelo("gemma3:1b", 1, 878, true, false),
    ];
    // Regla de un solo tiro, como en `tests/planner.rs`: el nivel sale de aquí por
    // construcción y no de lo que diga hoy el vocabulario versionado.
    let req = BrainRequest::nuevo("hatboo", "chat", "¿y ahora qué?").con_modelo("qwen3.5:0.8b");

    // Apagado: la conducta de siempre. La corrida termina y el modelo que responde
    // no es el que eligió él, y eso pasó por una carga que nadie pidió.
    let (sin, mock_sin) = cerebro(&modelos, false);
    let r = sin.run(&req).await.expect("sin la opción la corrida sigue");
    assert_eq!(r.plan.model, "gemma3:1b", "{:?}", r.plan);
    assert_eq!(mock_sin.n_peticiones(), 1, "se cargó otro modelo");

    // Encendido: nada corre, y se dice con las cifras del Governor.
    let (con, mock_con) = cerebro(&modelos, true);
    let e = match con.run(&req).await {
        Ok(r) => panic!("con respetarModelo no se firma otro modelo: {:?}", r.plan),
        Err(e) => e,
    };
    assert!(matches!(e, BrainError::ModeloPedidoNoCorre { .. }), "{e:?}");
    assert!(e.mensaje().contains("qwen3.5:0.8b"), "{}", e.mensaje());
    assert_eq!(
        mock_con.n_peticiones(),
        0,
        "con la opción puesta no se le mandó ni una petición al proveedor"
    );
}

/// Un `Brain` con los dos modelos de arriba y `respetarModelo` en el valor que se
/// prueba. Devuelve también el `MockProvider` para poder contar los modelos que de
/// verdad se cargaron.
fn cerebro(
    modelos: &[ModelInfo],
    respetar: bool,
) -> (hatboo_brain::brain::Brain, std::sync::Arc<hatboo_brain::providers::MockProvider>) {
    let mock = std::sync::Arc::new(
        hatboo_brain::providers::MockProvider::nuevo(modelos.to_vec()).con_nombre("ollama"),
    );
    mock.responde_texto("hola");
    let reglas = hatboo_brain::decision::rules::Reglas::desde_json(
        r#"{"version":1,"reglas":[{"id":"corto-a-n0",
             "cuando":[{"senal":"message_length","op":"<=","valor":60}],
             "entonces":{"intent":"ask","level":"N0","contrato":"texto"}}]}"#,
    )
    .unwrap();
    let montaje = hatboo_brain::brain::Montaje {
        config: hatboo_brain::config::schema::BrainConfig {
            respetar_modelo: respetar,
            ..Default::default()
        },
        proveedores: vec![mock.clone()],
        registry: Registry::nuevo(modelos.to_vec()),
        reglas,
        sonda: std::sync::Arc::new(sonda_de_dos_mil()),
        ..hatboo_brain::brain::Montaje::de_proveedor(mock.clone())
    };
    (
        hatboo_brain::brain::Brain::nuevo(montaje).unwrap(),
        mock,
    )
}
