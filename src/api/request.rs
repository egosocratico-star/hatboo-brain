//! Lo que entra en el Brain. Sin widgets de UI dentro (§VI del Canon).

use super::vocab::{
    ApprovalLevel, ExecutionPolicy, FailureClass, ModelId, Risk, ToolId,
};
use serde::{Deserialize, Serialize};

/// Un mensaje del historial de **esta** sesión. Nada de memoria entre chats: el
/// Canon lo cierra en §II y §13 del plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// `user` | `assistant` | `system` | `tool`
    pub role: String,
    pub content: String,
    /// Razonamiento visible del modelo, si lo tuvo. Entra al contexto cuando el
    /// producto lo pide, nunca como verdad de usuario.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl Message {
    pub fn usuario(texto: impl Into<String>) -> Self {
        Message {
            role: "user".into(),
            content: texto.into(),
            reasoning: None,
        }
    }

    pub fn asistente(texto: impl Into<String>) -> Self {
        Message {
            role: "assistant".into(),
            content: texto.into(),
            reasoning: None,
        }
    }
}

/// Qué puede tocar una tool. `es_write` manda en `max_write_actions`; el resto es
/// informativa para el prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub id: ToolId,
    #[serde(default)]
    pub escribe: bool,
    #[serde(default)]
    pub descripcion: String,
}

/// Las tools que el producto ofrece **en este modo**. El Engine candidatea sobre
/// esta lista; el Plan firma una parte; el Tool Gate rechaza en código todo lo
/// que salga de esa parte.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolSet {
    #[serde(default)]
    pub disponibles: Vec<ToolInfo>,
}

impl ToolSet {
    pub fn ids(&self) -> Vec<ToolId> {
        self.disponibles.iter().map(|t| t.id.clone()).collect()
    }

    pub fn es_write(&self, id: &str) -> bool {
        self.disponibles
            .iter()
            .any(|t| t.id == id && t.escribe)
    }

    pub fn contiene(&self, id: &str) -> bool {
        self.disponibles.iter().any(|t| t.id == id)
    }
}

/// Los comandos de verificación del proyecto (§15.7 del plan: los detecta el
/// producto, el Brain los corre en el sandbox del producto).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCommands {
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub lint: Option<String>,
    #[serde(default)]
    pub test: Option<String>,
}

impl VerifyCommands {
    pub fn alguno(&self) -> bool {
        self.check.is_some() || self.lint.is_some() || self.test.is_some()
    }
}

/// Estado de confianza de `HATBOO.md`. Sin `Aprobado` no entra al prompt, y nunca
/// amplía permisos ni apaga la redacción de secretos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    #[default]
    SinRevisar,
    Aprobado,
    Cambiado,
    Rechazado,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContext {
    pub root: String,
    #[serde(default)]
    pub has_tests: bool,
    #[serde(default)]
    pub has_lint: bool,
    #[serde(default)]
    pub has_build: bool,
    #[serde(default)]
    pub language: Option<String>,
    /// El `HATBOO.md` aprobado, con su hash. `None` = el producto no lo pidió o
    /// no está aprobado, y entonces no entra.
    #[serde(default)]
    pub hatboo_md: Option<String>,
    #[serde(default)]
    pub trust_state: TrustState,
    #[serde(default)]
    pub verify: VerifyCommands,
}

