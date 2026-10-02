//! El vocabulario que comparten Request · Decision · Plan · Verification.
//!
//! Son los cinco contratos estables del Canon (§V): si cambia el interior, estos
//! nombres aguantan. Por eso casi todo son enums cerrados con `serde` explícito:
//! un `#[serde(other)]` silencioso convertiría un typo de config en un
//! comportamiento distinto sin avisar, y eso es justo lo que el Canon prohíbe
//! («si Hatboo no sabe, lo dice»).

use serde::{Deserialize, Serialize};

/// El modelo que eligió el usuario o el que eligió el Selector. Un `String`
/// porque cada producto nombra sus modelos distinto (`qwen3.5:0.8b`,
/// `gpt-4o-mini`, `hf.co/…`): cerrarlo en un enum haría el crate inutilizable
/// para el segundo proyecto.
pub type ModelId = String;
pub type ToolId = String;
pub type ProviderId = String;

/// El modo es del producto: un `String`. Esta unidad solo existe para colgar las
/// dos constantes de Hatboo sin romper la regla huérfana (`impl String` no se
/// puede escribir en este crate). Un producto nuevo pasa el suyo y las reglas de
/// `brain-rules.json` deciden por modo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode;

impl Mode {
    pub const CHAT: &'static str = "chat";
    pub const WORK: &'static str = "work";
}

/// N0–N3: cuánto administra el Brain. No es cuánto razona el modelo (`thinking`)
/// ni es un `effort`; son ejes distintos y el Canon lo cierra en §1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Level {
    N0,
    N1,
    N2,
    N3,
}

