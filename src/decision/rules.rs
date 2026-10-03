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
    /// Los pesos de la decisión ponderada por costo (§3). No salen de este JSON:
    /// viven en `tuning.json`, y aquí llegan por `con_tuning`. Sin fichero están a
    /// 1,0 y la elección sale idéntica a la de siempre.
    #[serde(skip, default = "crate::decision::tuning::Tuning::neutral")]
    pub tuning: crate::decision::tuning::Tuning,
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
    /// Nombres de cosas que están **dentro del proyecto** («archivo», «files»,
    /// «carpeta»). Es lo que separa «¿en qué archivo está el total?» de una charla:
    /// sin esta señal ninguna regla podía ofrecer `list_dir` ni `search_files`.
    /// A propósito no incluye «función» ni «code»: «write a rust function» pide
    /// código en la respuesta, no buscar en el disco.
    #[serde(default)]
    pub menciona_archivos: Vec<String>,
    /// Lo que se dice cuando hay que **correr** algo. Igual que arriba: sin señal
    /// no había regla que llegara a `run_command`, y un N1 no puede llevar tools.
    #[serde(default)]
    pub menciona_comando: Vec<String>,
    /// Suelo de nivel por modo. Un `work` no puede caer a N0 con una tool de más.
    #[serde(default)]
    pub suelo_por_modo: std::collections::BTreeMap<String, Level>,
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
            menciona_archivos: Vec::new(),
            menciona_comando: Vec::new(),
            suelo_por_modo: Default::default(),
            tuning: crate::decision::tuning::Tuning::neutral(),
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
    /// Una regla sin condiciones se cumple siempre (cuantificación vacía) y ganaba
    /// por `prio`, así que dejarla pasar es entregar el routing a una línea de JSON.
    #[error("la regla «{0}» no tiene ninguna condición: ganaría siempre con confianza de certeza")]
    ReglaSinCondiciones(String),
}

impl Reglas {
    /// Le pone los pesos con los que se elige entre candidaturas (§3). Se llama
    /// desde el cargador cuando existe `config/tuning.json`; sin llamada, manda la
    /// puntuación a secas.
    pub fn con_tuning(mut self, t: crate::decision::tuning::Tuning) -> Reglas {
        self.tuning = t;
        self
    }

