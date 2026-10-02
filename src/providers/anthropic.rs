//! Anthropic: `/v1/messages`, con `system` fuera de `messages` y pensamiento
//! presupuestado. Detrás de la feature `anthropic`.

use super::{
    GenerationRequest, GenerationResult, LlamadaTool, ModelInfo, ModelProvider,
    ModelStream, ProviderError, StreamDelta,
};
use crate::api::vocab::{Profile, ThinkingLevel};
use async_trait::async_trait;
use std::time::Duration;

const VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    pub base_url: String,
    pub api_key: String,
    http: reqwest::Client,
}

impl AnthropicProvider {
    pub fn nuevo(api_key: impl Into<String>) -> AnthropicProvider {
        AnthropicProvider {
            base_url: "https://api.anthropic.com".into(),
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn con_base(base_url: impl Into<String>, api_key: impl Into<String>) -> AnthropicProvider {
        AnthropicProvider {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            ..AnthropicProvider::nuevo(api_key)
        }
    }

    pub fn tiene_clave(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    /// El presupuesto de pensamiento tiene que quedar **por debajo** de
    /// `max_tokens`: la API lo exige, y si no cabe da 400. Medido en el código de
    /// Hatboo (`anthropic.rs:94`), que ya hace `budget + 4096`.
    pub fn cuerpo_de(req: &GenerationRequest) -> serde_json::Value {
        let mensajes: Vec<serde_json::Value> = req
            .history
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| {
                serde_json::json!({
                    "role": if m.role == "assistant" { "assistant" } else { "user" },
                    "content": m.content,
                })
            })
            .chain(std::iter::once(serde_json::json!({
                "role": "user", "content": req.prompt,
            })))
            .collect();

        let mut cuerpo = serde_json::json!({
            "model": req.model,
            "messages": mensajes,
            "max_tokens": req.max_output_tokens,
            "stream": true,
            "temperature": req.temperature,
        });
        if !req.system.is_empty() {
            cuerpo["system"] = serde_json::Value::String(req.system.clone());
        }
        if req.thinking != ThinkingLevel::Off {
            let presupuesto = match req.thinking {
                ThinkingLevel::Low => 1024u32,
                ThinkingLevel::Medium => 4096,
                ThinkingLevel::High => 10_240,
                ThinkingLevel::Off => 0,
            };
            // Nunca se pasa del tope de salida: se baja el presupuesto, no se sube
            // el tope a escondidas.
            let seguro = presupuesto.min(req.max_output_tokens.saturating_sub(1));
            cuerpo["thinking"] = serde_json::json!({
                "type": "enabled", "budget_tokens": seguro
            });
            // Con pensamiento activado la API no admite temperature != 1.
            cuerpo["temperature"] = serde_json::json!(1.0);
        }
        if !req.tools.is_empty() {
            cuerpo["tools"] = serde_json::json!(
                req.tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "name": t.name, "description": t.description, "input_schema": t.parameters
                    }))
                    .collect::<Vec<_>>()
            );
        }
        // `seed` no existe en Anthropic: no se manda, y el bench lo sabe.
        cuerpo
    }

    pub fn interpretar(j: &serde_json::Value, modelo: &str) -> GenerationResult {
        let mut texto = String::new();
        let mut razon = String::new();
        let mut llamadas = Vec::new();
        for b in j.get("content").and_then(|c| c.as_array()).into_iter().flatten() {
            match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "text" => {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        texto.push_str(t);
                    }
                }
                "thinking" => {
                    if let Some(t) = b.get("thinking").and_then(|v| v.as_str()) {
                        razon.push_str(t);
                    }
                }
                "tool_use" => llamadas.push(LlamadaTool {
                    tool: b
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string(),
                    args: b.get("input").cloned().unwrap_or(serde_json::json!({})),
                }),
                _ => {}
            }
        }
        let u = j.get("usage").cloned().unwrap_or_default();
        GenerationResult {
            texto,
            razonamiento: if razon.is_empty() { None } else { Some(razon) },
            tool_calls: llamadas,
            modelo: j.get("model").and_then(|m| m.as_str()).unwrap_or(modelo).to_string(),
            tokens_entrada: u.get("input_tokens").and_then(|v| v.as_u64()),
            tokens_salida: u.get("output_tokens").and_then(|v| v.as_u64()),
            ttft_ms: None,
            tok_s: None,
            carga_ms: Some(0),
        }
    }

    /// Junta los eventos `content_block_delta` en un objeto con la forma de una
    /// respuesta normal, para reutilizar `interpretar`.
    fn juntar(eventos: &[serde_json::Value]) -> serde_json::Value {
        let mut bloques: Vec<serde_json::Value> = Vec::new();
        let mut uso = serde_json::json!({});
        let mut modelo = String::new();
        for e in eventos {
            if let Some(m) = e.pointer("/message/model").and_then(|v| v.as_str()) {
                modelo = m.to_string();
            }
            // El `usage` llega repartido: `input_tokens` en message_start y
            // `output_tokens` en message_delta. Sustituir perdería la entrada.
            if let Some(u) = e.get("usage").filter(|u| !u.is_null()) {
                let mut combinado = uso.as_object().cloned().unwrap_or_default();
                for (k, v) in u.as_object().into_iter().flatten() {
                    combinado.insert(k.clone(), v.clone());
                }
                uso = serde_json::Value::Object(combinado);
            }
            if let Some(u) = e.pointer("/delta/usage") {
                let combinado = uso.as_object().cloned().unwrap_or_default();
                let mut nuevo = combinado;
                for (k, v) in u.as_object().into_iter().flatten() {
                    nuevo.insert(k.clone(), v.clone());
                }
                uso = serde_json::Value::Object(nuevo);
            }
            let tipo = e.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let delta = e.get("delta").cloned().unwrap_or_default();
            let indice = e.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            while bloques.len() <= indice {
                bloques.push(serde_json::json!({"type":"text","text":"","thinking":"","input":{},"nombre":""}));
            }
            let b = &mut bloques[indice];
            match (tipo, delta.get("type").and_then(|t| t.as_str()).unwrap_or("")) {
                ("content_block_start", _) => {
                    if let Some(t) = delta.pointer("/tool_use/type").or_else(|| delta.get("type")) {
                        b["type"] = t.clone();
                    }
                    if let Some(n) = delta.get("name").and_then(|v| v.as_str()) {
                        b["nombre"] = serde_json::Value::String(n.to_string());
                    }
                }
                (_, "text_delta") => {
                    b["type"] = serde_json::json!("text");
                    if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                        let mut viejo = b["text"].as_str().unwrap_or("").to_string();
                        viejo.push_str(t);
                        b["text"] = serde_json::Value::String(viejo);
                    }
                }
                (_, "thinking_delta") => {
                    b["type"] = serde_json::json!("thinking");
                    if let Some(t) = delta.get("thinking").and_then(|v| v.as_str()) {
                        let mut viejo = b["thinking"].as_str().unwrap_or("").to_string();
                        viejo.push_str(t);
                        b["thinking"] = serde_json::Value::String(viejo);
                    }
                }
                (_, "input_json_delta") => {
                    b["type"] = serde_json::json!("tool_use");
                    // Los argumentos llegan como fragmentos de JSON: se concatenan
                    // y se parsean al final.
                    if let Some(p) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        let mut viejo = b["crudo"].as_str().unwrap_or("").to_string();
                        viejo.push_str(p);
                        b["crudo"] = serde_json::Value::String(viejo);
                    }
                }
                _ => {}
            }
        }
        let content: Vec<serde_json::Value> = bloques
            .into_iter()
            .map(|b| {
                let tipo = b["type"].as_str().unwrap_or("text").to_string();
                match tipo.as_str() {
                    "tool_use" => {
                        let crudo = b
                            .get("crudo")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = serde_json::from_str::<serde_json::Value>(&crudo)
                            .unwrap_or(serde_json::json!({}));
                        let nombre = b
                            .get("nombre")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        serde_json::json!({"type":"tool_use","name":nombre,"input":input})
                    }
                    "thinking" => serde_json::json!({
                        "type":"thinking","thinking":b["thinking"].as_str().unwrap_or("")
                    }),
                    _ => serde_json::json!({"type":"text","text":b["text"].as_str().unwrap_or("")}),
                }
            })
            .collect();
        let mut v = serde_json::json!({"content":content,"usage":uso});
        if !modelo.is_empty() {
            v["model"] = serde_json::Value::String(modelo);
        }
        v
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> String {
        "anthropic".into()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        if !self.tiene_clave() {
            return Err(ProviderError::SinCredencial("anthropic".into()));
        }
        // Anthropic no expone un catálogo estable por API; se describen los que
        // declaró el producto. Devolver vacío es honesto: el registry manda.
        Ok(Vec::new())
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationResult, ProviderError> {
        if !self.tiene_clave() {
            return Err(ProviderError::SinCredencial("anthropic".into()));
        }
        let mut cuerpo = AnthropicProvider::cuerpo_de(&req);
        cuerpo["stream"] = serde_json::json!(false);
        let r = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .timeout(Duration::from_secs(req.timeout_s.max(1) as u64))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", VERSION)
            .json(&cuerpo)
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        if !r.status().is_success() {
            let codigo = r.status().as_u16();
            let texto = r.text().await.unwrap_or_default();
            return Err(ProviderError::Status { codigo, cuerpo: texto });
        }
        let j: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ProviderError::RespuestaInvalida(e.to_string()))?;
        Ok(AnthropicProvider::interpretar(&j, &req.model))
    }

    async fn stream(&self, req: GenerationRequest) -> Result<ModelStream, ProviderError> {
        if !self.tiene_clave() {
            return Err(ProviderError::SinCredencial("anthropic".into()));
        }
        let cuerpo = AnthropicProvider::cuerpo_de(&req);
        let r = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", VERSION)
            .header("accept", "text/event-stream")
            .json(&cuerpo)
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        if !r.status().is_success() {
            let codigo = r.status().as_u16();
            let texto = r.text().await.unwrap_or_default();
            return Err(ProviderError::Status { codigo, cuerpo: texto });
        }
        let eventos = super::sse_hasta_final(r).await?;
        let mut partes: Vec<Result<StreamDelta, ProviderError>> = Vec::new();
        for e in &eventos {
            if let Some(t) = e
                .pointer("/delta/text")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty())
            {
                partes.push(Ok(StreamDelta::Texto(t.to_string())));
            }
            if let Some(t) = e
                .pointer("/delta/thinking")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty())
            {
                partes.push(Ok(StreamDelta::Razonamiento(t.to_string())));
            }
        }
        let junto = AnthropicProvider::juntar(&eventos);
        let mut final_r = AnthropicProvider::interpretar(&junto, &req.model);
        if final_r.modelo.is_empty() {
            final_r.modelo = req.model.clone();
        }
        partes.push(Ok(StreamDelta::Final(final_r)));
        Ok(Box::pin(futures_util::stream::iter(partes)))
    }

    async fn cargados(&self) -> Result<Vec<crate::models::ModeloCargado>, ProviderError> {
        Ok(vec![])
    }
}

