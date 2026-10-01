//! Las reglas viven en JSON, no en el código (§VII del Canon: «el código no
//! incrusta políticas de producto»). Aquí solo está el lenguaje para evaluarlas.

use super::DecisionResult;
use crate::api::vocab::{
    Confidence, DecisionSource, ExecutionTarget, Intent, Level, ModelTarget, OutputContract, Risk,
    Signals, ToolId, VerificationMode,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operador {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Señal booleana o cadena que contiene.
    Contiene,
    /// La señal está en una lista de valores.
    UnoDe,
}

impl Operador {
    pub fn desde(s: &str) -> Option<Self> {
        Some(match s {
            "==" | "eq" => Operador::Eq,
            "!=" => Operador::Ne,
            "<" => Operador::Lt,
            "<=" => Operador::Le,
            ">" => Operador::Gt,
            ">=" => Operador::Ge,
            "contiene" | "contains" => Operador::Contiene,
            "uno_de" | "one_of" => Operador::UnoDe,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condicion {
    /// Nombre exacto de una señal de [`Signals`]. Un nombre que no existe es un
    /// error de config, no un `false`.
    pub senal: String,
    pub op: String,
    pub valor: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Veredicto {
    #[serde(default)]
    pub intent: Option<Intent>,
    #[serde(default)]
    pub level: Option<Level>,
    #[serde(default)]
    pub risk: Option<Risk>,
    #[serde(default)]
    pub contrato: Option<OutputContract>,
    #[serde(default)]
    pub skip_generative: bool,
    /// Tools candidatas: el Planner interseca con el approval y con lo que el
    /// producto ofrece.
    #[serde(default)]
    pub tools: Vec<ToolId>,
    #[serde(default)]
    pub execution_target: Option<ExecutionTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regla {
    pub id: String,
    #[serde(default)]
    pub idiomas: Vec<String>,
    #[serde(default = "default_conjuncion")]
    pub y: String,
    pub cuando: Vec<Condicion>,
    pub entonces: Veredicto,
    /// Empate por especificidad; `prio` más alta gana antes de contar condiciones.
    #[serde(default)]
    pub prio: i16,
}

fn default_conjuncion() -> String {
    "all".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reglas {
    pub version: u32,
    #[serde(default)]
    pub reglas: Vec<Regla>,
    /// Los saludos que el Fast Path resuelve sin modelo.
    #[serde(default)]
    pub saludos: Vec<String>,
    /// Verbos de acción para la señal `has_action_verb`.
    #[serde(default)]
    pub verbos_accion: Vec<String>,
    /// Subconjunto que **solo mira**: los únicos que pueden disparar la lectura
    /// directa del Fast Path. «corrige src/main.rs» tiene un verbo de acción y una
    /// ruta, y no por eso es una orden de leer.
    #[serde(default)]
    pub verbos_lectura: Vec<String>,
    /// Palabras que suben el riesgo (borrar, forzar, push, claves…).
    #[serde(default)]
    pub riesgo_alto: Vec<String>,
    #[serde(default)]
    pub riesgo_medio: Vec<String>,
    /// Verbos que destruyen. Con uno de `riesgo_objeto` debajo, el riesgo es alto
    /// aunque la frase exacta no esté en `riesgo_alto`: así «borra la carpeta de
    /// logs» y «delete the logs folder» pesan lo mismo.
    #[serde(default)]
    pub riesgo_destructivo: Vec<String>,
    #[serde(default)]
    pub riesgo_objeto: Vec<String>,
    /// Términos que delatan git / web.
    #[serde(default)]
    pub menciona_git: Vec<String>,
    #[serde(default)]
    pub menciona_web: Vec<String>,
    /// Suelo de nivel por modo. Un `work` no puede caer a N0 con una tool de más.
    #[serde(default)]
    pub suelo_por_modo: std::collections::BTreeMap<String, Level>,
    #[serde(default)]
    pub umbral_duda: Option<f32>,
}

impl Default for Reglas {
    fn default() -> Self {
        Reglas {
            version: 1,
            reglas: Vec::new(),
            saludos: Vec::new(),
            verbos_accion: Vec::new(),
            verbos_lectura: Vec::new(),
            riesgo_alto: Vec::new(),
            riesgo_medio: Vec::new(),
            riesgo_destructivo: Vec::new(),
            riesgo_objeto: Vec::new(),
            menciona_git: Vec::new(),
            menciona_web: Vec::new(),
            suelo_por_modo: Default::default(),
            umbral_duda: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReglaError {
    /// El texto del error de JSON se guarda como `String`: `serde_json::Error`
    /// no es `Clone` y este enum tiene que poder compararse en los tests.
    #[error("el json de las reglas no parsea: {0}")]
    Json(String),
    /// Un nombre de señal que no existe: mejor gritar que evaluar a `false`.
    #[error("la regla pide una señal que no existe: {0}")]
    SenalDesconocida(String),
    #[error("la regla usa un operador que no existe: {0}")]
    OperadorDesconocido(String),
}

impl Reglas {
    pub fn desde_json(texto: &str) -> Result<Reglas, ReglaError> {
        let r: Reglas = serde_json::from_str(texto).map_err(|e| ReglaError::Json(e.to_string()))?;
        for regla in &r.reglas {
            for c in &regla.cuando {
                if crate::api::vocab::Signals::default().valor(&c.senal).is_none() {
                    return Err(ReglaError::SenalDesconocida(c.senal.clone()));
                }
                if Operador::desde(&c.op).is_none() {
                    return Err(ReglaError::OperadorDesconocido(c.op.clone()));
                }
            }
        }
        Ok(r)
    }

    fn eval(senal: &serde_json::Value, op: &Operador, valor: &serde_json::Value) -> bool {
        use serde_json::Value as V;
        let orden = |a: &V, b: &V| -> Option<std::cmp::Ordering> {
            match (a, b) {
                (V::Number(x), V::Number(y)) => x.as_f64().zip(y.as_f64()).map(|(x, y)| {
                    x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                }),
                (V::String(x), V::String(y)) => x.partial_cmp(y),
                _ => None,
            }
        };
        match op {
            Operador::Eq => senal == valor,
            Operador::Ne => senal != valor,
            Operador::Lt => orden(senal, valor) == Some(std::cmp::Ordering::Less),
            Operador::Le => {
                matches!(
                    orden(senal, valor),
                    Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)
                )
            }
            Operador::Gt => orden(senal, valor) == Some(std::cmp::Ordering::Greater),
            Operador::Ge => {
                matches!(
                    orden(senal, valor),
                    Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal)
                )
            }
            Operador::Contiene => match (senal, valor) {
                (V::String(a), V::String(b)) => a.contains(b.as_str()),
                (V::Array(a), b) => a.contains(b),
                _ => false,
            },
            Operador::UnoDe => match valor {
                V::Array(lista) => lista.contains(senal),
                _ => false,
            },
        }
    }

    /// Se cumple la regla para estas señales e idioma. `Err` si la regla pide una
    /// señal que no existe.
    pub fn cumple(&self, regla: &Regla, s: &Signals, idioma: &str) -> Result<bool, ReglaError> {
        if !regla.idiomas.is_empty() && !regla.idiomas.iter().any(|i| i == idioma) {
            return Ok(false);
        }
        let mut resultados = Vec::with_capacity(regla.cuando.len());
        for c in &regla.cuando {
            let senal = s
                .valor(&c.senal)
                .ok_or_else(|| ReglaError::SenalDesconocida(c.senal.clone()))?;
            let op = Operador::desde(&c.op).ok_or_else(|| ReglaError::OperadorDesconocido(c.op.clone()))?;
            resultados.push(Self::eval(&senal, &op, &c.valor));
        }
        Ok(if regla.y == "any" {
            resultados.iter().any(|x| *x)
        } else {
            resultados.iter().all(|x| *x)
        })
    }

    /// Candidatura. Puntúa por especificidad (nº de condiciones que se cumplen) y
    /// por `prio`; la confianza es el margen entre la primera y la segunda, como
    /// fija §1 para el origen «reglas». El segundo elemento del tuple es el nivel
    /// del segundo candidato: §1 resuelve la duda tomando «el más alto de los dos»,
    /// y para eso hay que conservar el otro.
    pub fn candidatar(
        &self,
        s: &Signals,
        idioma: &str,
        herramientas: &[ToolId],
    ) -> Result<Option<(DecisionResult, Option<Level>)>, ReglaError> {
        let mut empates: Vec<(&Regla, i32)> = Vec::new();
        for r in &self.reglas {
            if self.cumple(r, s, idioma)? {
                empates.push((r, (r.prio as i32) * 100 + r.cuando.len() as i32));
            }
        }
        if empates.is_empty() {
            return Ok(None);
        }
        empates.sort_by_key(|(_, p)| -*p);
        let (ganadora, puntos) = empates[0];
        let segunda = if ganadora.cuando.is_empty() {
            None
        } else {
            empates.get(1).copied()
        };
        let confianza = match segunda {
            Some((_, p2)) => Confidence::margen(puntos as f32, p2 as f32),
            None => Confidence::determinista(),
        };
        let lote = aplicar(ganadora, confianza, DecisionSource::Reglas, herramientas);
        let nivel_de_segunda = segunda.map(|(r, _)| r.entonces.level.unwrap_or(Level::N1));
        Ok(Some((lote, nivel_de_segunda)))
    }
}

/// Convierte un veredicto en lote completo, con los mínimos que el nivel exige.
pub fn aplicar(
    r: &Regla,
    confianza: Confidence,
    source: DecisionSource,
    herramientas: &[ToolId],
) -> DecisionResult {
    let v = &r.entonces;
    let level = v.level.unwrap_or(Level::N1);
    let intent = v.intent.unwrap_or(Intent::Ask);
    let contrato = v.contrato.unwrap_or_else(|| intent.contrato_por_defecto());
    let risk = v.risk.unwrap_or(Risk::Low);
    // Una tool que el producto no ofrece no puede ni candidatearse.
    let tools: Vec<ToolId> = v
        .tools
        .iter()
        .filter(|t| herramientas.iter().any(|h| h == *t))
        .cloned()
        .collect();
    let tools = if level.permite_tools() { tools } else { vec![] };
    DecisionResult {
        intent,
        level,
        risk,
        skip_generative: v.skip_generative,
        execution_target: v.execution_target.unwrap_or(ExecutionTarget::Local),
        model_target: ModelTarget::Indistinto,
        tools,
        output_contract: contrato,
        verification: nivelacion_verificacion(level, &contrato),
        confidence: confianza,
        source,
        por_que: format!("regla «{}»", r.id),
        salida_directa: None,
        lectura_directa: None,
    }
}

/// Verificación: nunca por debajo del mínimo del nivel, y un contrato con
/// estructura al menos pide formato.
pub fn nivelacion_verificacion(level: Level, contrato: &OutputContract) -> VerificationMode {
    let minimo = level.verificacion_minima();
    let del_contrato = match contrato {
        OutputContract::Json | OutputContract::ToolCall | OutputContract::Patch => {
            VerificationMode::Formato
        }
        _ => VerificationMode::Ninguna,
    };
    minimo.max(del_contrato)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JS: &str = r#"{
      "version": 1,
      "saludos": ["hola", "hey"],
      "reglas": [
        { "id": "archivo-nombrado",
          "cuando": [ {"senal": "has_file_path", "op": "==", "valor": true},
                      {"senal": "has_action_verb", "op": "==", "valor": true} ],
          "entonces": { "intent": "modify", "level": "N2", "contrato": "patch",
                        "tools": ["read_file","write_file","run_command"] } },
        { "id": "pregunta-corta",
          "cuando": [ {"senal": "message_length", "op": "<=", "valor": 40} ],
          "entonces": { "intent": "ask", "level": "N1" } },
        { "id": "riesgosa",
          "prio": 5,
          "cuando": [ {"senal": "risk_hint", "op": "==", "valor": "high"} ],
          "entonces": { "intent": "execute", "level": "N2", "risk": "high" } }
      ]
    }"#;

    fn senales(con_path: bool, largo: usize) -> Signals {
        Signals {
            message_length: largo,
            has_file_path: con_path,
            has_action_verb: con_path,
            ..Default::default()
        }
    }

    #[test]
    fn gana_la_mas_especifica_y_la_confianza_es_el_margen() {
        let r = Reglas::desde_json(JS).unwrap();
        let hs = vec!["read_file".to_string(), "write_file".to_string()];
        let (d, segunda) = r
            .candidatar(&senales(true, 90), "es", &hs)
            .unwrap()
            .expect("alguna regla");
        assert_eq!(d.intent, Intent::Modify);
        assert_eq!(d.level, Level::N2);
        // Solo las tools que el producto ofrece.
        assert_eq!(d.tools, vec!["read_file".to_string(), "write_file".to_string()]);
        assert!(d.confidence.valor() > 0.5);
        assert_eq!(segunda, None, "una sola regla habló: no hay segundo candidato");
    }

    #[test]
    fn el_segundo_candidato_se_conserva_para_la_duda() {
        let r = Reglas::desde_json(JS).unwrap();
        // Riesgo alto (prio 5) y pregunta corta coinciden: gana la riesgosa por
        // margen amplio, pero el nivel de la otra sigue disponible para §1.
        let s = Signals {
            message_length: 30,
            risk_hint: Risk::High,
            ..Default::default()
        };
        let (d, segunda) = r.candidatar(&s, "es", &[]).unwrap().unwrap();
        assert_eq!(d.intent, Intent::Execute);
        assert_eq!(segunda, Some(Level::N1));
        assert!(d.confidence.valor() > 0.9, "{:?}", d.confidence);
    }

    #[test]
    fn senal_desconocida_es_un_error_no_un_false() {
        let mal = r#"{"version":1,"reglas":[{"id":"x","cuando":[{"senal":"no_existe","op":"==","valor":1}],"entonces":{}}]}"#;
        assert_eq!(
            Reglas::desde_json(mal),
            Err(ReglaError::SenalDesconocida("no_existe".into()))
        );
    }

    #[test]
    fn operador_roto_tambien_grita() {
        let mal = r#"{"version":1,"reglas":[{"id":"x","cuando":[{"senal":"has_code","op":"cerca","valor":1}],"entonces":{}}]}"#;
        assert_eq!(
            Reglas::desde_json(mal),
            Err(ReglaError::OperadorDesconocido("cerca".into()))
        );
    }

    #[test]
    fn un_n1_no_leva_tools_aunque_la_regla_pida() {
        let r = Reglas::desde_json(JS).unwrap();
        let regla = r.reglas.iter().find(|x| x.id == "pregunta-corta").unwrap();
        let d = aplicar(regla, Confidence::determinista(), DecisionSource::Reglas, &["read_file".to_string()]);
        assert_eq!(d.level, Level::N1);
        assert!(d.tools.is_empty());
        assert_eq!(d.verification, VerificationMode::Formato);
    }

    #[test]
    fn el_riesgo_alto_no_se_queda_bajo() {
        let r = Reglas::desde_json(JS).unwrap();
        let hs: Vec<ToolId> = vec![];
        let s = Signals {
            risk_hint: Risk::High,
            message_length: 200,
            ..Default::default()
        };
        let (d, _) = r.candidatar(&s, "es", &hs).unwrap().unwrap();
        assert_eq!(d.risk, Risk::High);
        let subido = d.con_suelo_de_riesgo();
        assert!(subido.level >= Level::N2);
    }
}
