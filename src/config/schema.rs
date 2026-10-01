//! El esquema de configuración. Todo con `#[serde(default)]`: un config viejo no
//! puede romper un producto nuevo, y lo que falte se toma del default medido.

use crate::observability::logger::BitacoraConfig;
use crate::observability::metrics::Referencia;
use crate::resources::governor::GovernorConfig;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ids {
    /// Cuánto tarda un modelo en cargarse desde frío. Medido: 7324 ms la primera
    /// vez del día, 2393 ms con el caché tibio, 3700 ms al cambiar num_ctx.
    pub carga_fria_ms: u64,
    pub carga_tibia_ms: u64,
}

impl Default for Ids {
    fn default() -> Self {
        Ids {
            carga_fria_ms: 7324,
            carga_tibia_ms: 2393,
        }
    }
}

use serde::{Deserialize, Serialize};

/// Flags por pieza (§II.10: toda pieza nueva pasa por bench, y si empeora, flag
/// off). Sin flag activo la pieza no corre: no es un adorno.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Flags {
    /// El Fast Path aritmético y de lecturas directas. Barato y sin modelo.
    pub fast_path: bool,
    /// Caché de decisiones.
    pub cache_decisiones: bool,
    /// Reintentos por clase de fallo.
    pub recuperacion: bool,
    /// Escalada de tier al repetir clase.
    pub escalar: bool,
    /// Verificación determinista corriendo comandos del proyecto.
    pub verificacion_ejecucion: bool,
    /// Logprobs (Fase 6). Off hasta calibrar.
    pub logprobs: bool,
    /// Backend de decisión tipo Laya (Fase 7). Off.
    pub backend_decision: bool,
}

impl Default for Flags {
    fn default() -> Self {
        Flags {
            fast_path: true,
            cache_decisiones: true,
            recuperacion: true,
            escalar: true,
            verificacion_ejecucion: true,
            logprobs: false,
            backend_decision: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BrainConfig {
    pub flags: Flags,
    pub governor: GovernorConfig,
    pub ids: Ids,
    /// Dónde leer `brain-rules.json` / `tools.json` / `models.json`. El producto
    /// decide la ruta; el crate no adivina directorios de sistema.
    pub dir_config: Option<PathBuf>,
    pub bitacora: BitacoraConfig,
    /// Tope de la duda (§15.3, punto abierto). `None` = sin tope, que es lo que
    /// dice §1 hoy.
    pub tope_de_duda: Option<crate::api::vocab::Level>,
    /// Línea base de cada modelo para normalizar el coste. Si no está, `coste`
    /// sale `None` en vez de un número sin fundamento.
    #[serde(skip)]
    pub referencias: std::collections::BTreeMap<String, Referencia>,
    /// Máximo de entradas de la caché de decisiones.
    pub cache_max: usize,
    /// TTL de esa caché, en segundos.
    pub cache_ttl_s: u64,
    /// El techo de `thinking` que el producto ofrece por defecto. El Request puede
    /// bajarlo; nunca subirlo.
    pub thinking_ceiling: Option<crate::api::vocab::ThinkingLevel>,
}

impl Default for BrainConfig {
    fn default() -> Self {
        BrainConfig {
            flags: Flags::default(),
            governor: GovernorConfig::default(),
            ids: Ids::default(),
            dir_config: None,
            bitacora: BitacoraConfig::default(),
            tope_de_duda: None,
            referencias: Default::default(),
            cache_max: 256,
            cache_ttl_s: 900,
            thinking_ceiling: None,
        }
    }
}

impl BrainConfig {
    pub fn con_flags(mut self, f: Flags) -> Self {
        self.flags = f;
        self
    }

    pub fn con_referencia(mut self, modelo: &str, r: Referencia) -> Self {
        self.referencias.insert(modelo.to_string(), r);
        self
    }

    pub fn referencia(&self, modelo: &str) -> Option<Referencia> {
        self.referencias.get(modelo).copied()
    }
}

/// Errores de config. Se listan en concreto porque un config roto tiene que decir
/// **qué archivo y qué campo**, no «config: erróneo».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case", tag = "tipo", content = "dato")]
pub enum ConfigError {
    #[error("no se pudo leer {archivo}: {causa}")]
    Leyendo { archivo: String, causa: String },
    #[error("{archivo} no es JSON válido: {causa}")]
    Json { archivo: String, causa: String },
    /// El archivo existe pero dice algo imposible (p. ej. un margen negativo o un
    /// `num_ctx` fuera de conjunto).
    #[error("{0}")]
    Incoherente(String),
    /// No hay ningún proveedor habilitado: sin backend no hay Brain.
    #[error("no hay ningún proveedor habilitado: el Brain necesita al menos uno")]
    SinBackend,
    /// Se pidió un modelo que no está instalado ni descrito.
    #[error("el modelo «{0}» no está instalado en ninguno de los proveedores activos")]
    ModeloNoInstalado(String),
}

impl ConfigError {
    pub fn mensaje(&self) -> String {
        match self {
            ConfigError::Leyendo { archivo, causa } => {
                format!("no se pudo leer {archivo}: {causa}")
            }
            ConfigError::Json { archivo, causa } => format!("{archivo} no es JSON válido: {causa}"),
            ConfigError::Incoherente(s) => s.clone(),
            ConfigError::SinBackend => {
                "no hay ningún proveedor habilitado: el Brain necesita al menos uno".into()
            }
            ConfigError::ModeloNoInstalado(m) => {
                format!("el modelo «{m}» no está instalado en ninguno de los proveedores activos")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_config_vacio_toma_los_defaults_medidos() {
        let c: BrainConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.governor.margen_mb, 1500);
        assert_eq!(c.ids.carga_fria_ms, 7324);
        assert_eq!(c.cache_max, 256);
        assert!(c.flags.fast_path);
        assert!(!c.flags.logprobs, "las fases 6 y 7 nacen apagadas");
        assert_eq!(c.thinking_ceiling, None);
    }

    #[test]
    fn las_partes_se_pueden_escribir_sueltas() {
        let c: GovernorConfig = serde_json::from_str(r#"{"margenMb": 2048}"#).unwrap();
        assert_eq!(c.margen_mb, 2048);
        assert_eq!(c.ctx_permitidos, vec![2048, 4096, 8192]);
    }

    #[test]
    fn un_json_roto_dice_donde() {
        let e: Result<Flags, _> = serde_json::from_str("{\"fastPath\": \"sí\"}");
        assert!(e.is_err());
        let err = ConfigError::Json {
            archivo: "brain-rules.json".into(),
            causa: "x".into(),
        };
        assert!(err.mensaje().contains("brain-rules.json"));
    }
}
