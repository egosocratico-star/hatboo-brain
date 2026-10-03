//! Ollama, el primer backend (§X del Canon). API nativa: `/api/chat` con
//! `options.num_ctx`, `keep_alive`, `think` y tools; `/api/ps` para lo que ya está
//! cargado; `keep_alive: 0` para expulsar.
//!
//! Medido en esta máquina el 30-09-2026: `/api/chat` y `/api/generate` dan las
//! mismas cuentas (`load_duration`, `prompt_eval_count`, `eval_count`,
//! `*_duration` en nanosegundos).

use super::{
    GenerationRequest, GenerationResult, LlamadaTool, ModelProvider, ModelStream, ProviderError,
    StreamDelta,
};
use crate::api::vocab::{KeepAlive, ThinkingLevel, ToolId};
use crate::models::{ModelInfo, ModelKind, ModeloCargado};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    base: String,
    http: reqwest::Client,
    /// Para el log y el panel: ningún número de aquí es una estimación nuestra.
    pub timeout_de_conexion: Duration,
}

impl OllamaProvider {
    pub fn nuevo() -> OllamaProvider {
        OllamaProvider::con_base("http://127.0.0.1:11434")
    }

    pub fn con_base(base: impl Into<String>) -> OllamaProvider {
        let base = base.into().trim_end_matches('/').to_string();
        OllamaProvider {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            base,
            timeout_de_conexion: Duration::from_secs(5),
        }
    }

    pub fn con_clave(self, _clave: &str) -> OllamaProvider {
        // Ollama no pide clave; existe para que el adaptador trate igual a todos.
        self
    }

    fn url(&self, ruta: &str) -> String {
        format!("{}{}", self.base, ruta)
    }