/// Lo que el producto manda al Brain. `plan()` y `run()` toman exactamente esto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainRequest {
    /// Nombre del producto en el log: `hatboo`, u otro crate consumidor.
    pub product: String,
    /// El modo del producto, en su propio vocabulario (`chat`, `work`, …).
    pub mode: String,
    pub message: String,
    /// Lo que eligió el usuario. Se respeta si cabe y la policy lo permite; si
    /// no, siguiente viable con `reason` visible. No es un selector silencioso.
    #[serde(default)]
    pub preferred_model: Option<ModelId>,
    #[serde(default)]
    pub project: Option<ProjectContext>,
    /// Solo esta sesión.
    #[serde(default)]
    pub history: Vec<Message>,
    /// Lo que el producto añade a cada turno y **no es conversación**: las
    /// plantillas activas, las notas de memoria del usuario y cualquier otro texto
    /// de identidad o contexto que el producto quiera dentro del prompt.
    ///
    /// Viaja aquí, y no cabalgando en un mensaje `system` de `history`, por dos
    /// motivos que se vieron en el producto. El primero: el armador presupuesta el
    /// historial como un sufijo que se corta de atrás hacia adelante, así que lo
    /// primero que se caía cuando apretaba la ventana era justamente la identidad,
    /// las skills y la memoria. El segundo: `providers/anthropic.rs` filtra los
    /// `system` del historial, y por esa puerta el bloque desaparecía sin decirlo.
    ///
    /// Este campo entra en el system prompt por su propia capa (`producto`) y, por
    /// tanto, en `Presupuesto.fijo`: **no se recorta nunca**. Si con todo dentro no
    /// cabe, el Plan lo dice en `reason` en vez de cortar una skill a medias.
    #[serde(default)]
    pub contexto_producto: Vec<Message>,
    #[serde(default)]
    pub tools: ToolSet,
    #[serde(default)]
    pub policies: ExecutionPolicy,
    #[serde(default)]
    pub approval_level: ApprovalLevel,
    /// Techo del producto para `thinking`. `None` = off, y el Brain no sube.
    #[serde(default)]
    pub thinking_ceiling: Option<super::vocab::ThinkingLevel>,
    /// Fallos ya ocurridos en la sesión. Cambian el nivel ante duda (§XI:
    /// «un fallo repetido no se queda artificialmente bajo»).
    #[serde(default)]
    pub session_failures: Vec<FailureClass>,
    /// Riesgo que el producto ya vio (p. ej. una tool de borrado seleccionada).
    #[serde(default)]
    pub risk_hint: Option<Risk>,
}

impl BrainRequest {
    pub fn nuevo(
        product: impl Into<String>,
        mode: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        BrainRequest {
            product: product.into(),
            mode: mode.into(),
            message: message.into(),
            preferred_model: None,
            project: None,
            history: Vec::new(),
            contexto_producto: Vec::new(),
            tools: ToolSet::default(),
            policies: ExecutionPolicy::default(),
            approval_level: ApprovalLevel::AskAlways,
            thinking_ceiling: None,
            session_failures: Vec::new(),
            risk_hint: None,
        }
    }

    pub fn con_modelo(mut self, m: impl Into<ModelId>) -> Self {
        self.preferred_model = Some(m.into());
        self
    }

    pub fn con_tools(mut self, tools: Vec<ToolInfo>) -> Self {
        self.tools = ToolSet { disponibles: tools };
        self
    }

    pub fn en_proyecto(mut self, p: ProjectContext) -> Self {
        self.project = Some(p);
        self
    }

    pub fn con_aprobacion(mut self, a: ApprovalLevel) -> Self {
        self.approval_level = a;
        self
    }

    /// Ver el campo: texto del producto que entra en el system prompt y **no** en
    /// el historial.
    pub fn con_contexto_producto(mut self, m: Vec<Message>) -> Self {
        self.contexto_producto = m;
        self
    }

    pub fn con_policy(mut self, p: ExecutionPolicy) -> Self {
        self.policies = p;
        self
    }

    /// El bloque del producto como texto para la capa `producto` del system. `None`
    /// si está vacío, que es el caso de siempre: un pedido sin contexto de producto
    /// produce exactamente los mismos bytes que antes de este campo.
    ///
    /// Se unen los contenidos sin roles: aquí el rol no informa — lo que manda es
    /// que todo esto es material del producto, no turnos de una conversación — y
    /// el escape de la capa se aplica sobre el bloque entero.
    pub fn texto_contexto_producto(&self) -> Option<String> {
        let unido: Vec<String> = self
            .contexto_producto
            .iter()
            .map(|m| m.content.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if unido.is_empty() {
            None
        } else {
            Some(unido.join("\n\n"))
        }
    }

    /// Lo que el Brain sabe del proyecto sin Option<> gymnastics.
    pub fn tiene_tests(&self) -> bool {
        self.project.as_ref().map(|p| p.has_tests).unwrap_or(false)
    }

    pub fn raiz(&self) -> Option<&str> {
        self.project.as_ref().map(|p| p.root.as_str())
    }
}

/// Razones por las que un `BrainRequest` no es ejecutable. Tipadas: el Canon
/// prohíbe `String` sueltos en la API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestIssue {
    MessageVacio,
    ProductoVacio,
    ModoVacio,
    HerramientaDuplicada(ToolId),
    RootInexistente,
}

