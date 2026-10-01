//! Métricas y coste. Los pesos salen de lo medido en Fase 0
//! (`hatboo/benchmarks/fase0.md` §5 y §6): no son números redondos inventados.

use serde::{Deserialize, Serialize};

/// `costo = w_r·(ram_gb × s) + w_c·(tokens/1000) + w_t·reintentos`
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pesos {
    /// RAM retenida × segundos de reloj.
    pub w_r: f32,
    /// Solo tiene sentido en API, donde el token es dinero. En local es **0** por
    ///que la correlación tokens↔decode medida fue r = 0,999: sería contar lo
    /// mismo dos veces.
    pub w_c: f32,
    /// Coste propio del reintento (lo que nocaptura el tiempo de la corrida extra).
    /// Sin verificar hasta la Fase 5: mientras el verificador no exista, 0.
    pub w_t: f32,
}

impl Default for Pesos {
    fn default() -> Self {
        Pesos {
            w_r: 0.0361,
            w_c: 0.0,
            w_t: 0.0,
        }
    }
}

/// La línea base de **un** modelo en **esta** máquina: con ella el coste sale
/// adimensional y «1,00» significa «lo que cuesta hoy sin Brain».
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Referencia {
    pub ram_gb: f32,
    pub s_ref: f32,
    pub tokens_k_ref: f32,
}

impl Referencia {
    /// `gemma3:1b` medido el 30-09-2026: 878 MB residentes a num_ctx 2048, 31,6 s
    /// de media por corrida, 623 tokens por corrida.
    pub const GEMMA3_1B: Referencia = Referencia {
        ram_gb: 0.878,
        s_ref: 31.623,
        tokens_k_ref: 0.623,
    };

    pub fn gb_s(&self) -> f32 {
        self.ram_gb * self.s_ref
    }

    /// `w_r` normalizado: 1 / (ram_gb × s_ref).
    pub fn w_r(&self) -> f32 {
        if self.gb_s() <= 0.0 {
            return 0.0;
        }
        1.0 / self.gb_s()
    }

    pub fn pesos(&self) -> Pesos {
        Pesos {
            w_r: self.w_r(),
            w_c: 0.0,
            w_t: 0.0,
        }
    }

    /// Coste de una corrida. `s` es **segundos de reloj** del `run()` completo,
    /// carga del modelo incluida: lo que el usuario esperó es el coste (§III del
    /// Canon: los recursos reales mandan sobre las estimaciones).
    pub fn coste(&self, ram_gb: f32, duracion_s: f32, tokens: u64, reintentos: u8) -> Coste {
        let p = self.pesos();
        let br = p.w_r * (ram_gb * duracion_s.max(0.0));
        let bc = p.w_c * (tokens as f32 / 1000.0);
        let bt = p.w_t * reintentos as f32;
        Coste {
            total: br + bc + bt,
            ram_s: br,
            tokens: bc,
            reintentos: bt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Coste {
    pub total: f32,
    pub ram_s: f32,
    pub tokens: f32,
    pub reintentos: f32,
}

impl Coste {
    /// La regla de regresión de §1: ninguna métrica de coste empeora más del 5 %
    /// en validación. Medido: el ruido entre repeticiones de la misma petición es
    /// 1,9 % en el agregado y ±35 % en una corrida suelta, así que esto solo vale
    /// sobre medianas de ≥3 repes.
    pub const UMBRAL_REGRESION: f32 = 0.05;

    pub fn empeoro(&self, base: f32) -> bool {
        if base <= 0.0 {
            return false;
        }
        self.total / base - 1.0 > Self::UMBRAL_REGRESION
    }
}

/// Lo que el provider declaró de una corrida. Todo `Option`: si el proveedor no
/// lo dice, no se inventa.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Medida {
    pub tokens_entrada: Option<u64>,
    pub tokens_salida: Option<u64>,
    pub tok_s: Option<f32>,
    pub ttft_ms: Option<u64>,
    pub ram_mb: Option<u64>,
    pub carga_ms: Option<u64>,
    pub reintentos: u8,
    pub recargas: u32,
}

/// `Brain Efficiency = tareas_verificadas_ok / costo_normalizado` (§XV del
/// Canon). Unverifiable **no** suma en el numerador y sí cuesta en el denominador.
#[derive(Debug, Clone, Default)]
pub struct Eficiencia {
    pub verificadas: u64,
    pub intentos: u64,
    pub costo_total: f32,
    pub unverifiable: u64,
}

impl Eficiencia {
    pub fn anotar(&mut self, coste: f32, pas: bool, unverifiable: bool) {
        self.intentos += 1;
        self.costo_total += coste;
        if pas {
            self.verificadas += 1;
        }
        if unverifiable {
            self.unverifiable += 1;
        }
    }

    /// `None` si todavía no hay coste medido: 0/0 no es una eficiencia, es falta
    /// de datos.
    pub fn indice(&self) -> Option<f32> {
        if self.costo_total <= 0.0 {
            return None;
        }
        Some(self.verificadas as f32 / self.costo_total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_referencia_normaliza_a_uno() {
        let r = Referencia::GEMMA3_1B;
        let c = r.coste(r.ram_gb, r.s_ref, 623, 0);
        assert!((c.total - 1.0).abs() < 0.001, "{c:?}");
    }

    #[test]
    fn tope_de_salida_baja_el_coste() {
        let r = Referencia::GEMMA3_1B;
        // Medido: la corrida media con tope 512 fue 23,7 s en vez de 31,6 s.
        let c = r.coste(r.ram_gb, 23.7, 357, 0);
        assert!((c.total - 0.75).abs() < 0.02, "{c:?}");
    }

    #[test]
    fn el_5_es_el_piso_del_ruido_agregado() {
        let base = 1.0;
        assert!(Coste { total: 1.06, ..Default::default() }.empeoro(base));
        assert!(!Coste { total: 1.049, ..Default::default() }.empeoro(base));
        assert!(!Coste { total: 0.8, ..Default::default() }.empeoro(base));
        // Sin base no se afirma nada.
        assert!(!Coste { total: 5.0, ..Default::default() }.empeoro(0.0));
    }

    #[test]
    fn unverifiable_cuesta_y_no_suma() {
        let mut e = Eficiencia::default();
        e.anotar(1.0, false, true);
        assert_eq!(e.verificadas, 0);
        assert_eq!(e.unverifiable, 1);
        assert_eq!(e.indice(), Some(0.0));
        e.anotar(1.0, true, false);
        assert_eq!(e.indice(), Some(0.5));
        assert_eq!(Eficiencia::default().indice(), None);
    }
}
