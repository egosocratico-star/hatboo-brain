//! §15.7: quién dice cómo se verifica un proyecto. El Brain no lee el disco —el
//! que lo lee es el producto, que es el que tiene el sandbox y los permisos—, así
//! que aquí se trabaja sobre la **lista de rutas** que el producto ya tiene
//! (la del árbol de Archivos). Detectar no es ejecutar: lo que sale de aquí es
//! una propuesta con su nombre de comando, y se corre o se pregunta según el
//! nivel y la aprobación.

use crate::api::request::VerifyCommands;

/// Los hechos de verificación del proyecto, deducidos de sus rutas.
///
/// Con varios ecosistemas gana el primero en este orden —`rust`, `node`,
/// `python`— porque `VerifyCommands` tiene **un** hueco por acción y elegir
/// mezcla menos mentira que dejar los otros tres vacíos. El criterio se llama
/// aquí, no se esconde: `mande` dice cuál ganó.
pub fn project_facts(archivos: &[String]) -> VerifyCommands {
    let raiz: Vec<String> = archivos
        .iter()
        .map(|a| a.replace('\\', "/").to_lowercase())
        .filter(|a| !a.contains("/node_modules/") && !a.starts_with("node_modules/"))
        .map(|a| a.trim_start_matches("./").to_string())
        .collect();
    let tiene = |n: &str| raiz.iter().any(|a| a == n);
    let algun_nombre = |patrones: &[&str]| {
        raiz.iter().any(|a| {
            let nombre = a.rsplit('/').next().unwrap_or(a);
            patrones.iter().any(|p| nombre.starts_with(p))
        })
    };

    if tiene("cargo.toml") {
        return VerifyCommands {
            check: Some("cargo check --all-targets".into()),
            lint: Some("cargo clippy --all-targets -- -D warnings".into()),
            test: Some("cargo test --no-fail-fast".into()),
        };
    }
    if tiene("package.json") {
        // El `lint` solo se propone si hay config de eslint a la vista: declarar
        // un comando que el proyecto no tiene es justo el ruido que §15.7 evita.
        let lint = algun_nombre(&["eslint.config", ".eslintrc"]).then(|| "npx eslint .".to_string());
        let check = tiene("tsconfig.json").then(|| "npx tsc --noEmit".to_string());
        return VerifyCommands {
            check,
            lint,
            test: Some("npm test".into()),
        };
    }
    let py = tiene("pyproject.toml") || tiene("requirements.txt");
    let tiene_tests = raiz.iter().any(|a| {
        a.starts_with("tests/") || a.ends_with(".pytest") || a.ends_with("conftest.py")
    }) || algun_nombre(&["pytest.ini", "tox.ini"]);
    if py && tiene_tests {
        return VerifyCommands {
            check: None,
            lint: None,
            test: Some("pytest".into()),
        };
    }
    VerifyCommands::default()
}

/// Qué ecosistema ganó en `project_facts`, para el `reason` y el inspector: sin
/// esto el producto no puede decir por qué propuso `cargo test` y no `npm test`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosistema {
    Rust,
    Node,
    Python,
    Ninguno,
}

/// El ecosistema que gana, con el mismo orden que `project_facts`.
pub fn ecosistema_que_manda(archivos: &[String]) -> Ecosistema {
    match project_facts(archivos).alguno() {
        false => Ecosistema::Ninguno,
        true => {
            let raiz: Vec<String> = archivos
                .iter()
                .map(|a| a.replace('\\', "/").to_lowercase())
                .filter(|a| !a.contains("node_modules/"))
                .map(|a| a.trim_start_matches("./").to_string())
                .collect();
            let tiene = |n: &str| raiz.iter().any(|a| a == n);
            if tiene("cargo.toml") {
                Ecosistema::Rust
            } else if tiene("package.json") {
                Ecosistema::Node
            } else {
                Ecosistema::Python
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn un_proyecto_rust_declara_los_tres() {
        let v = project_facts(&r(&["Cargo.toml", "src/lib.rs", "tests/x.rs"]));
        assert_eq!(v.check.as_deref(), Some("cargo check --all-targets"));
        assert_eq!(v.lint.as_deref(), Some("cargo clippy --all-targets -- -D warnings"));
        assert_eq!(v.test.as_deref(), Some("cargo test --no-fail-fast"));
        assert_eq!(ecosistema_que_manda(&r(&["Cargo.toml"])), Ecosistema::Rust);
    }

    #[test]
    fn sin_config_de_lint_no_se_inventa_un_comando_de_lint() {
        let v = project_facts(&r(&["package.json", "src/app.js"]));
        assert_eq!(v.test.as_deref(), Some("npm test"));
        assert!(v.lint.is_none(), "no hay eslint: no se propone lint {:?}", v.lint);
        assert!(v.check.is_none(), "no hay tsconfig: no se propone check");
        let con_todo = project_facts(&r(&["package.json", "tsconfig.json", "eslint.config.mjs"]));
        assert_eq!(con_todo.lint.as_deref(), Some("npx eslint ."));
        assert_eq!(con_todo.check.as_deref(), Some("npx tsc --noEmit"));
    }

    #[test]
    fn node_no_entra_si_solo_hay_una_carpeta_de_dependencias() {
        // `node_modules/algo/package.json` no es un proyecto de Node propio.
        let v = project_facts(&r(&["src/main.rs", "node_modules/left-pad/package.json"]));
        assert!(!v.alguno(), "{v:?}");
    }

    #[test]
    fn rust_gana_en_un_poliglota_y_lo_dice() {
        let archivos = r(&["Cargo.toml", "package.json", "web/app.ts"]);
        assert_eq!(ecosistema_que_manda(&archivos), Ecosistema::Rust);
        assert!(project_facts(&archivos).test.unwrap().starts_with("cargo"));
    }

    #[test]
    fn sin_tests_no_hay_comando_que_finge() {
        let v = project_facts(&r(&["README.md", "docs/nota.md"]));
        assert!(!v.alguno(), "{v:?}");
        assert_eq!(ecosistema_que_manda(&r(&["README.md"])), Ecosistema::Ninguno);
        // Python sin nada que ejecutar tampoco.
        assert!(!project_facts(&r(&["pyproject.toml", "src/app.py"])).alguno());
        assert_eq!(
            project_facts(&r(&["pyproject.toml", "tests/test_app.py"])).test.as_deref(),
            Some("pytest")
        );
    }
}
