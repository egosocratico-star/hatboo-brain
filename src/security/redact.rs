//! Redacción. Nada de expresiones regulares (el crate no trae `regex`): un
//! explorador de bytes con las formas que de verdad aparecen en las respuestas
//! de los proveedores.
//!
//! Es defensa **blanda**: la dura es el Tool Gate, el sandbox y el approval del
//! producto. Su trabajo es que una clave no acabe en un log ni en un cuerpo de
//! API ajeno.

/// Claves que se reconocen por prefijo. El prefijo se deja ver (identifica el
/// motor sin filtrar el secreto); el resto se vuelve `***`.
const PREFIJOS: &[&str] = &[
    "sk-ant-",   // Anthropic
    "sk-proj-",  // OpenAI proyecto
    "sk-",       // OpenAI y amigos
    "hf_",       // Hugging Face
    "ghp_",      // GitHub personal
    "gho_",      // GitHub OAuth
    "ghs_",      // GitHub app
    "glpat-",    // GitLab
    "xoxb-",     // Slack
];

/// Cabeceras o campos cuyo valor es siempre opaco.
const CLAVES: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "secret",
    "password",
    "passwd",
    "key",
];

const OSCURO: &str = "***";

/// Caracteres de un nombre de campo o de una clave. El `:` **no** entra: si no,
/// «Authorization:» se leía como parte del nombre y la cabecera no se redactaba.
fn es_char_clave(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/' | b'\\')
}

/// Palabras que en una cabecera van *delante* del secreto.
const ESQUEMAS: &[&str] = &["bearer", "basic", "token", "apikey", "api_key", "key"];

/// Dónde acaba un valor a partir de `i`: el primer delimitador real.
fn fin_del_valor(s: &str, i: usize) -> usize {
    s[i..]
        .find(['"', '\'', ' ', ',', ';', '\n', '\r', '}'])
        .map(|k| i + k)
        .unwrap_or(s.len())
}

/// Un trozo de alfabeto base64 de 24+ caracteres que mezcla letras y dígitos:
/// parece un token, no prosa. Un path o una palabra larga no dispara la regla
/// porque le faltan o los dígitos o los signos de relleno.
fn trozo_opaco(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    let mut digitos = 0usize;
    let mut letras = 0usize;
    while j < bytes.len()
        && ((bytes[j] as char).is_ascii_alphanumeric()
            || matches!(bytes[j], b'+' | b'/' | b'=' | b'_' | b'-'))
    {
        if bytes[j].is_ascii_digit() {
            digitos += 1;
        } else if bytes[j].is_ascii_alphabetic() {
            letras += 1;
        }
        j += 1;
    }
    if j - i >= 24 && digitos >= 2 && letras >= 4 {
        Some(j)
    } else {
        None
    }
}

/// Redacta el texto. Idempotente: redactar dos veces sale igual.
pub fn texto(s: &str) -> String {
    let b = s.as_bytes();
    let mut salida = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < b.len() {
        // 1 · prefijos conocidos.
        let resto = &s[i..];
        if let Some(p) = PREFIJOS.iter().find(|p| resto.starts_with(**p)) {
            salida.push_str(p);
            salida.push_str(OSCURO);
            i += p.len();
            while i < b.len()
                && ((b[i] as char).is_ascii_alphanumeric() || matches!(b[i], b'-' | b'_'))
            {
                i += 1;
            }
            continue;
        }
        // 2 · `clave` seguida de separador y valor, con o sin comillas.
        if es_char_clave(b[i]) {
            let fin = s[i..]
                .find(|c: char| !es_char_clave(c as u8))
                .map(|k| i + k)
                .unwrap_or(s.len());
            let nombre = s[i..fin].to_ascii_lowercase();
            if CLAVES.contains(&nombre.as_str()) {
                salida.push_str(&s[i..fin]);
                i = fin;
                // El separador: `"`, `:`, `=`, espacios…
                while i < b.len() && matches!(b[i], b'"' | b'\'' | b' ' | b'\t') {
                    salida.push(b[i] as char);
                    i += 1;
                }
                if i < b.len() && matches!(b[i], b':' | b'=') {
                    salida.push(':');
                    i += 1;
                    while i < b.len() && matches!(b[i], b' ' | b'"' | b'\'') {
                        salida.push(b[i] as char);
                        i += 1;
                    }
                    // El valor: hasta el siguiente delimitador real.
                    let mut vf = fin_del_valor(s, i);
                    // En una cabecera el secreto suele venir detrás del esquema
                    // («Authorization: Bearer …»): si la primera palabra es un
                    // esquema conocido, lo que sigue también es el valor.
                    if ESQUEMAS.contains(&s[i..vf].to_ascii_lowercase().as_str()) {
                        let despues = s[vf..]
                            .find(|c: char| !c.is_whitespace())
                            .map(|k| vf + k)
                            .unwrap_or(s.len());
                        if despues < s.len() {
                            vf = fin_del_valor(s, despues);
                        }
                    }
                    if vf > i {
                        salida.push_str(OSCURO);
                        i = vf;
                        continue;
                    }
                }
                continue;
            }
            // 3 · token opaco largo sin clave delante.
            if let Some(j) = trozo_opaco(b, i) {
                salida.push_str(OSCURO);
                i = j;
                continue;
            }
            salida.push_str(&s[i..fin]);
            i = fin;
            continue;
        }
        // Copiar el byte actual (los multibyte se copian de golpe con el char).
        let ch = s[i..].chars().next().unwrap();
        salida.push(ch);
        i += ch.len_utf8();
    }
    salida
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefijos_conocidos_dejan_ver_la_forma() {
        assert_eq!(texto("clave sk-ant-abc123def456ghi789 lista"), "clave sk-ant-*** lista");
        assert_eq!(texto("token hf_AAAAAAAAAAAAAAAAAAAAAAAA fin"), "token hf_*** fin");
        assert_eq!(texto("ghp_16charsAAAAAAAAAAAA"), "ghp_***");
    }

    #[test]
    fn campos_de_json_y_cabeceras() {
        assert_eq!(
            texto(r#"{"api_key":"zzz-1234567890abcdef"}"#),
            r#"{"api_key":"***"}"#
        );
        assert_eq!(
            texto("Authorization: Bearer abcdefghijklmnopqrstuvwx"),
            "Authorization: ***"
        );
        assert_eq!(
            texto("x-api-key: AAAABBBBccccdddd11112222"),
            "x-api-key: ***"
        );
    }

    #[test]
    fn la_prosa_no_se_toca() {
        let s = "el modelo qwen3.5:0.8b respondió en 346 ms con 604 tokens";
        assert_eq!(texto(s), s);
        let json = r#"{"level":"N2","num_ctx":4096,"tools":["read_file","write_file"]}"#;
        assert_eq!(texto(json), json);
    }

    #[test]
    fn un_token_opaco_largo_se_corta_aunque_no_sepa_quien_es() {
        let s = "cabecera: abcdef0123456789ABCDEF+/=";
        let r = texto(s);
        assert!(!r.contains("abcdef0123456789"), "{r}");
    }

    #[test]
    fn idempotente() {
        let sucio = r#"{"api_key":"sk-proj-AAAAAAAAAAAAAAAAAAAA","otro":"normal"}"#;
        let una = texto(sucio);
        assert_eq!(texto(&una), una);
    }

    #[test]
    fn no_rompe_el_utf8() {
        assert_eq!(
            texto("el usuario escribió «borra todo» y nada más"),
            "el usuario escribió «borra todo» y nada más"
        );
    }
}
