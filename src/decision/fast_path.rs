//! El Fast Path: lo que se resuelve **sin** modelo generativo. Devuelve un
//! `DecisionResult` completo o `None`; nunca un resultado a medias (§1 del plan),
//! porque un `Some` salta el Engine entero y un `Some` parcial firmaría un Plan
//! con huecos.

use super::DecisionResult;
use super::rules::Reglas;
use crate::api::vocab::{
    Confidence, DecisionSource, ExecutionTarget, Intent, Level, ModelTarget, OutputContract, Risk,
    VerificationMode,
};

/// Normaliza para comparar: minúsculas, sin signos de puntuación sueltos, espacios
/// colapsados. Con esto «¡Hola!» y «hola  » son el mismo saludo.
pub fn normalizar(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut espacio = true;
    for ch in s.chars().flat_map(char::to_lowercase) {
        // Lo que va a `out` sin abrir un espacio: letras, números y los signos que
        // forman una ruta. Separadores (`/` incluido) y letras van al mismo lado
        // porque partir una ruta por la mitad volvería irreconocible el archivo.
        if ch.is_alphanumeric()
            || ch == '.'
            || ch == '/'
            || ch == '\\'
            || ch == '-'
            || ch == '_'
        {
            out.push(ch);
            espacio = false;
        } else if !espacio {
            out.push(' ');
            espacio = true;
        }
    }
    out.trim().to_string()
}

impl DecisionResult {
    /// Un Fast Path **nunca** se infla: la conservación de §XI dice que lo que ya
    /// era N0 no se sube a N2 porque sí.
    fn rapido(
        intent: Intent,
        contrato: OutputContract,
        por_que: String,
    ) -> DecisionResult {
        DecisionResult {
            intent,
            level: Level::N0,
            risk: Risk::Low,
            skip_generative: true,
            execution_target: ExecutionTarget::Local,
            model_target: ModelTarget::Indistinto,
            tools: vec![],
            output_contract: contrato,
            verification: VerificationMode::Ninguna,
            confidence: Confidence::determinista(),
            source: DecisionSource::FastPath,
            por_que,
            salida_directa: None,
            lectura_directa: None,
        }
    }
}

/// Aritmética simple y **sin eval**: suma, resta, producto, cociente y
/// paréntesis sobre decimales. Cualquier cosa rara devuelve `None` y el pedido
/// sigue su camino normal; adivinar un cálculo sería peor que generarlo.
pub fn aritmetica(s: &str) -> Option<String> {
    let limpio = s.trim().trim_end_matches('=').trim_end_matches('?');
    if limpio.is_empty() || !limpio.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    // Una sola operación de nivel cero. Con dos o más, lo más probable es que
    // sea una fecha o una versión, y contestar un número por no haber sabido
    // leerla es peor que dejarlo al modelo.
    if operadores_en_nivel_cero(limpio) > 1 {
        return None;
    }
    let mut it = limpio.chars().peekable();
    let v = expr(&mut it)?;
    // Si queda algo por consumir, no era una expresión limpia.
    if it.peek().is_some() {
        return None;
    }
    Some(formato_numero(v))
}

