//! Recursos (§10): apretar la ventana antes que negar el turno, y el fallback con
//! `reason` a la vista. La RAM que manda es la libre del sistema, medida por la
//! sonda que inyecta el producto — Fase 0 dejó demostrado que el `size` de Ollama
//! se queda corto (878 MB declarados contra 1,92 GB que se fueron de la máquina).

use hatboo_brain::api::error::BrainError;
use hatboo_brain::api::request::BrainRequest;
use hatboo_brain::api::vocab::{
    Confidence, ExecutionPolicy, ExecutionTarget, Intent, Level, ModelTarget, OutputContract, Risk,
};
use hatboo_brain::decision::DecisionResult;
use hatboo_brain::decision::DecisionResult as Lote;
use hatboo_brain::models::{selector::elegir, ModelInfo, ModelKind, ModeloCargado, Registry};
use hatboo_brain::resources::{Ajuste, Governor, GovernorConfig, SondaFija};
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
fn sin_margen_se_corre_en_el_peldano_mas_barato_que_cabe() {
    // 1.200 MB libres contra un modelo de 900 + margen de 1.500: con margen no cuadra
    // y el turno NO se niega. Se corre a 2048 —lo que pide el nivel— apurando el
    // margen, porque 900 MB sí caben en los 1200 que hay.
    let sonda = SondaFija {
        libre: Some(1200),
        ..Default::default()
    };
    let consejo = gov().aconsejar(&sonda, &modelo("nano:1b", 1, 900, true, false), Level::N1);
    assert!(consejo.cabe, "{:?}", consejo);
    assert_eq!(consejo.ajuste, Ajuste::SinMargen);
    assert_eq!(consejo.num_ctx, 2048, "la ventana del nivel se conserva");
    assert_eq!(consejo.ram_mb, Some(900));
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
    assert!(e.porque.contains("Elegiste tú este modelo"), "{}", e.porque);
}

