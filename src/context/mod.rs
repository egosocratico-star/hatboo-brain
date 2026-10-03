//! El contexto es un recurso (§III.5). Aquí se ordena, se presupuesta y se deja
//! constancia de lo que se tuvo que fuera.

pub mod bm25;
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
    /// §XII del Canon v1.3: sensibilidad **por ítem**. `true` = esto no sale del
    /// equipo: si el Plan acaba yendo a una API, la pieza se queda fuera y se
    /// registra, con su presupuesto entero libre o sin él.
    #[serde(default)]
    pub sensible: bool,
}

impl Pieza {
    pub fn nueva(prioridad: Prioridad, origen: impl Into<String>, texto: impl Into<String>) -> Pieza {
        Pieza {
            prioridad,
            origen: origen.into(),
            texto: texto.into(),
            sensible: false,
        }
    }

    /// Marca la pieza como no saliente. Se encadena: `Pieza::nueva(..).sensible()`.
    pub fn sensible(mut self) -> Pieza {
        self.sensible = true;
        self
    }
}

/// Las piezas que no pueden salir del equipo, separadas de las que sí. Devuelve
/// también los orígenes descartados, con el prefijo `sensibilidad:` para que el
/// `reason` y el inspector digan **por qué** faltaba ese trozo y no «no cabía».
pub fn fuera_del_equipo(piezas: Vec<Pieza>) -> (Vec<Pieza>, Vec<String>) {
    let (dentro, fuera): (Vec<Pieza>, Vec<Pieza>) =
        piezas.into_iter().partition(|p| !p.sensible);
    let nombres = fuera.iter().map(|p| format!("sensibilidad:{}", p.origen)).collect();
    (dentro, nombres)
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
    llenar(piezas, presupuesto, contador)
}

/// El llenado, separado del orden: `armar` ordena por prioridad y
/// `armar_por_relevancia` trae el orden puesto. El corte es el mismo en los dos.
fn llenar(piezas: Vec<Pieza>, presupuesto: u32, contador: &dyn ContadorTokens) -> Armado {
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

/// §5.1: el mismo presupuesto y el mismo corte, pero decide la **relevancia del
/// pedido** qué pieza se queda fuera. Está detrás de `Flags.bm25_contexto`
/// (apagado) por un motivo concreto: `k1 = 1,2` y `b = 0,75` son los de la
/// receta, no números medidos en esta máquina, y encenderlo sin medir cambiaría
/// el resultado sin poder decir que mejora algo. Las piezas de seguridad van
/// siempre delante.
pub fn armar_por_relevancia(
    piezas: Vec<Pieza>,
    presupuesto: u32,
    consulta: &str,
    contador: &dyn ContadorTokens,
) -> Armado {
    let orden = bm25::para_armar(&piezas, consulta, bm25::Params::default());
    let reordenadas: Vec<Pieza> = orden.into_iter().map(|i| piezas[i].clone()).collect();
    llenar(reordenadas, presupuesto, contador)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Estimador;

    fn pieza(p: Prioridad, origen: &str, n: usize) -> Pieza {
        Pieza::nueva(p, origen, "x".repeat(n))
    }

    #[test]
    fn lo_sensible_se_separa_y_dice_quien_es() {
        let mut secreta = pieza(Prioridad::ArchivoNombrado, "archivo:.env", 40);
        secreta.sensible = true;
        let (dentro, fuera) = fuera_del_equipo(vec![
            pieza(Prioridad::Objetivo, "objetivo", 20),
            secreta,
        ]);
        assert_eq!(dentro.len(), 1, "solo la no sensible puede salir del equipo");
        assert_eq!(fuera, vec!["sensibilidad:archivo:.env".to_string()]);
        // Y sin nada sensible no inventa descartes.
        let (d2, f2) = fuera_del_equipo(vec![pieza(Prioridad::Objetivo, "objetivo", 20)]);
        assert_eq!(d2.len(), 1);
        assert!(f2.is_empty(), "{f2:?}");
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

    /// §5.1 con el flag: el hueco que deja el presupuesto la prioridad lo reparte
    /// de una manera y la relevancia del pedido, de otra. Aquí las dos piezas son
    /// del mismo escalón de prioridad, así que lo único que puede cambiar el
    /// resultado es el BM25 — y si no lo cambiara, la pieza sería un adorno.
    #[test]
    fn armar_por_relevancia_deja_fuera_la_que_no_va_con_el_pedido() {
        let pedendo = Pieza::nueva(
            Prioridad::Resto,
            "archivo:pedendo.rs",
            "fn pedendo () { mutex lock }",
        );
        let otra = Pieza::nueva(Prioridad::Resto, "archivo:otra.rs", "fn otra () { contador suma }");
        // Mismo escalón de prioridad y la irrelevante **delante** en la lista: así
        // se ve la diferencia. Por prioridad entra la primera que cabe; por
        // relevancia, la que habla del `mutex`.
        let piezas = vec![otra.clone(), pedendo.clone()];
        // 9 tokens: cada pieza son ~7 con el estimador, así que entra una y las
        // dos juntas no.
        let por_prioridad = armar(piezas.clone(), 9, &Estimador);
        let por_relevancia = armar_por_relevancia(piezas, 9, "¿qué hace el mutex?", &Estimador);
        assert_eq!(por_prioridad.incluidas.len(), 1, "{:?}", por_prioridad.rechazadas);
        assert_eq!(por_relevancia.incluidas.len(), 1, "{:?}", por_relevancia.rechazadas);
        assert_eq!(por_prioridad.incluidas[0].origen, "archivo:otra.rs");
        assert_eq!(
            por_relevancia.incluidas[0].origen, "archivo:pedendo.rs",
            "la del mutex tiene que quedar dentro: {:?}",
            por_relevancia.rechazadas
        );
        assert_eq!(por_relevancia.rechazadas, vec!["archivo:otra.rs".to_string()]);
    }
}
