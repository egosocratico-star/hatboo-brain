//! Dónde vive la confianza del usuario sobre los archivos de proyecto: un mapa
//! clave→decisión. El producto lo persiste (Hatboo ya persiste su config); el
//! crate solo define la forma y las reglas.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decicion {
    /// El usuario vio este contenido exacto y lo aprobó.
    Aprobado { hash: String },
    /// Lo rechazó: hasta que no cambie o no se vuelva a preguntar, nada.
    Rechazado,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AlmacenDeConfianza {
    /// Clave = el identificador que el producto usa (normalmente la ruta del
    /// archivo dentro del proyecto).
    decisiones: std::collections::BTreeMap<String, Decicion>,
}

impl AlmacenDeConfianza {
    pub fn nuevo() -> Self {
        Self::default()
    }

    pub fn desde_json(t: &str) -> Result<AlmacenDeConfianza, serde_json::Error> {
        serde_json::from_str(t)
    }

    pub fn a_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn aprobar(&mut self, clave: &str, hash: &str) {
        self.decisiones.insert(
            clave.to_string(),
            Decicion::Aprobado {
                hash: hash.to_string(),
            },
        );
    }

    pub fn rechazar(&mut self, clave: &str) {
        self.decisiones
            .insert(clave.to_string(), Decicion::Rechazado);
    }

    /// `None` = nunca se preguntó.
    pub fn decision(&self, clave: &str) -> Option<&Decicion> {
        self.decisiones.get(clave)
    }

    /// El hash aprobado, si es que hay aprobación (no si hay rechazo).
    pub fn aprobado(&self, clave: &str) -> Option<String> {
        match self.decision(clave) {
            Some(Decicion::Aprobado { hash }) => Some(hash.clone()),
            _ => None,
        }
    }

    pub fn esta_rechazado(&self, clave: &str) -> bool {
        matches!(self.decision(clave), Some(Decicion::Rechazado))
    }

    pub fn olvidar(&mut self, clave: &str) {
        self.decisiones.remove(clave);
    }

    pub fn todas(&self) -> impl Iterator<Item = (&str, &Decicion)> {
        self.decisiones.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aprobar_rechazar_y_olvidar() {
        let mut a = AlmacenDeConfianza::nuevo();
        assert_eq!(a.aprobado("k"), None);
        a.aprobar("k", "abc");
        assert_eq!(a.aprobado("k").as_deref(), Some("abc"));
        a.rechazar("k");
        assert!(a.esta_rechazado("k"));
        assert_eq!(a.aprobado("k"), None, "el rechazo no deja hash aprobado");
        a.olvidar("k");
        assert_eq!(a.decision("k"), None);
    }

    #[test]
    fn viaja_en_json() {
        let mut a = AlmacenDeConfianza::nuevo();
        a.aprobar("C:/p/HATBOO.md", "hash1");
        let j = a.a_json();
        let b = AlmacenDeConfianza::desde_json(&j).unwrap();
        assert_eq!(b, a);
        assert_eq!(b.aprobado("C:/p/HATBOO.md").as_deref(), Some("hash1"));
    }
}
