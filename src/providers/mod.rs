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
    /// Los manda el producto por `BrainConfig` (`temperatura` / `semilla`).
    /// `None` = **no se manda la clave**, que es lo que hace un chat normal: deja
    /// decidir al proveedor (Ollama usa 0,8 por defecto). `Some(0.0)` con
    /// `Some(42)` es el protocolo de la Fase 0, medido: el recuento de tokens sale
    /// idéntico corrida a corrida. Estuvo fijo en el runtime, y un chat con un
    /// modelo de 1B en decodificación voraz repite la misma frase literal.
    pub temperature: Option<f32>,
    pub seed: Option<u64>,
    /// Fase 6. `true` pide al proveedor los log-probabilities de lo que genera, y
    /// el resultado devuelve `logprob_medio`. No cambia ninguna decisión: §1 del
    /// plan deja el origen estadístico en **solo registro** hasta que haya
    /// calibración medida, y ningún origen revierte un Pass/Fail determinista.
    pub logprobs: bool,
    pub timeout_s: u32,
    /// El Plan firmó un contrato `Json`. No es un capricho del prompt: en Ollama
    /// es la diferencia entre que `gemma3:1b` devuelva ```` ```json ```` envuelto
    /// (medido el 03-10, no parsea como JSON) o devuelva JSON suelto que sí
    /// parsea. Con `structured_output` del registry a `false` se pide `"json"` a
    /// secas; el `json_schema` de Ollama contestó **400** en los dos modelos
    /// locales, así que no se manda nunca desde aquí.
    pub salida_json: bool,
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
            temperature: None,
            seed: None,
            logprobs: false,
            timeout_s: 60,
            salida_json: false,
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
    /// El proveedor **cortó la salida por el techo de tokens** (`done_reason:
    /// "length"` en Ollama, `finish_reason: "length"` en las puertas
    /// OpenAI-compatibles, `stop_reason: "max_tokens"` en Anthropic). Importa
    /// porque un JSON cortado no es un problema de formato: reintentarlo con el
    /// mismo techo reproduce el corte, y eso son dos llamadas al modelo que no
    /// podían salir bien.
    #[serde(default)]
    pub truncado: bool,
    /// Fase 6: media de los log-probabilities de los tokens generados, en nats
    /// (negativos; 0 sería certeza absoluta). `None` = el proveedor no los mandó
    /// o no los tiene. Para la probabilidad del turno, `exp_de_logprob`.
    #[serde(default)]
    pub logprob_medio: Option<f32>,
}

/// Media de una lista de log-probabilities. `None` con la lista vacía: un
/// proveedor que no mandó nada no es un modelo seguro de salida 0.
pub fn media_logprobs(valores: &[f32]) -> Option<f32> {
    if valores.is_empty() {
        return None;
    }
    let suma: f32 = valores.iter().sum();
    Some(suma / valores.len() as f32)
}

/// La probabilidad que corresponde a una media de log-probabilities: la media
/// geométrica de las probabilidades por token. No es la probabilidad de que la
/// respuesta sea cierta, y no se vende como tal.
pub fn exp_de_logprob(logprob_medio: Option<f32>) -> Option<f32> {
    logprob_medio.map(|l| l.exp())
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
    let mut fin = String::new();
    // Fase 6: cada chunk puede traer `choices[0].logprobs.content` con los
    // log-probabilities de sus tokens. Se cosen en el objeto único que consume
    // `interpretar`, igual que se cose el texto.
    let mut registros: Vec<f32> = Vec::new();
    for e in eventos {
        if let Some(cs) = e
            .pointer("/choices/0/logprobs/content")
            .and_then(|v| v.as_array())
        {
            for c in cs {
                if let Some(l) = c.get("logprob").and_then(|v| v.as_f64()) {
                    registros.push(l as f32);
                }
            }
        }
        if let Some(m) = e.get("model").and_then(|v| v.as_str()) {
            modelo = m.to_string();
        }
        if let Some(u) = e.get("usage") {
            uso = u.clone();
        }
        // `finish_reason` viene como hermana del `delta`, en el último chunk.
        if let Some(f) = e.pointer("/choices/0/finish_reason").and_then(|v| v.as_str()) {
            fin = f.to_string();
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
    if !registros.is_empty() {
        v["choices"][0]["logprobs"] = serde_json::json!({
            "content": registros
                .iter()
                .map(|l| serde_json::json!({"logprob": l}))
                .collect::<Vec<_>>()
        });
    }
    if !fin.is_empty() {
        // Sin esta línea el stream perdía el aviso: `interpretar` mira
        // `finish_reason` para saber si la salida la cortó el techo.
        v["choices"][0]["finish_reason"] = serde_json::Value::String(fin);
    }
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