    /// El cuerpo exacto que se manda. Separado del HTTP para poder probarlo sin
    /// servidor: es donde se cometen los errores de `num_ctx` y de `think`.
    pub fn cuerpo_de(req: &GenerationRequest) -> serde_json::Value {
        let mut mensajes: Vec<serde_json::Value> = Vec::with_capacity(req.history.len() + 2);
        if !req.system.is_empty() {
            mensajes.push(serde_json::json!({"role":"system","content":req.system}));
        }
        for m in &req.history {
            let mut j = serde_json::json!({"role": m.role, "content": m.content});
            if let Some(r) = &m.reasoning {
                j["reasoning"] = serde_json::Value::String(r.clone());
            }
            mensajes.push(j);
        }
        mensajes.push(serde_json::json!({"role":"user","content":req.prompt}));

        let mut cuerpo = serde_json::json!({
            "model": req.model,
            "messages": mensajes,
            "stream": true,
            "options": {
                "num_ctx": req.num_ctx,
                "temperature": req.temperature,
                "seed": req.seed,
                // Con `think: true` Ollama gasta el razonamiento dentro de
                // `num_predict`, así que el techo es la suma que firmó el Plan.
                "num_predict": req.tope_de_generacion(),
            }
        });

        match req.keep_alive {
            KeepAlive::PorDefecto => {}
            KeepAlive::Segundos(s) => cuerpo["keep_alive"] = serde_json::json!(format!("{s}s")),
            KeepAlive::Expulsar => cuerpo["keep_alive"] = serde_json::json!(0),
        }

        // `think`: Ollama admite un booleano; en versiones recientes también
        // niveles, pero no se asume: se manda el booleano y el sondeo (§X) dice si
        // salió razonamiento de verdad.
        if req.thinking != ThinkingLevel::Off {
            cuerpo["think"] = serde_json::json!(true);
        }

        // El contrato `Json` se le dice al servidor, no se le pide al prompt.
        // Medido el 03-10: sin esta línea `gemma3:1b` contestaba ```json …```
        // envuelto (que no parsea); con `"json"` contesta JSON suelto. Con
        // `json_schema` la respuesta fue HTTP 400 en los dos modelos locales, así
        // que el esquema ceñido no se manda nunca por aquí.
        if req.salida_json {
            cuerpo["format"] = serde_json::json!("json");
        }

        if !req.tools.is_empty() {
            cuerpo["tools"] = serde_json::json!(
                req.tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    }))
                    .collect::<Vec<_>>()
            );
        }
        cuerpo
    }

    /// `carga_ms` y compañía salen de la última línea del stream.
    fn cuentas(de: &serde_json::Value) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
        let ms = |k: &str| de.get(k).and_then(|v| v.as_u64()).map(|ns| ns / 1_000_000);
        (
            de.get("prompt_eval_count").and_then(|v| v.as_u64()),
            de.get("eval_count").and_then(|v| v.as_u64()),
            ms("load_duration"),
            ms("eval_duration"),
        )
    }

    async fn pedir(
        &self,
        ruta: &str,
        cuerpo: &serde_json::Value,
        timeout_s: u32,
    ) -> Result<reqwest::Response, ProviderError> {
        let r = self
            .http
            .post(self.url(ruta))
            .json(cuerpo)
            .timeout(Duration::from_secs(timeout_s.max(1) as u64))
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        let status = r.status();
        if status.as_u16() == 404 {
            let modelo = cuerpo
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            if !modelo.is_empty() {
                return Err(ProviderError::ModeloNoInstalado(modelo));
            }
        }
        if !status.is_success() {
            let codigo = status.as_u16();
            let texto = r.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                codigo,
                cuerpo: texto,
            });
        }
        Ok(r)
    }

    /// Une los fragmentos de un stream NDJSON ya leído.
    fn juntar(lineas: &[serde_json::Value]) -> GenerationResult {
        let mut texto = String::new();
        let mut razon = String::new();
        let mut llamadas: Vec<LlamadaTool> = Vec::new();
        let mut modelo = String::new();
        let mut entrada = None;
        let mut salida = None;
        let mut carga = None;
        let mut decode = None;
        let mut truncado = false;
        for l in lineas {
            if let Some(m) = l.get("model").and_then(|v| v.as_str()) {
                modelo = m.to_string();
            }
            if let Some(msg) = l.get("message") {
                if let Some(c) = msg.get("content").and_then(|v| v.as_str()) {
                    texto.push_str(c);
                }
                if let Some(c) = msg
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .or_else(|| msg.get("thinking").and_then(|v| v.as_str()))
                {
                    razon.push_str(c);
                }
                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let nombre = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args = arguimentos_de(tc);
                        if !nombre.is_empty() {
                            llamadas.push(LlamadaTool {
                                tool: nombre,
                                args,
                            });
                        }
                    }
                }
            }
            if l.get("done").and_then(|v| v.as_bool()) == Some(true) {
                let (e, s, c, d) = Self::cuentas(l);
                entrada = e;
                salida = s;
                carga = c;
                decode = d;
                // `done_reason` distingue al modelo que terminó del techo que lo
                // paró. Sin esta línea los dos casos se parecían un `Formato`.
                truncado = l.get("done_reason").and_then(|v| v.as_str()) == Some("length");
            }
        }
        let tok_s = match (salida, decode) {
            (Some(n), Some(ms)) if ms > 0 => Some(n as f32 / (ms as f32 / 1000.0)),
            _ => None,
        };
        GenerationResult {
            texto,
            razonamiento: if razon.is_empty() {
                None
            } else {
                Some(razon)
            },
            tool_calls: llamadas,
            modelo,
            tokens_entrada: entrada,
            tokens_salida: salida,
            ttft_ms: None,
            tok_s,
            carga_ms: carga,
            truncado,
        }
    }
}

/// Ollama manda los argumentos como objeto o como string JSON, según versión.
fn arguimentos_de(tc: &serde_json::Value) -> serde_json::Value {
    let f = tc.get("function").cloned().unwrap_or_default();
    match f.get("arguments") {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.clone()))
        }
        Some(v) => v.clone(),
        None => serde_json::json!({}),
    }
}

