//! Contexto (§10): presupuesto chico y grande, lo que se queda fuera queda
//! registrado, y el orden de prioridad del Canon (§XII).

use hatboo_brain::api::request::{BrainRequest, Message, ProjectContext};
use hatboo_brain::context::{armar, Pieza, Prioridad};
use hatboo_brain::prompt::{ContadorTokens, Estimador};

fn pieza(p: Prioridad, origen: &str, n: usize) -> Pieza {
    Pieza::nueva(p, origen, "x".repeat(n))
}

/// 3,5 caracteres por token, redondeando hacia arriba.
fn tk(s: &str) -> u32 {
    Estimador.cuenta(s)
}

#[test]
fn entra_lo_que_importa_y_se_apunta_lo_que_se_fue() {
    let piezas = vec![
        pieza(Prioridad::Resto, "resto", 400),
        pieza(Prioridad::Objetivo, "objetivo", 70),
        pieza(Prioridad::ArchivoNombrado, "archivo:src/main.rs", 210),
        pieza(Prioridad::Historial, "historial:1", 140),
    ];
    let a = armar(piezas, 100, &Estimador);
    assert_eq!(a.presupuesto, 100);
    assert_eq!(a.tokens, 80, "objetivo (20) + archivo nombrado (60)");
    assert_eq!(a.rechazadas, vec!["historial:1", "resto"], "se corta desde el final de la lista");
    assert_eq!(a.incluidas.len(), 2);
}

#[test]
fn con_presupuesto_grande_no_se_tira_nada() {
    let piezas = vec![
        pieza(Prioridad::Resto, "resto", 400),
        pieza(Prioridad::Objetivo, "objetivo", 70),
        pieza(Prioridad::Pruebas, "tests:1", 210),
    ];
    let total: u32 = piezas.iter().map(|p| tk(&p.texto)).sum();
    let a = armar(piezas, total + 10, &Estimador);
    assert!(a.rechazadas.is_empty(), "{:?}", a.rechazadas);
    assert_eq!(a.tokens, total);
    assert!(a.incluidas.iter().any(|p| p.prioridad == Prioridad::Resto));
}

#[test]
fn una_pieza_no_se_parte_por_la_mitad() {
    // 60 tokens de presupuesto y una pieza de 80: no entra "media pieza", porque
    // a medias no se puede leer un archivo.
    let a = armar(vec![pieza(Prioridad::ArchivoNombrado, "a", 280)], 60, &Estimador);
    assert!(a.incluidas.is_empty());
    assert_eq!(a.tokens, 0);
    assert_eq!(a.rechazadas, vec!["a"]);
}

#[test]
fn la_seguridad_en_aunque_desborde_y_se_dice() {
    let a = armar(
        vec![
            pieza(Prioridad::Objetivo, "objetivo", 35),
            pieza(Prioridad::Seguridad, "permisos", 7000),
        ],
        20,
        &Estimador,
    );
    assert!(a.tokens > 20, "la seguridad no se deja fuera");
    assert!(
        a.rechazadas.iter().any(|r| r.starts_with("desbordado:permisos")),
        "{:?}",
        a.rechazadas
    );
    // Y el presupuesto declarado sigue siendo el que se pidió: el desborde se ve.
    assert_eq!(a.presupuesto, 20);
}

#[test]
fn la_prioridad_manda_sobre_el_orden_de_llegada() {
    let piezas = vec![
        pieza(Prioridad::Historial, "h", 140),
        pieza(Prioridad::Pruebas, "t", 140),
        pieza(Prioridad::Objetivo, "o", 140),
    ];
    let a = armar(piezas, 45, &Estimador);
    assert_eq!(a.incluidas.len(), 1);
    assert_eq!(a.incluidas[0].origen, "o");
    // Ordenadas por prioridad, pruebas (3) se descarta antes que historial (4):
    // el descarte sigue el mismo orden que la entrada.
    assert_eq!(a.rechazadas, vec!["t", "h"]);
}

#[test]
fn el_material_de_terceros_envuelve_en_datos_y_lo_otro_no() {
    let piezas = vec![
        Pieza::nueva(Prioridad::Objetivo, "objetivo", "haz el trabajo"),
        Pieza::nueva(Prioridad::Resto, "web", "texto de una página"),
    ];
    let a = armar(piezas, 4000, &Estimador);
    let texto = a.texto();
    assert!(texto.contains("haz el trabajo"));
    assert!(
        texto.contains("<datos origen=\"web\""),
        "el material entra como material: {texto}"
    );
    // El objetivo y la seguridad son instrucción; el resto no.
    assert_eq!(texto.matches("<datos").count(), 1, "{texto}");
}

#[test]
fn las_piezas_del_pedido_ponen_el_archivo_nombreado_alto() {
    let mut req = BrainRequest::nuevo("hatboo", "work", "mira src/main.rs");
    req.history = vec![Message::usuario("antes hablamos de esto"), Message::asistente("sí")];
    req.project = Some(ProjectContext {
        root: "C:/proyecto".into(),
        has_tests: false,
        has_lint: false,
        has_build: false,
        language: None,
        hatboo_md: None,
        trust_state: Default::default(),
        verify: Default::default(),
    });
    let v = hatboo_brain::context::sources::piezas_del_pedido(&req, None, Some("src/main.rs"), true);
    assert!(!v.is_empty());
    // El orden ya viene priorizado: lo primero que sale es lo más importante.
    let primera = &v[0];
    assert!(
        matches!(primera.prioridad, Prioridad::Objetivo | Prioridad::ArchivoNombrado),
        "{:?}",
        v.iter().map(|p| (p.prioridad, p.origen.clone())).collect::<Vec<_>>()
    );
    assert!(
        v.iter().any(|p| p.origen.contains("src/main.rs")),
        "{:?}",
        v.iter().map(|p| p.origen.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn sin_fuentes_el_pedido_no_se_queda_sin_contexto_de_seguridad() {
    let req = BrainRequest::nuevo("hatboo", "work", "hola");
    let seg = hatboo_brain::context::sources::piezas_de_seguridad(&req, "approve_for_me", true);
    assert!(
        seg.iter().any(|p| p.prioridad == Prioridad::Seguridad),
        "el approval activo tiene que estar en el prompt, siempre"
    );
    assert!(!armar(seg, 4000, &Estimador).vacio());
}
