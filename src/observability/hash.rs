//! Dos hashes, dos papeles distintos, y la diferencia importa.
//!
//! - `fnv1a64` — **linaje**: `plan_hash`, `parent_plan_hash`, la firma de la
//!   caché de decisiones. Sirve para emparejar y para decir «este Plan viene de
//!   aquel»; es trivial de colisionar a propósito y por eso no se usa para nada
//!   que decida un permiso.
//! - `sha256` — **confianza**: el estado de `HATBOO.md`. Aquí sí hay un
//!   adversario que puede escribir el archivo, así que el hash decide si un
//!   contenido entra al prompt y tiene que costar trabajo fingir otra cosa.
//!   §8 del plan lo pide explícito: SHA-256, no FNV.

use sha2::{Digest, Sha256};

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIMO: u64 = 0x0000_0100_0000_01b3;

pub fn fnv1a64(s: &str) -> String {
    let mut h = OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIMO);
    }
    format!("{h:016x}")
}

/// SHA-256 en hexadecimal, para el hash de confianza de `HATBOO.md`.
pub fn sha256(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let salida = h.finalize();
    let mut out = String::with_capacity(64);
    for b in salida {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Para los hashes que se muestran en la ficha de un proyecto: suficientes
/// dígitos para distinguir, pocos para no llenar la UI.
pub fn corto(completo: &str) -> String {
    completo.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estable_y_unicamente_ordenado() {
        assert_eq!(fnv1a64("plan"), fnv1a64("plan"));
        assert_ne!(fnv1a64("plan"), fnv1a64("plana"));
        assert_eq!(fnv1a64("").len(), 16);
        assert_eq!(corto(&fnv1a64("x")).len(), 12);
    }

    #[test]
    fn vector_conocido() {
        // FNV-1a-64 de "" y de "a" son valores públicos del algoritmo.
        assert_eq!(fnv1a64(""), "cbf29ce484222325");
        assert_eq!(fnv1a64("a"), "af63dc4c8601ec8c");
    }

    /// El vector del estándar: si alguien cambia el algoritmo o el orden de los
    /// bytes, esto lo dice sin depender de la implementación.
    #[test]
    fn sha256_coincide_con_el_estandar() {
        assert_eq!(
            sha256(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256("a"),
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
        );
        assert_eq!(sha256("hola").len(), 64);
        assert_ne!(sha256("hola"), sha256("hola "));
    }
}