impl RequestIssue {
    pub fn mensaje(&self) -> String {
        match self {
            RequestIssue::MessageVacio => "el mensaje está vacío".into(),
            RequestIssue::ProductoVacio => "falta el nombre de producto".into(),
            RequestIssue::ModoVacio => "falta el modo".into(),
            RequestIssue::HerramientaDuplicada(t) => {
                format!("la tool «{t}» está declarada dos veces")
            }
            RequestIssue::RootInexistente => "el proyecto no tiene carpeta raíz".into(),
        }
    }
}

impl std::fmt::Display for RequestIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.mensaje())
    }
}

impl BrainRequest {
    /// Comprueba lo que se puede comprobar sin conocer el registry. El resto lo
    /// valida el Planner.
    pub fn validar(&self) -> Result<(), RequestIssue> {
        if self.message.trim().is_empty() {
            return Err(RequestIssue::MessageVacio);
        }
        if self.product.trim().is_empty() {
            return Err(RequestIssue::ProductoVacio);
        }
        if self.mode.trim().is_empty() {
            return Err(RequestIssue::ModoVacio);
        }
        let mut vistos = std::collections::HashSet::new();
        for t in &self.tools.disponibles {
            if !vistos.insert(t.id.clone()) {
                return Err(RequestIssue::HerramientaDuplicada(t.id.clone()));
            }
        }
        if let Some(p) = &self.project {
            if p.root.trim().is_empty() {
                return Err(RequestIssue::RootInexistente);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::Mode;

    #[test]
    fn un_pedido_minimo_es_valido() {
        let r = BrainRequest::nuevo("hatboo", Mode::CHAT, "hola");
        assert_eq!(r.validar(), Ok(()));
        assert_eq!(r.policies, ExecutionPolicy::LocalPreferred);
        assert_eq!(r.approval_level, ApprovalLevel::AskAlways);
        assert_eq!(r.thinking_ceiling, None);
    }

    #[test]
    fn detecta_lo_imposible_antes_de_decidir() {
        assert_eq!(
            BrainRequest::nuevo("hatboo", Mode::CHAT, "   ").validar(),
            Err(RequestIssue::MessageVacio)
        );
        let mut r = BrainRequest::nuevo("hatboo", Mode::WORK, "ve");
        r.tools = ToolSet {
            disponibles: vec![
                ToolInfo {
                    id: "read_file".into(),
                    escribe: false,
                    descripcion: String::new(),
                },
                ToolInfo {
                    id: "read_file".into(),
                    escribe: false,
                    descripcion: String::new(),
                },
            ],
        };
        assert_eq!(r.validar(), Err(RequestIssue::HerramientaDuplicada("read_file".into())));
    }

    #[test]
    fn viaja_en_json_y_vuelve_igual() {
        let r = BrainRequest::nuevo("hatboo", Mode::WORK, "abre main.rs")
            .con_modelo("qwen3.5:0.8b")
            .con_aprobacion(ApprovalLevel::AutoSandbox);
        let s = serde_json::to_string(&r).unwrap();
        let v: BrainRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(v, r);
        // El nombre del campo que ve el producto es camelCase, como en Hatboo.
        assert!(s.contains("\"preferredModel\""));
        assert!(s.contains("\"auto_sandbox\""));
    }

    /// El consumidor viejo no se entera: un JSON sin el campo nuevo sigue
    /// parseando, y con el campo vacío `texto_contexto_producto` es `None`, que es
    /// exactamente lo que hace que el system salga byte a byte como antes.
    #[test]
    fn un_pedido_sin_el_campo_nuevo_sigue_parseando_y_no_mueve_el_system() {
        let sin_campo = r#"{"product":"hatboo","mode":"chat","message":"hola"}"#;
        let v: BrainRequest = serde_json::from_str(sin_campo).expect("un JSON viejo parsea");
        assert!(v.contexto_producto.is_empty());
        assert_eq!(v.texto_contexto_producto(), None);

        let r = BrainRequest::nuevo("hatboo", Mode::CHAT, "hola").con_contexto_producto(vec![
            Message::usuario("  "),
            Message::usuario("prefiero respuestas cortas"),
        ]);
        assert_eq!(
            r.texto_contexto_producto().as_deref(),
            Some("prefiero respuestas cortas"),
            "los mensajes en blanco no aportan nada y no deben abrir la capa"
        );

        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"contextoProducto\""), "{s}");
        let v: BrainRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(v, r);
    }
}
