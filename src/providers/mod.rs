//! Un trait, no dos cerebros (§X del Canon). Todo lo que sabe el Brain de un
//! proveedor es esto: describir modelos, generar, transmitir, y decir qué tiene
//! cargado.

use super::api::request::Message;
use super::api::vocab::{KeepAlive, ModelId, ProviderId, ThinkingLevel, ToolId};
use super::security::redact;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

/// Un trozo de stream. El `Final` lleva las cuentas medidas: lo que el producto
/// pinta en el panel.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamDelta {
    Texto(String),
    Razonamiento(String),
    Final(GenerationResult),
}

pub type ModelStream = BoxStream<'static, Result<StreamDelta, ProviderError>>;

/// La definición de tool que ve el modelo. La ejecuta el producto; el Brain solo
/// la describe y la puerta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: ToolId,
    #[serde(default)]
    pub description: String,
    /// JSON Schema de los argumentos.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationRequest {
    pub model: ModelId,
    pub system: String,
    pub prompt: String,
    /// Historial de **esta** sesión, ya recortado por el presupuesto.
    pub history: Vec<Message>,
    /// Exactamente `plan.tools`. Una llamada fuera de esto se rechaza en código.
    pub tools: Vec<ToolSchema>,
    pub num_ctx: u32,
    pub keep_alive: KeepAlive,
    pub thinking: ThinkingLevel,
    pub max_output_tokens: u32,
    /// Fijos para que el bench sea reproducible. Medido: con temperature 0 y seed
    /// 42 el recuento de tokens sale idéntico corrida a corrida.
    pub temperature: f32,
    pub seed: u64,
    pub timeout_s: u32,
}

impl GenerationRequest {
    pub fn nuevo(model: impl Into<ModelId>, system: impl Into<String>, prompt: impl Into<String>) -> Self {
        GenerationRequest {
            model: model.into(),
            system: system.into(),
            prompt: prompt.into(),
            history: Vec::new(),
            tools: Vec::new(),
            num_ctx: 2048,
            keep_alive: KeepAlive::PorDefecto,
            thinking: ThinkingLevel::Off,
            max_output_tokens: 512,
            temperature: 0.0,
            seed: 42,
            timeout_s: 60,
        }
    }

    /// El techo que se manda al proveedor por la línea: la respuesta que firmó el
    /// Plan **más** el razonamiento que §11 reservó para `thinking`. En las cuatro
    /// APIs el razonamiento sale del mismo bote que la respuesta, así que mandar
    /// solo `max_output_tokens` era quedarse con lo que sobrara: en Anthropic con
    /// `medium` y un N2 de 1024, un token de respuesta.
    pub fn tope_de_generacion(&self) -> u32 {
        self.max_output_tokens
            .saturating_add(self.thinking.presupuesto_tokens())
    }
}

/// Lo que salió. Ningún número aquí es inventado: si el proveedor no lo declara,
/// es `None`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GenerationResult {
    pub texto: String,
    #[serde(default)]
    pub razonamiento: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<LlamadaTool>,
    pub modelo: ModelId,
    #[serde(default)]
    pub tokens_entrada: Option<u64>,
    #[serde(default)]
    pub tokens_salida: Option<u64>,
    /// Tiempo hasta el primer token, medido por nosotros.
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    /// Tokens por segundo de decodificación, medidos por nosotros.
    #[serde(default)]
    pub tok_s: Option<f32>,
    /// Cuánto tardó en cargarse el modelo. 0 si ya estaba residente. Medido en
    /// Fase 0: ~7,3 s en frío, 2,4 s con el caché tibio, 3,7 s si cambia num_ctx.
    #[serde(default)]
    pub carga_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlamadaTool {
    pub tool: ToolId,
    pub args: serde_json::Value,
}

/// Errores de proveedor. El cuerpo de la respuesta se redacta antes de salir: un
/// 401 de una API suele reenviar la clave en el mensaje.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ProviderError {
    #[serde(with = "crate::security::string_serde")]
    #[error("no se pudo hablar con el proveedor: {0}")]
    Transporte(String),
    /// El servidor devolvió un código que no es 2xx.
    #[error("el proveedor respondió {codigo}: {cuerpo}")]
    Status {
        codigo: u16,
        #[serde(with = "crate::security::string_serde")]
        cuerpo: String,
    },
    /// La respuesta llegó pero no es lo que se esperaba (JSON roto, campo que
    /// falta, stream cortado).
    #[serde(with = "crate::security::string_serde")]
    #[error("respuesta inválida: {0}")]
    RespuestaInvalida(String),
    /// El modelo pedido no está instalado en el servidor.
    #[error("el modelo «{0}» no está instalado")]
    ModeloNoInstalado(ModelId),
    /// Falta la clave para un proveedor de API.
    #[error("falta la clave de {0}")]
    SinCredencial(ProviderId),
    #[error("el proveedor tardó demasiado")]
    TiempoFuera,
    #[error("cancelado")]
    Cancelado,
    /// Ruta que el crate todavía no implementa. Se dice en vez de hacer una
    /// petición muda que fallaría con un error de otro tipo.
    #[error("no implementado: {0}")]
    NoImplementado(String),
}