fn formato_numero(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 && v.abs() < 1e15 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn saltar_espacios<It: Iterator<Item = char>>(it: &mut std::iter::Peekable<It>) {
    while let Some(' ') | Some('\t') = it.peek() {
        it.next();
    }
}

fn expr<It: Iterator<Item = char>>(it: &mut std::iter::Peekable<It>) -> Option<f64> {
    let mut acc = termino(it)?;
    loop {
        saltar_espacios(it);
        match it.peek() {
            Some('+') => {
                it.next()?;
                acc += termino(it)?;
            }
            Some('-') => {
                it.next()?;
                acc -= termino(it)?;
            }
            _ => return Some(acc),
        }
    }
}

fn termino<It: Iterator<Item = char>>(it: &mut std::iter::Peekable<It>) -> Option<f64> {
    let mut acc = factor(it)?;
    loop {
        saltar_espacios(it);
        match it.peek() {
            Some('*') | Some('x') | Some('X') => {
                it.next()?;
                acc *= factor(it)?;
            }
            Some('/') => {
                it.next()?;
                let d = factor(it)?;
                if d == 0.0 {
                    return None;
                }
                acc /= d;
            }
            Some('%') => {
                it.next()?;
                let d = factor(it)?;
                if d == 0.0 {
                    return None;
                }
                acc %= d;
            }
            _ => return Some(acc),
        }
    }
}

fn factor<It: Iterator<Item = char>>(it: &mut std::iter::Peekable<It>) -> Option<f64> {
    saltar_espacios(it);
    match it.peek() {
        Some('(') => {
            it.next()?;
            let v = expr(it)?;
            if it.next()? == ')' {
                Some(v)
            } else {
                None
            }
        }
        Some('-') => {
            it.next()?;
            Some(-factor(it)?)
        }
        Some(c) if c.is_ascii_digit() || *c == '.' => {
            let mut num = String::new();
            while let Some(c) = it.peek() {
                if c.is_ascii_digit() || *c == '.' {
                    num.push(*c);
                    it.next()?;
                } else {
                    break;
                }
            }
            num.parse::<f64>().ok()
        }
        _ => None,
    }
}

/// ¿Esto parece una ruta de archivo? Sin `regex`: patrón de extensión conocida o
/// separador de ruta con nombre.
pub fn parece_ruta(s: &str) -> Option<String> {
    let candidato = s
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| "«»\"'()[]{},.;:!?".contains(c)))
        .find(|w| {
            let tiene_sep = w.contains('/') || w.contains('\\');
            let punto = w.rfind('.').unwrap_or(0);
            // Solo se corta por bytes cuando hay punto: `punto + 1` cae en medio
            // de un carácter multibyte si el token no lleva punto ninguno («¿qué»).
            let (nombre, ext) = if punto > 0 {
                (&w[..punto], &w[punto + 1..])
            } else {
                ("", "")
            };
            // La extensión tiene que tener letras, y el nombre también: sin esto
            // «12.99», «3.11» o «v1.2» pasaban por ruta, el Fast Path pedía leer
            // un archivo inexistente y la salida volvía vacía — el mismo bicho
            // del saludo, por otro camino.
            let tiene_ext = punto > 0
                && ext.len() <= 5
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
                && nombre.chars().any(|c| c.is_ascii_alphabetic());
            (tiene_sep || tiene_ext) && w.chars().count() >= 3 && !w.starts_with('.')
        })?;
    Some(candidato.to_string())
}

/// Cuántos operadores hay fuera de paréntesis. Una expresión con más de uno sin
/// paréntesis alrededor es ambigua: «12/03/2026» es una fecha y «2024-01-15»
/// también, y ninguna de las dos es una cuenta que se pueda resolver en código.
fn operadores_en_nivel_cero(s: &str) -> usize {
    let mut profundidad = 0i32;
    let mut vistos = 0usize;
    let mut anterior_es_operando = false;
    for c in s.chars() {
        match c {
            '(' | '[' => profundidad += 1,
            ')' | ']' => {
                profundidad -= 1;
                anterior_es_operando = true;
            }
            '+' | '-' | '*' | 'x' | 'X' | '/' | '%' => {
                if profundidad == 0 && anterior_es_operando {
                    vistos += 1;
                }
                anterior_es_operando = false;
            }
            '0'..='9' | '.' => anterior_es_operando = true,
            ' ' | '\t' => {}
            _ => anterior_es_operando = false,
        }
    }
    vistos
}

