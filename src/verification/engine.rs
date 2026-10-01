//! El despacho: qué se comprueba según el modo del Plan y el contrato de la
//! salida.

use super::execution::Ejecutor;
use super::{Candidato, Fallo, Unverifiable, Verdict, VerificationResult, json, patch, text};
use crate::api::vocab::{FailureClass, OutputContract, VerificationMode};

/// Lo que el verificatorio necesita del entorno. Todo lo que falte se declara
/// como `Unverifiable`, nunca como Pass.
#[derive(Clone, Copy)]
pub struct Entorno<'a> {
    /// El comando de verificación detectado por el producto (`cargo check`…).
    pub comando: Option<&'a str>,
    pub ejecutor: Option<&'a dyn Ejecutor>,
    /// Para leer el archivo que el parche quiere tocar. `Send + Sync` porque el
    /// runtime lo tiene detrás de un `Arc`.
    pub leer: Option<&'a (dyn Fn(&str) -> Option<String> + Send + Sync)>,
    pub esquema: Option<&'a serde_json::Value>,
    pub idioma_pedido: Option<&'a str>,
    pub timeout_s: u32,
}

impl<'a> Default for Entorno<'a> {
    fn default() -> Self {
        Entorno {
            comando: None,
            ejecutor: None,
            leer: None,
            esquema: None,
            idioma_pedido: None,
            timeout_s: 60,
        }
    }
}

pub fn verificar(modo: VerificationMode, c: &Candidato, e: &Entorno) -> VerificationResult {
    use VerificationMode as M;
    let f = forma(c, e);
    let veredicto = match modo {
        M::Ninguna => return VerificationResult::NoRequerida,
        M::Formato => f,
        M::Determinista | M::DeterministaRevision => {
            // Un `Fail` de forma sí bloquea el comando: no se gasta una ejecución en
            // una salida que ya se ve rota. Un `Unverifiable` de forma no bloquea:
            // el formato de un texto no sabe si el proyecto compila, y el comando
            // sí. Si tampoco hay comando, lo dice el propio `determinista`.
            if matches!(f, Verdict::Fail(_)) {
                f
            } else {
                determinista(c, e)
            }
        }
    };
    desde_veredicto(veredicto)
}

fn desde_veredicto(v: Verdict) -> VerificationResult {
    match v {
        Verdict::Pass => VerificationResult::Pass,
        Verdict::Fail(f) => VerificationResult::Fail {
            clase: f.clase,
            motivo: f.motivo,
        },
        Verdict::Unverifiable { motivo } => VerificationResult::Unverifiable { motivo },
    }
}

/// §XIV: N2 código = parse/compile/lint/tests en sandbox con timeout. Si no hay
/// comando, se dice que no hay; si lo hay pero no se pudo correr, `Unverifiable`.
fn determinista(c: &Candidato, e: &Entorno) -> Verdict {
    let (Some(ejecutor), Some(comando)) = (e.ejecutor, e.comando) else {
        return Verdict::Unverifiable {
            motivo: if e.comando.is_none() {
                Unverifiable::ProyectoSinVerificacion
            } else {
                Unverifiable::ComandoNoEjecutable(
                    "el producto no dio ejecutor: el comando no se corrió".into(),
                )
            },
        };
    };
    super::execution::comprobar(ejecutor, comando, c.root.unwrap_or("."), e.timeout_s)
}

fn forma(c: &Candidato, e: &Entorno) -> Verdict {
    let base = text::forma(c.texto, c.contrato, 2048);
    if !matches!(base, Verdict::Pass) {
        return base;
    }
    match c.contrato {
        OutputContract::Json => json::comprobar(c.texto, e.esquema),
        OutputContract::Patch => match (c.root, e.leer) {
            (Some(r), Some(l)) if !r.is_empty() => patch::comprobar(c.texto, Some(r), l),
            _ => Verdict::Unverifiable {
                motivo: Unverifiable::ComandoNoEjecutable(
                    "sin lector de archivos no se puede comprobar el parche".into(),
                ),
            },
        },
        OutputContract::ToolCall => forma_tool_call(c.texto),
        OutputContract::Texto | OutputContract::Markdown => {
            // El idioma es informativo (§XIV): si no se puede concluir, la forma
            // ya comprobada manda. Convertir un Pass en `Unverifiable` porque el
            // detector no tuvo suficientes palabras sería tirar por la borda la
            // única comprobación determinista que había.
            match text::idioma_coincide(c.texto, e.idioma_pedido) {
                Verdict::Unverifiable { .. } => base,
                otro => otro,
            }
        }
    }
}

