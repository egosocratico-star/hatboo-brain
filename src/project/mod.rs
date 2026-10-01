//! El proyecto y su archivo de reglas. El Brain no lee el disco: el producto le
//! pasa el contenido y aquí vive el **estado de confianza**.

pub mod hatboo_md;
pub mod trust;

pub use hatboo_md::{Archivo, LIMITES};
pub use trust::{AlmacenDeConfianza, Decicion};
