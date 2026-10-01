//! Carga de los archivos de config. El crate no busca en rutas de sistema: el
//! producto le dice dónde están, y aquí solo se lee, se valida y se reporta.

use super::schema::ConfigError;
use crate::decision::rules::Reglas;
use crate::models::Registry;
use crate::tools::Herramientas;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Cargada {
    pub reglas: Reglas,
    pub registry: Registry,
    pub herramientas: Herramientas,
    /// Qué archivos se encontraron de verdad. El panel «por qué» lo necesita:
    /// decir «reglas activas» cuando no había archivo sería mentira.
    pub leidos: Vec<PathBuf>,
    pub ausentes: Vec<String>,
    /// `true` si `models.json` no estaba y se usó `models.example.json`. El
    /// Governor puede trabajar con eso, pero el producto tiene que decir que las
    /// cifras no son de **esta** máquina: es un ejemplo, no una medición.
    pub models_desde_ejemplo: bool,
}

impl Cargada {
    /// Todo opcional: sin `models.json` se prueba con `models.example.json` y se
    /// deja constancia (`models_desde_ejemplo`). Si tampoco hay ejemplo, no hay
    /// Selector, y `Brain::nuevo` lo convierte en `ConfigError::SinBackend`.
    pub fn leer(dir: Option<&Path>) -> Result<Cargada, ConfigError> {
        let mut c = Cargada {
            ..Default::default()
        };
        let Some(dir) = dir else {
            c.ausentes.push("dir_config".into());
            return Ok(c);
        };
        c.reglas = Reglas::desde_json(&texto(&dir.join("brain-rules.json"), &mut c)?)
            .map_err(|e| ConfigError::Json {
                archivo: "brain-rules.json".into(),
                causa: e.to_string(),
            })?;
        c.herramientas = Herramientas::desde_json(&texto(&dir.join("tools.json"), &mut c)?)
            .map_err(|e| ConfigError::Json {
                archivo: "tools.json".into(),
                causa: e.to_string(),
            })?;
        let modelos = texto(&dir.join("models.json"), &mut c)?;
        let mut reg: Registry =
            Registry::desde_json(&modelos).map_err(|e| ConfigError::Json {
                archivo: "models.json".into(),
                causa: e.to_string(),
            })?;
        if reg.modelos.is_empty() {
            // El absence de `models.json` ya quedó apuntado en `ausentes`; ahora
            // se intenta el ejemplo versionado.
            let ej = texto(&dir.join("models.example.json"), &mut c)?;
            reg = Registry::desde_json(&ej).map_err(|e| ConfigError::Json {
                archivo: "models.example.json".into(),
                causa: e.to_string(),
            })?;
            c.models_desde_ejemplo = !reg.modelos.is_empty();
        }
        c.registry = reg;
        Ok(c)
    }

    /// Un `models.example.json` vale para arrancar y para los tests; no para
    /// afirmar que un modelo cabe en esta máquina.
    pub fn desde_ejemplo(texto_models: &str) -> Result<Cargada, ConfigError> {
        let mut c = Cargada {
            ..Default::default()
        };
        c.registry = Registry::desde_json(texto_models).map_err(|e| ConfigError::Json {
            archivo: "models.example.json".into(),
            causa: e.to_string(),
        })?;
        Ok(c)
    }
}

