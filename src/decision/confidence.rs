//! Los efectos de la confianza, tal como los cierra §1 del plan. Dos orígenes,
//! un solo sitio donde se aplican.

use super::DecisionResult;
use crate::api::vocab::{Confidence, Level, Risk, VerificationMode};

/// Aplica los umbrales sobre el lote. `otro_nivel` es el segundo candidato de la
/// votación: §1 habla de «el más alto de los dos candidatos», y si no hay
/// segundo candidato no hay a qué subir — el nivel que sale del Engine ya es el
/// más alto disponible. Lo que sí se aplica siempre es el suelo de `risk` y de
/// verificación de la franja baja.
///
/// `tope_de_duda` es la propuesta de §15.3 (punto abierto): dejar la duda en N2
/// como máximo. Viene **apagado** por defecto porque el plan no lo ha cerrado.
pub fn efectos(
    d: &mut DecisionResult,
    otro_nivel: Option<Level>,
    tope_de_duda: Option<Level>,
) {
    let c = d.confidence.valor();
    if c >= Confidence::UMBRAL_ALTO {
        return;
    }

    let antes = d.level;
    if let Some(otro) = otro_nivel {
        d.level = d.level.max(otro);
    }
    if c < Confidence::UMBRAL_BAJO {
        // `< 0.55` no se trata como seguro.
        d.risk = d.risk.max(Risk::Medium);
        if d.verification < VerificationMode::Formato {
            d.verification = VerificationMode::Formato;
        }
    }

    if let Some(tope) = tope_de_duda {
        d.level = d.level.min(tope);
    }
    if d.level != antes {
        d.por_que.push_str(&format!(" · duda {c:.2} sube {antes:?}→{:?}", d.level));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{
        DecisionSource, ExecutionTarget, Intent, ModelTarget, OutputContract,
    };

    fn lote(level: Level, c: f32) -> DecisionResult {
        DecisionResult {
            intent: Intent::Ask,
            level,
            risk: Risk::Low,
            skip_generative: false,
            execution_target: ExecutionTarget::Local,
            model_target: ModelTarget::Indistinto,
            tools: vec![],
            output_contract: OutputContract::Texto,
            verification: level.verificacion_minima(),
            confidence: Confidence(c),
            source: DecisionSource::Reglas,
            por_que: String::new(),
            salida_directa: None,
            lectura_directa: None,
        }
    }

    #[test]
    fn fuera_de_duda_no_toca_nada() {
        let mut d = lote(Level::N1, 0.9);
        let antes = d.clone();
        efectos(&mut d, Some(Level::N3), None);
        assert_eq!(d, antes);
    }

    #[test]
    fn duda_media_toma_el_mas_alto_de_dos() {
        let mut d = lote(Level::N1, 0.7);
        efectos(&mut d, Some(Level::N2), None);
        assert_eq!(d.level, Level::N2);
        // Y no sube si el otro candidato era más bajo.
        let mut e = lote(Level::N2, 0.7);
        efectos(&mut e, Some(Level::N1), None);
        assert_eq!(e.level, Level::N2);
    }

    #[test]
    fn sin_segundo_candidato_el_nivel_se_queda() {
        // §1 dice «el más alto de los dos candidatos». Con uno solo no hay
        // comparación: subir por subir sería inventarse la regla.
        let mut d = lote(Level::N1, 0.7);
        efectos(&mut d, None, None);
        assert_eq!(d.level, Level::N1);
        assert_eq!(d.risk, Risk::Low, "0,7 no es la franja baja");
    }

    #[test]
    fn inseguro_sube_riesgo_y_verificacion() {
        let mut d = lote(Level::N0, 0.4);
        d.verification = VerificationMode::Ninguna;
        efectos(&mut d, None, None);
        assert_eq!(d.level, Level::N0);
        assert_eq!(d.risk, Risk::Medium);
        assert_eq!(d.verification, VerificationMode::Formato);
    }

    #[test]
    fn el_tope_de_duda_es_opt_in() {
        let mut d = lote(Level::N2, 0.7);
        efectos(&mut d, Some(Level::N3), None);
        assert_eq!(d.level, Level::N3);
        let mut e = lote(Level::N2, 0.7);
        efectos(&mut e, Some(Level::N3), Some(Level::N2));
        assert_eq!(e.level, Level::N2);
    }

    #[test]
    fn la_duda_nunca_baja_el_nivel() {
        let mut d = lote(Level::N3, 0.2);
        efectos(&mut d, Some(Level::N1), None);
        assert_eq!(d.level, Level::N3);
        assert_eq!(d.risk, Risk::Medium);
    }

    /// §1 habla de tres tramos (≥0,85 · 0,55–0,85 · <0,55). Con la puntuación que
    /// sale de `brain-rules.json` —`prioridad × 100 + condiciones`— los tres son
    /// alcanzables, y esto lo fija: si alguien cambia el escalado y la banda del
    /// medio deja de existir, se ve aquí y no en un panel.
    #[test]
    fn las_tres_franjas_de_confianza_son_alcanzables() {
        // Regla solitaria: sin competidor el margen es total.
        assert!(Confidence::margen(6001.0, 0.0).valor() >= Confidence::UMBRAL_ALTO);
        // `riesgo-destrutivo` (60) contra `charla-corta` (1): 0,983.
        let claro = Confidence::margen(6001.0, 101.0).valor();
        assert!((0.85..1.0).contains(&claro), "{claro}");
        // `archivo-con-verbo` (40) contra `git-de-lectura` (25): 0,387 → duda.
        let dudan = Confidence::margen(4001.0, 2501.0).valor();
        assert!(dudan < 0.55, "{dudan}");
        // Y una pareja que cae justo en la banda del medio: 60 contra 10.
        let media = Confidence::margen(6001.0, 1001.0).valor();
        assert!(
            (Confidence::UMBRAL_BAJO..Confidence::UMBRAL_ALTO).contains(&media),
            "{media} no cae en 0,55–0,85"
        );
    }
}
