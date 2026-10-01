//! Comprobaciones de formato. Baratas, deterministas, y las únicas que hacen falta
//! en N1.

use super::{Fallo, Unverifiable, Verdict};
use crate::api::vocab::{FailureClass, OutputContract};

/// ¿Prosa que no se puede verificar en código?
pub fn sin_verificacion_posible(contrato: OutputContract) -> bool {
    matches!(contrato, OutputContract::Texto | OutputContract::Markdown)
}

/// Vacío, truncado o una cerdilla de marcador. No intenta entender el contenido:
/// eso lo hace el modelo, y un determinista no compite con él.
pub fn forma(texto: &str, contrato: OutputContract, max_tokens: u32) -> Verdict {
    let t = texto.trim();
    if t.is_empty() {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: "respuesta vacía".into(),
        });
    }
    // Marcas de corte del propio proveedor o del tope de tokens.
    for marca in ["…", "<truncado>", "[invalid generation]"] {
        if t.ends_with(marca) {
            return Verdict::Fail(Fallo {
                clase: FailureClass::Formato,
                motivo: format!("terminó cortado en «{marca}»"),
            });
        }
    }
    // Señal de que se pasó del tope: el modelo cerró mal un bloque.
    if contrato == OutputContract::Markdown {
        let cercas = t.matches("```").count();
        if cercas % 2 == 1 {
            return Verdict::Fail(Fallo {
                clase: FailureClass::Formato,
                motivo: "un bloque de código quedó sin cerrar".into(),
            });
        }
    }
    if matches!(contrato, OutputContract::Patch) && !t.contains("+++") && !t.contains("---") {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: "no tiene cabeceras de archivo, no es un parche".into(),
        });
    }
    // Longitud razonable frente al tope pedido: si se pasó muchísimo, el modelo
    // no paró y hay que reintentar con contrato.
    if t.chars().count() as u32 > max_tokens * 6 {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: format!(
                "{} caracteres para un tope de {max_tokens} tokens",
                t.chars().count()
            ),
        });
    }
    Verdict::Pass
}

/// El idioma de la salida frente al idioma pedido. §XIV del Canon lo pone en el
/// contrato `text`/`markdown`. Es blando: solo avisa si no coincide y no se
/// parece en nada.
pub fn idioma_coincide(texto: &str, esperado: Option<&str>) -> Verdict {
    let Some(e) = esperado else {
        return Verdict::Unverifiable {
            motivo: Unverifiable::SinContratoVerificable,
        };
    };
    let bajo = texto.to_lowercase();
    let pistas_es = [" que ", " porque ", " esto ", " puedes ", " están "];
    let pistas_en = [" that ", " because ", " this ", " you can ", " cannot "];
    let es = pistas_es.iter().filter(|p| bajo.contains(**p)).count();
    let en = pistas_en.iter().filter(|p| bajo.contains(**p)).count();
    let detectado = match (es, en) {
        (a, b) if a > b => "es",
        (a, b) if b > a => "en",
        _ => {
            return Verdict::Unverifiable {
                motivo: Unverifiable::SinContratoVerificable,
            }
        }
    };
    if detectado == e {
        Verdict::Pass
    } else {
        Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: format!("respondió en {detectado} y se pidió {e}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vacio_y_cortado_fallan() {
        assert!(matches!(forma("   ", OutputContract::Texto, 512), Verdict::Fail { .. }));
        for cortado in ["hola …", "texto <truncado>", "x [invalid generation]"] {
            assert!(
                matches!(forma(cortado, OutputContract::Texto, 512), Verdict::Fail { .. }),
                "{cortado}"
            );
        }
    }

    #[test]
    fn una_cerca_sin_cerrar_es_fallo_de_markdown() {
        let t = "explico:\n```rust\nfn a() {}";
        assert!(matches!(forma(t, OutputContract::Markdown, 512), Verdict::Fail { .. }));
        let ok = "explico:\n```rust\nfn a() {}\n```";
        assert_eq!(forma(ok, OutputContract::Markdown, 512), Verdict::Pass);
    }

    #[test]
    fn el_parche_sin_cabeceras_no_es_parche() {
        assert!(matches!(
            forma("cambia la línea 3", OutputContract::Patch, 512),
            Verdict::Fail { .. }
        ));
        let p = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-a\n+b";
        assert_eq!(forma(p, OutputContract::Patch, 512), Verdict::Pass);
    }

    #[test]
    fn pasar_de_largo_con_el_tope_se_detecta() {
        let largo = "a".repeat(4000);
        assert!(matches!(
            forma(&largo, OutputContract::Texto, 100),
            Verdict::Fail { .. }
        ));
    }

    #[test]
    fn el_idioma_es_informativo_no_una_orden() {
        assert!(matches!(
            idioma_coincide("esto es un texto en español porque lo escribí aquí", Some("es")),
            Verdict::Pass
        ));
        assert!(matches!(
            idioma_coincide("this is in English because I wrote it that way", Some("es")),
            Verdict::Fail { .. }
        ));
        assert!(matches!(
            idioma_coincide("hola", Some("es")),
            Verdict::Unverifiable { .. }
        ));
        assert!(matches!(
            idioma_coincide("hola", None),
            Verdict::Unverifiable { .. }
        ));
    }
}
