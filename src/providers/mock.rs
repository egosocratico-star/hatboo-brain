//! Proveedor de pruebas y smoke test sin red. Es también el andamio con el que un
//! producto nuevo comprueba que sabe hablar con el Brain antes de gastar RAM.

use super::{GenerationRequest, GenerationResult, ModelInfo, ModelProvider, ModelStream, ProviderError, StreamDelta};
use crate::models::ModeloCargado;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct Cola {
    pendiente: Vec<GenerationResult>,
    peticiones: Vec<GenerationRequest>,
}

#[derive(Debug, Clone)]
pub struct MockProvider {
    estado: Arc<Mutex<Cola>>,
    pub modelos: Vec<ModelInfo>,
    pub cargados: Vec<ModeloCargado>,
    pub error: Option<ProviderError>,
    pub nombre: String,
}

impl Default for MockProvider {
    fn default() -> Self {
        MockProvider {
            estado: Default::default(),
            modelos: vec![],
            cargados: vec![],
            error: None,
            nombre: "mock".into(),
        }
    }
}

impl MockProvider {
    pub fn nuevo(modelos: Vec<ModelInfo>) -> MockProvider {
        MockProvider {
            modelos,
            ..Default::default()
        }
    }

    pub fn con_nombre(mut self, n: &str) -> MockProvider {
        self.nombre = n.into();
        self
    }

    /// Encola una salida. Se consumen en orden; al agotar la cola se devuelve un
    /// error en vez de repetir la última: un test que pide más de lo que preparó
    /// tiene que romper, no pasar por casualidad.
    pub fn responde(&self, r: GenerationResult) {
        self.estado.lock().unwrap().pendiente.push(r);
    }

    pub fn responde_texto(&self, texto: &str) {
        self.responde(GenerationResult {
            texto: texto.into(),
            ..Default::default()
        });
    }

    pub fn falla_con(&mut self, e: ProviderError) {
        self.error = Some(e);
    }

    pub fn ultima_peticion(&self) -> Option<GenerationRequest> {
        self.estado.lock().unwrap().peticiones.last().cloned()
    }

    pub fn peticiones(&self) -> Vec<GenerationRequest> {
        self.estado.lock().unwrap().peticiones.clone()
    }

    pub fn n_peticiones(&self) -> usize {
        self.estado.lock().unwrap().peticiones.len()
    }

    fn sacar(&self, req: &GenerationRequest) -> Result<GenerationResult, ProviderError> {
        let mut g = self.estado.lock().unwrap();
        g.peticiones.push(req.clone());
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if g.pendiente.is_empty() {
            return Err(ProviderError::RespuestaInvalida(
                "el mock no tiene más respuestas preparadas".into(),
            ));
        }
        Ok(g.pendiente.remove(0))
    }
}

#[async_trait]
impl ModelProvider for MockProvider {
    fn id(&self) -> String {
        self.nombre.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(self.modelos.clone())
    }

    async fn generate(&self, req: GenerationRequest) -> Result<GenerationResult, ProviderError> {
        self.sacar(&req)
    }

    async fn stream(&self, req: GenerationRequest) -> Result<ModelStream, ProviderError> {
        let r = self.sacar(&req)?;
        let trozos: Vec<String> = r
            .texto
            .split_inclusive(' ')
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut partes: Vec<Result<StreamDelta, ProviderError>> = trozos
            .into_iter()
            .map(|t| Ok(StreamDelta::Texto(t)))
            .collect();
        partes.push(Ok(StreamDelta::Final(r)));
        Ok(Box::pin(futures_util::stream::iter(partes)))
    }

    async fn cargados(&self) -> Result<Vec<ModeloCargado>, ProviderError> {
        Ok(self.cargados.clone())
    }

    /// El mock no tiene servidor al que expulsar: dice que sí para que el runtime
    /// ejercite la ruta, y lo deja registrado en `peticiones`.
    async fn expulsar(&self, _model: &str) -> Result<(), ProviderError> {
        Ok(())
    }
}
