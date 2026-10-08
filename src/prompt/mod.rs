//! El Prompt Engine. Dos funciones puras, sin red ni estado: `build_system` y
//! `build_dinamico`. Si son puras se puede exigir que dos llamadas con las mismas
//! entradas den los mismos bytes, que es lo que hace barata la caché de prefijo
//! del proveedor.

pub mod dynamic;
pub mod escape;
pub mod system;

pub use dynamic::{presupuesto_efectivo, texto_del_turno, ContextoArmado};
pub use escape::{bloque_datos, sanea_origen};
pub use system::{build_system, instruccion_del_intent, Identidad, Capa};

/// Contador de tokens. El crate **no** trae tokenizador: una estimación honesta
/// por defecto, y el producto enchufa el suyo si lo tiene.
///
/// Medido en Fase 0 sobre Ollama: 19 tokens de entrada de media por prompt en
/// mensajes de ~60 caracteres, así que «1 token ≈ 4 caracteres» se queda corta
/// para el español; se usa 3,5 y se declara como estimación.
pub trait ContadorTokens: Send + Sync {
    fn cuenta(&self, s: &str) -> u32;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Estimador;

impl ContadorTokens for Estimador {
    fn cuenta(&self, s: &str) -> u32 {
        // ≈3,5 caracteres por token, redondeado hacia arriba: es mejor pasarse de
        // presupuesto que quedarse corto y que el proveedor corte el prompt.
        let c = s.chars().count() as f32;
        (c / 3.5).ceil() as u32
    }
}
