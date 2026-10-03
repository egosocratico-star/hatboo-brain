//! El registry (**describe**) y el **Selector** (elige). Son dos papeles y no se
//! mezclan: el registry no decide nada, y el Selector no inventa modelos que no
//! estén descritos.

pub mod selector;

use crate::api::vocab::{ExecutionPolicy, ExecutionTarget, Level, ModelId, Profile, ProviderId};
use serde::{Deserialize, Serialize};

/// `kind: decision` nunca responde al usuario (Fase 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    /// El JSON de registro del Canon dice «generative»; el crate habla español.
    #[serde(alias = "generative")]
    Generativo,
    Decision,
}

/// Un modelo descrito. Los números salen de **medir**, no de la ficha: §16 del
/// plan avisa de que `supports_tools`/`supports_thinking` declarados no son
/// reales en modelos chicos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelInfo {
    pub id: ModelId,
    pub provider: ProviderId,
    /// `false` = sale del equipo (API o `:cloud` de Ollama).
    pub local: bool,
    pub kind: ModelKind,
    pub profile: Profile,
    /// Orden de escalado. `tier` 1 es el más barato.
    pub tier: u8,
    /// RAM residente medida por `num_ctx` (el KV cache la cambia). Clave: el
    /// `num_ctx` en tokens; valor: MB. Si falta una clave, no se puede afirmar
    /// que quepa: el Selector la descarta y lo dice.
    #[serde(default)]
    pub ram_mb_by_ctx: std::collections::BTreeMap<u32, u64>,
    pub max_ctx: u32,
    #[serde(default)]
    pub strengths: Vec<String>,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub supports_vision: bool,
    /// Si el modelo sostiene una salida **ceñida a un esquema JSON** (§X del
    /// Canon). Como los otros `supports_*`, vale lo medido: `false` significa
    /// «no probado o probado y no», nunca «supongo que sí». Ollama no declara
    /// esta capacidad en local (medido el 03-10: `gemma3:1b` →
    /// `["completion"]`, `qwen3:1.7b` → `["completion","tools","thinking"]`),
    /// así que lo que la pone a `true` es la sonda de `--sondear`.
    #[serde(default)]
    pub structured_output: bool,
    /// Lo que ocupa **en disco**, para cuando nadie midió la RAM residente. Es una
    /// estimación y se dice como tal: §X del Canon quiere `ram_mb_by_ctx` medido.
    #[serde(default)]
    pub disco_mb: Option<u64>,
}

impl ModelInfo {
    /// RAM que costaría cargarlo a este `num_ctx`. Solo dice algo lo que está
    /// medido a ese ctx; si el modelo tiene mediciones pero ninguna en el escalón
    /// pedido, devuelve `None` y el Governor lo descarta **diciendo por qué**:
    /// extrapolar el KV cache es justo el error que §X del Canon quiere evitar.
    /// El tamaño en disco entra únicamente cuando nadie midió nunca ese modelo.
    pub fn ram_para(&self, num_ctx: u32) -> Option<u64> {
        match self.ram_mb_by_ctx.get(&num_ctx).copied() {
            Some(r) => Some(r),
            None if self.ram_mb_by_ctx.is_empty() => self.disco_mb,
            None => None,
        }
    }

    /// `true` si la cifra que daría `ram_para` para ese ctx no está medida.
    pub fn ram_es_estimada(&self, num_ctx: u32) -> bool {
        self.ram_mb_by_ctx.is_empty() && self.disco_mb.is_some() && self.ram_para(num_ctx).is_some()
    }

    pub fn es_nube(&self) -> bool {
        !self.local
    }

    /// Perfil a partir de parámetros medidos (nano ≤2B, small 3–9B, large >9B).
    pub fn perfil_desde_parametros(parametros: Option<f64>) -> Profile {
        match parametros {
            Some(p) if p <= 2.0 => Profile::Nano,
            Some(p) if p <= 9.0 => Profile::Small,
            _ => Profile::Large,
        }
    }
}

/// Lo que el Governor encontró cargado en el servidor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeloCargado {
    pub id: ModelId,
    pub ram_mb: u64,
    pub num_ctx: u32,
}

/// El registry de la máquina. Vive en el directorio de config del usuario; en el
/// repo solo viaja `models.example.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub modelos: Vec<ModelInfo>,
}

impl Registry {
    pub fn nuevo(modelos: Vec<ModelInfo>) -> Self {
        Registry { modelos }
    }

    pub fn desde_json(texto: &str) -> Result<Registry, serde_json::Error> {
        serde_json::from_str(texto)
    }

    pub fn a_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn find(&self, id: &str) -> Option<&ModelInfo> {
        self.modelos.iter().find(|m| m.id == id)
    }