#[test]
fn si_no_cabe_con_margen_el_pedido_se_corre_apurado() {
    let reg = Registry::nuevo(vec![
        modelo("grande:9b", 3, 6000, true, true),
        modelo("medio:4b", 2, 3000, true, true),
    ]);
    let req = BrainRequest::nuevo("hatboo", "work", "usa las tools").con_modelo("grande:9b");
    let sonda = SondaFija {
        // 5.000 libres: al de 9B (6.000 + 1.500 de margen) le falta, y al de 4B le
        // sobra con el escalón más barato del nivel.
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
    // Desde el 05-10 la RAM no es una puerta: el pedido se corre con la ventana
    // apretada en vez de irse a otro modelo. El desvío se queda para lo que no
    // puede cumplirse de otra manera (tools, policy, un techo por debajo del suelo).
    assert_eq!(e.modelo.id, "grande:9b", "{}", e.porque);
    assert_eq!(e.consejo.ajuste, Ajuste::SinMargen);
    assert_eq!(e.consejo.num_ctx, 1024, "N2 no firma por debajo de su suelo");
    assert!(e.porque.contains("ningún peldaño cabe con margen"), "{}", e.porque);
}

#[test]
fn la_maquina_sin_sitio_no_devuelve_un_error() {
    let reg = Registry::nuevo(vec![modelo("nano:0.8b", 1, 900, true, false)]);
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("nano:0.8b");
    let sonda = SondaFija {
        libre: Some(300),
        ..Default::default()
    };
    let e = elegir(&req, &lote(Level::N0, vec![]), &reg, &gov(), &sonda, false).unwrap();
    assert_eq!(e.consejo.ajuste, Ajuste::SinMargen);
    assert_eq!(e.consejo.num_ctx, 512);
    // 900 del modelo + 1500 de margen contra 300 libres: la cuenta se dice tal cual.
    let p = &e.consejo.porque;
    assert!(p.contains("900") && p.contains("300") && p.contains("1500"), "{p}");
}

/// El margen de la spec son 1500 MB, pero el comprobador lo estrecha a t/8 cuando
/// conoce el total. El aviso tiene que decir el número con el que comprobó, no el
/// de la spec: si no, explica una cuenta que no hizo.
#[test]
fn el_aviso_apurado_dice_el_margen_con_el_que_comprobó() {
    let reg = Registry::nuevo(vec![modelo("nano:0.8b", 1, 900, true, false)]);
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("nano:0.8b");
    let sonda = SondaFija {
        libre: Some(1800),
        total: Some(8450),
        ..Default::default()
    };
    // Con el margen estrechado (8450/8 = 1056) 900 + 1056 = 1956 > 1800: no cuadra
    // y el aviso tiene que hablar de 1056, no de los 1500 de la spec.
    let e = elegir(&req, &lote(Level::N0, vec![]), &reg, &gov(), &sonda, false).unwrap();
    assert_eq!(e.consejo.ajuste, Ajuste::SinMargen);
    let s = &e.consejo.porque;
    assert!(s.contains("1056"), "el consejo dice el margen de la spec: {s}");
    assert!(s.contains("octava parte"), "{s}");
    assert_eq!(e.consejo.margen_mb, 1056);
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
/// cuadra **con margen** y el segundo sí: era el caso en el que el crate decidía
/// solo, y ahora es el caso en el que los dos se corren.
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

/// La mitad apagada del interruptor ya no sirve para desviar por RAM: con 2000
/// libres `qwen3.5:0.8b` ocupa 1063, así que se corre el que pidió el usuario y el
/// desvío lo deja para lo que no se puede arreglar apretando (tools, policy).
#[test]
fn la_ram_apretada_no_desvia_a_otro_modelo() {
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
    assert_eq!(e.modelo.id, "qwen3.5:0.8b", "{}", e.porque);
    assert_eq!(e.consejo.num_ctx, 2048, "conserva la ventana del nivel");
    assert_eq!(e.consejo.ajuste, Ajuste::SinMargen);
    assert!(e.descartados.is_empty(), "{:?}", e.descartados);
}

/// La mitad encendida: nada de firmar otro modelo. El caso que el usuario vio el
/// 04-10 era un rechazo con cifras; desde que la RAM dejó de ser una puerta el
/// pedido se corre y lo que se registra es cuánto margen se dispuso. Las cifras
/// siguen en el aviso, que es lo que permite ir a Ajustes a comprobarlas.
#[test]
fn respetar_modelo_corre_el_pedido_y_deja_las_cifras_en_el_aviso() {
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("qwen3.5:0.8b");
    let e = elegir(
        &req,
        &lote(Level::N0, vec![]),
        &dos_modelos(),
        &gov(),
        &sonda_de_dos_mil(),
        true,
    )
    .unwrap();
    assert_eq!(e.modelo.id, "qwen3.5:0.8b");
    assert!(e.porque.contains("Elegiste tú este modelo"), "{}", e.porque);
    let p = &e.consejo.porque;
    assert!(p.contains("1063") && p.contains("1056") && p.contains("2000"), "{p}");
    assert!(!p.contains("gemma3"), "no se nombra al modelo que nadie pidió: {p}");
}

/// Y cuando de verdad no hay forma de cuadrar la cuenta —ni apurando el margen ni
/// apretando la ventana—, `respetarModelo` se niega a firmar OTRO modelo y lo dice
/// con las cifras: es la única puerta que queda y sigue siendo suya, no del Governor.
#[test]
fn respetar_modelo_sigue_negando_la_sustitucion_sin_datos() {
    let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("qwen3.5:0.8b");
    // Sonda ciega: el Governor no puede afirmar que nada quepa, y con la opción
    // puesta no se sustituye al que el usuario eligió.
    let err = elegir(
        &req,
        &lote(Level::N0, vec![]),
        &dos_modelos(),
        &gov(),
        &hatboo_brain::resources::SinSonda,
        true,
    )
    .unwrap_err();
    match &err {
        BrainError::ModeloPedidoNoCorre { modelo, .. } => assert_eq!(modelo, "qwen3.5:0.8b"),
        otro => panic!("tenía que negarse la sustitución, salió {otro:?}"),
    }
    assert!(err.mensaje().contains("qwen3.5:0.8b"), "{}", err.mensaje());
    assert!(!err.mensaje().contains("gemma3"), "{}", err.mensaje());
    // No es recuperable: «recuperable» es poder continuar con **otro** modelo.
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

/// Lo que de verdad le importa al usuario: que la corrida llame **al modelo que él
/// eligió** y a ningún otro. Medido en Hatboo el 04-10, desviarse costaba 34,7 s y
/// 3 tokens en un «Hola.» porque había que cargar el modelo que nadie pidió; contar
/// las peticiones al proveedor es la única prueba de que eso ya no pasa. Con la RAM
/// dejó de ser una puerta, las dos mitades del interruptor dan el mismo resultado.
#[tokio::test]
async fn la_corrida_llama_al_modelo_elegido_y_a_ningun_otro() {
    let modelos = vec![
        modelo("qwen3.5:0.8b", 1, 1063, true, false),
        modelo("gemma3:1b", 1, 878, true, false),
    ];
    // Regla de un solo tiro, como en `tests/planner.rs`: el nivel sale de aquí por
    // construcción y no de lo que diga hoy el vocabulario versionado.
    let req = BrainRequest::nuevo("hatboo", "chat", "¿y ahora qué?").con_modelo("qwen3.5:0.8b");

    // Apagado: 2000 libres para un modelo de 1063. Antes esto respondía con
    // `gemma3:1b`; hoy corre el pedido con el margen apurado.
    let (sin, mock_sin) = cerebro(&modelos, false);
    let r = sin.run(&req).await.expect("sin la opción la corrida sigue");
    assert_eq!(r.plan.model, "qwen3.5:0.8b", "{:?}", r.plan);
    assert_eq!(r.plan.num_ctx, 2048, "{:?}", r.plan);
    assert_eq!(mock_sin.n_peticiones(), 1, "una sola carga");

    // Encendido: lo mismo, que es justo lo que `respetarModelo` quiere decir.
    let (con, mock_con) = cerebro(&modelos, true);
    let r2 = con.run(&req).await.expect("con la opción también se corre el pedido");
    assert_eq!(r2.plan.model, "qwen3.5:0.8b", "{:?}", r2.plan);
    assert_eq!(mock_con.n_peticiones(), 1, "se cargó otro modelo");
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