impl Level {
    /// Etiqueta de UI. Instantáneo/Normal/Enfocado/Profundo nunca son un campo.
    pub fn etiqueta(&self) -> &'static str {
        match self {
            Level::N0 => "Instantáneo",
            Level::N1 => "Normal",
            Level::N2 => "Enfocado",
            Level::N3 => "Profundo",
        }
    }

    /// Mínimo de verificación que exige el nivel (§4 del plan).
    pub fn verificacion_minima(&self) -> VerificationMode {
        match self {
            Level::N0 => VerificationMode::Ninguna,
            Level::N1 => VerificationMode::Formato,
            Level::N2 | Level::N3 => VerificationMode::Determinista,
        }
    }

    pub fn reintentos(&self) -> u8 {
        match self {
            Level::N0 => 0,
            Level::N1 => 1,
            Level::N2 | Level::N3 => 2,
        }
    }

    /// `num_ctx` mínimo del nivel. Dentro de {2048, 4096, 8192}; ampliar el
    /// conjunto es config por modelo (§15.5 del plan), no una constante nueva.
    pub fn num_ctx_minimo(&self) -> u32 {
        match self {
            Level::N0 | Level::N1 => 2048,
            Level::N2 => 4096,
            Level::N3 => 8192,
        }
    }

    /// N0 y N1 no ven tools. La única excepción es la lectura directa de un
    /// camino `skip_generative`, que ejecuta el producto, no el modelo.
    pub fn permite_tools(&self) -> bool {
        matches!(self, Level::N2 | Level::N3)
    }

    pub fn siguiente(&self) -> Option<Level> {
        match self {
            Level::N0 => Some(Level::N1),
            Level::N1 => Some(Level::N2),
            Level::N2 => Some(Level::N3),
            Level::N3 => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Intent {
    Ask,
    Explain,
    Create,
    Modify,
    Search,
    Execute,
    Verify,
}

impl Intent {
    /// Contrato por defecto del nivel y del intent (§IX del Canon).
    pub fn contrato_por_defecto(&self) -> OutputContract {
        match self {
            Intent::Ask => OutputContract::Texto,
            Intent::Explain => OutputContract::Markdown,
            Intent::Create => OutputContract::Markdown,
            Intent::Modify => OutputContract::Patch,
            Intent::Search | Intent::Execute => OutputContract::ToolCall,
            Intent::Verify => OutputContract::Texto,
        }
    }

    /// Un intent que necesita actuar, no hablar.
    pub fn quiere_obras(&self) -> bool {
        matches!(self, Intent::Modify | Intent::Search | Intent::Execute)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionTarget {
    /// En la máquina del usuario (Ollama u homólogo).
    Local,
    /// Fuera: cualquier API. Nunca sin policy que lo permita y consentimiento.
    Api,
    /// El plan de contingencia: lo que queda cuando lo pedido no cabe.
    Fallback,
}

/// Perfil de modelo: solo presupuesta contexto. No decide cuál se usa (`tier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// ≤2B parámetros.
    Nano,
    /// 3–9B.
    Small,
    /// >9B o nube.
    Large,
}

/// Lo que el Engine pide, no el modelo concreto: el Selector + Governor ponen el
/// `ModelId` al final (§VII del Canon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelTarget {
    Perfil(Profile),
    Tier(u8),
    /// El Engine no tiene preferencia: que elija el Selector por coste.
    Indistinto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputContract {
    /// El Canon escribe los contratos en inglés; el config versionado puede
    /// venir de las dos formas.
    #[serde(alias = "text")]
    Texto,
    Markdown,
    Json,
    #[serde(rename = "tool_call")]
    ToolCall,
    Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    Ninguna,
    Formato,
    Determinista,
    /// Determinista + revisión por modelo. Solo si el bench la justifica.
    DeterministaRevision,
}

/// Cuánto razona el modelo. En Ollama es `think`; en Anthropic, presupuesto de
/// pensamiento; en otros, no existe y se ignora (con registro).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    /// Cuánto razonamiento pide este nivel al proveedor. Es **una sola tabla** para
    /// el presupuesto de §11 y para lo que se manda por la línea: mientras vivían
    /// en dos sitios distintos, el Plan reservaba 256 y Anthropic pedía 1024, así
    /// que el razonamiento se comía la respuesta en vez de sumarse a ella (medido en
    /// el cuerpo de `cuerpo_de`: un N2 con `medium` se quedaba con 1 token de
    /// salida). Los números son las tarifas de Anthropic, que es el único proveedor
    /// del crate con presupuesto explícito; los demás cuentan el razonamiento dentro
    /// del tope de salida.
    pub fn presupuesto_tokens(&self) -> u32 {
        match self {
            ThinkingLevel::Off => 0,
            ThinkingLevel::Low => 1024,
            ThinkingLevel::Medium => 4096,
            ThinkingLevel::High => 10_240,
        }
    }
}

/// Los cuatro niveles de aprobación del producto. Ordenados de menos a más
/// capacidad: el Brain interseca esto con la policy y **nunca** lo relaja.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalLevel {
    #[default]
    AskAlways,
    ApproveForMe,
    AutoSandbox,
    FullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPolicy {
    /// Air-gap: ni una petición sale del equipo aunque no haya modelo local.
    LocalOnly,
    /// El default: primero lo que ya está en la máquina, y la nube solo si no cabe.
    #[default]
    LocalPreferred,
    Balanced,
    CloudAllowed,
    CloudOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepAlive {
    /// Lo que diga el servidor (Ollama: 5 min).
    PorDefecto,
    /// Mantenerlo ese rato: barato entre turnos de la misma sesión.
    Segundos(u32),
    /// Expulsar al terminar. Se paga la carga (~7 s medidos) en el siguiente turno.
    Expulsar,
}

/// De dónde salió una decisión. Manda a log y al panel «por qué».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    FastPath,
    Reglas,
    Cache,
    /// Fase 6: logprobs del modelo ya cargado.
    Logprobs,
    /// Fase 7: backend de decisión (Laya u otro).
    Backend,
    /// El plan seguro tras un `Plan` inválido.
    PlanSeguro,
}

/// Confianza con umbral. Dos orígenes muy distintos (§1 del plan): reglas
/// (determinista, gobierna desde Fase 3) y backend de modelo (solo log hasta
/// `calibrated`). Ninguno revierte un Pass/Fail determinista.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Confidence(pub f32);

impl Confidence {
    pub const UMBRAL_ALTO: f32 = 0.85;
    pub const UMBRAL_BAJO: f32 = 0.55;

    pub fn determinista() -> Self {
        Confidence(1.0)
    }

    /// Coincidencia exacta de regla: 1.0. En scoring, el margen entre el primer
    /// y el segundo candidato.
    pub fn margen(primero: f32, segundo: f32) -> Self {
        Confidence(((primero - segundo) / primero.max(1e-6)).clamp(0.0, 1.0))
    }

    pub fn valor(&self) -> f32 {
        self.0.clamp(0.0, 1.0)
    }

    /// «Ante duda de nivel: el más alto» — la duda es exactamente esto.
    pub fn hay_duda(&self) -> bool {
        self.valor() < Self::UMBRAL_ALTO
    }

    /// `< 0.55` no se trata como seguro: `risk` ≥ medio y verificación ≥ format.
    pub fn es_insegura(&self) -> bool {
        self.valor() < Self::UMBRAL_BAJO
    }
}

/// Las seis clases de fallo (§XIV). Cada una hace *su* acción; solo
/// `model_capability` sube de tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Formato,
    Tool,
    Contexto,
    /// Timeout, OOM o proveedor caído: el entorno, no el modelo.
    Entorno,
    ModelCapability,
    /// El verificador no pudo correr → `Unverifiable`.
    Verificacion,
}

/// Señales medidas **antes** de decidir. El código las calcula; las reglas de
/// `brain-rules.json` las interpretan. El crate no incrusta políticas de producto.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Signals {
    pub message_length: usize,
    pub has_code: bool,
    pub has_file_path: bool,
    pub has_action_verb: bool,
    pub mentions_git: bool,
    pub mentions_web: bool,
    /// Pide algo que está dentro del proyecto («¿en qué archivo…?», «lista los
    /// archivos»). Sin esta señal no hay regla que ofrezca `list_dir` ni
    /// `search_files`, y esas tools eran inalcanzables.
    pub mentions_project_files: bool,
    /// Pide correr algo («ejecuta cargo check», «run the tests»). Lo mismo: sin
    /// señal no había regla que llegara a `run_command`.
    pub mentions_command: bool,
    pub risk_hint: Risk,
    pub session_failure: Option<FailureClass>,
    pub project_has_tests: bool,
    pub language: String,
}

