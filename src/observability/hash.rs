//! Hash de linaje. **FNV-1a de 64 bits, a mano**: el crate no trae `sha2`, y lo
//! que se necesita aquí es identificar y emparejar (`plan_hash`,
//! `parent_plan_hash`, la firma de una caché, el estado de `HATBOO.md`), no firmar
//! nada contra un adversario.
//!
//! Si algún día se usa para seguridad, esto **no** sirve: es trivial de
//! colisionar. Queda escrito para que nadie lo lea como lo que no es.

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
}
