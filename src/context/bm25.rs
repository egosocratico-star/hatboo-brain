//! BM25 sobre las piezas de contexto (Plan v1.3 §5.1). La ocurrencia suelta es
//! un contador y se compra sobreconfianza con términos frecuentes; BM25 satura la
//! frecuencia (`k1`) y castiga las piezas largas (`b`), y el `idf` es lo que hace
//! que «mutec» pese más que «el».
//!
//! **Para qué sirve aquí, exactamente.** El presupuesto de contexto no siempre
//! cabe: lo que decide *qué se queda fuera* es el orden de armado. Por defecto
//! manda la `Prioridad` (conducta medida, con su golden); con
//! `Flags.bm25_contexto` —apagado por defecto— manda la relevancia del pedido, que
//! es lo que §5.1 pide. No hay corpus que indexar: las piezas de este turno son
//! el corpus, así que `N` y `avgdl` salen de ellas y todo es determinista.

use super::{Pieza, Prioridad};
use serde::{Deserialize, Serialize};

const K1_NEUTRO: f64 = 1.0;
const B_NEUTRO: f64 = 0.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub k1: f64,
    pub b: f64,
}

impl Default for Params {
    /// Los valores de la receta (1,2 / 0,75). El crate arranca con estos y el
    /// flag apagado; ni una cosa ni la otra están medidas en esta máquina.
    fn default() -> Params {
        Params { k1: 1.2, b: 0.75 }
    }
}

impl Params {
    /// `k1 = 1, b = 0` deja el puntúe como un conteo de coincidencias por pieza.
    /// Se expone porque `armar_por_relevancia` se puede usar con parámetros que
    /// alguien midió, y para eso hace falta saber cuáles son.
    pub fn neutros() -> Params {
        Params {
            k1: K1_NEUTRO,
            b: B_NEUTRO,
        }
    }
}

/// Tokens: palabras en minúscula hechas de alfanuméricos. Sin stemming ni stopword
/// list —eso sería otro sitio donde meter una decisión sin medir.
fn tokens(texto: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut actual = String::new();
    for c in texto.chars() {
        if c.is_alphanumeric() {
            actual.extend(c.to_lowercase());
        } else if !actual.is_empty() {
            v.push(std::mem::take(&mut actual));
        }
    }
    if !actual.is_empty() {
        v.push(actual);
    }
    v
}

/// Puntúe cada pieza contra la consulta. Devuelve el mismo largo que `piezas`, en
/// su orden: quien quiera el orden de relevancia usa `orden_por_relevancia`.
pub fn puntúa(piezas: &[Pieza], consulta: &str, p: Params) -> Vec<f64> {
    let docs: Vec<Vec<String>> = piezas.iter().map(|x| tokens(&x.texto)).collect();
    let n = docs.len().max(1) as f64;
    let media: f64 = docs.iter().map(|d| d.len() as f64).sum::<f64>() / n;
    let media = if media > 0.0 { media } else { 1.0 };
    let término_doc = |t: &str| docs.iter().filter(|d| d.iter().any(|w| w == t)).count();

    piezas
        .iter()
        .zip(&docs)
        .map(|(_, doc)| {
            let mut score = 0.0;
            for term in tokens(consulta) {
                let tf = doc.iter().filter(|w| **w == term).count() as f64;
                if tf == 0.0 {
                    continue;
                }
                let df = término_doc(&term) as f64;
                // Robertson-Sparck Jones con +1 para que un término que está en
                // todas las piezas no reste ni explote.
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                let dl = doc.len() as f64;
                score += idf * (tf * (p.k1 + 1.0)) / (tf + p.k1 * (1.0 - p.b + p.b * dl / media));
            }
            score
        })
        .collect()
}

/// Índices de las piezas, de más a menos relevante. Empate → el orden de entrada,
/// que es el que trae la config: sin eso el resultado depende del hash map o del
/// día, y el golden se rompe sin motivo.
pub fn orden_por_relevancia(piezas: &[Pieza], consulta: &str, p: Params) -> Vec<usize> {
    let mut scores: Vec<(usize, f64)> = puntúa(piezas, consulta, p)
        .into_iter()
        .enumerate()
        .collect();
    scores.sort_by(|(i, a), (j, b)| match b.partial_cmp(a) {
        Some(o) if o != std::cmp::Ordering::Equal => o,
        _ => i.cmp(j),
    });
    scores.into_iter().map(|(i, _)| i).collect()
}

/// Las piezas en el orden en que entran al armado por relevancia: las de
/// `Seguridad` siempre delante (no se negocian, §XII) y el resto por BM25, con
/// empate resuelto por el orden de entrada.
pub fn para_armar(piezas: &[Pieza], consulta: &str, p: Params) -> Vec<usize> {
    let relevancia = orden_por_relevancia(piezas, consulta, p);
    let (seguras, resto): (Vec<usize>, Vec<usize>) = relevancia
        .into_iter()
        .partition(|i| piezas[*i].prioridad == Prioridad::Seguridad);
    seguras.into_iter().chain(resto).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Prioridad::*;

    fn p(origen: &str, texto: &str) -> Pieza {
        Pieza::nueva(ArchivoNombrado, origen, texto)
    }

    #[test]
    fn un_termino_raro_pesa_mas_que_uno_que_esta_en_todo() {
        let piezas = vec![
            p("a", "el el el el proyecto"),
            p("b", "el mutex protege el contador"),
        ];
        let s = puntúa(&piezas, "mutex", Params::default());
        assert!(s[1] > s[0], "{s:?}: el idf no está haciendo su trabajo");
    }

    #[test]
    fn el_orden_es_estable_y_la_seguridad_va_delante() {
        let mut segura = Pieza::nueva(Prioridad::Seguridad, "local_only", "sin red");
        segura.texto.push_str(" nada que ver con mutex");
        let piezas = vec![
            p("resto-lejos", "un archivo de logística"),
            p("cerca", "mutex: el mutex protege"),
            segura,
        ];
        let orden = para_armar(&piezas, "mutex", Params::default());
        assert_eq!(orden[0], 2, "la de seguridad no se negocia: {orden:?}");
        assert_eq!(orden[1], 1, "la relevante va antes que la irrelevante: {orden:?}");
        // Empate (ninguna dice nada del pedido) → el orden de entrada.
        let vacio = vec![p("z", "uno"), p("y", "dos")];
        assert_eq!(para_armar(&vacio, "nada", Params::default()), vec![0, 1]);
    }

    #[test]
    fn el_texto_largo_no_gana_solo_por_longitud() {
        // Con `b` a 0.75 una pieza cinco veces más larga con la misma frecuencia
        // no puede quedar por delante.
        let corto = p("corto", "mutex mutex");
        let largo = p("largo", "mutex mutex y unas cuantas palabras mas aqui");
        let s = puntúa(&[corto, largo], "mutex", Params::default());
        assert!(s[0] > s[1], "{s:?}: la normalización por longitud no obra");
    }
}