    /// Cumple lo que el nivel pide, sin mirar la RAM (eso lo añade el Governor).
    pub fn elegibles(
        &self,
        level: Level,
        necesita_tools: bool,
        policy: ExecutionPolicy,
    ) -> Vec<&ModelInfo> {
        self.modelos
            .iter()
            .filter(|m| m.kind == ModelKind::Generativo)
            .filter(|m| !necesita_tools || m.supports_tools)
            .filter(|m| match policy {
                ExecutionPolicy::LocalOnly | ExecutionPolicy::LocalPreferred => m.local,
                ExecutionPolicy::CloudOnly => !m.local,
                _ => true,
            })
            .filter(|m| m.max_ctx >= level.num_ctx_minimo())
            .collect()
    }

    /// Los que además **caben** ahora mismo: el criterio es la RAM libre real
    /// menos el margen, no el tamaño en disco. Medido en Fase 0: la cuantización
    /// pesa más que el número de parámetros, así que deducirlo miente.
    pub fn caben<'a>(
        &self,
        candidatos: impl Iterator<Item = &'a ModelInfo>,
        num_ctx: u32,
        libre_mb: u64,
        margen_mb: u64,
    ) -> Vec<&'a ModelInfo> {
        candidatos
            .filter(|m| {
                m.ram_para(num_ctx)
                    .map(|ram| ram + margen_mb <= libre_mb)
                    .unwrap_or(false)
            })
            .collect()
    }
}

/// El destino de ejecución que implica un modelo: local si el modelo es local.
pub fn target_de(modelo: &ModelInfo) -> ExecutionTarget {
    if modelo.local {
        ExecutionTarget::Local
    } else {
        ExecutionTarget::Api
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, tier: u8, ram: u64, ctx: u32, local: bool, tools: bool) -> ModelInfo {
        let mut ram_map = std::collections::BTreeMap::new();
        ram_map.insert(2048, ram);
        ram_map.insert(4096, ram + 200);
        ModelInfo {
            id: id.into(),
            provider: if local { "ollama".into() } else { "openai".into() },
            local,
            kind: ModelKind::Generativo,
            profile: Profile::Nano,
            tier,
            ram_mb_by_ctx: ram_map,
            max_ctx: ctx,
            strengths: vec![],
            supports_tools: tools,
            supports_thinking: false,
            supports_vision: false,
            structured_output: false,
            disco_mb: None,
        }
    }

    #[test]
    fn el_registry_ordena_por_tier_y_descarta_lo_que_no_cabe() {
        let r = Registry::nuevo(vec![
            m("cara:9b", 3, 6000, 8192, true, true),
            m("nano:0.8b", 1, 900, 32768, true, false),
            m("medio:4b", 2, 3000, 8192, true, true),
        ]);
        let elegibles = r.elegibles(Level::N2, true, ExecutionPolicy::LocalOnly);
        assert_eq!(elegibles.len(), 2, "solo los que declaran tools");
        let caben = r.caben(elegibles.into_iter(), 4096, 3500, 1500);
        assert!(caben.is_empty(), "con 3,5 GB libres no cabe ningún N2");
        // Con 5 GB libres y 1,5 de margen quedan 3,5: lo justo para el de 4B.
        let caben2 = r.caben(
            r.elegibles(Level::N2, true, ExecutionPolicy::LocalOnly)
                .into_iter(),
            4096,
            5000,
            1500,
        );
        assert_eq!(caben2.len(), 1);
        assert_eq!(caben2[0].id, "medio:4b");
    }

    #[test]
    fn sin_dato_de_ram_no_se_afirma_que_cabe() {
        let mut sin_medir = m("nuevo:1b", 1, 900, 8192, true, false);
        sin_medir.ram_mb_by_ctx.clear();
        let r = Registry::nuevo(vec![sin_medir]);
        let caben = r.caben(
            r.elegibles(Level::N1, false, ExecutionPolicy::LocalOnly).into_iter(),
            2048,
            8000,
            1500,
        );
        assert!(caben.is_empty());
    }

    #[test]
    fn la_policy_cierra_la_salida_del_equipo() {
        let r = Registry::nuevo(vec![
            m("local:1b", 1, 900, 8192, true, true),
            m("nube:gpt", 2, 0, 8192, false, true),
        ]);
        let solo_local = r.elegibles(Level::N1, false, ExecutionPolicy::LocalOnly);
        assert_eq!(solo_local.len(), 1);
        let solo_nube = r.elegibles(Level::N1, false, ExecutionPolicy::CloudOnly);
        assert_eq!(solo_nube.len(), 1);
        assert_eq!(solo_nube[0].id, "nube:gpt");
    }

    #[test]
    fn viaja_en_el_json_del_doc() {
        let j = r#"{
          "id": "qwen3.5:0.8b",
          "provider": "ollama",
          "local": true,
          "kind": "generative",
          "profile": "nano",
          "tier": 1,
          "ram_mb_by_ctx": { "2048": 1063, "8192": 1197 },
          "max_ctx": 262144,
          "strengths": ["chat"],
          "supports_tools": true,
          "supports_thinking": true
        }"#;
        let m: ModelInfo = serde_json::from_str(j).unwrap();
        assert_eq!(m.ram_para(8192), Some(1197));
        assert_eq!(m.ram_para(4096), None);
        assert_eq!(m.kind, ModelKind::Generativo);
        assert!(m.supports_thinking);
        assert!(!m.supports_vision);
    }
}