impl Signals {
    /// Nombre de la señal, tal y como se escribe en `brain-rules.json`.
    pub fn nombre(&self) -> &'static str {
        "signals"
    }

    /// Valor de una señal por su nombre, para que las reglas sean JSON y no
    /// código. `None` si la regla pide una señal que no existe: eso es un error
    /// de config, no un `false`.
    pub fn valor(&self, senal: &str) -> Option<serde_json::Value> {
        let v = match senal {
            "message_length" => serde_json::json!(self.message_length),
            "has_code" => serde_json::json!(self.has_code),
            "has_file_path" => serde_json::json!(self.has_file_path),
            "has_action_verb" => serde_json::json!(self.has_action_verb),
            "mentions_git" => serde_json::json!(self.mentions_git),
            "mentions_web" => serde_json::json!(self.mentions_web),
            "mentions_project_files" => serde_json::json!(self.mentions_project_files),
            "mentions_command" => serde_json::json!(self.mentions_command),
            "risk_hint" => serde_json::to_value(self.risk_hint).ok()?,
            "session_failure" => serde_json::to_value(self.session_failure).ok()?,
            "project_has_tests" => serde_json::json!(self.project_has_tests),
            "language" => serde_json::json!(self.language),
            _ => return None,
        };
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_arrastra_sus_minimos() {
        assert_eq!(Level::N2.num_ctx_minimo(), 4096);
        assert_eq!(Level::N1.verificacion_minima(), VerificationMode::Formato);
        assert!(!Level::N1.permite_tools());
        assert_eq!(Level::N0.reintentos(), 0);
        assert_eq!(Level::N3.siguiente(), None);
    }

    #[test]
    fn approval_se_ordena_como_el_producto() {
        assert!(ApprovalLevel::AskAlways < ApprovalLevel::FullAccess);
        // Los ids tienen que empatar con los que ya guarda Hatboo en Settings.
        assert_eq!(
            serde_json::to_string(&ApprovalLevel::ApproveForMe).unwrap(),
            "\"approve_for_me\""
        );
    }

    /// La reserva de §11 y lo que el proveedor pide por la línea son **la misma**
    /// tabla. Mientras fueron dos, el Plan apartaba 256 y Anthropic pedía 1024: el
    /// razonamiento se comía la respuesta en silencio.
    #[test]
    fn el_presupuesto_de_razonamiento_vive_en_un_solo_sitio() {
        assert_eq!(ThinkingLevel::Off.presupuesto_tokens(), 0);
        for t in [ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High] {
            // 1024 es el suelo que admite Anthropic; por debajo, el `budget_tokens`
            // que se manda es un 400.
            assert!(t.presupuesto_tokens() >= 1024, "{t:?}");
            assert_eq!(
                crate::planner::plan::thinking_extra_tokens(&t),
                t.presupuesto_tokens(),
                "{t:?}: dos tablas para la misma cifra"
            );
        }
        assert!(
            ThinkingLevel::Low.presupuesto_tokens() < ThinkingLevel::High.presupuesto_tokens()
        );
    }

    #[test]
    fn senales_hablan_en_el_idioma_de_las_reglas() {
        let s = Signals {
            message_length: 42,
            has_file_path: true,
            language: "es".into(),
            ..Default::default()
        };
        assert_eq!(s.valor("has_file_path"), Some(serde_json::json!(true)));
        assert_eq!(s.valor("message_length"), Some(serde_json::json!(42)));
        assert_eq!(s.valor("no_existe"), None);
    }

    #[test]
    fn confianzadefine_la_duda() {
        assert!(Confidence(0.84).hay_duda());
        assert!(!Confidence(0.85).hay_duda());
        assert!(Confidence(0.5).es_insegura());
        // El margen es relativo: 0,01 sobre 0,9 no es 0,01.
        let m = Confidence::margen(0.9, 0.89).valor();
        assert!((m - 0.011_111_1).abs() < 1e-5, "{m}");
        assert!(Confidence::margen(0.9, 0.5).valor() > 0.4);
    }
}
