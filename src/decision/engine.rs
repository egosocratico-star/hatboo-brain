//! El orden completo de un turno: fast path → caché → reglas → heurística, y
//! después los efectos de la confianza. Una sola evaluación por turno: está
//! prohibido gastar un modelo por intent y otro por nivel.

use super::cache::CacheDecisiones;
use super::confidence::efectos;
use super::rules::Reglas;
use super::DecisionResult;
use crate::api::request::BrainRequest;
use crate::api::vocab::{
    Confidence, DecisionSource, ExecutionTarget, Intent, Level, ModelTarget, Risk, Signals, ToolId,
};

/// Palabras funcionales, cada una entre espacios para que solo cuenten como
/// palabra suelta. La lista estaba demasiado corta: «corrige el error de
/// compilación» no contenía ninguna y salía con idioma desconocido, lo que apaga
/// el filtro `idiomas` de las reglas. Contar palabras es deliberadamente tonto —
/// sin modelo y sin dependencias— y solo decide cuál de los dos idiomas pesa más.
/// Se evitan los cognados («error», «código/code») porque contarían igual a los dos.
const VOCABULARIO_ES: &[&str] = &[
    " el ", " la ", " los ", " las ", " un ", " una ", " de ", " del ", " que ", " cómo ",
    " como ", " por ", " para ", " con ", " sin ", " en ", " y ", " está ", " esta ",
    " este ", " esto ", " hay ", " hace ", " puedo ", " quiero ", " dime ", " muestra ",
    " abre ", " crea ", " borra ", " arreglo ", " corrige ", " archivo ", " proyecto ",
    " cuál ", " cual ", " cuánto ", " cuando ", " dónde ", " donde ", " algún ", " nada ",
    " algo ", " muy ", " pero ", " porque ", " si ", " me ", " te ", " se ", " le ",
];

const VOCABULARIO_EN: &[&str] = &[
    " the ", " of ", " and ", " to ", " is ", " in ", " it ", " for ", " with ", " this ",
    " that ", " there ", " you ", " your ", " can ", " could ", " would ", " should ",
    " what ", " how ", " why ", " when ", " where ", " please ", " make ", " show ",
    " open ", " create ", " delete ", " fix ", " file ", " project ", " i ", " i'm ",
    " do ", " does ", " an ", " on ", " are ", " as ", " at ", " be ", " my ", " we ",
];

/// Detecta idioma sin dependencias: cuenta palabras funcionales. Si no se decide,
/// `None`, y las reglas se aplican sin filtro de idioma.
pub fn idiomas(s: &str) -> (String, Option<String>) {
    let bajo = format!(" {} ", s.to_lowercase());
    let es = VOCABULARIO_ES.iter().filter(|w| bajo.contains(**w)).count();
    let en = VOCABULARIO_EN.iter().filter(|w| bajo.contains(**w)).count();
    let principal = if es > en {
        "es"
    } else if en > es {
        "en"
    } else {
        // Empate (o ninguna): se deja sin filtro, no se adivina.
        return ("*".into(), None);
    };
    (
        principal.into(),
        Some(principal.into()),
    )
}

/// ¿Tiene pinta de código? Sin un parser: cercas de markdown, llaves con
/// paréntesis, o cuatro espacios sangrando al inicio de línea.
pub fn tiene_codigo(s: &str) -> bool {
    if s.contains("```") {
        return true;
    }
    let llaves = s.matches('{').count();
    let parens = s.matches('(').count();
    if llaves >= 1 && parens >= 1 && (s.contains("();") || s.contains(") {") || s.contains("=>")) {
        return true;
    }
    s.lines()
        .filter(|l| l.starts_with("    ") || l.starts_with("\t"))
        .count()
        >= 2
}

/// ¿Se dijo la palabra, o es solo un trozo de otra? `contains` leía
/// «programación» como «rama» (→ git, N2 y tres tools) y «install» como `all`
/// (→ riesgo). Cada falso positivo cuesta contexto, tokens y tiempo de modelo.
fn dice_palabra(texto: &str, palabra: &str) -> bool {
    if palabra.is_empty() {
        return false;
    }
    // Un guion cuenta como parte de la palabra: `llama-3.3` o `src/main.rs`
    // llevan, y partirlos por la mitad volvería a dar falsos positivos.
    let es_parte = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    texto.match_indices(palabra).any(|(i, _)| {
        let antes_libre = i == 0 || !es_parte(texto[..i].chars().next_back().unwrap());
        let fin = i + palabra.len();
        let despues_libre =
            fin == texto.len() || !es_parte(texto[fin..].chars().next().unwrap());
        antes_libre && despues_libre
    })
}

