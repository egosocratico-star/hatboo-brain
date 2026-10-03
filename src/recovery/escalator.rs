//! El escalador: con la clase de fallo decide si se reintenta el mismo Plan, se
//! firma uno nuevo, o se aborta diciendo la verdad.

use crate::api::vocab::FailureClass;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ajustes {
    /// Pedir más contexto dentro del presupuesto (§XIV: clase `context`).
    pub mas_contexto: bool,
    /// Volver a pedir la llamada de tool con los argumentos corregidos.
    pub corregir_args: bool,
    /// Reintentar el contrato (el stream se retracta).
    pub retractar: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accion {
    /// Mismo Plan; se reconstruye prompt/contexto.
    Reintentar {
        ajustes: Ajustes,
        intento: u8,
        porque: String,
    },
    /// Plan nuevo: `environment` cambia configuración, `model_capability` sube tier.
    NuevoPlan {
        subir_tier: bool,
        porque: String,
    },
    /// Se agotó `max_retries` o no hay tier elegible. Se informa con honestidad.
    Abortar { porque: String },
}

#[derive(Debug, Clone)]
pub struct Escalador {
    max_reintentos: u8,
    usados: u8,
    vistas: std::collections::BTreeMap<FailureClass, u8>,
    /// La escalada de tier tiene su propio tope: dos saltos seguidos es un bucle.
    subidas: u8,
    /// §II.10: «sin flag activo la pieza no corre». Apagada la recuperación,
    /// ningún fallo vuelve al modelo; apagada la escalada, ninguna clase sube de
    /// tier. Antes estos dos campos no existían y apagar las banderas no cambiaba
    /// nada, o sea que la pieza no se podía apagar.
    recuperacion: bool,
    escalar: bool,
}

impl Escalador {
    pub fn nuevo(max_reintentos: u8) -> Self {
        Escalador {
            max_reintentos,
            usados: 0,
            vistas: Default::default(),
            subidas: 0,
            recuperacion: true,
            escalar: true,
        }
    }

    pub fn con_flags(mut self, f: &crate::config::schema::Flags) -> Self {
        self.recuperacion = f.recuperacion;
        self.escalar = f.escalar;
        self
    }

    pub fn usados(&self) -> u8 {
        self.usados
    }

    pub fn restantes(&self) -> u8 {
        self.max_reintentos.saturating_sub(self.usados)
    }

    fn cuenta(&self, c: FailureClass) -> u8 {
        self.vistas.get(&c).copied().unwrap_or(0)
    }

    /// Un reintento por clase, tope global `max_retries`, y §1 del plan: si la
    /// misma clase vuelve a fallar, pasa a `model_capability`. Con `recuperacion`
    /// apagada toda clase aborta, y con `escalar` apagada no sube el tier.
    pub fn decidir(&mut self, clase: FailureClass) -> Accion {
        if !self.recuperacion {
            return Accion::Abortar {
                porque: "la recuperación está apagada: el fallo se reporta sin reintentar".into(),
            };
        }
        *self.vistas.entry(clase).or_insert(0) += 1;
        let veces = self.cuenta(clase);
        let agotado = self.usados >= self.max_reintentos;

        match clase {
            FailureClass::Entorno => {
                if agotado {
                    return Accion::Abortar {
                        porque: "el entorno sigue sin dar tregua y se agotaron los reintentos".into(),
                    };
                }
                self.usados += 1;
                // No sube tier: cambia configuración (ctx, keep_alive, modelo que
                // quepa ahora).
                Accion::NuevoPlan {
                    subir_tier: false,
                    porque: "fallo de entorno: el Governor rehace la configuración".into(),
                }
            }
            FailureClass::ModelCapability => {
                if !self.escalar {
                    // Sin escalada no hay remedio posible: esta clase *es* «el
                    // modelo no llegó», y lo único que la cura es otro tier.
                    return Accion::Abortar {
                        porque: "la escalada está apagada: el Brain no cambia de tier".into(),
                    };
                }
                if agotado || self.subidas >= 2 {
                    return Accion::Abortar {
                        porque: "no queda tier elegible para esta clase de fallo".into(),
                    };
                }
                self.usados += 1;
                self.subidas += 1;
                Accion::NuevoPlan {
                    subir_tier: true,
                    porque: "el modelo no llegó: se prueba el siguiente tier".into(),
                }
            }
            FailureClass::Verificacion => Accion::Abortar {
                // No es que haya fallado: es que no se pudo comprobar. Decirlo y
                // parar. §XIV: Unverifiable no es éxito.
                porque: "no había cómo verificar; se reporta como no verificable".into(),
            },
            FailureClass::Truncado => {
                // El techo vive en el Plan firmado, y un Plan no se parchea a
                // mitad de corrida (§VIII): reintentar igual reproduce el corte.
                // Parar y decir qué hay que cambiar cuesta menos que quemar otras
                // dos llamadas al modelo en lo mismo.
                Accion::Abortar {
                    porque: "la salida la cortó el techo del Plan: hace falta un Plan \
                             con más salida o menos contexto, no otro intento igual"
                        .into(),
                }
            }
            c @ (FailureClass::Formato | FailureClass::Tool | FailureClass::Contexto) => {
                if agotado {
                    return Accion::Abortar {
                        porque: format!("se agotaron los reintentos tras fallar {c:?}"),
                    };
                }
                self.usados += 1;
                if veces >= 2 && self.escalar {
                    // Misma clase repetida → se trata como capacidad (§1†).
                    if self.subidas >= 2 {
                        return Accion::Abortar {
                            porque: format!("«{c:?}» se repite y ya no queda tier que subir"),
                        };
                    }
                    self.subidas += 1;
                    return Accion::NuevoPlan {
                        subir_tier: true,
                        porque: format!("«{c:?}» falló dos veces: se sube de tier"),
                    };
                }
                Accion::Reintentar {
                    ajustes: match c {
                        FailureClass::Formato => Ajustes {
                            retractar: true,
                            ..Default::default()
                        },
                        FailureClass::Tool => Ajustes {
                            corregir_args: true,
                            ..Default::default()
                        },
                        _ => Ajustes {
                            mas_contexto: true,
                            ..Default::default()
                        },
                    },
                    intento: self.usados,
                    porque: format!("reintento de {c:?} con el mismo plan"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::Flags;
    use FailureClass as F;

    #[test]
    fn un_reintento_por_clase_y_pois_subir_tier() {
        let mut e = Escalador::nuevo(2);
        match e.decidir(F::Formato) {
            Accion::Reintentar { ajustes, .. } => assert!(ajustes.retractar),
            a => panic!("{a:?}"),
        }
        match e.decidir(F::Formato) {
            Accion::NuevoPlan { subir_tier, porque } => {
                assert!(subir_tier);
                assert!(porque.contains("dos veces"), "{porque}");
            }
            a => panic!("{a:?}"),
        }
    }

    #[test]
    fn el_tope_global_aborta() {
        let mut e = Escalador::nuevo(1);
        assert!(matches!(e.decidir(F::Formato), Accion::Reintentar { .. }));
        assert!(matches!(e.decidir(F::Tool), Accion::Abortar { .. }));
        assert_eq!(e.usados(), 1);
    }

    #[test]
    fn entorno_no_sube_tier() {
        let mut e = Escalador::nuevo(2);
        match e.decidir(F::Entorno) {
            Accion::NuevoPlan { subir_tier, .. } => assert!(!subir_tier),
            a => panic!("{a:?}"),
        }
    }

    #[test]
    fn unverifiable_no_es_un_reintento_infinito() {
        let mut e = Escalador::nuevo(3);
        assert!(matches!(e.decidir(F::Verificacion), Accion::Abortar { .. }));
        assert_eq!(e.usados(), 0, "no se gastó ningún reintento");
    }

    #[test]
    fn contexto_pide_mas_contexto() {
        let mut e = Escalador::nuevo(2);
        match e.decidir(F::Contexto) {
            Accion::Reintentar { ajustes, porque, .. } => {
                assert!(ajustes.mas_contexto);
                assert!(!ajustes.corregir_args);
                assert!(porque.contains("mismo plan"), "{porque}");
            }
            a => panic!("{a:?}"),
        }
    }

    #[test]
    fn dos_subidas_y_a_abortar_no_a_bucle() {
        let mut e = Escalador::nuevo(9);
        assert!(matches!(e.decidir(F::ModelCapability), Accion::NuevoPlan { subir_tier: true, .. }));
        assert!(matches!(e.decidir(F::ModelCapability), Accion::NuevoPlan { subir_tier: true, .. }));
        assert!(matches!(e.decidir(F::ModelCapability), Accion::Abortar { .. }));
    }

    /// §II.10: un flag que no cambia la conducta no es un flag. Estas tres
    /// pruebas son las que faltaban para que `recuperacion` y `escalar`
    /// significaran algo.
    #[test]
    fn sin_recuperacion_nada_vuelve_al_modelo() {
        let f = Flags {
            recuperacion: false,
            ..Default::default()
        };
        let mut e = Escalador::nuevo(3).con_flags(&f);
        match e.decidir(F::Formato) {
            Accion::Abortar { porque } => assert!(porque.contains("apagada"), "{porque}"),
            a => panic!("{a:?}"),
        }
        assert_eq!(e.usados(), 0, "apagada la pieza no se gasta presupuesto");
    }

    #[test]
    fn sin_escalada_no_sube_de_tier_pero_el_governor_rehace_config() {
        let f = Flags {
            escalar: false,
            ..Default::default()
        };
        let mut e = Escalador::nuevo(9).con_flags(&f);
        // `model_capability` es justo la clase que se cura subiendo tier.
        match e.decidir(F::ModelCapability) {
            Accion::Abortar { porque } => assert!(porque.contains("escalada"), "{porque}"),
            a => panic!("{a:?}"),
        }
        // La clase repetida ya no salta: reintenta mientras quede presupuesto.
        let mut e = Escalador::nuevo(9).con_flags(&f);
        assert!(matches!(e.decidir(F::Formato), Accion::Reintentar { .. }));
        assert!(matches!(e.decidir(F::Formato), Accion::Reintentar { .. }));
        // Cambiar configuración no es escalar: eso sigue.
        let mut e = Escalador::nuevo(9).con_flags(&f);
        assert!(matches!(
            e.decidir(F::Entorno),
            Accion::NuevoPlan { subir_tier: false, .. }
        ));
    }

    #[test]
    fn el_techo_cortado_no_quema_un_intento() {
        let mut e = Escalador::nuevo(3);
        match e.decidir(F::Truncado) {
            Accion::Abortar { porque } => assert!(porque.contains("techo"), "{porque}"),
            otra => panic!("con el techo hay que parar, no reintentar igual: {otra:?}"),
        }
        assert_eq!(e.usados(), 0, "abortar por truncado no gasta reintentos");
    }

    #[test]
    fn los_flags_de_default_dejan_la_conducta_de_siempre() {
        let mut e = Escalador::nuevo(2).con_flags(&Flags::default());
        assert!(matches!(e.decidir(F::Formato), Accion::Reintentar { .. }));
        assert!(matches!(
            e.decidir(F::Formato),
            Accion::NuevoPlan { subir_tier: true, .. }
        ));
    }
}
