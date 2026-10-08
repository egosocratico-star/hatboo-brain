//! De dónde salen las piezas de contexto. El Brain no lee el disco: lo que hace
//! falta del proyecto entra por el `Lector` que pone el producto en el `Montaje`
//! (mismo sandbox, mismos permisos que ya tiene Hatboo) o por la
//! `lectura_directa` de la decisión. Aquí se ensambla, no se lee.

use super::{Pieza, Prioridad};
use crate::api::request::BrainRequest;
use crate::brain::state::EstadoTarea;

/// Piezas que se derivan solo del pedido, sin tocar el disco.
///
/// `puede_actuar` es `!plan.tools.is_empty()`: lo que el turno puede hacer. Un
/// chat no tiene comandos que correr, y contarle el proyecto (raíz, tests, lint,
/// build) era darle tres líneas de material ajeno a la pregunta.
pub fn piezas_del_pedido(
    req: &BrainRequest,
    estado: Option<&EstadoTarea>,
    lectura_directa: Option<&str>,
    puede_actuar: bool,
) -> Vec<Pieza> {
    let mut v = Vec::new();

    if let Some(e) = estado {
        v.push(Pieza::nueva(Prioridad::Objetivo, "estado:tarea", e.al_prompt()));
    }

    // El archivo que nombró el usuario va alto, pero con su límite: si el producto
    // devuelve un fichero de 2 MB, el presupuesto lo corta y se registra.
    let mencionada: Option<String> = lectura_directa
        .map(|s| s.to_string())
        .or_else(|| crate::decision::fast_path::parece_ruta(&req.message));
    if let Some(r) = mencionada {
        let mut pieza = Pieza::nueva(
            Prioridad::ArchivoNombrado,
            format!("mencionado:{r}"),
            format!("El pedido menciona «{r}»."),
        );
        pieza.sensible = parece_secreto(&r);
        v.push(pieza);
    }

    if let Some(p) = &req.project {
        // Solo cuando el turno puede actuar: son los hechos que deciden *qué comando
        // de verificación se corre*, y un chat no corre ninguno.
        if puede_actuar {
            let hechos = format!(
                "Proyecto: {}\ntests: {} · lint: {} · build: {} · lenguaje: {}",
                p.root,
                si_no(p.has_tests),
                si_no(p.has_lint),
                si_no(p.has_build),
                p.language.clone().unwrap_or_else(|| "sin dato".into())
            );
            v.push(Pieza::nueva(Prioridad::Pruebas, "proyecto:hechos", hechos));
            if !p.verify.alguno() {
                v.push(Pieza::nueva(
                    Prioridad::Pruebas,
                    "proyecto:verificacion",
                    "El proyecto no declara comando de verificación: no se puede afirmar que algo compile o pase sin correrlo.",
                ));
            }
        }
    }

    // El historial NO va aquí. Viaja por el canal nativo de `messages` del
    // proveedor, recortado a `plan.historial_turnos` (lo que cabe según §11).
    // Meterlo además en el texto del turno era mandarlo dos veces: una recortada a
    // 700 caracteres y otra entera y sin presupuestar.
    v
}

/// Piezas de seguridad y permisos activos: van en la capa más alta y no se cortan.
///
/// Con `puede_actuar` a `false` **no entra ninguna**: el turno no tiene herramienta
/// que aprobar. La puerta real está en el código (una tool fuera del Plan se rechaza
/// y se cuenta como fallo) y la defensa contra la inyección va en la capa `contrato`
/// del system. Lo que se midió el 07-10 es lo contrario de lo que se suele creer:
/// escribir «no hay herramienta que aprobar ni permiso que pedir» en un turno de
/// charla es lo que hizo que un 0,8 B contestara «no tengo herramientas que aprobar
/// ni permiso necesario en este turno». Nombrar el tema, aunque sea para negarlo, le
/// da tema.
pub fn piezas_de_seguridad(
    req: &BrainRequest,
    approval: &str,
    puede_actuar: bool,
) -> Vec<Pieza> {
    if !puede_actuar {
        return Vec::new();
    }
    let mut v = vec![Pieza::nueva(
        Prioridad::Seguridad,
        "permisos",
        format!(
            "Aprobación activa: {approval}. Política: {:?}. Escrivuras permitidas: {}.",
            req.policies,
            matches!(
                req.approval_level,
                crate::api::vocab::ApprovalLevel::AutoSandbox
                    | crate::api::vocab::ApprovalLevel::FullAccess
            )
        ),
    )];
    if req.policies == crate::api::vocab::ExecutionPolicy::LocalOnly {
        v.push(Pieza::nueva(
            Prioridad::Seguridad,
            "local_only",
            "Este pedido es local-only: no debe pedirse nada que salga del equipo.",
        ));
    }
    v
}

fn si_no(b: bool) -> &'static str {
    if b {
        "sí"
    } else {
        "no"
    }
}

