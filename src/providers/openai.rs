//! Proveedores OpenAI-compatible: la API de OpenAI y cualquier puerta con el mismo
//! lenguaje (Hugging Face, vLLM, LM Studio, Haticoo…). Van detrás de la feature
//! `openai` (§XVII del Canon).

use super::{
    GenerationRequest, GenerationResult, LlamadaTool, ModelInfo, ModelProvider,
    ModelStream, ProviderError, StreamDelta, ToolSchema,
};
use crate::models::ModelKind;
use crate::api::vocab::{Profile, ThinkingLevel};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::time::Duration;

/// Catálogo público usado solo para describir modelos cuando el servidor no los
/// lista. vacíos→ el registro del producto sigue mandando.
pub const MODELOS_CONOCIDOS: &[(&str, Profile, u8)] = &[
    ("gpt-4o-mini", Profile::Small, 3),
    ("gpt-4.1-mini", Profile::Small, 3),
    ("o4-mini", Profile::Small, 3),
    ("claude-haiku-4-5", Profile::Small, 3),
];

#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    pub base_url: String,
    pub api_key: String,
    pub nombre: String,
    http: reqwest::Client,
}

impl OpenAiProvider {
    pub fn nuevo(api_key: impl Into<String>) -> OpenAiProvider {
        OpenAiProvider::con_base("https://api.openai.com/v1", api_key, "openai")
    }

    pub fn con_base(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        nombre: impl Into<String>,
    ) -> OpenAiProvider {
        OpenAiProvider {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            nombre: nombre.into(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn tiene_clave(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    fn cabecera(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// El cuerpo, aparte del HTTP, para poder escribir su prueba.
    pub fn cuerpo_de(req: &GenerationRequest, nombre_proveedor: &str) -> serde_json::Value {
        let mut mensajes: Vec<serde_json::Value> = Vec::new();
        if !req.system.is_empty() {
            mensajes.push(serde_json::json!({"role":"system","content":req.system}));
        }
        for m in &req.history {
            mensajes.push(serde_json::json!({"role": m.role, "content": m.content}));
        }
        mensajes.push(serde_json::json!({"role":"user","content":req.prompt}));

        let mut cuerpo = serde_json::json!({
            "model": req.model,
            "messages": mensajes,
            "stream": true,
            "temperature": req.temperature,
            // En `/chat/completions` el tope de salida es `max_tokens`; con
            // razonamiento encendido los modelos nuevos solo aceptan
            // `max_completion_tokens`. `max_output_tokens` es de otra API y aquí
            // se ignoraría en silencio, dejando el Plan sin su presupuesto.
            "max_tokens": req.max_output_tokens,
        });
        if req.thinking != ThinkingLevel::Off {
            cuerpo["max_completion_tokens"] = serde_json::json!(req.max_output_tokens);
        }
        // `seed` lo admiten OpenAI y la mayoría de puertas compatibles; si una no
        // lo conoce, el error viene en el status y se reporta.
        cuerpo["seed"] = serde_json::json!(req.seed);
        // `num_ctx` es de Ollama: aquí se traduce a la ventana que el proveedor
        // acepte, que es `max_tokens` para la salida y nada para la entrada.
        if let Some(t) = nivel_a_thinking(&req.thinking, nombre_proveedor) {
            cuerpo["reasoning_effort"] = serde_json::json!(t);
        }
        if !req.tools.is_empty() {
            cuerpo["tools"] = serde_json::json!(
                req.tools
                    .iter()
                    .map(tool_a_openai)
                    .collect::<Vec<_>>()
            );
        }
        cuerpo
    }

    /// Decodifica la respuesta no-stream.
    pub fn interpretar(j: &serde_json::Value, modelo: &str) -> GenerationResult {
        let mut texto = String::new();
        let mut llamadas = Vec::new();
        if let Some(choices) = j.get("choices").and_then(|c| c.as_array()) {
            for c in choices {
                let msg = c.get("message").cloned().unwrap_or_default();
                if let Some(t) = msg.get("content").and_then(|v| v.as_str()) {
                    texto.push_str(t);
                }
                for tc in msg
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    let nombre = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .cloned()
                        .map(deserializar_args)
                        .unwrap_or_else(|| serde_json::json!({}));
                    if !nombre.is_empty() {
                        llamadas.push(LlamadaTool { tool: nombre, args });
                    }
                }
            }
        }
        let u = j.get("usage").cloned().unwrap_or_default();
        GenerationResult {
            texto,
            razonamiento: j
                .pointer("/choices/0/message/reasoning_content")
                .and_then(|v| v.as_str())
                .map(String::from),
            tool_calls: llamadas,
            modelo: j
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or(modelo)
                .to_string(),
            tokens_entrada: u
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .or_else(|| u.get("input_tokens").and_then(|v| v.as_u64())),
            tokens_salida: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .or_else(|| u.get("output_tokens").and_then(|v| v.as_u64())),
            ttft_ms: None,
            tok_s: None,
            carga_ms: Some(0),
        }
    }
}

fn deserializar_args(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => {
            serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
        }
        otro => otro,
    }
}

pub(crate) fn tool_a_openai(t: &ToolSchema) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
    })
}