/// Un `tool_call` tiene que ser JSON con `tool` y `args`. Que la tool esté
/// habilitada lo decide la `Puerta`, no esto.
pub fn forma_tool_call(texto: &str) -> Verdict {
    let Some(v) = json::extraer_json(texto) else {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: "la llamada a tool no es JSON".into(),
        });
    };
    let Some(obj) = v.as_object() else {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: "la llamada a tool no es un objeto".into(),
        });
    };
    if !obj.contains_key("tool") {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: "falta «tool» en la llamada".into(),
        });
    }
    if !obj.contains_key("args") && !obj.contains_key("arguments") {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: "falta «args» en la llamada".into(),
        });
    }
    Verdict::Pass
}

/// El alias que usa el resto del crate.
pub fn verificar_con(
    modo: VerificationMode,
    texto: &str,
    contrato: OutputContract,
    root: Option<&str>,
    e: &Entorno,
) -> VerificationResult {
    let c = Candidato {
        texto,
        contrato,
        root,
        archivo_objetivo: None,
    };
    verificar(modo, &c, e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::VerificationMode as M;
    use crate::verification::execution::{Salida, SinEjecutor};

    fn cand(texto: &str, contrato: OutputContract) -> Candidato<'_> {
        Candidato {
            texto,
            contrato,
            root: Some("C:/p"),
            archivo_objetivo: None,
        }
    }

    #[test]
    fn ninguna_no_pide_nada() {
        let e = Entorno::default();
        assert_eq!(
            verificar(M::Ninguna, &cand("", OutputContract::Texto), &e),
            VerificationResult::NoRequerida
        );
    }

    #[test]
    fn formato_de_texto_vacio_falla_con_clase() {
        let e = Entorno::default();
        let r = verificar(M::Formato, &cand("  ", OutputContract::Texto), &e);
        match r {
            VerificationResult::Fail { clase, motivo } => {
                assert_eq!(clase, FailureClass::Formato);
                assert!(motivo.contains("vacía"), "{motivo}");
            }
            otro => panic!("{otro:?}"),
        }
    }

    #[test]
    fn un_proyecto_sin_comando_es_unverifiable_no_pass() {
        let e = Entorno {
            ejecutor: Some(&SinEjecutor),
            ..Default::default()
        };
        let r = verificar(
            M::Determinista,
            &cand("texto correcto", OutputContract::Texto),
            &e,
        );
        assert_eq!(
            r,
            VerificationResult::Unverifiable {
                motivo: Unverifiable::ProyectoSinVerificacion
            }
        );
    }

    #[test]
    fn el_comando_manda_el_veredicto_final() {
        struct Ok0;
        impl Ejecutor for Ok0 {
            fn correr(&self, _: &str, _: &str, _: u32) -> Result<Salida, String> {
                Ok(Salida {
                    codigo: 0,
                    stdout: "ok".into(),
                    stderr: String::new(),
                    dur_ms: 10,
                })
            }
        }
        let e = Entorno {
            comando: Some("cargo check"),
            ejecutor: Some(&Ok0),
            ..Default::default()
        };
        assert_eq!(
            verificar(
                M::Determinista,
                &cand("listo", OutputContract::Texto),
                &e
            ),
            VerificationResult::Pass
        );
    }

    #[test]
    fn formato_roto_ni_llega_a_correr_el_comando() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Contador(AtomicBool);
        impl Ejecutor for Contador {
            fn correr(&self, _: &str, _: &str, _: u32) -> Result<Salida, String> {
                self.0.store(true, Ordering::SeqCst);
                Ok(Salida {
                    codigo: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    dur_ms: 1,
                })
            }
        }
        let contador = Contador(AtomicBool::new(false));
        let e = Entorno {
            comando: Some("npm test"),
            ejecutor: Some(&contador),
            ..Default::default()
        };
        let r = verificar(
            M::Determinista,
            &cand("```rust\nsin cerrar", OutputContract::Markdown),
            &e,
        );
        assert!(matches!(r, VerificationResult::Fail { .. }), "{r:?}");
        assert!(
            !contador.0.load(Ordering::SeqCst),
            "con la forma rota no se ejecuta nada"
        );
    }

    #[test]
    fn tool_call_pide_tool_y_args() {
        assert_eq!(forma_tool_call("{\"tool\":\"read_file\",\"args\":{}}"), Verdict::Pass);
        assert!(matches!(
            forma_tool_call("{\"args\":{}}"),
            Verdict::Fail(Fallo { .. })
        ));
        assert!(matches!(
            forma_tool_call("leo el archivo"),
            Verdict::Fail(Fallo { .. })
        ));
        assert!(matches!(
            forma_tool_call("{\"tool\":\"read_file\"}"),
            Verdict::Fail(Fallo { .. })
        ));
    }

    #[test]
    fn revision_por_nowodelo_no_cambia_nada_sin_bench() {
        // DeterministaRevision es Determinista + revisión del producto: el Brain no
        // gasta un segundo modelo para decir «listo».
        let e = Entorno::default();
        let a = verificar(M::DeterministaRevision, &cand("hola", OutputContract::Texto), &e);
        let b = verificar(M::Determinista, &cand("hola", OutputContract::Texto), &e);
        assert_eq!(a, b);
    }
}