impl ProviderError {
    /// Todo error que sale hacia el usuario o el log pasa por aquí.
    pub fn redactado(e: ProviderError) -> ProviderError {
        match e {
            ProviderError::Transporte(s) => ProviderError::Transporte(redact::texto(&s)),
            ProviderError::Status { codigo, cuerpo } => ProviderError::Status {
                codigo,
                cuerpo: redact::texto(&cuerpo),
            },
            ProviderError::RespuestaInvalida(s) => {
                ProviderError::RespuestaInvalida(redact::texto(&s))
            }
            otro => otro,
        }
    }

    pub fn es_no_recuperable(&self) -> bool {
        matches!(
            self,
            ProviderError::SinCredencial(_) | ProviderError::ModeloNoInstalado(_)
        )
    }
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// Describe: lista lo que hay con lo que se puede medir de cada uno.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationResult, ProviderError>;

    async fn stream(&self, req: GenerationRequest) -> Result<ModelStream, ProviderError>;

    /// Qué tiene cargado el servidor y cuánto ocupa. El Governor lo lee antes de
    /// proponer cargar nada. Los proveedores sin esta idea devuelven vacío.
    async fn cargados(&self) -> Result<Vec<ModeloCargado>, ProviderError> {
        Ok(Vec::new())
    }

    /// Expulsar para devolver RAM al sistema.
    async fn expulsar(&self, _model: &str) -> Result<(), ProviderError> {
        Ok(())
    }
}

use super::models::{ModelInfo, ModeloCargado};

/// Lee un `text/event-stream` entero y devuelve los JSON de sus `data:`. Se hace
/// todo en memoria porque el `BrainResult` necesita el total para verificar: el
/// stream de red y el stream que ve el usuario son dos cosas distintas.
pub(crate) async fn sse_hasta_final(
    resp: reqwest::Response,
) -> Result<Vec<serde_json::Value>, ProviderError> {
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut colchon = String::new();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| ProviderError::Transporte(e.to_string()))?;
        colchon.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(i) = colchon.find("\n\n") {
            let bloque = colchon[..i].to_string();
            colchon.drain(..i + 2);
            for linea in bloque.lines() {
                let Some(data) = linea.strip_prefix("data:") else {
                    continue;
                };
                let d = data.trim();
                if d.is_empty() || d == "[DONE]" {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(d)
                    .map_err(|e| ProviderError::RespuestaInvalida(format!("{e}: {d}")))?;
                out.push(v);
            }
        }
    }
    if out.is_empty() {
        return Err(ProviderError::RespuestaInvalida(
            "el stream no traía ningún evento".into(),
        ));
    }
    Ok(out)
}

/// Junta los trozos de un stream SSE de chat en un solo objeto con la forma de una
/// respuesta no-stream, para reutilizar el decodificador.
pub(crate) fn json_de_sse(eventos: &[serde_json::Value]) -> serde_json::Value {
    let mut texto = String::new();
    let mut razon = String::new();
    // `index` → (nombre acumulado, argumentos acumulados).
    let mut fragmentos: std::collections::BTreeMap<usize, (String, String)> =
        std::collections::BTreeMap::new();
    let mut modelo = String::new();
    let mut uso = serde_json::json!({});
    for e in eventos {
        if let Some(m) = e.get("model").and_then(|v| v.as_str()) {
            modelo = m.to_string();
        }
        if let Some(u) = e.get("usage") {
            uso = u.clone();
        }
        let Some(delta) = e.pointer("/choices/0/delta") else {
            continue;
        };
        if let Some(t) = delta.get("content").and_then(|v| v.as_str()) {
            texto.push_str(t);
        }
        if let Some(t) = delta
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()))
        {
            razon.push_str(t);
        }
        for tc in delta
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            // OpenAI parte la llamada en trozos con un `index`: hay que unirlos,
            // no agregarlos, o salen N llamadas inventadas por cada fragmento.
            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let entrada = fragmentos.entry(idx).or_insert_with(|| (String::new(), String::new()));
            if let Some(n) = tc
                .pointer("/function/name")
                .and_then(|v| v.as_str())
            {
                entrada.0.push_str(n);
            }
            if let Some(a) = tc
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
            {
                entrada.1.push_str(a);
            }
        }
    }
    let llamadas: Vec<serde_json::Value> = fragmentos
        .into_values()
        .filter(|(n, _)| !n.is_empty())
        .map(|(n, a)| {
            serde_json::json!({"function": {"name": n, "arguments": a}})
        })
        .collect();
    let mut mensaje = serde_json::json!({"role":"assistant","content":texto});
    if !razon.is_empty() {
        mensaje["reasoning_content"] = serde_json::Value::String(razon);
    }
    if !llamadas.is_empty() {
        mensaje["tool_calls"] = serde_json::Value::Array(llamadas);
    }
    let mut v = serde_json::json!({"choices":[{"message":mensaje}]});
    if !uso.is_null() {
        v["usage"] = uso;
    }
    if !modelo.is_empty() {
        v["model"] = serde_json::Value::String(modelo);
    }
    v
}

pub mod mock;
#[cfg(feature = "ollama")]
pub mod ollama;
#[cfg(feature = "openai")]
pub mod openai;
#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "generic")]
pub mod generic;

pub use mock::MockProvider;
#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicProvider;
#[cfg(feature = "generic")]
pub use generic::GenericProvider;
#[cfg(feature = "ollama")]
pub use ollama::OllamaProvider;
#[cfg(feature = "openai")]
pub use openai::OpenAiProvider;
