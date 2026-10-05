//! §15.9: el dataset de decisiones de Fase 7. Afinar un decisor tipado necesita
//! texto de usuario, y el log normal **no** lo guarda. Esta pieza lo guarda, con
//! tres condiciones que no son negociables: viene apagada de fábrica (`Flags.
//! record_decisions`), el texto pasa por `security::redact` antes de salir del
//! Brain, y el que lo escribe y el que lo borra es el producto — el crate no
//! toca el disco, solo entrega líneas ya redactadas.

use serde::Serialize;

/// Una decisión, en la forma que se entrega al producto. Una línea JSON por
/// turno: `to_linea` cierra el objeto con el salto para que se pueda anexar.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistroDecision {
    pub producto: String,
    pub modo: String,
    /// El texto del usuario, ya redactado por `security::redact::texto`.
    pub mensaje: String,
    pub intent: String,
    pub nivel: String,
    pub riesgo: String,
    pub contrato: String,
    /// Lo que exigió el nivel: `ninguna`, `formato`, `determinista`…
    pub verificacion: String,
    pub modelo: String,
    pub proveedor: String,
    /// `true` si el turno se quedó en el equipo. Con `mensaje` guardado, este
    /// campo es el que dice si ese texto salió o no del portátil.
    pub local: bool,
    /// Si el turno acabó verificado, propuesto, sin verificar o rechazado.
    pub resultado: String,
    pub motivo_plan: String,
    /// **Hash**, no texto: lo que se quiere poder responder es «¿esta decisión la
    /// cambió una skill o una nota de memoria?», y para eso basta con saber si el
    /// bloque del producto era el mismo o distinto. El contenido lo escribe el
    /// usuario, y el dataset no se convierte en un espejo de sus notas.
    ///
    /// Vacío cuando el producto no mandó bloque: un consumidor viejo sigue teniendo
    /// la misma línea de siempre.
    pub contexto_producto_hash: String,
}

impl RegistroDecision {
    pub fn to_linea(&self) -> String {
        let mut linea =
            serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        linea.push('\n');
        linea
    }
}

/// El almacén del producto: un archivo, una base de datos, nada. El Brain no
/// decide dónde se guarda ni cuánto tiempo.
pub trait SinkDecisiones: Send + Sync {
    fn guarda(&self, registro: &RegistroDecision);
}

/// Sin destino: es lo que hay hasta que el producto pone un almacén.
#[derive(Debug, Clone, Copy, Default)]
pub struct SinDecisiones;

impl SinkDecisiones for SinDecisiones {
    fn guarda(&self, _registro: &RegistroDecision) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recoge(Arc<Mutex<Vec<String>>>);

    impl SinkDecisiones for Recoge {
        fn guarda(&self, r: &RegistroDecision) {
            self.0.lock().unwrap().push(r.to_linea());
        }
    }

    fn registro() -> RegistroDecision {
        RegistroDecision {
            producto: "hatboo".into(),
            modo: "chat".into(),
            mensaje: "hola".into(),
            intent: "ask".into(),
            nivel: "N1".into(),
            riesgo: "Low".into(),
            contrato: "texto".into(),
            verificacion: "formato".into(),
            modelo: "gemma3:1b".into(),
            proveedor: "ollama".into(),
            local: true,
            resultado: "verificado".into(),
            motivo_plan: "charla corta".into(),
            contexto_producto_hash: String::new(),
        }
    }

    #[test]
    fn una_linea_por_decision_y_es_json() {
        let s = SinDecisiones;
        s.guarda(&registro());
        let recolector = Recoge::default();
        recolector.guarda(&registro());
        let guardado = recolector.0.lock().unwrap().pop().expect("una línea");
        assert!(guardado.ends_with('\n'), "{guardado:?}");
        let j: serde_json::Value = serde_json::from_str(&guardado).expect("JSON por línea");
        assert_eq!(j["nivel"], "N1");
        assert_eq!(j["mensaje"], "hola");
    }

    /// Lo que se guarda del bloque del producto es su **hash**. El dataset tiene
    /// que poder responder «¿esto lo decidió así con esta skill puesta?» sin
    /// convertirse en una copia de las notas que el usuario escribe para sí mismo.
    #[test]
    fn del_bloque_del_producto_se_guarda_el_hash_no_el_texto() {
        use crate::observability::hash::sha256;
        let mut r = registro();
        r.contexto_producto_hash = sha256("prefiero respuestas cortas");
        let linea = r.to_linea();
        let j: serde_json::Value = serde_json::from_str(&linea).unwrap();
        let guardado = j["contexto_producto_hash"].as_str().expect("un hash");
        assert_eq!(guardado.len(), 64, "sha-256 en hex: {guardado}");
        assert!(!linea.contains("respuestas cortas"), "el texto no viaja: {linea}");
        // Mismo bloque, misma firma; otro bloque, otra firma. Es lo que permite
        // emparejar una decisión con el contexto que la produjo.
        assert_eq!(sha256("prefiero respuestas cortas"), guardado);
        assert_ne!(sha256("prefiero respuestas largas"), guardado);
        // Vacío = el producto no mandó bloque, y la línea lo dice con una cadena
        // vacía en vez de con un hash de la nada.
        assert_eq!(registro().contexto_producto_hash, "");
    }
}
