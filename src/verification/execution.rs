//! Correr los comandos del proyecto. El crate **no** lanza procesos: quien los
//! lanza es el producto, dentro de su sandbox y con sus permisos. Aquí está la
//! puerta y la interpretación del resultado.

use super::{Fallo, Unverifiable, Verdict};
use crate::api::vocab::FailureClass;

#[derive(Debug, Clone, PartialEq)]
pub struct Salida {
    pub codigo: i32,
    pub stdout: String,
    pub stderr: String,
    pub dur_ms: u64,
}

/// El ejecutor del adaptador. `Err` = no se pudo correr (sin permiso, comando
/// inexistente, el sandbox lo cortó).
pub trait Ejecutor: Send + Sync {
    fn correr(&self, comando: &str, root: &str, timeout_s: u32) -> Result<Salida, String>;
}

/// Sin ejecutor: todo comando es `Unverifiable`. Mejor decirlo que fingir un Pass.
#[derive(Debug, Clone, Default)]
pub struct SinEjecutor;

impl Ejecutor for SinEjecutor {
    fn correr(&self, comando: &str, _root: &str, _timeout_s: u32) -> Result<Salida, String> {
        Err(format!("el producto no dio ejecutor: «{comando}» no se corrió"))
    }
}

pub fn comprobar(
    ejecutor: &dyn Ejecutor,
    comando: &str,
    root: &str,
    timeout_s: u32,
) -> Verdict {
    match ejecutor.correr(comando, root, timeout_s) {
        Err(e) => Verdict::Unverifiable {
            motivo: Unverifiable::ComandoNoEjecutable(e),
        },
        Ok(s) if s.codigo == 0 => Verdict::Pass,
        Ok(s) => {
            let resumen = resumen(&s);
            // Código distinto de cero: el entorno o el modelo. Se clasifica por lo
            // que dice la salida, no por corazonada: si ni siquiera arrancó, es
            // `Entorno`; si compiló y falló, es del modelo.
            let clase = if si_no_arranco(&resumen) {
                FailureClass::Entorno
            } else {
                FailureClass::Formato
            };
            Verdict::Fail(Fallo {
                clase,
                motivo: format!("«{comando}» salió {codigo}: {resumen}", codigo = s.codigo),
            })
        }
    }
}

/// Sin `regex`: las marcas de que el comando no llegó a correr.
fn si_no_arranco(s: &str) -> bool {
    let bajo = s.to_lowercase();
    [
        "command not found",
        "not recognized as an internal",
        "no such file or directory",
        "permission denied",
        "cannot find module",
        "error: could not compile `cargo",
        "spawn",
    ]
    .iter()
    .any(|p| bajo.contains(p))
}

fn resumen(s: &Salida) -> String {
    let mut t = if !s.stderr.trim().is_empty() {
        s.stderr.trim().to_string()
    } else {
        s.stdout.trim().to_string()
    };
    if t.chars().count() > 600 {
        t = t.chars().take(600).collect();
        t.push_str(" […]");
    }
    if t.is_empty() {
        "(sin salida)".into()
    } else {
        t
    }
}

/// Si el proyecto no declaró comando: se dice, no se inventa (§XIV: «Sin tests del
/// proyecto: decirlo. No fingir.»).
pub fn sin_comando() -> Verdict {
    Verdict::Unverifiable {
        motivo: Unverifiable::ProyectoSinVerificacion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fijo(Option<Result<Salida, String>>);

    impl Ejecutor for Fijo {
        fn correr(&self, _c: &str, _r: &str, _t: u32) -> Result<Salida, String> {
            self.0.clone().unwrap_or_else(|| Err("sin prueba".into()))
        }
    }

    #[test]
    fn cero_es_pass() {
        let e = Fijo(Some(Ok(Salida {
            codigo: 0,
            stdout: "ok".into(),
            stderr: String::new(),
            dur_ms: 1200,
        })));
        assert_eq!(comprobar(&e, "cargo check", "/p", 30), Verdict::Pass);
    }

    #[test]
    fn un_fallo_de_compilar_es_del_modelo_no_del_entorno() {
        let e = Fijo(Some(Ok(Salida {
            codigo: 101,
            stdout: String::new(),
            stderr: "error[E0308]: mismatched types".into(),
            dur_ms: 900,
        })));
        match comprobar(&e, "cargo check", "/p", 30) {
            Verdict::Fail(f) => {
                assert_eq!(f.clase, FailureClass::Formato);
                assert!(f.motivo.contains("E0308"), "{}", f.motivo);
            }
            x => panic!("{x:?}"),
        }
    }

    #[test]
    fn el_comando_que_no_arranco_es_entorno() {
        let e = Fijo(Some(Ok(Salida {
            codigo: 127,
            stdout: String::new(),
            stderr: "cargo: command not found".into(),
            dur_ms: 3,
        })));
        match comprobar(&e, "cargo check", "/p", 30) {
            Verdict::Fail(f) => assert_eq!(f.clase, FailureClass::Entorno),
            x => panic!("{x:?}"),
        }
    }

    #[test]
    fn sin_ejecutor_no_se_finge_nada() {
        let v = comprobar(&SinEjecutor, "npm test", "/p", 30);
        assert!(matches!(v, Verdict::Unverifiable { .. }), "{v:?}");
        assert!(matches!(sin_comando(), Verdict::Unverifiable { .. }));
    }

    #[test]
    fn la_salida_larga_se_recorta() {
        let s = Salida {
            codigo: 1,
            stdout: String::new(),
            stderr: "e".repeat(4000),
            dur_ms: 1,
        };
        assert!(resumen(&s).chars().count() <= 610);
        let vacia = Salida {
            codigo: 1,
            stdout: String::new(),
            stderr: "   ".into(),
            dur_ms: 1,
        };
        assert_eq!(resumen(&vacia), "(sin salida)");
    }
}