/// Lee un archivo opcional. Si no está, se apunta en `ausentes` y se devuelve el
/// JSON vacío equivalente (`{}` / `[]`), nunca un panic.
fn texto(ruta: &Path, c: &mut Cargada) -> Result<String, ConfigError> {
    match std::fs::read_to_string(ruta) {
        Ok(t) => {
            c.leidos.push(ruta.to_path_buf());
            Ok(t)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            c.ausentes.push(ruta.display().to_string());
            // `brain-rules.json` vacío = cero reglas = todo por heurística.
            if ruta.extension().map(|x| x == "json").unwrap_or(false) {
                return Ok(if ruta.file_stem().map(|s| s == "tools").unwrap_or(false) {
                    r#"{"version":1,"tools":[]}"#.into()
                } else if ruta.file_stem().map(|s| s == "models").unwrap_or(false) {
                    r#"{"modelos":[]}"#.into()
                } else {
                    r#"{"version":1}"#.into()
                });
            }
            Err(ConfigError::Leyendo {
                archivo: ruta.display().to_string(),
                causa: e.to_string(),
            })
        }
        Err(e) => Err(ConfigError::Leyendo {
            archivo: ruta.display().to_string(),
            causa: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(nombre: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("hatboo-brain-cfg-{}-{nombre}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    }

    #[test]
    fn sin_directorio_no_hay_nada_y_no_falla() {
        let c = Cargada::leer(None).unwrap();
        assert!(c.reglas.reglas.is_empty());
        assert_eq!(c.ausentes, vec!["dir_config".to_string()]);
    }

    #[test]
    fn archivos_ausentes_se_declaran() {
        let d = tmpdir("vacio");
        let c = Cargada::leer(Some(&d)).unwrap();
        assert_eq!(c.leidos.len(), 0);
        // Cuatro, no tres: sin `models.json` se busca también el ejemplo.
        assert_eq!(c.ausentes.len(), 4, "{:?}", c.ausentes);
        assert!(c.ausentes.iter().any(|a| a.ends_with("models.json")));
        assert!(c.ausentes.iter().any(|a| a.ends_with("models.example.json")));
        assert!(c.registry.modelos.is_empty());
        assert!(!c.models_desde_ejemplo);
    }

    /// Sin `models.json` el ejemplo versionado sirve para arrancar, pero queda
    /// marcado: decir «cabe» con las cifras de otra máquina sería una mentira.
    #[test]
    fn sin_models_json_se_usa_el_ejemplo_y_se_dice() {
        let d = tmpdir("ejemplo");
        std::fs::write(
            d.join("models.example.json"),
            r#"{"modelos":[{"id":"gemma3:1b","provider":"ollama","local":true,"kind":"generative","profile":"nano","tier":1,"ram_mb_by_ctx":{"2048":878},"max_ctx":32768}]}"#,
        )
        .unwrap();
        let c = Cargada::leer(Some(&d)).unwrap();
        assert_eq!(c.registry.modelos.len(), 1);
        assert!(c.models_desde_ejemplo, "hay que poder decirlo en el panel");
        assert!(c.ausentes.iter().any(|a| a.ends_with("models.json")));

        // Y con un `models.json` propio, la bandera vuelve a `false`.
        std::fs::write(
            d.join("models.json"),
            std::fs::read_to_string(d.join("models.example.json")).unwrap(),
        )
        .unwrap();
        let con = Cargada::leer(Some(&d)).unwrap();
        assert_eq!(con.registry.modelos.len(), 1);
        assert!(!con.models_desde_ejemplo);
    }

    #[test]
    fn lee_los_tres_cuando_estan() {        let d = tmpdir("lleno");
        std::fs::write(
            d.join("brain-rules.json"),
            r#"{"version":1,"saludos":["hola"],"reglas":[]}"#,
        )
        .unwrap();
        std::fs::write(
            d.join("tools.json"),
            r#"{"version":1,"tools":[{"id":"read_file","escribe":false}]}"#,
        )
        .unwrap();
        std::fs::write(
            d.join("models.json"),
            r#"{"modelos":[{"id":"gemma3:1b","provider":"ollama","local":true,"kind":"generative","profile":"nano","tier":1,"ram_mb_by_ctx":{"2048":878},"max_ctx":32768,"supports_tools":false,"supports_thinking":false}]}"#,
        )
        .unwrap();
        let c = Cargada::leer(Some(&d)).unwrap();
        assert_eq!(c.leidos.len(), 3);
        assert_eq!(c.registry.modelos.len(), 1);
        assert_eq!(c.herramientas.disponibles().len(), 1);
    }

    #[test]
    fn un_json_roto_dice_cual() {
        let d = tmpdir("roto");
        std::fs::write(d.join("brain-rules.json"), "{ esto no es json").unwrap();
        let e = Cargada::leer(Some(&d)).unwrap_err();
        assert!(e.mensaje().contains("brain-rules.json"), "{e}");
    }

    /// Los JSON que viajan en el repo tienen que parsear: se rompen al cambiar
    /// una señal de nombre y ningún otro test los lee.
    #[test]
    fn los_configs_del_repo_cargan() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
        let leer = |n: &str| -> String {
            let p = dir.join(n);
            assert!(p.exists(), "falta {}", p.display());
            std::fs::read_to_string(&p).unwrap()
        };

        let r = Reglas::desde_json(&leer("brain-rules.json"))
            .unwrap_or_else(|e| panic!("brain-rules.json: {e}"));
        assert!(!r.reglas.is_empty(), "el config versionado trae reglas");
        assert!(!r.verbos_accion.is_empty() && !r.riesgo_alto.is_empty());
        let req = crate::api::request::BrainRequest::nuevo("hatboo", "work", "hola");
        let sondeo = crate::decision::engine::senales(&req, &r);
        for re in &r.reglas {
            for c in &re.cuando {
                assert!(
                    sondeo.valor(&c.senal).is_some(),
                    "regla «{}» pide la señal inexistente «{}»",
                    re.id,
                    c.senal
                );
                assert!(
                    crate::decision::rules::Operador::desde(&c.op).is_some(),
                    "regla «{}» usa el operador «{}»",
                    re.id,
                    c.op
                );
            }
        }

        let h = Herramientas::desde_json(&leer("tools.json")).unwrap();
        assert!(h.escribe("write_file") && h.escribe("run_command"));
        assert!(!h.escribe("read_file") && !h.escribe("git_status"));
        for t in h.disponibles() {
            assert_eq!(
                t.argumentos["type"], "object",
                "el schema de «{}» no declara un objeto",
                t.id
            );
        }

        let reg = Registry::desde_json(&leer("models.example.json")).unwrap();
        assert!(!reg.modelos.is_empty());
        for m in &reg.modelos {
            assert!(
                m.ram_para(2048).is_some(),
                "«{}» sin RAM a 2048: el Governor no podría afirmar que cabe",
                m.id
            );
        }
    }

    /// El recorrido de referencia del plan (§12) tiene que caer en N2 con tools
    /// de lectura y escritura usando solo el config versionado.
    #[test]
    fn el_recorrido_de_referencia_se_planifica_solo() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
        let c = Cargada::leer(Some(&dir)).unwrap();
        let mut motor = crate::decision::engine::Motor::nuevo(c.reglas.clone());
        let req =
            crate::api::request::BrainRequest::nuevo(
                "hatboo",
                "work",
                "Corrige el error de compilación de src/main.rs",
            )
            .con_tools(c.herramientas.disponibles().iter().map(|t| t.info()).collect());
        let d = motor.evaluar(&req);
        assert_eq!(d.level, crate::api::vocab::Level::N2, "{:?}", d.por_que);
        assert!(d.tools.iter().any(|t| t == "write_file"), "{:?}", d.tools);
        assert_eq!(
            d.output_contract,
            crate::api::vocab::OutputContract::Patch
        );
    }
}
