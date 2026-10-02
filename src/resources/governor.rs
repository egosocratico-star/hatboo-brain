//! El consejo del Governor. Números de §11 del Canon y de lo medido en Fase 0:
//! margen 1,5 GB, `num_ctx` en valores fijos, y **3,7 s por cada cambio de
//! `num_ctx`** porque Ollama rehace el grafo y el KV.

use super::ResourceProbe;
use crate::api::vocab::Level;
use crate::models::{ModelInfo, ModeloCargado};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GovernorConfig {
    /// Margen de RAM libre que hay que dejar sin tocar. 1500 MB por defecto.
    pub margen_mb: u64,
    /// Conjunto de `num_ctx` válidos. Ampliarlo es config por modelo (§15.5).
    pub ctx_permitidos: Vec<u32>,
    /// Perfil ligero bajo presión: baja el contexto pedido un escalón.
    pub perfil_ligero_presion: bool,
    /// A partir de qué batería (o CPU alta) se activa el perfil ligero.
    pub presion_bateria: Option<f32>,
    pub presion_cpu: Option<f32>,
    /// Cuánto cuesta cambiar num_ctx. Medido: 3700 ms.
    pub recarga_por_ctx_ms: u64,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        GovernorConfig {
            margen_mb: 1500,
            ctx_permitidos: vec![2048, 4096, 8192],
            perfil_ligero_presion: true,
            presion_bateria: Some(0.35),
            presion_cpu: Some(0.9),
            recarga_por_ctx_ms: 3700,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consejo {
    /// El `num_ctx` recomendado. 0 = no se puede firmar nada.
    pub num_ctx: u32,
    pub cabe: bool,
    /// RAM que ocuparía el modelo a ese `num_ctx`, si la tenemos medida.
    pub ram_mb: Option<u64>,
    /// Cuánta RAM queda según la sonda. `None` = la sonda no lo sabe.
    pub libre_mb: Option<u64>,
    /// El modelo ya está residente a este ctx: no hay que cargarlo.
    pub residente: bool,
    /// Cambiar el ctx de algo que ya está cargado cuesta una recarga.
    pub recarga_ms: Option<u64>,
    /// Perfíl ligero activo (batería o CPU alta).
    pub presion: bool,
    /// El margen **con el que se comprobó** este consejo. No es `config.margen_mb`
    /// cuando la sonda conoce el total (ahí se estrecha a la octava parte): el
    /// Selector y el aviso de «no cabe» tienen que citar este número, porque es el
    /// que decidión. Mientras citaron el de la spec, se reportaba «hacen falta
    /// 2400» por una comprobación que pedía 1956.
    #[serde(default)]
    pub margen_mb: u64,
    /// Lo que hay cargado, para el panel y para decidir expulsiones.
    pub cargados: Vec<ModeloCargado>,
    /// Por qué este consejo. Se muestra.
    pub porque: String,
}

pub struct Governor {
    pub config: GovernorConfig,
}

impl Governor {
    pub fn nuevo(config: GovernorConfig) -> Self {
        let mut g = Governor { config };
        g.config.ctx_permitidos.sort_unstable();
        g.config.ctx_permitidos.dedup();
        g
    }

    pub fn por_defecto() -> Self {
        Governor::nuevo(GovernorConfig::default())
    }

    fn bajo_presion(&self, sonda: &dyn ResourceProbe) -> bool {
        if !self.config.perfil_ligero_presion {
            return false;
        }
        let bateria = self
            .config
            .presion_bateria
            .zip(sonda.bateria())
            .map(|(lim, b)| b <= lim)
            .unwrap_or(false);
        let cpu = self
            .config
            .presion_cpu
            .zip(sonda.cpu())
            .map(|(lim, c)| c >= lim)
            .unwrap_or(false);
        bateria || cpu
    }

    /// El margen de §11 son 1500 MB, pero en una máquina de 8,45 GB eso hace
    /// que NUNCA quepa un modelo local: con 1,6 GB libres y un modelo de 878,
    /// pedir 2378 es rechazarlo siempre. Donde se conoce el total, el margen
    /// se estrecha a la octava parte (1056 MB aquí) y se sigue dejando constar
    /// en el aviso; sin ese dato, manda el de la spec.
    pub fn margen_efectivo(&self, total_mb: Option<u64>) -> u64 {
        match total_mb {
            Some(t) if t > 0 => self.config.margen_mb.min(t / 8),
            _ => self.config.margen_mb,
        }
    }

    /// ¿Cabe el modelo pedido, y a qué `num_ctx`? Recorre la escalera de abajo
    /// arriba: el contexto más pequeño que cumpla el nivel gana, porque cada
    /// escalón cuesta RAM y a veces una recarga.
    pub fn aconsejar(&self, sonda: &dyn ResourceProbe, modelo: &ModelInfo, nivel: Level) -> Consejo {
        let libre = sonda.libre_mb();
        let cargados = sonda.cargados();
        let residente_actual = cargados.iter().find(|c| c.id == modelo.id).map(|c| c.num_ctx);
        let presion = self.bajo_presion(sonda);

        let minimo = nivel.num_ctx_minimo();
        let margen = self.margen_efectivo(sonda.total_mb());
        let escalera: Vec<u32> = self
            .config
            .ctx_permitidos
            .iter()
            .filter(|c| **c >= minimo)
            .filter(|c| **c <= modelo.max_ctx)
            .copied()
            .collect();

        if escalera.is_empty() {
            return Consejo {
                num_ctx: 0,
                cabe: false,
                ram_mb: None,
                libre_mb: libre,
                residente: false,
                recarga_ms: None,
                presion,
                margen_mb: margen,
                cargados,
                porque: format!(
                    "{} no admite ningún num_ctx del conjunto permitido para {nivel:?} (máx. declarado {})",
                    modelo.id, modelo.max_ctx
                ),
            };
        }

        // Bajo presión se pide un escalón menos, si aún cumple el nivel.
        let intentos: Vec<u32> = if presion && escalera.len() > 1 {
            escalera[..escalera.len() - 1].to_vec()
        } else {
            escalera.clone()
        };

        for &ctx in &intentos {
            let Some(ram) = modelo.ram_para(ctx) else {
                continue;
            };
            let cabe = match libre {
                // Sin dato de RAM no se afirma que quepa. El Selector lo tratará
                // como «no elegible ahora» y lo dirá con cifras.
                None => false,
                // Si ya está residente a este `num_ctx` no sale RAM nueva: exigir
                // el margen otra vez hacía rechazar el modelo que tenía delante,
                // y expulsarlo y recargarlo cuesta 7,3 s medidos.
                Some(l) => ram + margen <= l || residente_actual == Some(ctx),
            };
            if cabe {
                let residente = residente_actual == Some(ctx);
                let recarga = match residente_actual {
                    Some(otro) if otro != ctx => Some(self.config.recarga_por_ctx_ms),
                    Some(_) => Some(0),
                    None => None,
                };
                return Consejo {
                    num_ctx: ctx,
                    cabe: true,
                    ram_mb: Some(ram),
                    libre_mb: libre,
                    residente,
                    recarga_ms: recarga,
                    presion,
                    margen_mb: margen,
                    cargados,
                    porque: match (residente, recarga) {
                        (true, _) => format!("«{}» ya está residente a {ctx}", modelo.id),
                        (false, Some(ms)) if ms > 0 => format!(
                            "«{}» cabe a {ctx} ({} MB + {} de margen); cambia de ctx: +{ms} ms de recarga",
                            modelo.id,
                            ram,
                            margen
                        ),
                        _ => format!(
                            "«{}» cabe a {ctx}: {} MB + {} MB de margen sobre {} MB libres",
                            modelo.id,
                            ram,
                            margen,
                            libre.unwrap_or(0)
                        ),
                    },
                };
            }
        }

        // No cabe ninguno: se dice con los números que faltan.
        let menor = intentos.first().copied().unwrap_or(0);
        let ram = modelo.ram_para(menor);
        Consejo {
            num_ctx: 0,
            cabe: false,
            ram_mb: ram,
            libre_mb: libre,
            residente: residente_actual.is_some(),
            recarga_ms: None,
            presion,
            margen_mb: margen,
            cargados,
            porque: match (ram, libre) {
                // Las cifras tienen que ser las que se usaron de verdad. Sacar
                // `config.margen_mb` aquí era decir «margen 1500» en una máquina
                // donde el comprobador estrechó el margen a t/8: el aviso no
                // explicaba el rechazo que acababa de producir.
                (Some(r), Some(l)) if margen != self.config.margen_mb => format!(
                    "no cabe: «{}» necesita {} MB a {menor} y quedan {} libres (margen {} de los {} de la spec, estrechado a la octava parte del total)",
                    modelo.id,
                    r,
                    l,
                    margen,
                    self.config.margen_mb
                ),
                (Some(r), Some(l)) => format!(
                    "no cabe: «{}» necesita {} MB a {menor} y quedan {} libres (margen {})",
                    modelo.id,
                    r,
                    l,
                    margen
                ),
                (Some(_), None) => format!("no se puede afirmar que «{}» quepa: la sonda no mide RAM libre", modelo.id),
                (None, _) => format!("«{}» no tiene RAM medida para ningún num_ctx del conjunto", modelo.id),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::Profile;
    use crate::models::ModelKind;
    use crate::resources::SondaFija;
    use std::collections::BTreeMap;

    fn modelo(id: &str, ram_2048: u64, ram_8192: u64, max_ctx: u32) -> ModelInfo {
        let mut m: BTreeMap<u32, u64> = BTreeMap::new();
        m.insert(2048, ram_2048);
        m.insert(4096, ram_2048 + (ram_8192 - ram_2048) / 2);
        m.insert(8192, ram_8192);
        ModelInfo {
            id: id.into(),
            provider: "ollama".into(),
            local: true,
            kind: ModelKind::Generativo,
            profile: Profile::Nano,
            tier: 1,
            ram_mb_by_ctx: m,
            max_ctx,
            strengths: vec![],
            supports_tools: true,
            supports_thinking: false,
            supports_vision: false,
            disco_mb: None,
        }
    }

    #[test]
    fn elige_el_ctx_mas_barato_que_cumple_el_nivel() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(4000),
            ..Default::default()
        };
        let m = modelo("gemma3:1b", 878, 1197, 32768);
        let c = g.aconsejar(&sonda, &m, Level::N1);
        assert!(c.cabe);
        assert_eq!(c.num_ctx, 2048, "N1 no pide más contexto del necesario");
        let c2 = g.aconsejar(&sonda, &m, Level::N2);
        assert_eq!(c2.num_ctx, 4096, "N2 exige 4096 de mínimo");
    }

    #[test]
    fn sin_margen_no_cabe_y_lo_dice_concifras() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(1200),
            ..Default::default()
        };
        let c = g.aconsejar(&sonda, &modelo("qwen3:1.7b", 1645, 2419, 40960), Level::N0);
        assert!(!c.cabe);
        assert!(c.porque.contains("necesita 1645") && c.porque.contains("1200"), "{}", c.porque);
    }

    #[test]
    fn sin_sonda_no_se_afirma_que_quepa() {
        let g = Governor::por_defecto();
        let c = g.aconsejar(&crate::resources::SinSonda, &modelo("a:1b", 900, 1200, 32768), Level::N0);
        assert!(!c.cabe);
        assert!(c.porque.contains("no mide RAM"), "{}", c.porque);
    }

    #[test]
    fn cambiar_de_ctx_cuesta_la_recarga_medida() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(8000),
            cargados: vec![ModeloCargado {
                id: "gemma3:1b".into(),
                ram_mb: 878,
                num_ctx: 2048,
            }],
            ..Default::default()
        };
        let m = modelo("gemma3:1b", 878, 1197, 32768);
        let mismo = g.aconsejar(&sonda, &m, Level::N0);
        assert!(mismo.residente);
        assert_eq!(mismo.recarga_ms, Some(0));
        let subido = g.aconsejar(&sonda, &m, Level::N3);
        assert_eq!(subido.recarga_ms, Some(3700), "cambiar ctx recarga");
        assert!(subido.porque.contains("recarga"));
    }

    #[test]
    fn bateria_baja_baja_un_escalon_el_contexto() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(8000),
            bateria: Some(0.2),
            ..Default::default()
        };
        let m = modelo("gemma3:1b", 878, 1197, 32768);
        let c = g.aconsejar(&sonda, &m, Level::N2);
        assert!(c.presion);
        assert_eq!(c.num_ctx, 4096, "con presión se baja a N2 su mínimo, no a 2048");
        let c3 = g.aconsejar(&sonda, &m, Level::N3);
        // N3 solo tiene un escalón válido (8192): si no hay escalón que ceder, se queda.
        assert_eq!(c3.num_ctx, 8192);
    }

    #[test]
    fn un_modelo_de_ctx_corto_no_alcanza_n3() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(16000),
            ..Default::default()
        };
        let m = modelo("chiquito:1b", 800, 800, 4096);
        let c = g.aconsejar(&sonda, &m, Level::N3);
        assert!(!c.cabe);
        assert!(c.porque.contains("ningún num_ctx"), "{}", c.porque);
    }
}