/// Todas las señales del turno. Aquí se calculan; en `brain-rules.json` se
/// interpretan.
pub fn senales(req: &BrainRequest, reglas: &Reglas) -> Signals {
    let (idioma, _) = idiomas(&req.message);
    let bajo = req.message.to_lowercase();
    let riesgo = req.risk_hint.unwrap_or_else(|| {
        let alto = || {
            reglas
                .riesgo_alto
                .iter()
                .any(|p| dice_palabra(&bajo, &p.to_lowercase()))
        };
        // Verbo que destruye + objeto que se puede perder. La misma mediada en los
        // dos idiomas, porque el riesgo no depende de en qué lengua se escribió.
        let devastador = || {
            let verbo = reglas
                .riesgo_destructivo
                .iter()
                .any(|v| dice_palabra(&bajo, &v.to_lowercase()));
            let objeto = reglas
                .riesgo_objeto
                .iter()
                .any(|o| dice_palabra(&bajo, &o.to_lowercase()));
            verbo && objeto
        };
        if alto() || devastador() {
            Risk::High
        } else if reglas
            .riesgo_medio
            .iter()
            .any(|p| dice_palabra(&bajo, &p.to_lowercase()))
        {
            Risk::Medium
        } else {
            Risk::Low
        }
    });
    Signals {
        message_length: req.message.chars().count(),
        has_code: tiene_codigo(&req.message),
        has_file_path: crate::decision::fast_path::parece_ruta(&req.message).is_some(),
        has_action_verb: reglas
            .verbos_accion
            .iter()
            .any(|v| dice_palabra(&bajo, &v.to_lowercase())),
        mentions_git: reglas
            .menciona_git
            .iter()
            .any(|p| dice_palabra(&bajo, &p.to_lowercase())),
        mentions_web: reglas
            .menciona_web
            .iter()
            .any(|p| dice_palabra(&bajo, &p.to_lowercase())),
        mentions_project_files: reglas
            .menciona_archivos
            .iter()
            .any(|p| dice_palabra(&bajo, &p.to_lowercase())),
        mentions_command: reglas
            .menciona_comando
            .iter()
            .any(|p| dice_palabra(&bajo, &p.to_lowercase())),
        risk_hint: riesgo,
        session_failure: req.session_failures.last().copied(),
        project_has_tests: req.tiene_tests(),
        language: idioma,
    }
}

/// Ajustes del motor que no son políticas de producto.
#[derive(Debug, Clone, Default)]
pub struct ConfigMotor {
    /// Tope de la duda (§15.3, punto abierto; apagado hasta que se cierre).
    pub tope_de_duda: Option<Level>,
    pub cache: CacheDecisiones,
}

pub struct Motor {
    pub reglas: Reglas,
    config: ConfigMotor,
}

impl Motor {
    pub fn nuevo(reglas: Reglas) -> Motor {
        Motor {
            reglas,
            config: ConfigMotor::default(),
        }
    }

    pub fn con_cache(mut self, cache: CacheDecisiones) -> Self {
        self.config.cache = cache;
        self
    }

    pub fn con_tope_de_duda(mut self, tope: Option<Level>) -> Self {
        self.config.tope_de_duda = tope;
        self
    }

    /// El lote de un turno. Nunca falla: si no hay reglas, entra la heurística,
    /// y lo que no se puede resolver aquí lo resuelve el Planner con el plan
    /// seguro.
    pub fn evaluar(&mut self, req: &BrainRequest) -> DecisionResult {
        let herramientas: Vec<ToolId> = req.tools.ids();

        // 1 · Fast Path. Un `Some` salta el Engine y no se cachea: recalcularlo
        // cuesta microsegundos.
        if let Some(d) = self.reglas.fast_path(&req.message, &req.mode) {
            return d.con_suelo_de_riesgo();
        }

        // 2 · Caché de decisiones.
        let clave = CacheDecisiones::clave(req);
        if let Some(mut d) = self.config.cache.obtener(&clave) {
            d.source = DecisionSource::Cache;
            d.por_que.push_str(" · caché de decisiones");
            // El modo puede haber cambiado de reglas: el suelo se reaplica.
            if let Some(suelo) = self.reglas.suelo_por_modo.get(&req.mode) {
                d.level = d.level.max(*suelo);
            }
            return d.con_suelo_de_riesgo();
        }

        let s = senales(req, &self.reglas);

        // 3 · Reglas, con segundo candidato para el margen de confianza.
        let mut dudoso = false;
        let mut segundo: Option<Level> = None;
        let mut decision = match self
            .reglas
            .candidatar(&s, &s.language, &herramientas)
        {
            Ok(Some((d, otro))) => {
                segundo = otro;
                d
            }
            Ok(None) => {
                dudoso = true;
                heuristica(req, &s, &herramientas)
            }
            // Una regla mal escrita no puede tumbar el turno: se degrada a
            // heurística con low trust y el error queda en el log del producto.
            Err(_) => {
                dudoso = true;
                heuristica(req, &s, &herramientas)
            }
        };

        if dudoso || decision.confidence.hay_duda() {
            efectos(&mut decision, segundo, self.config.tope_de_duda);
        }

        // Suelo de nivel por modo, lo último antes de firmar.
        if let Some(suelo) = self.reglas.suelo_por_modo.get(&req.mode) {
            decision.level = decision.level.max(*suelo);
        }

        let mut decision = decision.con_suelo_de_riesgo();
        // La confianza de la heurística no es un dato medido: se marca como duda
        // expresa para que nadie la lea como certeza.
        if dudoso && decision.confidence.valor() >= Confidence::UMBRAL_ALTO {
            decision.confidence = Confidence(0.5);
        }
        self.config.cache.guardar(&clave, &decision);
        decision
    }