/// `reasoning_effort` es de OpenAI y sus clones; en Anthropic se traduce a
/// presupuesto de pensamiento.
pub(crate) fn nivel_a_thinking(t: &ThinkingLevel, proveedor: &str) -> Option<&'static str> {
    if *t == ThinkingLevel::Off {
        return None;
    }
    match proveedor {
        "anthropic" => None,
        _ => Some(match t {
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Off => "low",
        }),
    }
}

/// `keep_alive` y `num_ctx` no existen en las APIs de chat: lo que se manda es
/// `messages` + contrato. El Plan los sigue firmando (los necesita el Governor
/// para los locales) y aquí se declaran y se ignoran.
#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn id(&self) -> String {
        self.nombre.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        if !self.tiene_clave() {
            return Err(ProviderError::SinCredencial(self.nombre.clone()));
        }
        let r = self
            .http
            .get(format!("{}/models", self.base_url))
            .timeout(Duration::from_secs(20))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        if !r.status().is_success() {
            let codigo = r.status().as_u16();
            let cuerpo = r.text().await.unwrap_or_default();
            return Err(ProviderError::Status { codigo, cuerpo });
        }
        let j: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ProviderError::RespuestaInvalida(e.to_string()))?;
        let mut out = Vec::new();
        for m in j.get("data").and_then(|d| d.as_array()).into_iter().flatten() {
            let Some(id) = m.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let conocido = MODELOS_CONOCIDOS.iter().find(|(k, _, _)| *k == id);
            out.push(ModelInfo {
                id: id.into(),
                provider: self.nombre.clone(),
                local: false,
                kind: ModelKind::Generativo,
                profile: conocido.map(|(_, p, _)| *p).unwrap_or(Profile::Large),
                tier: conocido.map(|(_, _, t)| *t).unwrap_or(4),
                // No hay forma de medir la RAM de un modelo que no está en tu
                // disco: se deja vacío y el Governor no lo descarta por RAM.
                ram_mb_by_ctx: BTreeMap::new(),
                max_ctx: m
                    .get("context_length")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(128_000),
                strengths: vec![],
                supports_tools: true,
                supports_thinking: id.starts_with("o") || id.contains("think"),
                supports_vision: true,
                disco_mb: None,
            });
        }
        Ok(out)
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationResult, ProviderError> {
        if !self.tiene_clave() {
            return Err(ProviderError::SinCredencial(self.nombre.clone()));
        }
        let mut cuerpo = OpenAiProvider::cuerpo_de(&req, &self.nombre);
        cuerpo["stream"] = serde_json::json!(false);
        let r = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .timeout(Duration::from_secs(req.timeout_s.max(1) as u64))
            .bearer_auth(self.cabecera())
            .json(&cuerpo)
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        let status = r.status();
        if !status.is_success() {
            let codigo = status.as_u16();
            let texto = r.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                codigo,
                cuerpo: texto,
            });
        }
        let j: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ProviderError::RespuestaInvalida(e.to_string()))?;
        Ok(OpenAiProvider::interpretar(&j, &req.model))
    }

    async fn stream(&self, req: GenerationRequest) -> Result<ModelStream, ProviderError> {
        if !self.tiene_clave() {
            return Err(ProviderError::SinCredencial(self.nombre.clone()));
        }
        let cuerpo = OpenAiProvider::cuerpo_de(&req, &self.nombre);
        let r = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(self.cabecera())
            .header("accept", "text/event-stream")
            .json(&cuerpo)
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        if !r.status().is_success() {
            let codigo = r.status().as_u16();
            let texto = r.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                codigo,
                cuerpo: texto,
            });
        }
        let lineas = super::sse_hasta_final(r).await?;
        let mut partes: Vec<Result<StreamDelta, ProviderError>> = Vec::new();
        for l in &lineas {
            if let Some(t) = l
                .pointer("/choices/0/delta/content")
                .and_then(|v| v.as_str())
            {
                if !t.is_empty() {
                    partes.push(Ok(StreamDelta::Texto(t.to_string())));
                }
            }
            if let Some(t) = l
                .pointer("/choices/0/delta/reasoning_content")
                .and_then(|v| v.as_str())
            {
                if !t.is_empty() {
                    partes.push(Ok(StreamDelta::Razonamiento(t.to_string())));
                }
            }
        }
        let completo = super::json_de_sse(&lineas);
        let mut final_r = OpenAiProvider::interpretar(&completo, &req.model);
        if final_r.modelo.is_empty() {
            final_r.modelo = req.model.clone();
        }
        partes.push(Ok(StreamDelta::Final(final_r)));
        Ok(Box::pin(futures_util::stream::iter(partes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::request::Message;
    use crate::api::vocab::KeepAlive;

    fn req() -> GenerationRequest {
        GenerationRequest {
            model: "gpt-4o-mini".into(),
            system: "Eres Hatboo.".into(),
            prompt: "hola".into(),
            history: vec![Message::usuario("¿que hora es?")],
            tools: vec![ToolSchema {
                name: "read_file".into(),
                description: "lee".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            num_ctx: 4096,
            keep_alive: KeepAlive::PorDefecto,
            thinking: ThinkingLevel::Off,
            max_output_tokens: 512,
            temperature: 0.0,
            seed: 42,
            timeout_s: 60,
        }
    }

    #[test]
    fn el_cuerpo_usa_el_habla_de_openai() {
        let c = OpenAiProvider::cuerpo_de(&req(), "openai");
        assert_eq!(c["model"], "gpt-4o-mini");
        assert_eq!(c["max_tokens"], 512);
        assert!(c.get("max_output_tokens").is_none(), "ese campo es de otra API");
        assert!(c.get("max_completion_tokens").is_none(), "con thinking off, no va");
        assert_eq!(c["seed"], 42);
        assert_eq!(c["messages"].as_array().unwrap().len(), 3);
        assert!(c.get("reasoning_effort").is_none());
        assert_eq!(c["tools"].as_array().unwrap()[0]["function"]["name"], "read_file");
    }

    #[test]
    fn thinking_va_como_reasoning_effort() {
        let mut r = req();
        r.thinking = ThinkingLevel::High;
        let c = OpenAiProvider::cuerpo_de(&r, "openai");
        assert_eq!(c["reasoning_effort"], "high");
        assert_eq!(c["max_completion_tokens"], 512);
        assert_eq!(nivel_a_thinking(&r.thinking, "anthropic"), None);
    }

    #[test]
    fn interpretar_lee_usage_y_herramientas() {
        let j = serde_json::json!({
            "model":"gpt-4o-mini-2024",
            "choices":[{"message":{"content":"hola","tool_calls":[{"function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]}}],
            "usage":{"prompt_tokens":19,"completion_tokens":12}
        });
        let r = OpenAiProvider::interpretar(&j, "gpt-4o-mini");
        assert_eq!(r.texto, "hola");
        assert_eq!(r.tokens_entrada, Some(19));
        assert_eq!(r.tokens_salida, Some(12));
        assert_eq!(r.tool_calls[0].args["path"], "a.rs");
        assert_eq!(r.carga_ms, Some(0), "la API no carga nada en tu máquina");
    }

    #[test]
    fn sin_clave_no_se_intenta_ni_un_http() {
        let p = OpenAiProvider::nuevo("   ");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let e = rt.block_on(p.list_models()).unwrap_err();
        assert_eq!(e, ProviderError::SinCredencial("openai".into()));
        assert!(!p.tiene_clave());
    }

    #[test]
    fn keep_alive_y_num_ctx_no_salen_hacia_la_api() {
        // Lo que el Plan firma para un local no puede colarse en el cuerpo de una
        // API: la sesión y la RAM las administra el proveedor, no nosotros.
        let mut r = req();
        r.keep_alive = KeepAlive::Expulsar;
        r.num_ctx = 8192;
        let c = OpenAiProvider::cuerpo_de(&r, "openai");
        assert!(c.get("keep_alive").is_none(), "{c}");
        assert!(c.get("num_ctx").is_none(), "{c}");
    }
}