async fn leer_ndjson(
    resp: reqwest::Response,
) -> Result<Vec<serde_json::Value>, ProviderError> {
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut colchon = String::new();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| ProviderError::Transporte(e.to_string()))?;
        colchon.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(i) = colchon.find('\n') {
            let linea = colchon[..i].trim().to_string();
            colchon.drain(..=i);
            if linea.is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&linea)
                .map_err(|e| ProviderError::RespuestaInvalida(format!("{e}: {linea}")))?;
            out.push(v);
        }
    }
    let resto = colchon.trim();
    if !resto.is_empty() {
        let v = serde_json::from_str(resto)
            .map_err(|e| ProviderError::RespuestaInvalida(format!("{e}")))?;
        out.push(v);
    }
    if out.is_empty() {
        return Err(ProviderError::RespuestaInvalida("respuesta vacía".into()));
    }
    Ok(out)
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn id(&self) -> ToolId {
        "ollama".into()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let r = self
            .http
            .get(self.url("/api/tags"))
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| {
                ProviderError::Transporte(format!("no se pudo hablar con Ollama: {e}"))
            })?;
        if !r.status().is_success() {
            return Err(ProviderError::Status {
                codigo: r.status().as_u16(),
                cuerpo: r.text().await.unwrap_or_default(),
            });
        }
        let j: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ProviderError::RespuestaInvalida(e.to_string()))?;
        let mut out = Vec::new();
        for m in j.get("models").and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(nombre) = m.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let disco_mb = m.get("size").and_then(|v| v.as_u64()).map(|b| b / 1_000_000);
            let params = m
                .get("details")
                .and_then(|d| d.get("parameter_size"))
                .and_then(|v| v.as_str())
                .and_then(parse_parametros);
            let (capacidades, ctx_max, modelfile) = self.ficha(nombre).await;
            out.push(ModelInfo {
                id: nombre.into(),
                provider: "ollama".into(),
                // Un `:cloud` de Ollama se ejecuta fuera del equipo: no es local.
                local: !es_nube(nombre) && disco_mb.unwrap_or(0) > 0,
                kind: ModelKind::Generativo,
                profile: ModelInfo::perfil_desde_parametros(params),
                tier: tier_desde(params, es_nube(nombre)),
                // RAM **no** medida aquí: la mide el barrido de Fase 0.
                ram_mb_by_ctx: BTreeMap::new(),
                max_ctx: ctx_max.unwrap_or(8192),
                strengths: fortalezas(&capacidades),
                supports_tools: capacidades.iter().any(|c| c == "tools"),
                supports_thinking: capacidades.iter().any(|c| c == "thinking"),
                supports_vision: capacidades.iter().any(|c| c == "vision"),
                // Ni una versión local de Ollama lo declara (medido el 03-10); si
                // alguien lo pone a `true` tiene que ser la sonda, no la ficha.
                structured_output: capacidades
                    .iter()
                    .any(|c| c == "structured_output" || c == "structured_outputs"),
                disco_mb,
            });
            let _ = modelfile;
        }
        Ok(out)
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationResult, ProviderError> {
        let mut cuerpo = OllamaProvider::cuerpo_de(&req);
        cuerpo["stream"] = serde_json::json!(false);
        let resp = self.pedir("/api/chat", &cuerpo, req.timeout_s).await?;
        let j: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::RespuestaInvalida(e.to_string()))?;
        let mut r = OllamaProvider::juntar(&[j]);
        r.ttft_ms = None;
        Ok(r)
    }

    async fn stream(&self, req: GenerationRequest) -> Result<ModelStream, ProviderError> {
        let cuerpo = OllamaProvider::cuerpo_de(&req);
        // Sin timeout total: un N3 con razonamiento puede tardar minutos. Lo corta
        // el Brain con `plan.timeout_s` y con la cancelación.
        let resp = self
            .http
            .post(self.url("/api/chat"))
            .json(&cuerpo)
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let codigo = status.as_u16();
            let texto = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status {
                codigo,
                cuerpo: texto,
            });
        }
        let lineas = leer_ndjson(resp).await?;
        let esperado = req.model.clone();
        let mut partes: Vec<Result<StreamDelta, ProviderError>> = Vec::new();
        for l in &lineas {
            if let Some(c) = l
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                if !c.is_empty() {
                    partes.push(Ok(StreamDelta::Texto(c.to_string())));
                }
            }
            if let Some(rc) = l
                .get("message")
                .and_then(|m| m.get("reasoning"))
                .and_then(|c| c.as_str())
            {
                if !rc.is_empty() {
                    partes.push(Ok(StreamDelta::Razonamiento(rc.to_string())));
                }
            }
        }
        let mut final_r = OllamaProvider::juntar(&lineas);
        if final_r.modelo.is_empty() {
            final_r.modelo = esperado;
        }
        partes.push(Ok(StreamDelta::Final(final_r)));
        Ok(Box::pin(futures_util::stream::iter(partes)))
    }

    async fn cargados(&self) -> Result<Vec<ModeloCargado>, ProviderError> {
        let r = self
            .http
            .get(self.url("/api/ps"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ProviderError::Transporte(e.to_string()))?;
        if !r.status().is_success() {
            return Err(ProviderError::Status {
                codigo: r.status().as_u16(),
                cuerpo: r.text().await.unwrap_or_default(),
            });
        }
        let j: serde_json::Value = r
            .json()
            .await
            .map_err(|e| ProviderError::RespuestaInvalida(e.to_string()))?;
        Ok(j.get("models")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|m| {
                Some(ModeloCargado {
                    id: m.get("name")?.as_str()?.to_string(),
                    ram_mb: m.get("size")?.as_u64()? / 1_000_000,
                    num_ctx: m.get("context_length").and_then(|v| v.as_u64()).unwrap_or(2048) as u32,
                })
            })
            .collect())
    }

    async fn expulsar(&self, model: &str) -> Result<(), ProviderError> {
        let cuerpo = serde_json::json!({"model": model, "keep_alive": 0});
        self.pedir("/api/generate", &cuerpo, 30).await?;
        Ok(())
    }
}

