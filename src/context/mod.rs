//! El contexto es un recurso (§III.5). Aquí se ordena, se presupuesta y se deja
//! constancia de lo que se tuvo que fuera.

pub mod sources;

use crate::prompt::ContadorTokens;
use serde::{Deserialize, Serialize};

/// Orden de prioridad de §XII. Número bajo = más importante: si hay que cortar,
/// se corta desde el final de la lista.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Prioridad {
    /// Seguridad y permisos activos. Nunca se corta.
    Seguridad = 0,
    /// Objetivo + estado de la tarea.
    Objetivo = 1,
    /// El archivo o fragmento que el usuario nombró.
    ArchivoNombrado = 2,
    /// Tests o errores recientes del proyecto.
    Pruebas = 3,
    /// Historial de **esta** sesión.
    Historial = 4,
    /// Todo lo demás.
    Resto = 5,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pieza {
    pub prioridad: Prioridad,
    /// `archivo:src/main.rs`, `historial:3`, `estado:tarea`… Entra en el log de
    /// `dropped_context` y en el inspector.
    pub origen: String,
    pub texto: String,
}

impl Pieza {
    pub fn nueva(prioridad: Prioridad, origen: impl Into<String>, texto: impl Into<String>) -> Pieza {
        Pieza {
            prioridad,
            origen: origen.into(),
            texto: texto.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Armado {
    pub incluidas: Vec<Pieza>,
    /// Nombres de lo que se quedó fuera, en el orden en que se descartó.
    pub rechazadas: Vec<String>,
    pub tokens: u32,
    /// El presupuesto que se pidió cumplir.
    pub presupuesto: u32,
}

impl Armado {
    pub fn vacio(&self) -> bool {
        self.incluidas.is_empty()
    }

    /// El texto dinámico ya montado, con cada pieza en su bloque `<datos>` salvo
    /// las de seguridad y objetivo, que van como instrucción.
    pub fn texto(&self) -> String {
        let mut bloques = Vec::with_capacity(self.incluidas.len());
        for p in &self.incluidas {
            if matches!(p.prioridad, Prioridad::Seguridad | Prioridad::Objetivo) {
                bloques.push(p.texto.clone());
            } else {
                bloques.push(crate::prompt::escape::bloque_datos(&p.origen, &p.texto));
            }
        }
        bloques.join("\n\n")
    }
}

/// Ordena por prioridad (estable dentro de la misma prioridad) y mete piezas
/// hasta agotar el presupuesto. No parte una pieza por la mitad: o entra o se
/// registra como rechazada, porque a medias no se puede leer un archivo.
pub fn armar(mut piezas: Vec<Pieza>, presupuesto: u32, contador: &dyn ContadorTokens) -> Armado {
    piezas.sort_by_key(|p| p.prioridad);
    let mut armado = Armado {
        presupuesto,
        ..Default::default()
    };
    for p in piezas {
        let cuesta = contador.cuenta(&p.texto);
        if armado.tokens + cuesta <= presupuesto {
            armado.tokens += cuesta;
            armado.incluidas.push(p);
        } else if p.prioridad == Prioridad::Seguridad {
            // La seguridad no se deja fuera ni aunque desborde: se corta lo que
            // haya por debajo y esto entra. Se avisa en el reason.
            armado.rechazadas.push(format!("desbordado:{}", p.origen));
            armado.tokens += cuesta;
            armado.incluidas.push(p);
        } else {
            armado.rechazadas.push(p.origen.clone());
        }
    }
    armado
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Estimador;

    fn pieza(p: Prioridad, origen: &str, n: usize) -> Pieza {
        Pieza::nueva(p, origen, "x".repeat(n))
    }

    #[test]
    fn entra_lo_importante_y_se_registra_lo_que_fuera() {
        let piezas = vec![
            pieza(Prioridad::Resto, "resto", 400),
            pieza(Prioridad::Objetivo, "objetivo", 70),
            pieza(Prioridad::ArchivoNombrado, "archivo:src/main.rs", 210),
            pieza(Prioridad::Historial, "historial:1", 140),
        ];
        let a = armar(piezas, 100, &Estimador);
        // Con 100 tokens entran objetivo (20) y el archivo nombrado (60); el
        // historial (40) ya no cabe y el resto (115) nunca cabría.
        assert_eq!(a.tokens, 80);
        assert_eq!(a.incluidas.len(), 2);
        assert_eq!(a.rechazadas.len(), 2, "{:?}", a.rechazadas);
        assert_eq!(a.rechazadas[0], "historial:1");
        assert_eq!(a.rechazadas[1], "resto");
    }

    #[test]
    fn el_presupuesto_nunca_se_excede_salvo_por_seguridad() {
        let a = armar(
            vec![pieza(Prioridad::Objetivo, "o", 3500), pieza(Prioridad::Resto, "r", 3500)],
            100,
            &Estimador,
        );
        assert!(a.tokens <= 100);
        let b = armar(
            vec![
                pieza(Prioridad::Objetivo, "o", 35),
                pieza(Prioridad::Seguridad, "permisos", 7000),
            ],
            20,
            &Estimador,
        );
        assert!(b.tokens > 20, "la seguridad entra aunque desborde");
        assert!(b.rechazadas.iter().any(|r| r.starts_with("desbordado")));
    }

    #[test]
    fn el_texto_envuelve_lo_que_es_material() {
        let a = armar(
            vec![
                pieza(Prioridad::Objetivo, "objetivo", 7),
                pieza(Prioridad::ArchivoNombrado, "archivo:a.rs", 14),
            ],
            1000,
            &Estimador,
        );
        let t = a.texto();
        assert!(t.starts_with("xxxxxxx"), "{t}");
        assert!(t.contains("<datos origen=\"archivo:a.rs\">"));
        // El objetivo del usuario no va en un bloque de datos: es instrucción.
        assert!(!t.contains("<datos origen=\"objetivo\""));
    }

    #[test]
    fn sin_presupuesto_no_entra_nada() {
        let a = armar(vec![pieza(Prioridad::Objetivo, "o", 4)], 0, &Estimador);
        assert!(a.vacio());
        assert_eq!(a.rechazadas, vec!["o".to_string()]);
    }
}
