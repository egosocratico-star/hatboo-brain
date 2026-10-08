//! El Gobernador de recursos: mira lo que **hay** y recomienda lo que cabe. No
//! decide qué se ejecuta (eso es del Selector y del Planner) y no toca el
//! sandbox.

pub mod governor;

pub use governor::{mantener, Ajuste, Consejo, Governor, GovernorConfig};

use crate::models::ModeloCargado;

/// La sonda de hardware la pone el producto. El crate a propósito no trae
/// `sysinfo`: en la máquina de Hatboo ya lo compila el desktop, y en otro
/// proyecto lo traerá u ofrecerá otra cosa. `None` en una lectura significa
/// «no lo sé», y el Governor **no** asume margen de sobra cuando no lo sabe.
pub trait ResourceProbe: Send + Sync {
    /// MB libres del sistema. Medido en Fase 0: es la cifra que manda, no el
    /// `size` que declara Ollama (los pesos salen del page cache y se recuperan).
    fn libre_mb(&self) -> Option<u64>;
    fn total_mb(&self) -> Option<u64> {
        None
    }
    /// 0,0–1,0. `None` = escritorio sin battery.
    fn bateria(&self) -> Option<f32> {
        None
    }
    /// Carga instantánea 0,0–1,0.
    fn cpu(&self) -> Option<f32> {
        None
    }
    /// Qué tiene el servidor cargado ya.
    fn cargados(&self) -> Vec<ModeloCargado> {
        Vec::new()
    }
}

/// Sonda que no sabe nada. Con ella el Governor es conservador: no afirma que
/// quepa lo que no puede medir.
#[derive(Debug, Clone, Default)]
pub struct SinSonda;

impl ResourceProbe for SinSonda {
    fn libre_mb(&self) -> Option<u64> {
        None
    }
}

/// Sonda fija para tests y para el bench.
#[derive(Debug, Clone)]
pub struct SondaFija {
    pub libre: Option<u64>,
    pub total: Option<u64>,
    pub bateria: Option<f32>,
    pub cpu: Option<f32>,
    pub cargados: Vec<ModeloCargado>,
}

impl Default for SondaFija {
    fn default() -> Self {
        SondaFija {
            libre: Some(8000),
            total: Some(16384),
            bateria: None,
            cpu: None,
            cargados: Vec::new(),
        }
    }
}

impl ResourceProbe for SondaFija {
    fn libre_mb(&self) -> Option<u64> {
        self.libre
    }
    fn total_mb(&self) -> Option<u64> {
        self.total
    }
    fn bateria(&self) -> Option<f32> {
        self.bateria
    }
    fn cpu(&self) -> Option<f32> {
        self.cpu
    }
    fn cargados(&self) -> Vec<ModeloCargado> {
        self.cargados.clone()
    }
}
