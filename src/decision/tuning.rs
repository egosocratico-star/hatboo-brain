//! Los pesos de la decisión ponderada por costo (Plan v1.3 §3 y §15.3). La idea:
//! cuando hay más de una candidatura no se elige la que más puntos saca, sino
//! `argmax p(nivel) × peso(nivel, riesgo)`, donde `p` es la fracción de puntos.
//! El peso dice cuánto cuesta quedarse corto comparado con subir un nivel.
//!
//! **Los pesos de este fichero no están medidos.** `Tuning::neutral()` pone todos
//! a 1,0, y con todo a 1,0 el argmax sale igual que antes: la pieza existe, el
//! comportamiento no se mueve hasta que alguien ponga números arriba del papel,
//! que es lo que pide §15.3 («los pesos iniciales se fijan en Fase 3»).

use crate::api::vocab::{Level, Risk};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Nivel y riesgo en la forma que usa el JSON: `"N0".."N3"` y `"low|medium|high"`.
fn clave_riesgo(r: Risk) -> &'static str {
    match r {
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "high",
    }
}

fn clave_nivel(n: Level) -> &'static str {
    match n {
        Level::N0 => "N0",
        Level::N1 => "N1",
        Level::N2 => "N2",
        Level::N3 => "N3",
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Tuning {
    /// `pesos[nivel][riesgo]`. Lo que falte vale 1,0: un fichero a medias no
    /// puede inventar un coste que nadie midió.
    #[serde(default)]
    pub pesos: BTreeMap<String, BTreeMap<String, f64>>,
}

impl Tuning {
    /// Todos a 1,0. Es el default del crate y el que deja la conducta de siempre.
    pub fn neutral() -> Tuning {
        Tuning::default()
    }

    /// `true` si ningún peso se sale de 1,0 — lo que lee el producto para poder
    /// decir «esto todavía decide sin ponderar».
    pub fn es_neutral(&self) -> bool {
        self.pesos
            .values()
            .flat_map(|v| v.values())
            .all(|p| (p - 1.0).abs() < f64::EPSILON)
    }

    pub fn desde_json(texto: &str) -> Result<Tuning, String> {
        serde_json::from_str(texto).map_err(|e| format!("tuning.json inválido: {e}"))
    }

    pub fn peso(&self, nivel: Level, riesgo: Risk) -> f64 {
        self.pesos
            .get(clave_nivel(nivel))
            .and_then(|m| m.get(clave_riesgo(riesgo)))
            .copied()
            .unwrap_or(1.0)
    }

    /// Qué gana entre las candidaturas ya puntuadas. `candidatas` es
    /// `(índice, nivel, riesgo, puntos)` en el orden de `empates`. Devuelve el
    /// índice del ganador y la probabilidad que le tocó a cada nivel, para que el
    /// `reason` pueda decir por qué se decidió lo que se decidió.
    ///
    /// Con pesos a 1,0 el resultado es el de siempre (mayor puntuación); el orden
    /// de desempate es el de la lista, que es estable.
    pub fn elegir(&self, candidatas: &[(usize, Level, Risk, i32)]) -> Option<(usize, f64)> {
        if candidatas.is_empty() {
            return None;
        }
        let total: i32 = candidatas.iter().map(|(_, _, _, p)| *p).sum();
        if total <= 0 {
            return candidatas.first().map(|(i, _, _, p)| (*i, *p as f64));
        }
        // Empate → la primera de la lista, como hasta ahora. `max_by` de Rust
        // devolvería la última, y eso movería decisiones sin que nadie lo pida.
        let mut mejor: Option<(usize, f64, f64)> = None;
        for (i, n, r, p) in candidatas {
            let prob = *p as f64 / total as f64;
            let score = prob * self.peso(*n, *r);
            match mejor {
                Some((_, _, best)) if score <= best => {}
                _ => mejor = Some((*i, prob, score)),
            }
        }
        mejor.map(|(i, prob, _)| (i, prob))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn con(peso_n3: f64) -> Tuning {
        let mut niveles = BTreeMap::new();
        let mut r = BTreeMap::new();
        r.insert("low".to_string(), peso_n3);
        niveles.insert("N3".to_string(), r);
        Tuning { pesos: niveles }
    }

    #[test]
    fn con_pesos_neutrales_gana_la_puntuacion_de_siempre() {
        let c = vec![(0usize, Level::N2, Risk::Low, 400), (1, Level::N3, Risk::Low, 300)];
        assert_eq!(Tuning::neutral().elegir(&c).map(|(i, _)| i), Some(0));
        assert!(Tuning::neutral().es_neutral());
    }

    #[test]
    fn un_peso_alto_en_el_nivel_caro_gira_la_decision() {
        // Mismos puntos: la de N3 pierde por puntuación y gana si quedarse corto
        // cuesta mucho más que subir.
        let c = vec![(0usize, Level::N2, Risk::Low, 400), (1, Level::N3, Risk::Low, 300)];
        let s = con(3.0);
        assert_eq!(s.elegir(&c).map(|(i, _)| i), Some(1));
        assert!(!s.es_neutral(), "un fichero con 3,0 no es neutro");
    }

    #[test]
    fn lo_que_no_esta_en_el_fichero_vale_uno() {
        let s = con(3.0);
        assert_eq!(s.peso(Level::N2, Risk::Low), 1.0);
        assert_eq!(s.peso(Level::N3, Risk::Medium), 1.0, "sin dato no se extrapola");
        assert_eq!(s.peso(Level::N3, Risk::Low), 3.0);
    }

    #[test]
    fn el_fichero_invalido_se_rechaza_sin_adivinar() {
        let e = Tuning::desde_json("{\"pesos\": {\"N3\": 3}}").unwrap_err();
        assert!(e.contains("tuning.json inválido"), "{e}");
    }
}