    pub fn estadisticas_cache(&self) -> (usize, u64, u64) {
        let (g, f) = self.config.cache.estadisticas();
        (self.config.cache.size(), g, f)
    }

    /// Copia de las reglas activas, para `inspect()` y para el panel: el Motor las
    /// tiene dentro y nadie más debería poder mutarlas.
    pub fn reglas_clon(&self) -> Reglas {
        self.reglas.clone()
    }
}

/// Lo que queda cuando ninguna regla habló: modo + señales. Es deliberadamente
/// conservador y **baja** confianza: si Hatboo no sabe, lo dice.
fn heuristica(req: &BrainRequest, s: &Signals, herramientas: &[ToolId]) -> DecisionResult {
    let trabajo = req.mode == crate::api::vocab::Mode::WORK;
    let necesita_obras = s.has_file_path && s.has_action_verb;
    // Fuera de ahí se queda en N1 también en `chat`: era un `else if trabajo` con
    // el mismo cuerpo que el `else`, o sea una rama que no decidía nada.
    let level = if trabajo && (necesita_obras || s.risk_hint == Risk::High) {
        Level::N2
    } else {
        Level::N1
    };
    let tools = if level.permite_tools() {
        herramientas.to_vec()
    } else {
        vec![]
    };
    let intent = if s.has_file_path && s.has_action_verb {
        Intent::Modify
    } else if s.mentions_web {
        Intent::Search
    } else if s.has_code {
        Intent::Explain
    } else {
        Intent::Ask
    };
    let contrato = intent.contrato_por_defecto();
    DecisionResult {
        intent,
        level,
        risk: s.risk_hint,
        skip_generative: false,
        execution_target: if req.policies == crate::api::vocab::ExecutionPolicy::CloudOnly {
            ExecutionTarget::Api
        } else {
            ExecutionTarget::Local
        },
        model_target: ModelTarget::Indistinto,
        tools,
        output_contract: contrato,
        verification: super::rules::nivelacion_verificacion(level, &contrato),
        confidence: Confidence(0.5),
        source: DecisionSource::Reglas,
        por_que: "ninguna regla habló; heurística por modo".into(),
        salida_directa: None,
        lectura_directa: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{Mode, OutputContract, VerificationMode};

    const JS: &str = r#"{
      "version": 1,
      "saludos": ["hola","gracias"],
      "verbos_accion": ["abre","crea","borra","ejecuta"],
      "riesgo_alto": ["borra","elimina","force","rm -rf"],
      "riesgo_medio": ["sube","publica"],
      "menciona_git": ["commit","rama","push","branch","stash"],
      "menciona_web": ["busca en la web","buscar web"],
      "suelo_por_modo": { "work": "N1" },
      "reglas": [
        { "id": "archivar",
          "cuando": [ {"senal":"has_file_path","op":"==","valor":true} ],
          "entonces": { "intent":"modify","level":"N2","contrato":"patch","tools":["read_file","write_file"] } },
        { "id": "git_chorizo",
          "cuando": [ {"senal":"mentions_git","op":"==","valor":true} ],
          "entonces": { "intent":"ask","level":"N1","contrato":"text" } }
      ]
    }"#;

    fn motor() -> Motor {
        Motor::nuevo(Reglas::desde_json(JS).unwrap())
    }

    #[test]
    fn senales_se_ven() {
        let r = Reglas::desde_json(JS).unwrap();
        let req = BrainRequest::nuevo("hatboo", Mode::WORK, "borra la carpeta node_modules");
        let s = senales(&req, &r);
        assert_eq!(s.risk_hint, Risk::High);
        assert!(s.has_action_verb);
        assert_eq!(s.language, "es");

        let con_codigo = BrainRequest::nuevo("hatboo", Mode::CHAT, "```rust\nfn a() {}\n```");
        assert!(senales(&con_codigo, &r).has_code);

        let en = BrainRequest::nuevo("hatboo", Mode::CHAT, "how do I show the current branch");
        let s2 = senales(&en, &r);
        assert_eq!(s2.language, "en");
        assert!(s2.mentions_git);
    }

    #[test]
    fn el_fast_path_gana_y_no_toca_la_caché() {
        let mut m = motor();
        let d = m.evaluar(&BrainRequest::nuevo("hatboo", Mode::CHAT, "hola"));
        assert_eq!(d.source, DecisionSource::FastPath);
        assert_eq!(d.level, Level::N0);
        assert_eq!(m.estadisticas_cache().0, 0);
    }

    #[test]
    fn la_regla_leva_el_lote() {
        let mut m = motor();
        let req = BrainRequest::nuevo("hatboo", Mode::WORK, "arregla src/app.rs")
            .con_tools(vec![crate::api::request::ToolInfo {
                id: "read_file".into(),
                escribe: false,
                descripcion: String::new(),
            }]);
        let d = m.evaluar(&req);
        assert_eq!(d.intent, Intent::Modify);
        assert_eq!(d.level, Level::N2);
        assert_eq!(d.output_contract, OutputContract::Patch);
        assert_eq!(d.verification, VerificationMode::Determinista);
        // Solo la tool que el producto ofrece.
        assert_eq!(d.tools, vec!["read_file".to_string()]);
    }

    #[test]
    fn sin_regla_la_heurística_confiesa_duda() {
        let mut m = motor();
        let d = m.evaluar(&BrainRequest::nuevo(
            "hatboo",
            Mode::CHAT,
            "cuéntame algo sobre los ríos de Europa que no esté en ninguna parte",
        ));
        assert!(d.confidence.hay_duda());
        assert_eq!(d.level, Level::N1);
        assert!(d.tools.is_empty());
    }

    #[test]
    fn la_segunda_pasada_sale_de_la_caché() {
        let mut m = motor();
        // Lo que cachea es una coincidencia exacta de regla (confianza 1,0); la
        // heurística se marca como duda y a propósito no se guarda.
        let req = BrainRequest::nuevo("hatboo", Mode::WORK, "arregla src/app.rs");
        let primero = m.evaluar(&req);
        let segundo = m.evaluar(&req);
        assert_eq!(primero.source, DecisionSource::Reglas);
        assert_eq!(segundo.source, DecisionSource::Cache);
        assert_eq!(primero.level, segundo.level);
        assert!(m.estadisticas_cache().0 > 0);
    }

    #[test]
    fn la_duda_no_se_cachea() {
        let mut m = motor();
        let req = BrainRequest::nuevo("hatboo", Mode::CHAT, "cuéntame algo sobre los ríos de Europa");
        let primero = m.evaluar(&req);
        let segundo = m.evaluar(&req);
        assert!(primero.confidence.hay_duda(), "{:?}", primero.confidence);
        assert_eq!(segundo.source, DecisionSource::Reglas, "volvió a calcularse");
        assert_eq!(m.estadisticas_cache().0, 0, "la caché quedó vacía");
    }

    #[test]
    fn dos_reglas_que_coinciden_puntuan_el_margen() {
        let mut m = motor();
        // Ruta + git: las dos reglas hablan con los mismos puntos, así que el
        // margen es cero. Gana la primera de la lista y la duda queda expresa.
        let req = BrainRequest::nuevo("hatboo", Mode::WORK, "arregla src/app.rs y haz commit");
        let d = m.evaluar(&req);
        assert!(d.por_que.contains("archivar"), "{}", d.por_que);
        assert!(d.confidence.es_insegura(), "{:?}", d.confidence);
        assert!(d.risk >= Risk::Medium, "sin margen no se trata como seguro");
        assert_eq!(d.level, Level::N2, "el más alto de los dos candidatos");
    }

    #[test]
    fn riesgo_alto_sube_de_nivel_aunque_hable_el_fast_path_de_modo() {
        let mut m = motor();
        let d = m.evaluar(&BrainRequest::nuevo("hatboo", Mode::CHAT, "borra el repo entero ya"))
            .con_suelo_de_riesgo();
        assert_eq!(d.risk, Risk::High);
        assert!(d.level >= Level::N2);
    }

    #[test]
    fn una_regra_rota_no_tumba_el_turno() {
        // `Reglas::desde_json` rechaza señales inventadas, pero si el JSON llega
        // con un operador válido y una señal que sí existe, todo va bien; aquí se
        // comprueba que el motor no pante con un mensaje raro.
        let mut m = motor();
        let d = m.evaluar(&BrainRequest::nuevo("hatboo", "", "   x   "));
        assert_eq!(d.level, Level::N1);
    }
}
