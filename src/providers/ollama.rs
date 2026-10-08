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
use std::collections::{BTreeMap, VecDeque};
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
                // Con `think: true` Ollama gasta el razonamiento dentro de
                // `num_predict`, así que el techo es la suma que firmó el Plan.
                "num_predict": req.tope_de_generacion(),
            }
        });
        // `temperature` y `seed` solo viajan si el producto los pidió: mandarlos
        // siempre era fijar el muestreo de todas las conversaciones.
        if let Some(t) = req.temperature {
            cuerpo["options"]["temperature"] = serde_json::json!(t);
        }
        if let Some(s) = req.seed {
            cuerpo["options"]["seed"] = serde_json::json!(s);
        }
        // Fase 6, medido el 04-10 contra el Ollama de esta máquina: `/api/chat`
        // con `logprobs: true` contesta en **cada chunk** del stream un
        // `logprobs:[{token,logprob,bytes,top_logprobs:[…]}]`, y el chunk final
        // (`done: true`) no trae ninguno. Con `top_logprobs: 1` solo viene el
        // token elegido, que es lo que hace falta para la media.
        if req.logprobs {
            cuerpo["options"]["logprobs"] = serde_json::json!(true);
            cuerpo["options"]["top_logprobs"] = serde_json::json!(1);
        }

        match req.keep_alive {
            KeepAlive::PorDefecto => {}
            KeepAlive::Segundos(s) => cuerpo["keep_alive"] = serde_json::json!(format!("{s}s")),
            KeepAlive::Expulsar => cuerpo["keep_alive"] = serde_json::json!(0),
        }

        // `think`: se manda **siempre**, con su booleano. Omitir la clave cuando el
        // nivel es `Off` no significaba «no pienses»: significaba «que decida el
        // servidor», y los modelos que nacen pensando deciden pensar. Medido el 07-10
        // en esta máquina con `qwen3.5:0.8b`: callado el campo, los 160 tokens de
        // `num_predict` se fueron en `thinking` y la respuesta visible salió **vacía**
        // y truncada; con `think: false` el mismo pedido contesta en 3090 ms. Y no
        // rompe a los que no piensan: `gemma3:1b` y `deepseek-r1:1.5b` aceptan el
        // booleano y responden igual.
        cuerpo["think"] = serde_json::json!(req.thinking != ThinkingLevel::Off);

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
        let mut registros: Vec<f32> = Vec::new();
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
            if let Some(lp) = l.get("logprobs").and_then(|v| v.as_array()) {
                for e in lp {
                    if let Some(v) = e.get("logprob").and_then(|x| x.as_f64()) {
                        registros.push(v as f32);
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
            logprob_medio: super::media_logprobs(&registros),
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

/// La lectura del NDJSON **mientras llega**: cada línea que se cierra se convierte
/// en su delta y se suelta, sin esperar al final del cuerpo.
///
/// Ollama manda un token por línea. Hasta el 07-10 el cuerpo entero se acumulaba
/// (`leer_ndjson`) y se reenviaba ya terminado, así que una respuesta de 300 tokens
/// llegaba a la interfaz de golpe, en un solo bulto. Las cuentas del `Final` siguen
/// saliendo de `juntar` con **todas** las líneas: lo que cambia es *cuándo* se
/// emiten las letras, no lo que se deja de contar.
struct Transmision {
    /// Lo que llegó del proveedor y todavía no cierra un `\n`.
    colchon: String,
    /// Deltas ya parseados que faltan emitir.
    pendientes: VecDeque<StreamDelta>,
    /// Todas las líneas, para `juntar` al cerrar.
    lineas: Vec<serde_json::Value>,
    /// El modelo pedido: si el servidor no lo nombra en ninguna línea, el `Final`
    /// no puede devolver un id vacío.
    esperado: String,
    /// El cuerpo se acabó (o rompió): ya no se lee nada más.
    cerrado: bool,
}

impl Transmision {
    fn nueva(esperado: String) -> Transmision {
        Transmision {
            colchon: String::new(),
            pendientes: VecDeque::new(),
            lineas: Vec::new(),
            esperado,
            cerrado: false,
        }
    }

    /// Una línea suelta: se guarda para las cuentas y se encola lo que haya que
    /// mostrar.
    fn linea(&mut self, texto: &str) -> Result<(), ProviderError> {
        let v: serde_json::Value = serde_json::from_str(texto)
            .map_err(|e| ProviderError::RespuestaInvalida(format!("{e}: {texto}")))?;
        if let Some(msg) = v.get("message") {
            if let Some(c) = msg.get("content").and_then(|c| c.as_str()) {
                if !c.is_empty() {
                    self.pendientes.push_back(StreamDelta::Texto(c.to_string()));
                }
            }
            // Medido el 07-10 contra el Ollama de esta máquina: `qwen3.5:0.8b` manda
            // el razonamiento en `message.thinking`, y `reasoning` no aparece nunca.
            // Sin este espejo del `juntar` de abajo, un turno que piensa no emitía **ni
            // un delta**: la pantalla se quedaba en blanco hasta el final, y si el tope
            // de tokens caía dentro del pensamiento, la respuesta salía vacía.
            if let Some(rc) = msg
                .get("reasoning")
                .or_else(|| msg.get("thinking"))
                .and_then(|c| c.as_str())
            {
                if !rc.is_empty() {
                    self.pendientes.push_back(StreamDelta::Razonamiento(rc.to_string()));
                }
            }
        }
        self.lineas.push(v);
        Ok(())
    }

    /// Bytes recién llegados: se cortan por `\n` y cada parte completa queda lista
    /// para emitir. Lo incompleto se queda en el colchón.
    fn recibir(&mut self, nuevos: &str) -> Result<(), ProviderError> {
        self.colchon.push_str(nuevos);
        while let Some(i) = self.colchon.find('\n') {
            let linea = self.colchon[..i].trim().to_string();
            self.colchon.drain(..=i);
            if linea.is_empty() {
                continue;
            }
            self.linea(&linea)?;
        }
        Ok(())
    }

    /// Se acabó el cuerpo. La última línea llega **sin** salto (`done: true`), así
    /// que el resto del colchón también es línea; después van las cuentas medidas.
    fn rematar(&mut self) -> Result<(), ProviderError> {
        let resto = self.colchon.trim().to_string();
        self.colchon.clear();
        if !resto.is_empty() {
            self.linea(&resto)?;
        }
        if self.lineas.is_empty() {
            return Err(ProviderError::RespuestaInvalida("respuesta vacía".into()));
        }
        let mut final_r = OllamaProvider::juntar(&self.lineas);
        if final_r.modelo.is_empty() {
            final_r.modelo = self.esperado.clone();
        }
        self.pendientes.push_back(StreamDelta::Final(final_r));
        Ok(())
    }
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
        // Primero las entradas de `/api/tags`, y **después** todas las fichas a la
        // vez. Pedirlas de una en una costaba 3028 ms con los once modelos del
        // equipo (medido el 06-10: 306 ms en paralelo), y este listado se reconstruye
        // en cada turno del chat: era el coste que él llamaba «el brain continúa
        // después de que cargue el modelo».
        let mut bases: Vec<(String, Option<u64>, Option<f64>)> = Vec::new();
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
            bases.push((nombre.to_string(), disco_mb, params));
        }
        let fichas = futures_util::future::join_all(bases.iter().map(|(n, _, _)| self.ficha(n))).await;
        let mut out = Vec::new();
        for ((nombre, disco_mb, params), (capacidades, ctx_max, _modelfile)) in
            bases.into_iter().zip(fichas)
        {
            out.push(ModelInfo {
                id: nombre.clone(),
                provider: "ollama".into(),
                // Un `:cloud` de Ollama se ejecuta fuera del equipo: no es local.
                local: !es_nube(&nombre) && disco_mb.unwrap_or(0) > 0,
                kind: ModelKind::Generativo,
                profile: ModelInfo::perfil_desde_parametros(params),
                tier: tier_desde(params, es_nube(&nombre)),
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
        // La lectura va delta a delta: una línea cerrada del NDJSON es una letra que
        // el usuario puede ver ya. Esperar al cuerpo entero era lo que hacía que la
        // respuesta apareciera de golpe.
        let trans = Transmision::nueva(req.model.clone());
        let bytes = resp.bytes_stream();
        Ok(Box::pin(futures_util::stream::unfold(
            (trans, bytes),
            |(mut trans, mut bytes)| async move {
                use futures_util::StreamExt;
                loop {
                    if let Some(delta) = trans.pendientes.pop_front() {
                        return Some((Ok(delta), (trans, bytes)));
                    }
                    if trans.cerrado {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            if let Err(e) = trans.recibir(&String::from_utf8_lossy(&chunk)) {
                                trans.cerrado = true;
                                return Some((Err(e), (trans, bytes)));
                            }
                        }
                        Some(Err(e)) => {
                            trans.cerrado = true;
                            return Some((Err(ProviderError::Transporte(e.to_string())), (trans, bytes)));
                        }
                        None => {
                            let resto = trans.rematar();
                            trans.cerrado = true;
                            if let Err(e) = resto {
                                return Some((Err(e), (trans, bytes)));
                            }
                        }
                    }
                }
            },
        )))
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
            temperature: Some(0.0),
            seed: Some(42),
            logprobs: false,
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
        // `Off` se dice **explícitamente**: callar la clave dejaba decidir al
        // servidor, y `qwen3.5:0.8b` se gastaba los 160 tokens del tope en pensamiento
        // y devolvía la respuesta vacía (medido el 07-10 en esta máquina).
        assert_eq!(c["think"], false);
    }

    /// El muestreo lo decide el producto, no el crate. Con `None` la clave no
    /// llega al cuerpo —Ollama aplica su defecto, 0,8—, y solo aparece cuando
    /// alguien la pidió, que es el caso del banco de medidas con 0 y 42. Estuvo
    /// fijado en el runtime: un chat con un modelo de 1B repetía la misma frase
    /// literal turno tras turno.
    #[test]
    fn sin_temperatura_pedida_no_se_manda_temperatura() {
        let mut r = req();
        r.temperature = None;
        r.seed = None;
        let c = OllamaProvider::cuerpo_de(&r);
        assert!(c["options"].get("temperature").is_none(), "el crate no fija el muestreo");
        assert!(c["options"].get("seed").is_none(), "tampoco la semilla");
        // Quitar eso no puede llevarse el resto del Plan por delante.
        assert_eq!(c["options"]["num_ctx"], 4096);
        assert_eq!(c["options"]["num_predict"], 512);

        let con = OllamaProvider::cuerpo_de(&req());
        assert_eq!(con["options"]["temperature"], 0.0);
        assert_eq!(con["options"]["seed"], 42);
    }

    /// Fase 6, contra la forma que devolvió el Ollama de esta máquina el 04-10:
    /// cada chunk del stream trae `logprobs:[{token,logprob,…}]` y el `done:true`
    /// no trae ninguno.
    #[test]
    fn los_logprobs_del_stream_salen_en_una_media() {
        let mut r = req();
        r.logprobs = true;
        let c = OllamaProvider::cuerpo_de(&r);
        assert_eq!(c["options"]["logprobs"], true);
        assert_eq!(c["options"]["top_logprobs"], 1);
        assert!(
            OllamaProvider::cuerpo_de(&req())["options"].get("logprobs").is_none(),
            "con la fase apagada no se le pide nada al servidor"
        );

        let lineas = vec![
            serde_json::json!({"model":"gemma3:1b","message":{"content":"a"},"done":false,
                "logprobs":[{"token":"a","logprob":-0.4}]}),
            serde_json::json!({"model":"gemma3:1b","message":{"content":"b"},"done":false,
                "logprobs":[{"token":"b","logprob":-0.8}]}),
            serde_json::json!({"model":"gemma3:1b","message":{"content":""},"done":true,"eval_count":2}),
        ];
        let g = OllamaProvider::juntar(&lineas);
        let media = g.logprob_medio.expect("la media de los dos tokens");
        assert!((media + 0.6).abs() < 1e-6, "media {media}, esperado -0,6");

        // Un stream sin logprobs no es un modelo segurísimo: es None.
        let sin = serde_json::json!({"model":"g","message":{"content":"x"},"done":true});
        assert_eq!(OllamaProvider::juntar(&[sin]).logprob_medio, None);
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

    /// Lo que se arregló el 07-10: el stream de Ollama **no** espera al final del
    /// cuerpo. Cada línea cerrada es una letra que ya se puede pintar.
    #[test]
    fn cada_linea_se_emite_al_cerrarse_no_al_terminar_el_cuerpo() {
        let mut t = Transmision::nueva("gemma3:1b".into());
        t.recibir(
            "{\"model\":\"gemma3:1b\",\"message\":{\"content\":\"Ho\"},\"done\":false}\n",
        )
        .expect("línea válida");
        assert_eq!(
            t.pendientes.len(),
            1,
            "con una línea ya hay delta que emitir"
        );
        assert_eq!(t.pendientes[0], StreamDelta::Texto("Ho".into()));

        t.recibir("{\"model\":\"gemma3:1b\",\"message\":{\"content\":\"la\"},\"done\":false}\n")
            .expect("línea válida");
        assert_eq!(t.pendientes.len(), 2);
        assert_eq!(t.pendientes[1], StreamDelta::Texto("la".into()));
        // Las cuentas siguen contando las dos.
        assert_eq!(t.lineas.len(), 2);
    }

    /// El corte de bytes de la red no coincide con el de las líneas: lo incompleto
    /// no se emite ni se duplica cuando llega el resto.
    #[test]
    fn una_linea_partida_sale_cuando_se_completa() {
        let mut t = Transmision::nueva("g".into());
        t.recibir("{\"model\":\"g\",\"message\":{\"cont").expect("aún no es línea");
        assert!(t.pendientes.is_empty(), "sin \\n no se suelta nada");
        assert!(t.lineas.is_empty());
        t.recibir("ent\":\"x\"},\"done\":false}\n").expect("ahora sí");
        assert_eq!(t.pendientes.len(), 1);
        assert_eq!(t.pendientes[0], StreamDelta::Texto("x".into()));
        assert_eq!(t.lineas.len(), 1, "la línea partida no se cuenta dos veces");
    }

    /// Medido el 07-10 contra el Ollama de esta máquina: `qwen3.5:0.8b` manda el
    /// razonamiento en `message.thinking` y `reasoning` no aparece. `juntar` ya leía
    /// las dos claves; el stream leía solo `reasoning`, así que un turno que pensaba
    /// no emitía **ni un delta** y la pantalla se quedaba vacía hasta el final.
    #[test]
    fn el_pensamiento_de_thinking_llega_al_stream() {
        let mut t = Transmision::nueva("qwen3.5:0.8b".into());
        t.recibir("{\"model\":\"q\",\"message\":{\"content\":\"\",\"thinking\":\"Pienso\"},\"done\":false}\n")
            .unwrap();
        t.recibir("{\"model\":\"q\",\"message\":{\"content\":\"luego \"},\"done\":false}\n")
            .unwrap();
        t.recibir("{\"model\":\"q\",\"message\":{\"content\":\"hablo\"},\"done\":true,\"eval_count\":3}")
            .unwrap();
        t.rematar().unwrap();
        let tipos: Vec<&str> = t
            .pendientes
            .iter()
            .map(|d| match d {
                StreamDelta::Razonamiento(_) => "razón",
                StreamDelta::Texto(_) => "texto",
                StreamDelta::Final(_) => "final",
            })
            .collect();
        assert_eq!(tipos, vec!["razón", "texto", "texto", "final"], "{tipos:?}");
    }

    /// Ollama manda la línea `done: true` **sin** salto de línea final: si ese resto
    /// se perdiera, las cuentas del turno irían a cero.
    #[test]
    fn el_remate_se_come_la_ultima_linea_sin_salto_y_da_las_cuentas() {
        let mut t = Transmision::nueva("gemma3:1b".into());
        t.recibir("{\"model\":\"gemma3:1b\",\"message\":{\"content\":\"Hola\"},\"done\":false}\n")
            .unwrap();
        t.recibir("{\"model\":\"gemma3:1b\",\"message\":{\"content\":\"\"},\"done\":true,\"prompt_eval_count\":19,\"eval_count\":12}")
            .unwrap();
        assert_eq!(t.pendientes.len(), 1, "la segunda línea no tenía letra");
        t.rematar().expect("hay líneas que rematar");
        let final_r = match t
            .pendientes
            .iter()
            .find(|d| matches!(d, StreamDelta::Final(_)))
            .expect("el remate cierra con las cuentas")
        {
            StreamDelta::Final(r) => r.clone(),
            _ => unreachable!(),
        };
        assert_eq!(final_r.texto, "Hola");
        assert_eq!(final_r.tokens_entrada, Some(19));
        assert_eq!(final_r.tokens_salida, Some(12));
    }

    #[test]
    fn una_respuesta_vacia_sigue_siendo_un_error() {
        let mut t = Transmision::nueva("g".into());
        let e = t.rematar().expect_err("cuerpo vacío no es un turno terminado");
        assert!(matches!(e, ProviderError::RespuestaInvalida(_)));
    }

    /// El arreglo del 07-10, probado con un servidor que se niega a seguir escribiendo
    /// hasta que el cliente pida la primera letra: si alguien vuelve a acumular el
    /// NDJSON entero antes de emitir, esto no pasa — se cuelga y el `timeout` lo
    /// convierte en un fallo claro. Con el cuerpo ya cerrado del todo, lo que se
    /// veía en el chat era la respuesta entera de golpe.
    #[tokio::test]
    async fn cada_letra_sale_antes_de_que_termine_el_cuerpo() {
        use futures_util::StreamExt;
        use std::io::{BufRead, Read, Write};

        let linea = |c: &str| {
            format!("{{\"model\":\"gemma3:1b\",\"message\":{{\"content\":\"{c}\"}},\"done\":false}}")
        };
        let lineas: Vec<String> = vec![
            linea("Ho"),
            linea("la"),
            "{\"model\":\"gemma3:1b\",\"message\":{\"content\":\"!\"},\"done\":true,\"eval_count\":3}"
                .to_string(),
        ];
        let (aviso_tx, aviso_rx) = std::sync::mpsc::channel::<()>();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("puerto libre");
        let puerto = listener.local_addr().expect("dirección local").port();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("el cliente conecta");
            let mut cabecera = String::new();
            {
                let mut lector = std::io::BufReader::new(&sock);
                loop {
                    cabecera.clear();
                    match lector.read_line(&mut cabecera) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if cabecera.trim().is_empty() => break,
                        Ok(_) => {}
                    }
                }
            }
            // HTTP/1.0 sin `Content-Length`: el cuerpo se lee hasta el cierre, que es
            // justo lo que permite soltar una línea y esperar.
            let _ = sock.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: application/x-ndjson\r\n\r\n");
            let _ = sock.write_all(lineas[0].as_bytes());
            let _ = sock.write_all(b"\n");
            let _ = sock.flush();
            // Y aquí se juega el test: el servidor no escribe el resto hasta que el
            // consumidor de arriba haya visto la primera línea.
            if aviso_rx.recv_timeout(Duration::from_secs(10)).is_err() {
                return;
            }
            for l in &lineas[1..] {
                let _ = sock.write_all(l.as_bytes());
                let _ = sock.write_all(b"\n");
                let _ = sock.flush();
            }
            let _ = sock.shutdown(std::net::Shutdown::Write);
            // Drenar lo que quedó de la petición: cerrar con bytes sin leer manda un
            // RST y el cliente lo lee como stream roto.
            let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
            let mut sobrante = Vec::new();
            let _ = sock.read_to_end(&mut sobrante);
        });

        let proveedor = OllamaProvider::con_base(format!("http://127.0.0.1:{puerto}"));
        let mut stream = tokio::time::timeout(Duration::from_secs(10), proveedor.stream(req()))
            .await
            .expect("el servidor contestó")
            .expect("el stream se abrió");

        let primero = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("el primer delta llegó con el cuerpo aún abierto")
            .expect("el stream no se cerró antes de tiempo")
            .expect("sin error de proveedor");
        assert_eq!(primero, StreamDelta::Texto("Ho".into()));
        aviso_tx.send(()).expect("aviso al servidor");

        let mut deltas = vec![primero];
        loop {
            match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
                Err(_) => panic!("el stream se quedó colgado después del aviso"),
                Ok(None) => break,
                Ok(Some(Ok(d))) => deltas.push(d),
                Ok(Some(Err(e))) => panic!("error de proveedor: {e}"),
            }
        }
        assert_eq!(deltas.len(), 4, "un delta por línea, más las cuentas finales");
        let texto: String = deltas
            .iter()
            .filter_map(|d| match d {
                StreamDelta::Texto(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texto, "Hola!", "se perdió letra por el camino");
        match deltas.last() {
            Some(StreamDelta::Final(r)) => {
                assert_eq!(r.tokens_salida, Some(3), "las cuentas salen del cuerpo entero");
                assert_eq!(r.modelo, "gemma3:1b");
            }
            otra => panic!("el stream no cerró con las cuentas: {otra:?}"),
        }
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
