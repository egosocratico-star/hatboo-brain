//! Contrato `json`: que parseé y que tenga lo que se pidió. Mini-schema propio,
//! sin `schemars`: cuatro palabras del JSON Schema alcanzan para lo que el Brain
//! necesita comprobar, y así el crate no arrastra un generador de código.

use super::{Fallo, Unverifiable, Verdict};
use crate::api::vocab::FailureClass;
use serde_json::Value;

/// ¿Es JSON, aunque venga envuelto en un cercado de markdown?
pub fn extraer_json(texto: &str) -> Option<Value> {
    let t = texto.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    // ```json … ```
    if let Some(i) = t.find("```") {
        let resto = &t[i + 3..];
        let cuerpo = resto.strip_prefix("json").unwrap_or(resto);
        if let Some(fin) = cuerpo.find("```") {
            if let Ok(v) = serde_json::from_str::<Value>(cuerpo[..fin].trim()) {
                return Some(v);
            }
        }
    }
    // Primer objeto o array balanceado en el texto.
    let bytes = t.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'{' || *b == b'[' {
            let (abre, cierra) = if *b == b'{' { (b'{', b'}') } else { (b'[', b']') };
            let mut depth = 0usize;
            let mut dentro = false;
            let mut escape = false;
            for j in i..bytes.len() {
                let c = bytes[j];
                if escape {
                    escape = false;
                    continue;
                }
                match c {
                    b'\\' => escape = true,
                    b'"' => dentro = !dentro,
                    _ if dentro => {}
                    x if x == abre => depth += 1,
                    x if x == cierra => {
                        depth -= 1;
                        if depth == 0 {
                            if let Ok(v) = serde_json::from_str(&t[i..=j]) {
                                return Some(v);
                            }
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

/// Valida contra un mini-schema. Devuelve el camino del primer fallo.
pub fn mini_schema(v: &Value, esquema: &Value) -> Result<(), String> {
    let tipo = esquema.get("type").and_then(|t| t.as_str());
    match tipo {
        Some("object") => {
            let obj = v.as_object().ok_or_else(|| "no es un objeto".to_string())?;
            if let Some(req) = esquema.get("required").and_then(|r| r.as_array()) {
                for k in req {
                    let Some(k) = k.as_str() else { continue };
                    if !obj.contains_key(k) {
                        return Err(format!("falta la clave «{k}»"));
                    }
                }
            }
            if let Some(props) = esquema.get("properties").and_then(|p| p.as_object()) {
                for (k, sub) in props {
                    if let Some(vale) = obj.get(k) {
                        mini_schema(vale, sub).map_err(|e| format!("{k}: {e}"))?;
                    }
                }
            }
            Ok(())
        }
        Some("array") => {
            let arr = v.as_array().ok_or_else(|| "no es un array".to_string())?;
            if let Some(item) = esquema.get("items") {
                for (i, x) in arr.iter().enumerate() {
                    mini_schema(x, item).map_err(|e| format!("[{i}]: {e}"))?;
                }
            }
            if let Some(n) = esquema.get("minItems").and_then(|n| n.as_u64()) {
                if (arr.len() as u64) < n {
                    return Err(format!("menos de {n} elementos"));
                }
            }
            Ok(())
        }
        Some("string") => {
            let s = v.as_str().ok_or_else(|| "no es texto".to_string())?;
            if let Some(n) = esquema.get("minLength").and_then(|n| n.as_u64()) {
                if (s.chars().count() as u64) < n {
                    return Err(format!("texto de menos de {n}"));
                }
            }
            Ok(())
        }
        Some("number") | Some("integer") => {
            if v.as_f64().is_none() {
                return Err("no es número".into());
            }
            Ok(())
        }
        Some("boolean") => {
            if v.as_bool().is_none() {
                return Err("no es sí/no".into());
            }
            Ok(())
        }
        _ => {
            if let Some(opciones) = esquema.get("enum").and_then(|e| e.as_array()) {
                if !opciones.contains(v) {
                    return Err(format!("«{v}» no está entre los valores admitidos"));
                }
            }
            Ok(())
        }
    }
}

pub fn comprobar(texto: &str, esquema: Option<&Value>) -> Verdict {
    let Some(v) = extraer_json(texto) else {
        return Verdict::Fail(Fallo {
            clase: FailureClass::Formato,
            motivo: "no contiene JSON parseable".into(),
        });
    };
    match esquema {
        None => Verdict::Unverifiable {
            motivo: Unverifiable::SinContratoVerificable,
        },
        Some(e) => match mini_schema(&v, e) {
            Ok(()) => Verdict::Pass,
            Err(camino) => Verdict::Fail(Fallo {
                clase: FailureClass::Formato,
                motivo: format!("el JSON no cumple el esquema en {camino}"),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saca_el_json_unque_esté_envuelto() {
        assert_eq!(
            extraer_json(r#"{"a":1}"#),
            Some(serde_json::json!({"a":1}))
        );
        let envuelto = "mira:\n```json\n{\"niveles\": [1,2]}\n```\ny listo";
        assert_eq!(
            extraer_json(envuelto),
            Some(serde_json::json!({"niveles":[1,2]}))
        );
        let con_texto = "el resultado es {\"ok\": true} según lo que veo";
        assert_eq!(extraer_json(con_texto), Some(serde_json::json!({"ok":true})));
        assert_eq!(extraer_json("nada de json aquí"), None);
    }

    #[test]
    fn llaves_escapadas_no_confunden_el_corte() {
        let t = r#"{"frase": "tiene } dentro", "otro": 1}"#;
        assert_eq!(
            extraer_json(t),
            Some(serde_json::json!({"frase":"tiene } dentro","otro":1}))
        );
    }

    #[test]
    fn el_esquema_exige_lo_que_dice() {
        let e = serde_json::json!({"type":"object","required":["id","nivel"],"properties":{"nivel":{"type":"integer"}}});
        assert_eq!(
            comprobar(r#"{"id":"a","nivel":2}"#, Some(&e)),
            Verdict::Pass
        );
        let r = comprobar(r#"{"id":"a"}"#, Some(&e));
        match &r {
            Verdict::Fail(f) => assert!(f.motivo.contains("nivel"), "{:?}", f),
            otro => panic!("{otro:?}"),
        }
        assert!(matches!(
            comprobar(r#"{"id":"a","nivel":"dos"}"#, Some(&e)),
            Verdict::Fail(Fallo { .. })
        ));
    }

    #[test]
    fn sin_esquema_no_hay_farsa() {
        assert!(matches!(
            comprobar("{\"a\":1}", None),
            Verdict::Unverifiable { .. }
        ));
    }
}
