//! Puerta genérica OpenAI-compatible: Hugging Face (`https://router.huggingface.co`),
//! vLLM, LM Studio, llama.cpp server, Cerebras, Groq… Vale con dar la base URL.
//!
//! Detrás de la feature `generic`. Reutiliza el cuerpo y el decodificador de
//! `openai`: si un servidor habla OpenAI, habla esto; si no, es un proveedor nuevo
//! y hay que escribirlo, no camuflarlo.

use super::{
    GenerationRequest, GenerationResult, ModelInfo, ModelProvider, ModelStream, ProviderError,
};
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct GenericProvider {
    inner: super::openai::OpenAiProvider,
    /// Cabecera extra para los servidores que no usan `Authorization: Bearer`
    /// (p. ej. `x-api-key`). Nunca se loguea su valor.
    cabecera_extra: Option<(String, String)>,
}

impl GenericProvider {
    /// `base_url` con el prefijo ya incluido (p. ej. `http://127.0.0.1:8080/v1`).
    pub fn nuevo(
        nombre: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> GenericProvider {
        GenericProvider {
            inner: super::openai::OpenAiProvider::con_base(base_url, api_key, nombre),
            cabecera_extra: None,
        }
    }

    pub fn con_cabecera(mut self, clave: &str, valor: &str) -> GenericProvider {
        self.cabecera_extra = Some((clave.to_string(), valor.to_string()));
        self
    }

    pub fn nombre(&self) -> String {
        self.inner.nombre.clone()
    }

    pub fn tiene_clave(&self) -> bool {
        self.inner.tiene_clave()
    }

    /// Expone el cuerpo para que el adaptador lo revise sin red.
    pub fn cuerpo_de(&self, req: &GenerationRequest) -> serde_json::Value {
        super::openai::OpenAiProvider::cuerpo_de(req, &self.inner.nombre)
    }

    pub fn interpretar(&self, j: &serde_json::Value, modelo: &str) -> GenerationResult {
        super::openai::OpenAiProvider::interpretar(j, modelo)
    }
}

#[async_trait]
impl ModelProvider for GenericProvider {
    fn id(&self) -> String {
        self.inner.nombre.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.inner.list_models().await
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationResult, ProviderError> {
        // La cabecera extra no la soporta el inner: se rehace la petición aquí.
        if let Some((k, v)) = self.cabecera_extra.clone() {
            if !self.inner.tiene_clave() {
                return Err(ProviderError::SinCredencial(self.nombre()));
            }
            let cuerpo = {
                let mut c = self.cuerpo_de(&req);
                c["stream"] = serde_json::json!(false);
                c
            };
            let r = reqwest::Client::new()
                .post(format!("{}/chat/completions", self.inner.base_url))
                .timeout(std::time::Duration::from_secs(req.timeout_s.max(1) as u64))
                .header(k, v)
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
            return Ok(self.interpretar(&j, &req.model));
        }
        self.inner.generate(req).await
    }

    async fn stream(&self, req: GenerationRequest) -> Result<ModelStream, ProviderError> {
        if self.cabecera_extra.is_some() {
            // Con cabecera propia no se puede reutilizar el stream del inner: se
            // dice en vez de hacer una petición muda que saldría 401.
            return Err(ProviderError::NoImplementado(
                "el stream con cabecera extra aún no está implementado; usa generate()".into(),
            ));
        }
        self.inner.stream(req).await
    }

    async fn cargados(&self) -> Result<Vec<crate::models::ModeloCargado>, ProviderError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::request::Message;
    use crate::api::vocab::{KeepAlive, ThinkingLevel};

    fn req() -> GenerationRequest {
        GenerationRequest {
            model: "hf:nano".into(),
            system: "s".into(),
            prompt: "hola".into(),
            history: vec![Message::usuario("hola")],
            tools: vec![],
            num_ctx: 2048,
            keep_alive: KeepAlive::PorDefecto,
            thinking: ThinkingLevel::Off,
            max_output_tokens: 256,
            temperature: 0.0,
            seed: 42,
            timeout_s: 30,
        }
    }

    #[test]
    fn habla_el_idioma_openai_con_otra_base() {
        let p = GenericProvider::nuevo("hf", "https://router.huggingface.co/v1", "hf_x")
            .con_cabecera("x-api-key", "zz");
        let c = p.cuerpo_de(&req());
        assert_eq!(c["model"], "hf:nano");
        // system + historial + el turno actual.
        let msgs = c["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[2]["content"], "hola");
        assert_eq!(p.nombre(), "hf");
        assert!(p.tiene_clave());
    }

    #[test]
    fn sin_clave_y_sin_cabecera_extra_es_error_tipado() {
        let p = GenericProvider::nuevo("vllm", "http://127.0.0.1:8000/v1", "");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(
            rt.block_on(p.list_models()).unwrap_err(),
            ProviderError::SinCredencial("vllm".into())
        );
    }
}