impl Reglas {
    /// El Fast Path de un turno. `Some` = ya se planifica, sin reglas ni modelo.
    pub fn fast_path(&self, message: &str, _mode: &str) -> Option<DecisionResult> {
        let n = normalizar(message);
        if n.is_empty() {
            return None;
        }

        // 1 · Saludos y cortesías: N0, sin tools y con la salida corta. El `reason`
        // lo ve el usuario. Lo que NO hace es saltarse el modelo: saltárselo
        // exige traer la respuesta calculada, y un saludo no la trae — devolverlo
        // vacío era el bicho.
        let solo_saludo = self
            .saludos
            .iter()
            .any(|s| normalizar(s) == n || n.split(' ').all(|w| self.saludos.iter().any(|s| normalizar(s) == w)));
        if solo_saludo && message.chars().count() <= 40 {
            let mut d = DecisionResult::rapido(
                Intent::Ask,
                OutputContract::Texto,
                "fast path · saludo".into(),
            );
            d.skip_generative = false;
            return Some(d);
        }

        // 2 · Aritmética: «20+8» no merece un LLM.
        if let Some(res) = aritmetica(message) {
            let mut d = DecisionResult::rapido(
                Intent::Ask,
                OutputContract::Texto,
                "fast path · aritmética resuelta en código".into(),
            );
            d.salida_directa = Some(res);
            return Some(d);
        }

        // 3 · Lectura directa de un archivo **nombrado**: la ejecuta el producto,
        // acotada por el approval. Es la excepción explícita a «N0 sin tools».
        // Solo con un verbo de los que miran y en un pedido corto: «corrige
        // src/main.rs» tiene ruta y verbo pero es una escritura, y una prosa larga
        // que menciona una ruta no es una orden de leer.
        if let Some(ruta) = parece_ruta(message) {
            let corto = n.split_whitespace().count() <= 6;
            let verbo_de_lectura = self
                .verbos_lectura
                .iter()
                .any(|v| n.contains(&normalizar(v)));
            if corto && (verbo_de_lectura || n == normalizar(&ruta)) {
                let mut d = DecisionResult::rapido(
                    Intent::Search,
                    OutputContract::Texto,
                    format!("fast path · lectura directa de «{ruta}»"),
                );
                d.lectura_directa = Some(ruta);
                return Some(d);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reglas() -> Reglas {
        Reglas::desde_json(
            r#"{"version":1,"saludos":["hola","buenas","gracias","hey"],
                "verbos_accion":["abre","lee","muestra","corrige"],
                "verbos_lectura":["abre","abrir","lee","leer","muestra","enseña","what's","show"],
                "reglas":[]}"#,
        )
        .unwrap()
    }

    #[test]
    fn un_saludo_se_qeda_en_n0_pero_lo_contesta_el_modelo() {
        let d = reglas().fast_path("¡Hola!", "chat").expect("saludo");
        assert_eq!(d.level, Level::N0);
        assert_eq!(d.source, DecisionSource::FastPath);
        assert_eq!(d.confidence, Confidence(1.0));
        assert!(d.tools.is_empty());
        // El atajo decidió la forma, no la respuesta: sin `salida_directa` no
        // puede decir que se salta el modelo, o la burbuja sale vacía.
        assert!(!d.skip_generative);
        assert!(d.salida_directa.is_none());
    }

    #[test]
    fn la_aritmetica_se_resuelve_en_codigo() {
        for (p, r) in [
            ("20+8", "28"),
            ("3 * (2+4)", "18"),
            ("100 / 4", "25"),
            ("2.5 + 2.5", "5"),
            ("7 x 6", "42"),
        ] {
            assert_eq!(aritmetica(p).as_deref(), Some(r), "{p}");
        }
        // Nada de adivinar: si no es una expresión limpia, None.
        assert_eq!(aritmetica("cuánto es 20+8 en binario?"), None);
        assert_eq!(aritmetica("1/0"), None);
        assert_eq!(aritmetica("hola"), None);
        // Dos operadores de nivel cero: es una fecha o una versión, no una cuenta.
        assert_eq!(aritmetica("12/03/2026"), None);
        assert_eq!(aritmetica("2024-01-15"), None);
        assert_eq!(aritmetica("1.2.3"), None);
        // Una resta con negativos sigue siendo una cuenta.
        assert_eq!(aritmetica("-5+3").as_deref(), Some("-2"));
    }

    #[test]
    fn la_lectura_directa_lleva_la_ruta() {
        let d = reglas().fast_path("abre src/main.rs", "work").expect("ruta");
        assert_eq!(d.intent, Intent::Search);
        assert_eq!(d.lectura_directa.as_deref(), Some("src/main.rs"));
        assert!(d.skip_generative);
        // Y no es una tool que llame el modelo: el Plan no lleva tools.
        assert!(d.tools.is_empty());
    }

    #[test]
    fn un_numero_no_es_un_archivo() {
        // «muestra 12.99» se leía como una ruta, el Fast Path pedía leer un
        // archivo que no existe y la salida volvía vacía.
        for s in ["muestra 12.99", "peso 3.11 kg", "versión v1.2", "12.99"] {
            assert_eq!(parece_ruta(s), None, "{s}");
        }
        // Sin punto en el token no se corta por bytes: «¿» ocupa dos y el corte
        // caía en medio del carácter. Pánico, no un `None`.
        assert_eq!(parece_ruta("¿qué tal vas?"), None);
        for s in ["abre main.rs", "mira src/lib/rs", "config.toml", "léeme.md"] {
            assert!(parece_ruta(s).is_some(), "{s}");
        }
    }

    #[test]
    fn una_orden_de_escribir_no_es_una_lectura() {
        // El verbo está en `verbos_accion` pero no en `verbos_lectura`: esto tiene
        // que llegar al Engine y firmar un Plan de trabajo, no leer el archivo y
        // contestar con su contenido.
        for m in ["corrige src/main.rs", "fix src/main.rs", "borra src/main.rs"] {
            let d = reglas().fast_path(m, "work");
            let es_lectura = d.as_ref().map(|x| x.lectura_directa.is_some()).unwrap_or(false);
            assert!(!es_lectura, "«{m}» se lo tomó el Fast Path como una lectura: {d:?}");
        }
    }

    #[test]
    fn no_inventa_lecturas_en_prosa_larga() {
        let d = reglas().fast_path(
            "explícame por qué este proyecto tarda tanto y qué cambiarías de src/main.rs y de todo lo demás que rodea el sistema",
            "work",
        );
        assert!(d.is_none(), "{d:?}");
    }

    #[test]
    fn un_fast_path_nunca_se_infla() {
        let d = reglas().fast_path("gracias", "chat").unwrap().con_suelo_de_riesgo();
        assert_eq!(d.level, Level::N0);
    }
}
