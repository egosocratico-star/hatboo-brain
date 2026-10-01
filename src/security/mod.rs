//! Redacción de secretos. Compartida por todos los productos porque es la única
//! defensa que existe **antes** de que un cuerpo de error salga hacia afuera: el
//! Canon (§XVI) la exige antes de la API y antes del log.

pub mod redact;

/// Serde que redacta al **serializar**. Así es imposible que un `BrainError`
/// guardado en log o devuelto a la UI lleve una clave: no depende de que el que
/// llama se acuerde de pasar `redact::texto()`.
pub mod string_serde {
    use super::redact;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(s: &String, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&redact::texto(s))
    }

    pub fn deserialize<'d, D: Deserializer<'d>>(des: D) -> Result<String, D::Error> {
        String::deserialize(des)
    }
}
