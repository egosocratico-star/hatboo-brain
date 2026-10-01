//! Contrato `patch`: que sea un unificado, que el contexto coincida y que las
//! rutas no se salgan del proyecto. §XVI del Canon: las rutas fuera del proyecto
//! se rechazan **aquí y en el producto**, no en el prompt.

use super::{Fallo, Unverifiable, Verdict};
use crate::api::vocab::FailureClass;

#[derive(Debug, Clone, PartialEq)]
pub enum LineaHunk {
    Contexto(String),
    Baja(String),
    Sube(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub lineas: Vec<LineaHunk>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArchivoParche {
    pub origen: String,
    pub destino: String,
    pub hunks: Vec<Hunk>,
}

/// ¿La ruta cae dentro de la raíz del proyecto? Se normaliza `..` a mano: un
/// `../` colado en un parche es la forma clásica de escribir fuera.
pub fn dentro_de(root: &str, ruta: &str) -> bool {
    let r = separadores(root);
    let p = separadores(ruta);
    let absoluto = p.starts_with('/') || (p.len() > 1 && &p[1..2] == ":");
    let completo = if absoluto {
        p.clone()
    } else {
        format!("{}/{}", r.trim_end_matches('/'), p)
    };
    let trozos: Vec<&str> = completo
        .split('/')
        .filter(|t| !t.is_empty() && *t != ".")
        .collect();
    let mut pila: Vec<&str> = Vec::new();
    for t in trozos {
        if t == ".." {
            if pila.pop().is_none() {
                return false;
            }
        } else {
            pila.push(t);
        }
    }
    let raiz: Vec<&str> = r.split('/').filter(|t| !t.is_empty() && *t != ".").collect();
    pila.len() >= raiz.len() && pila[..raiz.len()] == raiz[..]
}

fn separadores(s: &str) -> String {
    s.replace('\\', "/")
}

pub fn parsear(texto: &str) -> Result<Vec<ArchivoParche>, String> {
    let mut archivos: Vec<ArchivoParche> = Vec::new();
    let mut actual: Option<(String, String)> = None;
    let mut hunks: Vec<Hunk> = Vec::new();

    for cruda in texto.lines() {
        if let Some(resto) = cruda.strip_prefix("--- ") {
            if let Some((a, b)) = actual.take() {
                archivos.push(ArchivoParche {
                    origen: a,
                    destino: b,
                    hunks: std::mem::take(&mut hunks),
                });
            }
            actual = Some((limpiar_ruta(resto), String::new()));
            continue;
        }
        if let Some(resto) = cruda.strip_prefix("+++ ") {
            if let Some((a, _)) = actual.as_mut() {
                let b = limpiar_ruta(resto);
                actual = Some((std::mem::take(a), b));
            } else {
                return Err("un @@/+ sin su ---".into());
            }
            continue;
        }
        if cruda.starts_with("@@") {
            let (old_start, old_len, new_start, new_len) = cabecera(cruda)?;
            hunks.push(Hunk {
                old_start,
                old_len,
                new_start,
                new_len,
                lineas: Vec::new(),
            });
            continue;
        }
        if let Some(h) = hunks.last_mut() {
            if let Some(l) = cruda.strip_prefix(' ') {
                h.lineas.push(LineaHunk::Contexto(l.to_string()));
            } else if let Some(l) = cruda.strip_prefix('-') {
                h.lineas.push(LineaHunk::Baja(l.to_string()));
            } else if let Some(l) = cruda.strip_prefix('+') {
                h.lineas.push(LineaHunk::Sube(l.to_string()));
            } else if cruda.is_empty() {
                h.lineas.push(LineaHunk::Contexto(String::new()));
            } else if cruda.starts_with('\\') {
                // "\ No newline at end of file": se ignora, no cambia el contenido.
            } else {
                return Err(format!("línea de parche imposible: {cruda}"));
            }
        }
    }
    if let Some((a, b)) = actual {
        archivos.push(ArchivoParche {
            origen: a,
            destino: b,
            hunks,
        });
    }
    if archivos.is_empty() {
        return Err("no hay ningún archivo en el parche".into());
    }
    Ok(archivos)
}

fn limpiar_ruta(s: &str) -> String {
    let sin = s.split_whitespace().next().unwrap_or("");
    sin.trim_start_matches("a/").trim_start_matches("b/").to_string()
}

fn cabecera(l: &str) -> Result<(usize, usize, usize, usize), String> {
    // @@ -a,b +c,d @@ …
    let interior = l
        .strip_prefix("@@ -")
        .ok_or_else(|| "cabecera @@ rota".to_string())?;
    let (viejo, nuevo) = interior
        .split_once(" +")
        .ok_or_else(|| "cabecera @@ sin +".to_string())?;
    let parsear = |s: &str| -> (usize, usize) {
        let (a, b) = s.split_once(',').unwrap_or((s, "1"));
        let n = b.trim_start_matches(')').split_whitespace().next().unwrap_or(b);
        (
            a.trim_start_matches(')').parse().unwrap_or(0),
            n.parse().unwrap_or(1),
        )
    };
    let (old_start, old_len) = parsear(viejo);
    let (new_start, new_len) = parsear(nuevo.split_whitespace().next().unwrap_or(""));
    Ok((old_start, old_len, new_start, new_len))
}

/// Aplica los hunks sobre el contenido original. La comprobación de contexto es
/// lo que hace esto una **verificación**, no un formateo: si el contexto no
/// cuadra, el parche es mentira.
pub fn aplicar(original: &str, hunks: &[Hunk]) -> Result<String, String> {
    let lineas: Vec<&str> = original.split('\n').collect();
    let mut salida: Vec<String> = Vec::new();
    let mut cursor = 0usize; // índice 0 en `lineas`
    for h in hunks {
        let inicio = h.old_start.saturating_sub(1);
        if inicio > lineas.len() {
            return Err(format!(
                "el hunk no cuadra: empieza en la línea {} y el archivo tiene {}",
                h.old_start,
                lineas.len()
            ));
        }
        salida.extend(lineas[cursor..inicio].iter().map(|s| s.to_string()));
        cursor = inicio;
        for l in &h.lineas {
            match l {
                LineaHunk::Contexto(t) => {
                    let real = lineas.get(cursor).copied().unwrap_or("");
                    if real != t.as_str() {
                        return Err(format!(
                            "contexto no cuadra en la línea {}: esperado {t:?}, había {real:?}",
                            cursor + 1
                        ));
                    }
                    salida.push(t.clone());
                    cursor += 1;
                }
                LineaHunk::Baja(t) => {
                    let real = lineas.get(cursor).copied().unwrap_or("");
                    if real != t.as_str() {
                        return Err(format!(
                            "el borrado no cuadra en la línea {}: esperado {t:?}, había {real:?}",
                            cursor + 1
                        ));
                    }
                    cursor += 1;
                }
                LineaHunk::Sube(t) => salida.push(t.clone()),
            }
        }
    }
    salida.extend(lineas[cursor..].iter().map(|s| s.to_string()));
    Ok(salida.join("\n"))
}

/// Verificación completa del contrato `patch`.
pub fn comprobar(
    texto: &str,
    root: Option<&str>,
    leer: &dyn Fn(&str) -> Option<String>,
) -> Verdict {
    let archivos = match parsear(texto) {
        Ok(a) => a,
        Err(e) => {
            return Verdict::Fail(Fallo {
                clase: FailureClass::Formato,
                motivo: e,
            })
        }
    };
    for a in &archivos {
        if a.destino.is_empty() || a.destino == "/dev/null" {
            return Verdict::Fail(Fallo {
                clase: FailureClass::Formato,
                motivo: "el parche no declara un archivo destino".into(),
            });
        }
        if let Some(r) = root {
            if !dentro_de(r, &a.destino) || !dentro_de(r, &a.origen) {
                return Verdict::Fail(Fallo {
                    clase: FailureClass::Tool,
                    motivo: format!("la ruta «{}» sale del proyecto", a.destino),
                });
            }
        }
        if a.hunks.is_empty() {
            return Verdict::Fail(Fallo {
                clase: FailureClass::Formato,
                motivo: format!("«{}» no trae hunks", a.destino),
            });
        }
        let Some(original) = leer(&a.destino) else {
            // No lo podemos leer: no es que falle, es que no se puede afirmar.
            return Verdict::Unverifiable {
                motivo: Unverifiable::ComandoNoEjecutable(format!(
                    "no se puede leer {} para comprobar el parche",
                    a.destino
                )),
            };
        };
        if let Err(e) = aplicar(&original, &a.hunks) {
            return Verdict::Fail(Fallo {
                clase: FailureClass::Formato,
                motivo: format!("{}: {e}", a.destino),
            });
        }
    }
    Verdict::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!(\"a\");
+    println!(\"b\");
 }
";

    #[test]
    fn parsea_un_parche_normal() {
        let a = parsear(P).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].destino, "src/main.rs");
        assert_eq!(a[0].hunks.len(), 1);
        // contexto + baja + sube + contexto: cuatro líneas en el hunk.
        assert_eq!(a[0].hunks[0].lineas.len(), 4);
    }

    #[test]
    fn aplica_cuando_el_contexto_cuadra() {
        let original = "fn main() {\n    println!(\"a\");\n}\n";
        let a = parsear(P).unwrap();
        let nuevo = aplicar(original, &a[0].hunks).unwrap();
        assert_eq!(nuevo, "fn main() {\n    println!(\"b\");\n}\n");
    }

    #[test]
    fn contexto_falso_es_un_fallo_no_una_escritura() {
        let original = "fn main() {\n    println!(\"otra cosa\");\n}\n";
        let a = parsear(P).unwrap();
        let e = aplicar(original, &a[0].hunks).unwrap_err();
        assert!(e.contains("no cuadra"), "{e}");
    }

    #[test]
    fn el_escape_de_ruta_se_corta() {
        assert!(dentro_de("C:/proyecto", "C:/proyecto/src/a.rs"));
        assert!(dentro_de("C:/proyecto", "src/a.rs"));
        assert!(!dentro_de("C:/proyecto", "../otra-cosa/a.rs"));
        assert!(!dentro_de("C:/proyecto", "C:/Windows/a.rs"));
        assert!(!dentro_de("C:/proyecto", "/etc/passwd"));
        // Un `..` que sube hasta la raíz y sigue dentro, vale.
        assert!(dentro_de("C:/proyecto", "src/../src/a.rs"));
    }

    #[test]
    fn comprobar_dice_lo_que_hace_falta() {
        let leer = |ruta: &str| -> Option<String> {
            if ruta == "src/main.rs" {
                Some("fn main() {\n    println!(\"a\");\n}\n".into())
            } else {
                None
            }
        };
        assert_eq!(comprobar(P, Some("C:/proyecto"), &leer), Verdict::Pass);
        // Archivo que no existe → no verificable, no «mal».
        let otro = P.replace("src/main.rs", "src/otro.rs");
        assert!(matches!(
            comprobar(&otro, Some("C:/proyecto"), &leer),
            Verdict::Unverifiable { .. }
        ));
        // Ruta fuera del proyecto.
        let fuera = P.replace("src/main.rs", "../../etc/passwd");
        match comprobar(&fuera, Some("C:/proyecto"), &leer) {
            Verdict::Fail(f) => assert_eq!(f.clase, FailureClass::Tool),
            x => panic!("{x:?}"),
        }
        // Prosa que no es un parche.
        assert!(matches!(
            comprobar("cambia la línea 3", Some("C:/proyecto"), &leer),
            Verdict::Fail { .. }
        ));
    }
}