/// §XII del Canon v1.3: sensibilidad **por ítem de contexto**, resuelta por nombre
/// de archivo. A propósito corta —`config.toml` no es un secreto y `.env.local`
/// sí— porque marcarlo todo equivale a no marcar nada. Lo marcado aquí no sale
/// del equipo: si el Plan acaba en una API se descarta y se registra con
/// `sensibilidad:` delante (ver `context::fuera_del_equipo`).
fn parece_secreto(ruta: &str) -> bool {
    let nombre = ruta
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(ruta)
        .to_ascii_lowercase();
    nombre == ".env"
        || nombre.starts_with(".env.")
        || nombre.ends_with(".pem")
        || nombre.ends_with(".key")
        || nombre.starts_with("id_rsa")
        || nombre.contains("credentials")
        || nombre.contains("secrets")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{ApprovalLevel, ExecutionPolicy, Mode};

    #[test]
    fn el_pedidos_solo_aporta_lo_que_sabe() {
        let req = BrainRequest::nuevo("hatboo", Mode::WORK, "mira src/app.rs y arreglalo");
        let p = piezas_del_pedido(&req, None, None, true);
        assert!(p.iter().any(|x| x.origen == "mencionado:src/app.rs"));
        assert!(!p.iter().any(|x| x.origen == "proyecto:hechos"), "sin proyecto no hay hechos");
        let con = BrainRequest {
            project: Some(crate::api::request::ProjectContext {
                root: "C:/p".into(),
                has_tests: false,
                has_lint: false,
                has_build: true,
                language: Some("rust".into()),
                hatboo_md: None,
                trust_state: Default::default(),
                verify: Default::default(),
            }),
            ..req
        };
        let p2 = piezas_del_pedido(&con, None, None, true);
        let hechos = p2
            .iter()
            .find(|x| x.origen == "proyecto:hechos")
            .unwrap()
            .texto
            .clone();
        assert!(hechos.contains("tests: no") && hechos.contains("build: sí"), "{hechos}");
        assert!(p2.iter().any(|x| x.origen == "proyecto:verificacion"));
    }

    #[test]
    fn la_seguridad_es_la_primera_y_no_se_corta() {
        let req = BrainRequest::nuevo("hatboo", Mode::CHAT, "hola")
            .con_policy(ExecutionPolicy::LocalOnly)
            .con_aprobacion(ApprovalLevel::AskAlways);
        let s = piezas_de_seguridad(&req, "preguntar siempre", true);
        assert!(s.iter().all(|p| p.prioridad == Prioridad::Seguridad));
        assert!(s.iter().any(|p| p.origen == "local_only"));
    }

    #[test]
    fn un_archivo_de_claves_se_marca_como_no_saliente() {
        let req = BrainRequest::nuevo("hatboo", Mode::CHAT, "mira y dime qué falta");
        let p = piezas_del_pedido(&req, None, Some(".env.local"), true);
        let marcada = p
            .iter()
            .find(|x| x.origen.contains(".env.local"))
            .expect("la pieza del archivo mencionado");
        assert!(marcada.sensible, "un `.env.local` no se manda a una API");

        // Y una fuente normal no se marca: si todo fuese sensible, la regla no
        // filtraría nada.
        let p2 = piezas_del_pedido(&req, None, Some("src/main.rs"), true);
        assert!(
            p2.iter().all(|x| !x.sensible),
            "{:?}",
            p2.iter().map(|x| &x.origen).collect::<Vec<_>>()
        );
    }

    #[test]
    fn el_historial_no_se_duplica_en_el_texto_del_turno() {
        // Viaja por el canal nativo de `messages` del proveedor, recortado a lo que
        // el Plan admitió por presupuesto. Meterlo además aquí era mandarlo dos
        // veces: una recortada y otra entera y sin contar.
        let largo = "k".repeat(4000);
        let req = BrainRequest {
            history: vec![crate::api::request::Message::usuario(&largo)],
            ..BrainRequest::nuevo("hatboo", Mode::CHAT, "sigue")
        };
        let p = piezas_del_pedido(&req, None, None, true);
        assert!(
            !p.iter().any(|x| x.origen.starts_with("historial:")),
            "{:?}",
            p.iter().map(|x| &x.origen).collect::<Vec<_>>()
        );
    }

    /// Un chat no tiene herramienta que correr: los hechos del proyecto y la
    /// negociación de permisos no son cosas *de ese* turno. Estaban entrando siempre,
    /// y en un modelo de 0,8 B el resultado se vio el 07-10 en el chat: contestaba de
    /// herramientas y de aprobaciones a un saludo.
    #[test]
    fn un_turno_sin_herramientas_no_habla_de_proyecto_ni_de_permisos() {
        let req = BrainRequest::nuevo("hatboo", Mode::CHAT, "hola")
            .con_policy(ExecutionPolicy::LocalOnly)
            .con_aprobacion(ApprovalLevel::AskAlways);
        let con = BrainRequest {
            project: Some(crate::api::request::ProjectContext {
                root: "C:/p".into(),
                has_tests: true,
                has_lint: true,
                has_build: true,
                language: Some("rust".into()),
                hatboo_md: None,
                trust_state: Default::default(),
                verify: Default::default(),
            }),
            ..req
        };
        let piezas = piezas_del_pedido(&con, None, None, false);
        assert!(
            !piezas.iter().any(|x| x.origen.starts_with("proyecto:")),
            "{:?}",
            piezas.iter().map(|x| &x.origen).collect::<Vec<_>>()
        );
        // Con herramientas, las mismas piezas vuelven: es el turno el que manda, no
        // un interruptor global.
        let actuando = piezas_del_pedido(&con, None, None, true);
        assert!(actuando.iter().any(|x| x.origen == "proyecto:hechos"));

        // Y ninguna pieza de permisos: nombrar el tema, aunque sea para negarlo, le
        // da tema al modelo (así salió «no tengo herramientas que aprobar ni permiso
        // necesario en este turno» en un chat del 07-10).
        let seg = piezas_de_seguridad(&con, "preguntar siempre", false);
        assert!(seg.is_empty(), "{:?}", seg.iter().map(|p| &p.texto).collect::<Vec<_>>());
    }
}