    pub fn desde_json(texto: &str) -> Result<Reglas, ReglaError> {
        let r: Reglas = serde_json::from_str(texto).map_err(|e| ReglaError::Json(e.to_string()))?;
        for regla in &r.reglas {
            if regla.cuando.is_empty() {
                return Err(ReglaError::ReglaSinCondiciones(regla.id.clone()));
            }
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

    /// Las reglas versionadas con el crate (`config/brain-rules.json`), para un
    /// producto que no tiene un directorio de config donde leer: el instalador de
    /// Hatboo no lleva `brain-rules.json` detrás. `Reglas::default()` está
    /// **vacío** a propósito, así que un fallo aquí se propaga en vez de sustituir
    /// unas reglas por ninguna.
    pub fn empotradas() -> Result<Reglas, ReglaError> {
        const JSON: &str = include_str!("../../config/brain-rules.json");
        Self::desde_json(JSON)
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
        // La confianza se mide siempre sobre la puntuación cruda (§1: el margen
        // entre la primera y la segunda), no sobre el costo ponderado.
        let puntos = empates[0].1;
        // §3 del Plan v1.3: entre candidaturas manda `argmax p(nivel) ×
        // peso(nivel, riesgo)`. Con los pesos neutros (todos a 1,0) el ganador es
        // el de la puntuación, o sea: la conducta medida hasta hoy.
        let candidatas: Vec<(usize, crate::api::vocab::Level, crate::api::vocab::Risk, i32)> = empates
            .iter()
            .enumerate()
            .map(|(i, (r, p))| {
                (
                    i,
                    r.entonces.level.unwrap_or(crate::api::vocab::Level::N1),
                    r.entonces.risk.unwrap_or(crate::api::vocab::Risk::Low),
                    *p,
                )
            })
            .collect();
        let elegida = self.tuning.elegir(&candidatas).map(|(i, _)| i).unwrap_or(0);
        let (ganadora, _) = empates[elegida];
        // La segunda candidatura se mira siempre. Antes se anulaba si la ganadora
        // no tenía condiciones, y eso valía un 1,0 de confianza: el Engine se
        // saltaba los efectos de la duda y la caché guardaba el resultado como
        // cierto. Una regla que no pide nada no sabe más que las demás.
        let segunda = empates.get(1).copied();
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

    #[test]
    fn las_reglas_embebidas_candidatean_solo() {
        let r = Reglas::empotradas().expect("brain-rules.json embebido");
        assert!(!r.reglas.is_empty(), "sin reglas el Brain no candidatea nada");
        assert!(!r.verbos_accion.is_empty() && !r.verbos_lectura.is_empty());
        assert!(!r.saludos.is_empty(), "el Fast Path de saludos se queda vacío");
        // `desde_json` ya validó señales y operadores; aquí se comprueba que lo
        // embebido es el mismo catálogo que el archivo, no un muñeco.
        let archivo = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/brain-rules.json"
        ))
        .unwrap();
        assert_eq!(r, Reglas::desde_json(&archivo).unwrap());
    }

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
    fn una_regla_sin_condiciones_es_un_error_de_config() {
        // `cuando: []` se cumple por cuantificación vacía: con `prio` ganaría
        // siempre, y al no haber segunda candidatura la confianza salía en 1,0,
        // con lo que el Engine se saltaba los efectos de la duda y la caché lo
        // guardaba como cierto.
        let mal = r#"{"version":1,"reglas":[{"id":"todas","cuando":[],"entonces":{"level":"N0"}}]}"#;
        assert_eq!(
            Reglas::desde_json(mal),
            Err(ReglaError::ReglaSinCondiciones("todas".into()))
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

    /// §3 del Plan v1.3: la elección entre candidaturas es `argmax p × peso`. Con
    /// pesos neutros tiene que salir la de siempre; con el nivel barato pesado, la
    /// otra. Sin esto el `tuning.json` sería un adorno que nadie lee.
    #[test]
    fn los_pesos_eligen_entre_dos_candidatas() {
        let r = Reglas::desde_json(
            r#"{"version":1,"reglas":[
                {"id":"cara","prio":40,"cuando":[{"senal":"has_code","op":"==","valor":true}],
                 "entonces":{"intent":"modify","level":"N3","risk":"low"}},
                {"id":"barata","prio":20,"cuando":[{"senal":"has_code","op":"==","valor":true}],
                 "entonces":{"intent":"modify","level":"N2","risk":"low"}}
            ]}"#,
        )
        .unwrap();
        let s = Signals {
            has_code: true,
            message_length: 40,
            ..Default::default()
        };
        let hs: Vec<ToolId> = vec![];

        let (d, _) = r.candidatar(&s, "es", &hs).unwrap().expect("candidatura");
        assert_eq!(d.level, Level::N3, "sin pesos manda la puntuación de siempre");
        assert!(d.por_que.contains("cara"), "por_que: {}", d.por_que);

        let mut caro_quedarse_corto = std::collections::BTreeMap::new();
        let mut por_riesgo = std::collections::BTreeMap::new();
        por_riesgo.insert("low".to_string(), 3.0);
        caro_quedarse_corto.insert("N2".to_string(), por_riesgo);
        let con_pesos = r.con_tuning(crate::decision::tuning::Tuning {
            pesos: caro_quedarse_corto,
        });
        let (d2, _) = con_pesos
            .candidatar(&s, "es", &hs)
            .unwrap()
            .expect("candidatura");
        assert_eq!(
            d2.level,
            Level::N2,
            "con el nivel barato ponderado al triple tiene que ganar esa regla"
        );
        assert!(d2.por_que.contains("barata"), "por_que: {}", d2.por_que);
    }
}
