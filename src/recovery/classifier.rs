//! De dónde viene cada clase de fallo. Si esto se clasifica mal, la recuperación
//! se equivoca de puerta: reintentar un `Entorno` como si fuera `Formato` es
//! gastar tiempo en lo mismo.

use crate::api::error::BrainError;
use crate::api::vocab::FailureClass;
use crate::providers::ProviderError;
use crate::verification::VerificationResult;

/// Lo que pasó.
#[derive(Debug, Clone)]
pub enum Origen<'a> {
    Verificacion(&'a VerificationResult),
    Error(&'a BrainError),
    /// El Tool Gate rechazó una llamada.
    ToolRechazada,
    /// El modelo no llamó a ninguna tool cuando el contrato lo pedía.
    SinLlamadaDeTool,
    /// Se agotó `max_tool_calls` o `max_write_actions`.
    PresupuestoAgotado,
}

pub fn clasificar(o: &Origen) -> FailureClass {
    match o {
        Origen::ToolRechazada | Origen::SinLlamadaDeTool | Origen::PresupuestoAgotado => {
            FailureClass::Tool
        }
        Origen::Verificacion(VerificationResult::Fail { clase, .. }) => *clase,
        Origen::Verificacion(VerificationResult::Unverifiable { .. }) => FailureClass::Verificacion,
        Origen::Verificacion(_) => FailureClass::Formato,
        Origen::Error(e) => match e {
            BrainError::Timeout => FailureClass::Entorno,
            BrainError::ResourceExhausted { .. } => FailureClass::Entorno,
            BrainError::Provider(p) => match p {
                // No está instalado o falta la clave: no es capacidad del modelo,
                // es que el entorno no está listo.
                ProviderError::ModeloNoInstalado(_) | ProviderError::SinCredencial(_) => {
                    FailureClass::Entorno
                }
                ProviderError::TiempoFuera | ProviderError::Transporte(_) => {
                    FailureClass::Entorno
                }
                // Un 5xx o un 429 son del proveedor; un 413, del plan. Lo que
                // queda de 4xx NO es capacidad del modelo: un 401/403 es una clave
                // que no sirve, un 404 es un modelo que no está en el plan (el
                // caso real medido con Groq el 01-10) y un 400/422 es un cuerpo
                // mal formado nuestro. Tratarlos de «capacidad» hacía subir de tier
                // —en local, a un modelo más grande que no cabe en 8,45 GB— justo
                // cuando subir de tier no arregla nada.
                ProviderError::Status { codigo, .. } => {
                    if *codigo == 413 {
                        FailureClass::Contexto
                    } else if matches!(*codigo, 400 | 406 | 415 | 422) {
                        FailureClass::Formato
                    } else {
                        FailureClass::Entorno
                    }
                }
                // El stream se cortó o no parsea: es el transporte, no el modelo.
                ProviderError::RespuestaInvalida(_) => FailureClass::Entorno,
                ProviderError::Cancelado | ProviderError::NoImplementado(_) => {
                    FailureClass::Entorno
                }
            },
            BrainError::NoEligibleModel => FailureClass::ModelCapability,
            _ => FailureClass::Entorno,
        },
    }
}

/// El texto de la salida, cuando el fallo es de contrato.
pub fn motivo(o: &Origen) -> String {
    match o {
        Origen::Verificacion(VerificationResult::Fail { motivo, .. }) => motivo.clone(),
        Origen::Verificacion(VerificationResult::Unverifiable { motivo }) => {
            format!("no verificable: {motivo:?}")
        }
        Origen::Error(e) => e.mensaje(),
        Origen::ToolRechazada => "llamada a tool fuera del plan".into(),
        Origen::SinLlamadaDeTool => "el contrato pedía una tool y no llamó a ninguna".into(),
        Origen::PresupuestoAgotado => "se agotó el presupuesto de acciones".into(),
        Origen::Verificacion(_) => "verificación fallida".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigError;
    use crate::verification::Unverifiable;

    #[test]
    fn entorno_no_es_capacidad() {
        for e in [
            BrainError::Timeout,
            BrainError::ResourceExhausted {
                needed_mb: 2000,
                free_mb: 300,
            },
            BrainError::Provider(ProviderError::TiempoFuera),
            BrainError::Provider(ProviderError::Transporte("connection refused".into())),
            BrainError::Provider(ProviderError::Status {
                codigo: 503,
                cuerpo: "up down".into(),
            }),
            BrainError::Provider(ProviderError::ModeloNoInstalado("x:1b".into())),
        ] {
            assert_eq!(clasificar(&Origen::Error(&e)), FailureClass::Entorno, "{e:?}");
        }
    }

    #[test]
    fn capacidad_es_lo_que_no_sabe_hacerse() {
        // `NoEligibleModel` es la única vía honesta a «hace falta otro modelo».
        let e2 = BrainError::NoEligibleModel;
        assert_eq!(clasificar(&Origen::Error(&e2)), FailureClass::ModelCapability);
        // Un stream roto o un JSON que no parsea es del transporte: §XIV lo pone
        // en el entorno. Antes caía en capacidad y subía el tier — en local, a un
        // modelo más grande que no cabe en 8,45 GB — por culpa de la conexión.
        let e = BrainError::Provider(ProviderError::RespuestaInvalida("corto".into()));
        assert_eq!(clasificar(&Origen::Error(&e)), FailureClass::Entorno);
    }

    #[test]
    fn un_4xx_de_autenticacion_no_pide_otro_modelo() {
        // El caso real, medido el 01-10 con Groq: `llama-3.3-70b-versatile` no
        // está en el plan gratuito y devuelve 404. Clasificarlo de capacidad hacía
        // firmar un Plan con un modelo más grande, que en este equipo es RAM que
        // no cabe. Lo mismo un 401/403 con la clave caducada.
        for codigo in [401u16, 403, 404, 409, 410, 500, 503, 429] {
            let e = BrainError::Provider(ProviderError::Status {
                codigo,
                cuerpo: "no".into(),
            });
            assert_eq!(
                clasificar(&Origen::Error(&e)),
                FailureClass::Entorno,
                "HTTP {codigo}"
            );
        }
        // Y un 400/422 es un cuerpo mal formado nuestro: contrato, no modelo.
        for codigo in [400u16, 422] {
            let e = BrainError::Provider(ProviderError::Status {
                codigo,
                cuerpo: "json malo".into(),
            });
            assert_eq!(
                clasificar(&Origen::Error(&e)),
                FailureClass::Formato,
                "HTTP {codigo}"
            );
        }
    }

    #[test]
    fn un_413_pide_mas_contexto_no_otro_modelo() {
        let e = BrainError::Provider(ProviderError::Status {
            codigo: 413,
            cuerpo: "too large".into(),
        });
        assert_eq!(clasificar(&Origen::Error(&e)), FailureClass::Contexto);
    }

    #[test]
    fn unverifiable_es_clase_verificacion() {
        let v = VerificationResult::Unverifiable {
            motivo: Unverifiable::ProyectoSinVerificacion,
        };
        assert_eq!(clasificar(&Origen::Verificacion(&v)), FailureClass::Verificacion);
        let f = VerificationResult::Fail {
            clase: FailureClass::Tool,
            motivo: "x".into(),
        };
        assert_eq!(clasificar(&Origen::Verificacion(&f)), FailureClass::Tool);
    }

    #[test]
    fn las_herramientas_suenan_a_herramienta() {
        assert_eq!(clasificar(&Origen::ToolRechazada), FailureClass::Tool);
        assert_eq!(clasificar(&Origen::SinLlamadaDeTool), FailureClass::Tool);
        assert_eq!(
            motivo(&Origen::SinLlamadaDeTool),
            "el contrato pedía una tool y no llamó a ninguna"
        );
    }

    #[test]
    fn un_error_de_config_no_se_disfraza_de_fallo_del_modelo() {
        let e = BrainError::Config(ConfigError::SinBackend);
        assert_eq!(clasificar(&Origen::Error(&e)), FailureClass::Entorno);
        assert_eq!(motivo(&Origen::Error(&e)), e.mensaje());
    }
}