impl OllamaProvider {
    /// Ficha del modelo: capacidades, contexto máximo declarado. Lo que Ollama ya
    /// sabe decir sin cargar nada.
    /// Sonda de §X: ¿obedece este modelo un `format: json_schema`? Sale por HTTP
    /// directo y no por `generate` porque el camino normal **nunca** pide un
    /// esquema; lo que se quiere medir es justamente si el servidor lo acepta.
    /// Medido el 03-10 en este equipo: `gemma3:1b` y `qwen3:1.7b` contestaron
    /// HTTP 400, así que `structured_output` sigue siendo `false` hasta que una
    /// sonda diga lo contrario.
    pub async fn prueba_esquema(
        &self,
        modelo: &str,
        esquema: &serde_json::Value,
    ) -> Result<String, String> {
        let cuerpo = serde_json::json!({
            "model": modelo,
            "stream": false,
            "format": { "type": "json_schema", "schema": esquema },
            "options": { "temperature": 0.0, "seed": 42, "num_ctx": 2048, "num_predict": 96 },
            "messages": [{
                "role": "user",
                "content": "Responde solo con un JSON que cumpla el esquema."
            }],
        });
        let resp = self
            .http
            .post(self.url("/api/chat"))
            .json(&cuerpo)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| format!("el pedido de sonda no salió: {e}"))?;
        let estado = resp.status();
        let val: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("la respuesta no es JSON (HTTP {estado}): {e}"))?;
        if !estado.is_success() {
            let motivo = val.get("error").and_then(|v| v.as_str()).unwrap_or("sin motivo");
            return Err(format!("HTTP {estado}: {motivo}"));
        }
        Ok(val
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string())
    }

    async fn ficha(&self, nombre: &str) -> (Vec<String>, Option<u32>, Option<String>) {
        let cuerpo = serde_json::json!({"model": nombre});
        let Ok(resp) = self
            .http
            .post(self.url("/api/show"))
            .json(&cuerpo)
            .timeout(Duration::from_secs(15))
            .send()
            .await
        else {
            return (vec![], None, None);
        };
        let Ok(j) = resp.json::<serde_json::Value>().await else {
            return (vec![], None, None);
        };
        let capacidades: Vec<String> = j
            .get("capabilities")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str().map(|s| s.to_string()))
            .collect();
        let info = j.get("model_info").cloned().unwrap_or_default();
        let arch = info
            .get("general.architecture")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ctx = if arch.is_empty() {
            None
        } else {
            info.get(format!("{arch}.context_length"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
        };
        (capacidades, ctx, j.get("modelfile").and_then(|v| v.as_str()).map(String::from))
    }
}

/// `7.6B`, `999.89M`, `1.5B` → número de parámetros en miles de millones.
pub fn parse_parametros(s: &str) -> Option<f64> {
    let limpio = s.trim().to_lowercase().replace(' ', "");
    let (num, mult) = if let Some(n) = limpio.strip_suffix('b') {
        (n, 1.0)
    } else if let Some(n) = limpio.strip_suffix('m') {
        (n, 0.001)
    } else if let Some(n) = limpio.strip_suffix('t') {
        (n, 1000.0)
    } else {
        (limpio.as_str(), 1.0)
    };
    num.trim_end_matches(char::is_alphabetic)
        .parse::<f64>()
        .ok()
        .map(|v| v * mult)
}

