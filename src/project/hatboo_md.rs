//! `HATBOO.md`: cuatro estados, un hash y una regla que no se negocia — sin hash
//! aprobado no entra al prompt, y aprobado no significa que amplíe permisos.

use super::trust::AlmacenDeConfianza;
use crate::api::request::TrustState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Archivo {
    /// Hash del contenido actual. Identificador de linaje, no firma.
    pub current_hash: String,
    /// Hash que el usuario aprobó, si alguna vez lo aprobó.
    pub approved_hash: Option<String>,
    pub trust_state: TrustState,
    /// El contenido, solo si `trust_state == Aprobado`. Un `Cambiar` o
    /// `SinRevisar` no viaja con texto: no sea que alguien lo lea como instrucción.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contenido_aprobado: Option<String>,
}

impl Archivo {
    /// Estado a partir del contenido que trajo el producto y del almacén.
    pub fn desde_contenido(contenido: &str, almacen: &AlmacenDeConfianza, clave: &str) -> Archivo {
        let current_hash = crate::observability::hash::fnv1a64(contenido);
        let aprobado = almacen.aprobado(clave);
        let trust_state = if almacen.esta_rechazado(clave) {
            TrustState::Rechazado
        } else {
            match &aprobado {
                None => TrustState::SinRevisar,
                Some(h) if h == &current_hash => TrustState::Aprobado,
                Some(_) => TrustState::Cambiado,
            }
        };
        let contenido_aprobado = if trust_state == TrustState::Aprobado {
            Some(contenido.to_string())
        } else {
            None
        };
        Archivo {
            current_hash,
            approved_hash: aprobado,
            trust_state,
            contenido_aprobado,
        }
    }

    /// ¿Entra en el system prompt? Solo aprobado.
    pub fn entra_al_prompt(&self) -> bool {
        self.trust_state == TrustState::Aprobado && self.contenido_aprobado.is_some()
    }

    pub fn texto_para_el_prompt(&self) -> Option<&str> {
        self.contenido_aprobado.as_deref()
    }

    /// Lo que el producto muestra para que el usuario decida.
    pub fn aviso(&self) -> &'static str {
        match self.trust_state {
            TrustState::SinRevisar => "el proyecto trae reglas que aún no has aprobado; no se usan",
            TrustState::Aprobado => "reglas del proyecto aprobadas y activas",
            TrustState::Cambiado => "las reglas del proyecto cambiaron desde que las aprobaste; no se usan hasta que las veas",
            TrustState::Rechazado => "rechazaste estas reglas del proyecto; no se usan",
        }
    }
}

/// Lo que NUNCA hace el archivo aprobado. Está escrito porque es la tentación
/// obvia: un archivo dentro del proyecto que se inyecta en el system prompt es
/// el canal natural de una inyección de permisos.
pub const LIMITES: &[&str] = &[
    "no amplía permisos ni niveles de aprobación",
    "no apaga la redacción de secretos",
    "no añade tools que el producto no ofrece",
    "no cambia la política de local/API",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_aprobar_no_entra() {
        let a = AlmacenDeConfianza::nuevo();
        let f = Archivo::desde_contenido("usa cargo test", &a, "C:/p/HATBOO.md");
        assert_eq!(f.trust_state, TrustState::SinRevisar);
        assert!(!f.entra_al_prompt());
        assert!(f.contenido_aprobado.is_none());
    }

    #[test]
    fn aprobar_un_hash_y_que_no_cambie_es_lo_unico_que_activa() {
        let mut a = AlmacenDeConfianza::nuevo();
        let f0 = Archivo::desde_contenido("usa cargo test", &a, "C:/p/HATBOO.md");
        a.aprobar("C:/p/HATBOO.md", &f0.current_hash);
        let f = Archivo::desde_contenido("usa cargo test", &a, "C:/p/HATBOO.md");
        assert_eq!(f.trust_state, TrustState::Aprobado);
        assert!(f.entra_al_prompt());
        assert_eq!(f.texto_para_el_prompt(), Some("usa cargo test"));
    }

    #[test]
    fn cambiar_el_archivo_apaga_hasta_volver_a_aprobar() {
        let mut a = AlmacenDeConfianza::nuevo();
        let f0 = Archivo::desde_contenido("v1", &a, "k");
        a.aprobar("k", &f0.current_hash);
        let f1 = Archivo::desde_contenido("v2", &a, "k");
        assert_eq!(f1.trust_state, TrustState::Cambiado);
        assert!(!f1.entra_al_prompt());
        assert!(f1.aviso().contains("cambiaron"), "{}", f1.aviso());
    }

    #[test]
    fn rechazado_se_queda_rechazado() {
        let mut a = AlmacenDeConfianza::nuevo();
        a.rechazar("k");
        let f = Archivo::desde_contenido("x", &a, "k");
        assert_eq!(f.trust_state, TrustState::Rechazado);
        assert!(!f.entra_al_prompt());
    }

    #[test]
    fn el_hash_no_depende_del_order_de_los_bytes() {
        let a = AlmacenDeConfianza::nuevo();
        let b = Archivo::desde_contenido("mismo texto", &a, "k1");
        let c = Archivo::desde_contenido("mismo texto", &a, "k2");
        assert_eq!(b.current_hash, c.current_hash);
    }
}