/// Perfil aproximado por nombre de familia. Es describir, no elegir.
pub fn perfil_por_nombre(id: &str) -> Profile {
    let b = id.to_lowercase();
    // `Profile` no tiene término medio: es Nano (≤2B), Small (3–9B) o Large (>9B o
    // nube). Haiku es el único de la familia que cae en Small; «sonnet» y lo
    // desconocido acaban igual, así que no hay una rama que decida nada.
    if b.contains("haiku") {
        Profile::Small
    } else {
        Profile::Large
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::request::Message;
    use crate::api::vocab::KeepAlive;

    fn req() -> GenerationRequest {
        GenerationRequest {
            model: "claude-haiku-4-5".into(),
            system: "Eres Hatboo.".into(),
            prompt: "hola".into(),
            history: vec![
                Message {
                    role: "system".into(),
                    content: "no debe ir aquí".into(),
                    reasoning: None,
                },
                Message::usuario("¿qué?"),
                Message::asistente("nada"),
            ],
            tools: vec![],
            num_ctx: 2048,
            keep_alive: KeepAlive::PorDefecto,
            thinking: ThinkingLevel::Off,
            max_output_tokens: 512,
            temperature: 0.0,
            seed: 42,
            timeout_s: 30,
        }
    }

    #[test]
    fn el_system_va_fuera_de_messages() {
        let c = AnthropicProvider::cuerpo_de(&req());
        assert_eq!(c["system"], "Eres Hatboo.");
        let ms = c["messages"].as_array().unwrap();
        assert_eq!(ms.len(), 3, "history sin el system duplicado + el turno");
        assert!(ms.iter().all(|m| m["role"] != "system"));
        assert_eq!(ms[0]["role"], "user");
        assert!(c.get("seed").is_none());
    }

    #[test]
    fn thinking_pide_presupuesto_y_baja_la_temperatura() {
        let mut r = req();
        r.thinking = ThinkingLevel::Medium;
        let c = AnthropicProvider::cuerpo_de(&r);
        assert_eq!(c["thinking"]["type"], "enabled");
        assert!(c["thinking"]["budget_tokens"].as_u64().unwrap() < 512);
        assert_eq!(c["temperature"], 1.0);
    }

    #[test]
    fn el_presupuesto_nunca_iguala_al_tope_de_salida() {
        let mut r = req();
        r.thinking = ThinkingLevel::High;
        r.max_output_tokens = 200;
        let c = AnthropicProvider::cuerpo_de(&r);
        assert_eq!(c["thinking"]["budget_tokens"], 199);
    }

    #[test]
    fn interpretar_los_tres_tipos_de_bloque() {
        let j = serde_json::json!({
            "model":"claude-x",
            "content":[
                {"type":"thinking","thinking":"rumia"},
                {"type":"text","text":"hola"},
                {"type":"tool_use","name":"read_file","input":{"path":"a.rs"}}
            ],
            "usage":{"input_tokens":19,"output_tokens":12}
        });
        let r = AnthropicProvider::interpretar(&j, "x");
        assert_eq!(r.texto, "hola");
        assert_eq!(r.razonamiento.as_deref(), Some("rumia"));
        assert_eq!(r.tool_calls[0].args["path"], "a.rs");
        assert_eq!(r.tokens_entrada, Some(19));
        assert_eq!(r.carga_ms, Some(0));
    }

    #[test]
    fn juntar_reconstruye_un_stream_de_eventos() {
        let eventos = vec![
            serde_json::json!({"type":"message_start","message":{"model":"claude-x"},"usage":{"input_tokens":19}}),
            serde_json::json!({"type":"content_block_start","index":0,"delta":{"type":"text"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ho"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"la"}}),
            serde_json::json!({"type":"content_block_start","index":1,"delta":{"type":"tool_use","name":"read_file"}}),
            serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}),
        ];
        let junto = AnthropicProvider::juntar(&eventos);
        let r = AnthropicProvider::interpretar(&junto, "claude-x");
        assert_eq!(r.texto, "hola");
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].tool, "read_file");
        assert_eq!(r.tool_calls[0].args["path"], "a.rs");
        assert_eq!(r.tokens_entrada, Some(19));
        assert_eq!(r.tokens_salida, Some(12));
    }

    #[test]
    fn sin_clave_no_sale_el_http() {
        let mut p = AnthropicProvider::nuevo("");
        p.api_key = "  ".into();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            rt.block_on(p.list_models()).unwrap_err(),
            ProviderError::SinCredencial("anthropic".into())
        );
        assert_eq!(
            rt.block_on(p.generate(req())).unwrap_err(),
            ProviderError::SinCredencial("anthropic".into())
        );
    }

    #[test]
    fn perfies_por_nombre() {
        assert_eq!(perfil_por_nombre("claude-haiku-4-5"), Profile::Small);
        assert_eq!(perfil_por_nombre("claude-sonnet-4"), Profile::Large);
    }
}