/// Ollama ofrece modelos que no bajan a tu disco: los ejecuta en su nube y el
/// nombre lo avisa.
pub fn es_nube(nombre: &str) -> bool {
    nombre.contains(":cloud") || nombre.ends_with("-cloud")
}

/// Tier de escalado: nano=1 … nube=4. Nunca «el más grande».
pub fn tier_desde(params: Option<f64>, nube: bool) -> u8 {
    if nube {
        return 4;
    }
    match params {
        Some(p) if p <= 1.0 => 1,
        Some(p) if p <= 2.5 => 2,
        Some(p) if p <= 9.0 => 3,
        _ => 4,
    }
}

fn fortalezas(capacidades: &[String]) -> Vec<String> {
    let mapa = [
        ("tools", "herramientas"),
        ("vision", "imágenes"),
        ("thinking", "razonamiento"),
        ("insert", "relleno"),
    ];
    mapa.iter()
        .filter(|(c, _)| capacidades.iter().any(|x| x == *c))
        .map(|(_, f)| (*f).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::request::Message;
    use crate::api::vocab::{OutputContract, Profile};

    fn req() -> GenerationRequest {
        GenerationRequest {
            model: "gemma3:1b".into(),
            system: "Eres Hatboo.".into(),
            prompt: "hola".into(),
            history: vec![Message::usuario("anda"), Message::asistente("vámonos")],
            tools: vec![super::super::ToolSchema {
                name: "read_file".into(),
                description: "lee".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            num_ctx: 4096,
            keep_alive: KeepAlive::Segundos(300),
            thinking: ThinkingLevel::Off,
            max_output_tokens: 512,
            temperature: 0.0,
            seed: 42,
            timeout_s: 60,
            salida_json: false,
        }
    }

    /// El contrato `Json` se le pide al servidor, no se le ruega en el prompt.
    /// Medido el 03-10 con `gemma3:1b`: sin `format` contestaba con un bloque
    /// ```` ```json ```` que no parsea; con `"json"` contesta JSON suelto. Y con
    /// `json_schema` la contestación fue HTTP 400, así que por aquí no se manda.
    #[test]
    fn un_contrato_json_le_pide_el_formato_al_servidor() {
        let mut r = req();
        r.salida_json = true;
        assert_eq!(OllamaProvider::cuerpo_de(&r)["format"], "json");
        // Fuera del contrato JSON no se manda: cambiar el formato cambia la salida.
        assert!(
            OllamaProvider::cuerpo_de(&req()).get("format").is_none(),
            "sin contrato JSON el cuerpo no debe tocar el formato"
        );
    }

    #[test]
    fn el_cuerpo_lleva_lo_que_manda_el_plan() {
        let c = OllamaProvider::cuerpo_de(&req());
        assert_eq!(c["options"]["num_ctx"], 4096);
        assert_eq!(c["options"]["num_predict"], 512);
        assert_eq!(c["options"]["seed"], 42);
        assert_eq!(c["keep_alive"], "300s");
        assert_eq!(c["messages"].as_array().unwrap().len(), 4);
        assert_eq!(c["messages"][0]["role"], "system");
        assert!(c["tools"].as_array().unwrap().len() == 1);
        assert!(c.get("think").is_none(), "off no manda think");
    }

    #[test]
    fn thinking_y_expulsion_se_mandan_como_tocan() {
        let mut r = req();
        r.thinking = ThinkingLevel::Low;
        let c = OllamaProvider::cuerpo_de(&r);
        assert_eq!(c["think"], true);
        // `num_predict` es todo lo que genera el modelo, razonamiento incluido: con
        // el tope del Plan a secas, pensar se pagaba recortando la respuesta.
        assert_eq!(
            c["options"]["num_predict"],
            512 + ThinkingLevel::Low.presupuesto_tokens()
        );
        r.keep_alive = KeepAlive::Expulsar;
        assert_eq!(
            OllamaProvider::cuerpo_de(&r)["keep_alive"],
            serde_json::json!(0)
        );
    }

    #[test]
    fn junta_un_stream_ndjson_y_saca_las_cuentas() {
        let lineas = vec![
            serde_json::json!({"model":"gemma3:1b","message":{"content":"Ho"},"done":false}),
            serde_json::json!({"model":"gemma3:1b","message":{"content":"la "},"done":false}),
            serde_json::json!({"model":"gemma3:1b","message":{"reasoning":"pienso"},"done":false}),
            serde_json::json!({
                "model":"gemma3:1b","message":{"content":"mundo"},"done":true,
                "prompt_eval_count":19,"eval_count":12,
                "load_duration":7_300_000_000u64,"eval_duration":700_000_000u64
            }),
        ];
        let r = OllamaProvider::juntar(&lineas);
        assert_eq!(r.texto, "Hola mundo");
        assert_eq!(r.razonamiento.as_deref(), Some("pienso"));
        assert_eq!(r.tokens_entrada, Some(19));
        assert_eq!(r.tokens_salida, Some(12));
        assert_eq!(r.carga_ms, Some(7300));
        assert!((r.tok_s.unwrap() - 17.14).abs() < 0.1, "{:?}", r.tok_s);
    }

    #[test]
    fn las_herramientas_vuelven_como_llamadas() {
        let lineas = vec![serde_json::json!({
            "model":"q","done":true,
            "message":{"content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"a.rs"}}}]}
        })];
        let r = OllamaProvider::juntar(&lineas);
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].tool, "read_file");
        assert_eq!(r.tool_calls[0].args["path"], "a.rs");
    }

    #[test]
    fn argumentos_como_string_json() {
        // Cadena cruda, parseada en tiempo de ejecución: el `json!` con `\"`
        // anidados se vuelve ilegible para el macro.
        let crudo = r#"{"done":true,"message":{"tool_calls":[{"function":{"name":"run_command","arguments":"{\"cmd\":\"ls\"}"}}]}}"#;
        let linea: serde_json::Value = serde_json::from_str(crudo).unwrap();
        let r = OllamaProvider::juntar(&[linea]);
        assert_eq!(r.tool_calls[0].args["cmd"], "ls");
    }

    #[test]
    fn el_techo_de_salida_se_avisa_como_truncado() {
        let cortada = serde_json::json!({"model":"g","done":true,"done_reason":"length","message":{"content":"{\"a\":"}});
        assert!(
            OllamaProvider::juntar(&[cortada]).truncado,
            "un `done_reason: length` es el techo, no el modelo"
        );
        let completo = serde_json::json!({"model":"g","done":true,"done_reason":"stop","message":{"content":"hola"}});
        assert!(!OllamaProvider::juntar(&[completo]).truncado);
        // Sin `done_reason` (versiones que no lo mandan) no se inventa nada.
        let sin = serde_json::json!({"model":"g","done":true,"message":{"content":"hola"}});
        assert!(!OllamaProvider::juntar(&[sin]).truncado);
    }

    #[test]
    fn nombres_de_modelo_y_perfiles() {
        assert_eq!(parse_parametros("7.6B"), Some(7.6));
        // 999,89 M no es exacto en binario: se compara con tolerancia.
        let mil_megas = parse_parametros("999.89M").expect("se parsea");
        assert!((mil_megas - 0.99989).abs() < 1e-6, "{mil_megas}");
        assert_eq!(parse_parametros("1.5 Billion"), Some(1.5));
        assert_eq!(parse_parametros("no-se"), None);
        assert!(es_nube("nemotron-3-ultra:cloud"));
        assert!(es_nube("gpt-oss:120b-cloud"));
        assert!(!es_nube("qwen3.5:0.8b"));
        assert_eq!(ModelInfo::perfil_desde_parametros(Some(0.87)), Profile::Nano);
        assert_eq!(tier_desde(Some(0.87), false), 1);
        assert_eq!(tier_desde(Some(4.0), false), 3);
        assert_eq!(tier_desde(None, true), 4);
        assert_eq!(
            fortalezas(&["tools".into(), "vision".into()]),
            vec!["herramientas".to_string(), "imágenes".to_string()]
        );
        let _ = OutputContract::Texto;
    }
}
