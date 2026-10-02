//! De dónde salen las piezas de contexto. El Brain no lee el disco: le pide al
//! producto el contenido de lo que el usuario nombró (mismo sandbox, mismos
//! permisos que ya tiene Hatboo).

use super::{Pieza, Prioridad};
use crate::api::request::BrainRequest;
use crate::brain::state::EstadoTarea;

/// El adaptador del producto lo implementa. `None` = no lo tienes (fuera del
/// proyecto, sin permiso, o no existe).
pub trait Fuentes: Send + Sync {
    fn contenido(&self, origen: &str) -> Option<String>;
}

#[derive(Debug, Clone, Default)]
pub struct SinFuentes;

impl Fuentes for SinFuentes {
    fn contenido(&self, _origen: &str) -> Option<String> {
        None
    }
}

/// Piezas que se derivan solo del pedido, sin tocar el disco.
pub fn piezas_del_pedido(
    req: &BrainRequest,
    estado: Option<&EstadoTarea>,
    lectura_directa: Option<&str>,
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
        v.push(Pieza::nueva(
            Prioridad::ArchivoNombrado,
            format!("mencionado:{r}"),
            format!("El pedido menciona «{r}»."),
        ));
    }

    if let Some(p) = &req.project {
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

    // El historial NO va aquí. Viaja por el canal nativo de `messages` del
    // proveedor, recortado a `plan.historial_turnos` (lo que cabe según §11).
    // Meterlo además en el texto del turno era mandarlo dos veces: una recortada a
    // 700 caracteres y otra entera y sin presupuestar.
    v
}

/// Piezas de seguridad y permisos activos: van en la capa más alta y no se cortan.
pub fn piezas_de_seguridad(req: &BrainRequest, approval: &str) -> Vec<Pieza> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{ApprovalLevel, ExecutionPolicy, Mode};

    #[test]
    fn el_pedidos_solo_aporta_lo_que_sabe() {
        let req = BrainRequest::nuevo("hatboo", Mode::WORK, "mira src/app.rs y arreglalo");
        let p = piezas_del_pedido(&req, None, None);
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
        let p2 = piezas_del_pedido(&con, None, None);
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
        let s = piezas_de_seguridad(&req, "preguntar siempre");
        assert!(s.iter().all(|p| p.prioridad == Prioridad::Seguridad));
        assert!(s.iter().any(|p| p.origen == "local_only"));
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
        let p = piezas_del_pedido(&req, None, None);
        assert!(
            !p.iter().any(|x| x.origen.starts_with("historial:")),
            "{:?}",
            p.iter().map(|x| &x.origen).collect::<Vec<_>>()
        );
    }
}
