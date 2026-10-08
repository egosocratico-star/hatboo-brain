//! `brain-bench` — el arnés de §9 del plan. Mide el Brain contra una línea base
//! con la misma máquina, el mismo Ollama y los mismos prompts.
//!
//! Dos modos:
//! - por defecto: suite → Brain → `check` determinista → tabla con mediana y rango.
//! - `--sondear`: carga cada modelo local escalón por escalón, lee la RAM
//!   residente de `/api/ps` y prueba una tool call y un `think` **reales**. Escribe
//!   `models.sondeado.json` con lo medido, que es lo único que debe entrar al
//!   registry (§X del Canon: se guarda lo medido, no lo declarado).
//!
//! Regla de la casa: lo que no se pudo medir se dice sin medir. Un `Unverifiable`
//! no se suma a los aciertos y una RAM sin medida no se inventa.

use async_trait::async_trait;
use hatboo_brain::api::request::{BrainRequest, ProjectContext, ToolInfo, VerifyCommands};
use hatboo_brain::api::vocab::{
    ApprovalLevel, ExecutionPolicy, KeepAlive, Level, Risk, ThinkingLevel,
};
use hatboo_brain::brain::{Brain, EjecutarTool, Montaje};
use hatboo_brain::config::loader::Cargada;
use hatboo_brain::config::schema::BrainConfig;
use hatboo_brain::models::{ModelInfo, ModeloCargado, Registry};
use hatboo_brain::prompt::{build_system, Estimador, Identidad};
use hatboo_brain::providers::{GenerationRequest, ModelProvider, OllamaProvider, ToolSchema};
use hatboo_brain::resources::ResourceProbe;
use hatboo_brain::verification::execution::{Ejecutor, Salida};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ─────────────────────────────── la suite ───────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
struct Check {
    #[serde(rename = "type")]
    tipo: String,
    #[serde(default)]
    cmd: String,
    /// `exit0` | `exit1` para `type: command`.
    #[serde(default)]
    expect: String,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    exact: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Entrada {
    id: String,
    #[serde(default)]
    lang: String,
    /// `saludo` | `codigo` | `archivo` | `riesgo` | `tool` | `ambiguedad`
    categoria: String,
    /// `dev` o `validacion`: §9 pide 70/30 y la puerta de Fase 5 se mide en validación.
    #[serde(default = "por_defecto_dev")]
    split: String,
    /// Lo que el Engine debería decidir. Mide la precisión del §3, no del modelo.
    #[serde(default, alias = "level_expected")]
    nivel_esperado: Option<Level>,
    /// Riesgo que debería salir. Sin él, un 100 % de nivel puede estar llamando
    /// `Low` a volcar una clave de API en un README compartido.
    #[serde(default, alias = "risk_expected")]
    riesgo_esperado: Option<Risk>,
    prompt: String,
    #[serde(default)]
    fixture: Option<String>,
    #[serde(default)]
    check: Option<Check>,
}

fn por_defecto_dev() -> String {
    "dev".into()
}

#[derive(Debug, serde::Deserialize)]
struct Suite {
    #[serde(default, alias = "prompts")]
    entradas: Vec<Entrada>,
}

// ─────────────────────────────── opciones ───────────────────────────────

struct Opciones {
    sondear: bool,
    /// Solo el Engine, sin modelo: mide la toma de decisiones sobre la suite.
    solo_decisiones: bool,
    suite: PathBuf,
    config: PathBuf,
    modelos: Vec<String>,
    reps: u32,
    /// `brain` o `baseline`.
    modo: String,
    split: String,
    solo_categoria: Option<String>,
    limite_salida: u32,
    contexto: Option<u32>,
    /// Margen de RAM libre del Governor, para medir en máquinas de 8 GB donde
    /// los 1500 MB de §11 no dejan cargar nada. No cambia el producto.
    margen: Option<u64>,
    salida: Option<PathBuf>,
    /// `--comparar A B`: los dos volcados de `--salida` (baseline y brain) que se
    /// enfrentan con las reglas de §9. No necesita config ni modelo.
    comparar: Option<(PathBuf, PathBuf)>,
    /// Donde se escribe el informe en markdown del `--comparar`.
    informe: Option<PathBuf>,
}

impl Default for Opciones {
    fn default() -> Self {
        Opciones {
            sondear: false,
            solo_decisiones: false,
            suite: PathBuf::from("benchmarks/base.json"),
            config: PathBuf::from("config"),
            modelos: Vec::new(),
            reps: 3,
            modo: "brain".into(),
            split: "dev".into(),
            solo_categoria: None,
            limite_salida: 1024,
            contexto: None,
            margen: None,
            salida: None,
            comparar: None,
            informe: None,
        }
    }
}

const AYUDA: &str = "\
brain-bench — arnés de medición del Brain (§9)

  --suite RUTA        base.json con las entradas  (default benchmarks/base.json)
  --config DIR        de dónde se lee brain-rules.json / tools.json / models.json
  --modelo X          repitable; sin ninguno se toma el primer local del registry
  --reps N            repeticiones por entrada (default 3; se reporta mediana y rango)
  --modo brain|baseline
  --split dev|validacion|todos
  --categoria N       correr solo una categoría
  --toks N            tope de salida en tokens (default 1024)
  --ctx N             forzar num_ctx en vez de dejárselo al Governor
  --margen N          MB libres que exige el Governor (su valor por defecto son
                      1500, que en un portátil de 8 GB no deja cargar nada)
  --salida RUTA       volcar los datos crudos en JSON
  --sondear           medir RAM por ctx y probar tools, thinking y salida
                      estructurada; escribe models.sondeado.json
  --decisiones        solo el Engine sobre la suite: precisión de nivel, riesgo,
                      contrato, tools y presupuesto firmado. Sin modelo, sin RAM.
  --comparar A B      enfrenta dos volcados de --salida (el de `baseline` y el de
                      `brain`) con las reglas de §9: medianas, umbral de regresión
                      del 5 % y por qué a veces no se puede afirmar nada.
  --informe RUTA      con --comparar, escribe el veredicto en markdown ahí.
";

fn parseo() -> Result<Opciones, String> {
    let mut o = Opciones::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    // Cada opción con valor se escribe a mano: un error de tipeo tiene que decir
    // cuál, no comerse el argumento siguiente.
    macro_rules! val {
        ($k:expr) => {{
            let v = args
                .get(i + 1)
                .cloned()
                .ok_or_else(|| format!("falta el valor de {}", $k))?;
            i += 1;
            v
        }};
    }
    macro_rules! num {
        ($k:expr, $t:ty) => {{
            let v = val!($k);
            v.parse::<$t>().map_err(|e| format!("{}: {e}", $k))?
        }};
    }
    while i < args.len() {
        match args[i].as_str() {
            "--sondear" => o.sondear = true,
            "--suite" => {
                let v = val!("--suite");
                o.suite = PathBuf::from(v);
            }
            "--config" => {
                let v = val!("--config");
                o.config = PathBuf::from(v);
            }
            "--modelo" => {
                let v = val!("--modelo");
                if o.modelos.iter().any(|x| x == &v) {
                    return Err(format!("«{v}» está repetido: --modelo cuenta una vez por modelo"));
                }
                o.modelos.push(v);
            }
            "--reps" => o.reps = num!("--reps", u32),
            "--modo" => {
                o.modo = val!("--modo");
                if !["brain", "baseline"].contains(&o.modo.as_str()) {
                    return Err(format!("--modo ha de ser brain|baseline, no «{}»", o.modo));
                }
            }
            "--split" => {
                o.split = val!("--split");
                if !["dev", "validacion", "todos"].contains(&o.split.as_str()) {
                    return Err("--split ha de ser dev|validacion|todos".into());
                }
            }
            "--categoria" => o.solo_categoria = Some(val!("--categoria")),
            "--toks" => o.limite_salida = num!("--toks", u32),
            "--ctx" => o.contexto = Some(num!("--ctx", u32)),
            "--margen" => o.margen = Some(num!("--margen", u64)),
            "--decisiones" => o.solo_decisiones = true,
            "--salida" => {
                let v = val!("--salida");
                o.salida = Some(PathBuf::from(v));
            }
            "--comparar" => {
                let a = val!("--comparar");
                let b = val!("segundo fichero de --comparar");
                o.comparar = Some((PathBuf::from(a), PathBuf::from(b)));
            }
            "--informe" => {
                let v = val!("--informe");
                o.informe = Some(PathBuf::from(v));
            }
            "--ayuda" | "-h" => return Err("AYUDA".into()),
            otro => return Err(format!("opción desconocida «{otro}»")),
        }
        i += 1;
    }
    if o.reps == 0 {
        return Err("--reps ha de ser al menos 1".into());
    }
    if o.limite_salida < 16 {
        return Err("--toks menor de 16 tokens no mide nada".into());
    }
    Ok(o)
}

// ─────────────────────────── RAM libre, medida ───────────────────────────

/// RAM libre del sistema operativo, no la que declara `/api/ps`. Medido en
/// Fase 0: gemma3:1b declaraba 878 MB residentes y a la máquina le faltaban
/// 1,92 GB, así que la cifra que manda para decidir es la del SO.
///
/// El refresco va en un hilo aparte: una consulta cuesta ~0,5 s y el bucle de
/// medición no puede esperar eso entre prompts.
struct SondaReal {
    libre: Arc<AtomicU64>,
    cargados: Arc<Mutex<Vec<ModeloCargado>>>,
}

impl SondaReal {
    fn arrancar() -> SondaReal {
        let libre = Arc::new(AtomicU64::new(0));
        let copia = libre.clone();
        std::thread::spawn(move || loop {
            if let Some(mb) = libre_del_sistema() {
                copia.store(mb, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_secs(1));
        });
        let s = SondaReal {
            libre,
            cargados: Arc::new(Mutex::new(Vec::new())),
        };
        // La primera lectura cuesta medio segundo, y el Fast Path no gasta ni un
        // milisegundo: sin esperar, la primera entrada que sí necesita modelo
        // llegaría con la sonda a cero, el Governor diría «no afirma que quepa»
        // y el arnés se pararía solo por culpa de la carrera.
        for _ in 0..60 {
            if s.libre_mb().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        s
    }

    fn guarda_cargados(&self, v: Vec<ModeloCargado>) {
        if let Ok(mut g) = self.cargados.lock() {
            *g = v;
        }
    }

    fn ram_de(&self, modelo: &str) -> Option<u64> {
        self.cargados
            .lock()
            .ok()
            .and_then(|v| v.iter().find(|m| m.id == modelo).map(|m| m.ram_mb))
    }
}

impl ResourceProbe for SondaReal {
    fn libre_mb(&self) -> Option<u64> {
        match self.libre.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    fn cargados(&self) -> Vec<ModeloCargado> {
        self.cargados.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

#[cfg(windows)]
fn libre_del_sistema() -> Option<u64> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // KiB → MB.
    t.parse::<u64>().ok().map(|kib| kib / 1024)
}

#[cfg(not(windows))]
fn libre_del_sistema() -> Option<u64> {
    let f = std::fs::read_to_string("/proc/meminfo").ok()?;
    let linea = f.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = linea
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb / 1024)
}

// ─────────────────────────── ejecutores del arnés ───────────────────────────

/// Corre un comando con timeout. La salida va a un archivo temporal en vez de a
/// una tubería: con la tubería llena, esperar con `try_wait` en bucle se muere de
/// espera mutua y el bench colgaría sin decir por qué.
struct ComandoReal {
    contador: AtomicU64,
}

impl Default for ComandoReal {
    fn default() -> Self {
        ComandoReal {
            contador: AtomicU64::new(0),
        }
    }
}

impl Ejecutor for ComandoReal {
    fn correr(&self, comando: &str, root: &str, timeout_s: u32) -> Result<Salida, String> {
        let inicio = Instant::now();
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", comando]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", comando]);
            c
        };
        if !root.is_empty() {
            cmd.current_dir(root);
        }
        let n = self.contador.fetch_add(1, Ordering::Relaxed);
        let ruta_out = std::env::temp_dir().join(format!("brain-bench-out-{n}.log"));
        let ruta_err = std::env::temp_dir().join(format!("brain-bench-err-{n}.log"));
        cmd.stdout(Stdio::from(
            std::fs::File::create(&ruta_out).map_err(|e| e.to_string())?,
        ));
        cmd.stderr(Stdio::from(
            std::fs::File::create(&ruta_err).map_err(|e| e.to_string())?,
        ));
        let hijo = Arc::new(Mutex::new(
            cmd.spawn()
                .map_err(|e| format!("no se pudo lanzar «{comando}»: {e}"))?,
        ));
        let limite = Duration::from_secs(timeout_s.max(1) as u64);
        let estado = loop {
            let mut g = hijo.lock().map_err(|_| "candado roto".to_string())?;
            match g.try_wait().map_err(|e| e.to_string())? {
                Some(s) => break s,
                None => {
                    if inicio.elapsed() > limite {
                        let _ = g.kill();
                        let _ = g.wait();
                        drop(g);
                        return Err(format!("timeout de {timeout_s} s: «{comando}» no paró"));
                    }
                    drop(g);
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        };
        let leer = |p: &Path| -> String {
            let mut s = String::new();
            if let Ok(mut f) = std::fs::File::open(p) {
                let _ = f.read_to_string(&mut s);
            }
            let _ = std::fs::remove_file(p);
            // Un compilador puede escupir no-UTF8: se queda lo legible.
            s.chars().take(8000).collect()
        };
        Ok(Salida {
            codigo: estado.code().unwrap_or(-1),
            stdout: leer(&ruta_out),
            stderr: leer(&ruta_err),
            dur_ms: inicio.elapsed().as_millis() as u64,
        })
    }
}

/// La raíz limpia de los prefijos `a/` y `b/` de un diff, con `..` resueltos sin
/// tocar el disco y la comprobación de que no sale de la raíz.
fn ruta_de(raiz: &Path, ruta: &str) -> Option<PathBuf> {
    let sin_prefijo = ruta
        .strip_prefix("a/")
        .or_else(|| ruta.strip_prefix("b/"))
        .unwrap_or(ruta);
    if Path::new(sin_prefijo).is_absolute() {
        // Una ruta absoluta solo vale si ya está dentro de la raíz.
        let p = PathBuf::from(sin_prefijo);
        return p.starts_with(raiz).then_some(p);
    }
    let mut pila: Vec<&std::ffi::OsStr> = Vec::new();
    for c in Path::new(sin_prefijo).components() {
        match c {
            Component::Normal(s) => pila.push(s),
            Component::CurDir => {}
            // Un `..` solo puede deshacer un tramo ya pisado: si la pila está
            // vacía, es que quiere salir de la raíz, y eso se rechaza en código.
            Component::ParentDir => {
                pila.pop()?;
            }
            _ => return None,
        }
    }
    let mut limpio = raiz.to_path_buf();
    for s in pila {
        limpio.push(s);
    }
    Some(limpio)
}

/// Lo que el arnés ejecuta por el producto: las tools del fixture, con las rutas
/// cerradas dentro de él. Un `write_file` que salga de la carpeta se rechaza
/// aquí, en código — la defensa dura no está en el prompt.
struct ToolsDelArnies {
    raiz: PathBuf,
    ejecutor: Arc<dyn Ejecutor>,
}

#[async_trait]
impl EjecutarTool for ToolsDelArnies {
    async fn ejecutar(&self, tool: &String, args: &serde_json::Value) -> Result<String, String> {
        let raiz = self.raiz.to_string_lossy().to_string();
        match tool.as_str() {
            "read_file" => {
                let ruta = args["path"].as_str().ok_or("read_file sin «path»")?;
                let p = ruta_de(&self.raiz, ruta).ok_or(format!("«{ruta}» sale del proyecto"))?;
                std::fs::read_to_string(&p).map_err(|e| format!("no se pudo leer {ruta}: {e}"))
            }
            "write_file" => {
                let ruta = args["path"].as_str().ok_or("write_file sin «path»")?;
                let contenido = args["content"].as_str().ok_or("write_file sin «content»")?;
                let p = ruta_de(&self.raiz, ruta).ok_or(format!("«{ruta}» sale del proyecto"))?;
                if let Some(padre) = p.parent() {
                    let _ = std::fs::create_dir_all(padre);
                }
                std::fs::write(&p, contenido)
                    .map_err(|e| format!("no se pudo escribir {ruta}: {e}"))?;
                Ok(format!("escrito {ruta} ({} bytes)", contenido.len()))
            }
            "run_command" => {
                let cmd = args["command"].as_str().ok_or("run_command sin «command»")?;
                let t = args["timeout"].as_u64().unwrap_or(60).min(300) as u32;
                let s = self.ejecutor.correr(cmd, &raiz, t)?;
                let mezcla = format!("{}{}", s.stdout, s.stderr);
                if s.codigo == 0 {
                    Ok(format!("{mezla}\n[exit 0]", mezla = recorta(&mezcla)))
                } else {
                    Err(format!("[exit {}] {}", s.codigo, recorta(&mezcla)))
                }
            }
            "list_dir" => {
                let ruta = args["path"].as_str().unwrap_or(".");
                let p = ruta_de(&self.raiz, ruta).ok_or("ruta fuera del proyecto")?;
                let mut v: Vec<String> = Vec::new();
                for e in std::fs::read_dir(&p).map_err(|e| format!("no se pudo listar: {e}"))? {
                    let e = e.map_err(|e| e.to_string())?;
                    let nombre = e.file_name().to_string_lossy().to_string();
                    v.push(if e.path().is_dir() {
                        format!("{nombre}/")
                    } else {
                        nombre
                    });
                }
                v.sort();
                Ok(v.join("\n"))
            }
            otra => Err(format!(
                "el arnés no ejecuta «{otra}»: esta máquina de prueba no la tiene"
            )),
        }
    }

    /// El arnés no pide aprobación: corre sobre una copia del fixture. La
    /// aprobación es del producto y ahí es donde se prueba (§10, policy × approval).
    fn requiere_aprobacion(&self, _tool: &String, _args: &serde_json::Value) -> bool {
        false
    }
}

fn recorta(s: &str) -> String {
    s.trim().chars().take(4000).collect::<String>().replace('\n', " · ")
}

// ─────────────────────────────── el fixture ───────────────────────────────

fn copia_fixture(origen: &Path, destino: &Path) -> Result<(), String> {
    std::fs::create_dir_all(destino).map_err(|e| e.to_string())?;
    let mut pendientes = vec![origen.to_path_buf()];
    while let Some(dir) = pendientes.pop() {
        for e in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            let nombre = e.file_name();
            // `target/`, `node_modules/` y `.git/` de un fixture no se copian:
            // pesan gigas y no cambian el resultado de un `cargo check`.
            if ["target", "node_modules", ".git"].iter().any(|x| nombre == *x) {
                continue;
            }
            let adentro = destino.join(nombre);
            if e.path().is_dir() {
                std::fs::create_dir_all(&adentro).map_err(|e| e.to_string())?;
                pendientes.push(e.path());
            } else {
                std::fs::copy(e.path(), &adentro).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

enum Resultado {
    Pasa,
    Falla(String),
    /// No se pudo comprobar: nunca cuenta como acierto.
    SinComprobar(String),
}

fn comprobar_entrada(
    e: &Entrada,
    raiz: &Path,
    salida: &str,
    ejecutor: &dyn Ejecutor,
) -> Resultado {
    let Some(c) = &e.check else {
        return Resultado::SinComprobar("la entrada no declaró `check`".into());
    };
    let raiz_texto = raiz.to_string_lossy().to_string();
    match c.tipo.as_str() {
        "command" => {
            if c.cmd.trim().is_empty() {
                return Resultado::SinComprobar("`cmd` vacío".into());
            }
            let esperado = if c.expect.is_empty() { "exit0" } else { &c.expect };
            let codigo_esperado = match esperado {
                "exit0" => 0,
                "exit1" => 1,
                otro => return Resultado::SinComprobar(format!("`expect` «{otro}» no es exit0|exit1")),
            };
            match ejecutor.correr(&c.cmd, &raiz_texto, 180) {
                Ok(s) if s.codigo == codigo_esperado => Resultado::Pasa,
                Ok(s) => Resultado::Falla(format!(
                    "`{}` devolvió {} (se pedía {esperado}): {}",
                    c.cmd,
                    s.codigo,
                    // Lo que habla de un comando roto es el stderr; si está vacío,
                    // el stdout. Mezclarlos y recortar dejaba el motivo cortado.
                    if s.stderr.trim().is_empty() { recorta(&s.stdout) } else { recorta(&s.stderr) }
                )),
                Err(m) => Resultado::SinComprobar(format!("el check no se pudo correr: {m}")),
            }
        }
        "exact" => {
            if salida.trim() == c.exact.trim() {
                Resultado::Pasa
            } else {
                Resultado::Falla("la salida no coincide exactamente".into())
            }
        }
        "regex" => {
            // Sin `regex` en el crate: se busca la subcadena literal entre las
            // barras, que es lo que piden los checks de esta suite.
            let aguja = c.pattern.trim().trim_start_matches('/').trim_end_matches('/');
            if aguja.is_empty() {
                return Resultado::SinComprobar("`pattern` vacío".into());
            }
            if salida.contains(aguja) {
                Resultado::Pasa
            } else {
                Resultado::Falla(format!("la salida no contiene «{aguja}»"))
            }
        }
        "json_schema" => {
            let Some(json) = hatboo_brain::verification::json::extraer_json(salida) else {
                return Resultado::Falla("no hay JSON parseable en la salida".into());
            };
            let esquema: serde_json::Value = match serde_json::from_str(&c.pattern) {
                Ok(v) => v,
                Err(e) => {
                    return Resultado::SinComprobar(format!("el esquema de la entrada no es JSON: {e}"))
                }
            };
            match hatboo_brain::verification::json::mini_schema(&json, &esquema) {
                Ok(()) => Resultado::Pasa,
                Err(camino) => Resultado::Falla(format!("el JSON no cumple el esquema en {camino}")),
            }
        }
        otro => Resultado::SinComprobar(format!("tipo de `check` desconocido «{otro}»")),
    }
}

// ───────────────────────────────── corridas ─────────────────────────────────

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Corrida {
    entrada: String,
    categoria: String,
    split: String,
    modelo: String,
    modo: String,
    rep: u32,
    nivel: Option<String>,
    nivel_esperado: Option<String>,
    nivel_acertado: Option<bool>,
    resultado: String,
    motivo: String,
    estado_salida: String,
    verificacion: String,
    duracion_ms: u64,
    ttft_ms: Option<u64>,
    tokens_entrada: Option<u64>,
    tokens_salida: Option<u64>,
    tok_s: Option<f32>,
    ram_mb: Option<u64>,
    recargas: u32,
    reintentos: u8,
    contexto_rechazado: u32,
    clase_fallo: Option<String>,
}

fn mediana(v: &[f64]) -> f64 {
    let mut m = v.to_vec();
    if m.is_empty() {
        return 0.0;
    }
    m.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = m.len();
    if n % 2 == 1 {
        m[n / 2]
    } else {
        (m[n / 2 - 1] + m[n / 2]) / 2.0
    }
}

/// mediana + rango: §9 pide las dos cosas.
fn resumen(v: &[f64]) -> String {
    if v.is_empty() {
        return "— (nada medido)".into();
    }
    let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    format!("{:.0}  [{}–{}]  (n={})", mediana(v), min.round(), max.round(), v.len())
}

// ─────────────────────────────────── main ───────────────────────────────────

#[tokio::main]
async fn main() {
    let opts = match parseo() {
        Ok(o) => o,
        Err(e) => {
            if e == "AYUDA" {
                println!("{AYUDA}");
                return;
            }
            eprintln!("argumentos: {e}\n\n{AYUDA}");
            std::process::exit(2);
        }
    };
    // `--comparar` solo lee dos JSON que ya están escritos: no necesita config,
    // ni modelo, ni RAM, así que se resuelve antes que todo eso.
    if let Some((a, b)) = opts.comparar.clone() {
        if let Err(e) = comparar(&a, &b, opts.informe.as_deref()) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    let config = match Cargada::leer(Some(&opts.config)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no se pudo leer {}: {e}", opts.config.display());
            std::process::exit(2);
        }
    };
    if config.leidos.is_empty() {
        eprintln!(
            "aviso: en {} no apareció ningún config; se corre con reglas y tools vacías \
             y el resultado no representa al producto",
            opts.config.display()
        );
    }
    let ollama = Arc::new(OllamaProvider::nuevo());
    if opts.sondear {
        sondeo(&ollama, &config.registry).await;
        return;
    }
    let texto = match std::fs::read_to_string(&opts.suite) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("no se pudo leer {}: {e}", opts.suite.display());
            std::process::exit(2);
        }
    };
    let suite: Suite = match serde_json::from_str(&texto) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{} no es una suite: {e}", opts.suite.display());
            std::process::exit(2);
        }
    };
    let mut modelos = opts.modelos.clone();
    if modelos.is_empty() {
        match config.registry.modelos.iter().find(|m| m.local) {
            Some(m) => modelos.push(m.id.clone()),
            None => {
                eprintln!(
                    "el registry de {} no describe ningún modelo local; pasa --modelo",
                    opts.config.display()
                );
                std::process::exit(2);
            }
        }
    }
    let entradas: Vec<&Entrada> = suite
        .entradas
        .iter()
        .filter(|e| opts.split == "todos" || e.split == opts.split)
        .filter(|e| {
            opts.solo_categoria
                .as_ref()
                .map(|c| c == &e.categoria)
                .unwrap_or(true)
        })
        .collect();
    if entradas.is_empty() {
        eprintln!(
            "ninguna entrada pasa el filtro (split={}, categoría={:?})",
            opts.split, opts.solo_categoria
        );
        std::process::exit(2);
    }
    if opts.solo_decisiones {
        bateria_decisiones(&entradas, &config, &opts);
        return;
    }
    println!(
        "brain-bench {} · {} de {} entradas (split {}) · modelos [{}] · modo {} · reps {} · tope {} tokens",
        hatboo_brain::version(),
        entradas.len(),
        suite.entradas.len(),
        opts.split,
        modelos.join(", "),
        opts.modo,
        opts.reps,
        opts.limite_salida
    );
    // Una sola sonda para el aviso y para la corrida: la cabecera no puede
    // preguntarle a una copia distinta de la que va a decidir.
    let sonda = Arc::new(SondaReal::arrancar());
    if let Some(l) = sonda.libre_mb() {
        println!("RAM libre medida: {l} MB");
    } else {
        println!(
            "RAM libre: sin medir (el SO no la dio); el Governor rechazará las cargas, \
             porque sin dato no se afirma que quepan"
        );
    }
    if let Some(m) = opts.margen {
        println!("margen del Governor puesto a {m} MB para esta corrida (por defecto serían 1500)");
    }

    let ejecutor = Arc::new(ComandoReal::default());
    let eq = Equipo {
        opts: &opts,
        config: &config,
        ollama: &ollama,
        sonda: &sonda,
        ejecutor: &ejecutor,
    };
    let mut todas: Vec<Corrida> = Vec::new();
    for modelo in &modelos {
        match corre_suite(modelo, &entradas, &eq).await {
            Ok(v) => todas.extend(v),
            Err(e) => eprintln!("[{modelo}] se paró: {e}"),
        }
    }
    informe(&todas, &opts);
    if let Some(ruta) = &opts.salida {
        let j = serde_json::json!({
            "brain_version": hatboo_brain::version(),
            "suite": opts.suite.to_string_lossy(),
            "modo": opts.modo,
            "split": opts.split,
            "reps": opts.reps,
            "tope_salida_tokens": opts.limite_salida,
            "num_ctx_forzado": opts.contexto,
            "corridas": todas,
        });
        match std::fs::write(ruta, serde_json::to_string_pretty(&j).unwrap()) {
            Ok(()) => println!("datos crudos en {}", ruta.display()),
            Err(e) => eprintln!("no se pudo escribir {}: {e}", ruta.display()),
        }
    }
}


/// Lo que la suite monta una vez y le pasa a cada corrida. Eran cinco parámetros
/// que viajaban siempre juntos por tres funciones; agrupados, ninguna firma
/// necesita un `#[allow]` de los argumentos.
struct Equipo<'a> {
    opts: &'a Opciones,
    config: &'a Cargada,
    ollama: &'a Arc<OllamaProvider>,
    sonda: &'a Arc<SondaReal>,
    ejecutor: &'a Arc<ComandoReal>,
}

async fn corre_suite(
    modelo: &str,
    entradas: &[&Entrada],
    eq: &Equipo<'_>,
) -> Result<Vec<Corrida>, String> {
    // `sonda` y `ejecutor` no se usan aquí: viajan dentro de `eq` hasta la corrida.
    let Equipo { opts, config, ollama, .. } = *eq;
    let ficha = match ficha_de(config, ollama, modelo).await {
        Some(f) => f,
        None => {
            let descriptos: Vec<&str> = config.registry.modelos.iter().map(|m| m.id.as_str()).collect();
            return Err(format!(
                "«{modelo}» no está en el registry ({}) y el proveedor no lo listó",
                descriptos.join(", ")
            ));
        }
    };
    let registry = Registry::nuevo(vec![ficha]);
    let mut salida = Vec::new();
    for rep in 1..=opts.reps {
        for e in entradas {
            let raiz = std::env::temp_dir().join(format!(
                "brain-bench-{}-{}-{rep}",
                e.id.replace([':', '/'], "_"),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&raiz);
            if let Some(f) = &e.fixture {
                let origen = opts
                    .suite
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(f.trim_start_matches("benchmarks/").trim_start_matches("./"));
                if !origen.exists() {
                    eprintln!("  {} · fixture {} no existe: se reporta sin comprobar", e.id, origen.display());
                    continue;
                }
                if let Err(err) = copia_fixture(&origen, &raiz) {
                    eprintln!("  {} · no se pudo copiar el fixture: {err}", e.id);
                    continue;
                }
            }
            match una_corrida(e, rep, &raiz, eq, &registry, modelo).await {
                Ok(c) => {
                    println!(
                        "  rep {rep:>2} · {:<3} {:>6} ms · {:>5} tok · {:<13} {}{}",
                        c.resultado,
                        c.duracion_ms,
                        c.tokens_salida.unwrap_or(0),
                        c.nivel.clone().unwrap_or_else(|| "—".into()),
                        e.id,
                        if c.motivo.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", c.motivo)
                        }
                    );
                    salida.push(c);
                }
                Err(err) => {
                    // Una entrada que no pudo correrse **no** borra las demás.
                    // Antes devolvía `Err` y se tiraba el paso entero del modelo:
                    // medido el 04-10, `codigo-01` con `--modelo gemma3:1b` (un
                    // modelo que no declara tools) dejó el volcado en **cero
                    // corridas**, o sea la comparación de §9 imposible por un
                    // solo caso. Se registra como falla y se sigue.
                    eprintln!("  rep {rep:>2} · ERROR en {}: {err}", e.id);
                    salida.push(Corrida {
                        entrada: e.id.clone(),
                        categoria: e.categoria.clone(),
                        split: e.split.clone(),
                        modelo: modelo.to_string(),
                        modo: opts.modo.clone(),
                        rep,
                        nivel_esperado: e.nivel_esperado.map(|l| format!("{l:?}")),
                        resultado: "falla".into(),
                        motivo: format!("no se pudo correr: {err}"),
                        estado_salida: "Rechazado".into(),
                        ..Default::default()
                    });
                }
            }
            let _ = std::fs::remove_dir_all(&raiz);
        }
    }
    Ok(salida)
}

/// La ficha del modelo: la del registry si está, y si no la que describe el
/// proveedor. Nunca se inventa una RAM.
async fn ficha_de(config: &Cargada, ollama: &Arc<OllamaProvider>, modelo: &str) -> Option<ModelInfo> {
    if let Some(m) = config.registry.find(modelo) {
        return Some(m.clone());
    }
    ollama
        .list_models()
        .await
        .ok()?
        .into_iter()
        .find(|m| m.id == modelo)
}

/// La categoría decide el modo, como en la app: saludos y ambigüedad se contestan
/// en chat; el resto es trabajo.
fn modo_de(e: &Entrada) -> &'static str {
    match e.categoria.as_str() {
        "saludo" | "ambiguedad" => "chat",
        _ => "work",
    }
}

/// La petición que se le manda al Brain. La construye una sola función para que
/// la batería de decisiones y la corrida midan exactamente el mismo pedido; si
/// cada una montara su `BrainRequest`, la precisión mediría otra cosa.
fn pedido_de(e: &Entrada, config: &Cargada, raiz: &Path, modelo: &str) -> BrainRequest {
    let raiz_texto = raiz.to_string_lossy().to_string();
    let mut req = BrainRequest::nuevo("brain-bench", modo_de(e), &e.prompt)
        .con_modelo(modelo)
        .con_tools(
            config
                .herramientas
                .disponibles()
                .iter()
                .map(|t| ToolInfo {
                    id: t.id.clone(),
                    escribe: t.escribe,
                    descripcion: t.descripcion.clone(),
                })
                .collect(),
        )
        .con_policy(ExecutionPolicy::LocalOnly)
        .con_aprobacion(ApprovalLevel::FullAccess);
    // El techo de `thinking` lo pone el producto; el arnés lo deja en off para
    // que las dos cifras comparables (tokens y latencia) no dependan de cuánto
    // razona cada modelo. Se mide aparte, con --sondear.
    req.thinking_ceiling = Some(ThinkingLevel::Off);
    if e.fixture.is_some() {
        req.project = Some(ProjectContext {
            root: raiz_texto,
            has_tests: raiz.join("tests").is_dir() || raiz.join("package.json").is_file(),
            has_lint: false,
            has_build: raiz.join("Cargo.toml").is_file() || raiz.join("package.json").is_file(),
            language: None,
            hatboo_md: None,
            trust_state: Default::default(),
            verify: match &e.check {
                Some(c) if c.tipo == "command" && !c.cmd.trim().is_empty() => VerifyCommands {
                    check: Some(c.cmd.clone()),
                    lint: None,
                    test: None,
                },
                _ => VerifyCommands::default(),
            },
        });
    }
    req
}


async fn una_corrida(
    e: &Entrada,
    rep: u32,
    raiz: &Path,
    eq: &Equipo<'_>,
    registry: &Registry,
    modelo: &str,
) -> Result<Corrida, String> {
    let Equipo { opts, config, ollama, sonda, ejecutor } = *eq;
    let modo = modo_de(e);
    let con_fixture = e.fixture.is_some();
    let req = pedido_de(e, config, raiz, modelo);

    let inicio = Instant::now();
    let (texto, nivel, verificacion, estado, metricos) = if opts.modo == "baseline" {
        let r = llama_directo(ollama, e, modelo, modo, config, opts).await?;
        let dur = inicio.elapsed().as_millis() as u64;
        let m = hatboo_brain::api::response::TaskMetrics {
            duracion_ms: dur,
            ttft_ms: r.ttft_ms,
            tokens_entrada: r.tokens_entrada,
            tokens_salida: r.tokens_salida,
            tok_s: r.tok_s,
            ram_mb: None,
            logprob_medio: r.logprob_medio,
            probabilidad: hatboo_brain::providers::exp_de_logprob(r.logprob_medio),
            recargas: 0,
            reintentos: 0,
            contexto_rechazado: 0,
            clase_fallo: None,
            coste: None,
        };
        (r.texto, None, "—".into(), "—".into(), m)
    } else {
        let brain = arma_brain(
            opts,
            config,
            registry,
            ollama,
            sonda,
            ejecutor,
            con_fixture.then_some(raiz),
        )?;
        let r = brain
            .run(&req)
            .await
            .map_err(|err| format!("run: {}", err.mensaje()))?;
        let estado = format!("{:?}", r.output.status);
        let verif = format!("{:?}", r.verification);
        (
            r.output.texto.clone(),
            Some(format!("{:?}", r.plan.level)),
            verif,
            estado,
            r.metrics.clone(),
        )
    };

    // Lo que quedó cargado después, para el informe de RAM.
    if let Ok(lista) = ollama.cargados().await {
        sonda.guarda_cargados(lista);
    }
    let ram = sonda.ram_de(modelo).or(metricos.ram_mb);

    // En `baseline` el arnés no aplica lo que el modelo propuso: no hay tool ni
    // parche ejecutado. Comprobar contra el fixture sin tocar diría «pasa» por
    // accidente, así que se reporta como sin comprobar, con el motivo.
    let resultado = if opts.modo == "baseline" && con_fixture {
        Resultado::SinComprobar("baseline: el arnés no aplicó la salida".into())
    } else {
        comprobar_entrada(e, raiz, &texto, &**ejecutor)
    };
    let (etiqueta, motivo) = match &resultado {
        Resultado::Pasa => ("pasa".to_string(), String::new()),
        Resultado::Falla(m) => ("falla".to_string(), m.clone()),
        Resultado::SinComprobar(m) => ("sin_comprobar".to_string(), m.clone()),
    };
    let nivel_acertado = match (e.nivel_esperado, &nivel) {
        (Some(esperado), Some(dicho)) => Some(dicho == &format!("{esperado:?}")),
        _ => None,
    };
    Ok(Corrida {
        entrada: e.id.clone(),
        categoria: e.categoria.clone(),
        split: e.split.clone(),
        modelo: modelo.into(),
        modo: opts.modo.clone(),
        rep,
        nivel,
        nivel_esperado: e.nivel_esperado.map(|l| format!("{l:?}")),
        nivel_acertado,
        resultado: etiqueta,
        motivo,
        estado_salida: estado,
        verificacion,
        duracion_ms: metricos.duracion_ms,
        ttft_ms: metricos.ttft_ms,
        tokens_entrada: metricos.tokens_entrada,
        tokens_salida: metricos.tokens_salida,
        tok_s: metricos.tok_s,
        ram_mb: ram,
        recargas: metricos.recargas,
        reintentos: metricos.reintentos,
        contexto_rechazado: metricos.contexto_rechazado,
        clase_fallo: metricos.clase_fallo.map(|f| format!("{f:?}")),
    })
}

/// El camino sin Brain: mismo system, mismo modelo, mismo tope, ninguna
/// administración. Es la línea base que pide §9.
async fn llama_directo(
    ollama: &Arc<OllamaProvider>,
    e: &Entrada,
    modelo: &str,
    modo: &str,
    config: &Cargada,
    opts: &Opciones,
) -> Result<hatboo_brain::providers::GenerationResult, String> {
    let sys = build_system(
        &Identidad::default(),
        modo,
        &config.herramientas.ids(),
        None,
        None,
        None,
        if e.lang == "en" { "en" } else { "es" },
    );
    let g = GenerationRequest {
        model: modelo.into(),
        system: sys,
        prompt: e.prompt.clone(),
        history: vec![],
        tools: vec![],
        num_ctx: opts.contexto.unwrap_or(4096),
        keep_alive: KeepAlive::Segundos(300),
        thinking: ThinkingLevel::Off,
        max_output_tokens: opts.limite_salida,
        // El protocolo de la Fase 0: temperatura 0 y semilla fija para que el
        // recuento de tokens sea idéntico corrida a corrida. Es del banco de
        // medidas, no del runtime (allí `BrainConfig` manda, y su defecto es no
        // mandar nada).
        temperature: Some(0.0),
        seed: Some(42),
        logprobs: false,
        timeout_s: 300,
        salida_json: false,
    };
    ollama.generate(g).await.map_err(|err| format!("proveedor: {err}"))
}

fn arma_brain(
    opts: &Opciones,
    config: &Cargada,
    registry: &Registry,
    ollama: &Arc<OllamaProvider>,
    sonda: &Arc<SondaReal>,
    ejecutor: &Arc<ComandoReal>,
    // La raíz es la copia del fixture: sin fixture no hay raíz que leer, así que
    // un `Option` dice las dos cosas a la vez y el `raiz: &Path` + `con_fixture:
    // bool` de antes deja de ser una pareja que tenía que cuadrar a mano.
    fixture: Option<&Path>,
) -> Result<Brain, String> {
    let mut montaje = Montaje::de_proveedor(ollama.clone());
    montaje.registry = registry.clone();
    montaje.reglas = config.reglas.clone();
    montaje.herramientas = config.herramientas.clone();
    montaje.sonda = sonda.clone();
    montaje.contador = Arc::new(Estimador);
    if let Some(raiz) = fixture {
        montaje.ejecutor_tools = Some(Arc::new(ToolsDelArnies {
            raiz: raiz.to_path_buf(),
            ejecutor: ejecutor.clone(),
        }));
        montaje.ejecutor_comandos = Some(ejecutor.clone());
        let r = raiz.to_path_buf();
        montaje.lector = Some(Arc::new(move |ruta: &str| {
            ruta_de(&r, ruta).and_then(|p| std::fs::read_to_string(p).ok())
        }));
    }
    let mut cfg = BrainConfig {
        dir_config: Some(opts.config.clone()),
        // El banco mide con el muestreo atado (temperature 0 y semilla 42: el
        // protocolo de la Fase 0, medido — el recuento de tokens sale idéntico
        // corrida a corrida). Un producto normal no los pone: su defecto es `None`,
        // que es no mandar la clave.
        temperatura: Some(0.0),
        semilla: Some(42),
        ..Default::default()
    };
    if let Some(ctx) = opts.contexto {
        cfg.governor.ctx_permitidos = vec![ctx];
    }
    if let Some(m) = opts.margen {
        cfg.governor.margen_mb = m;
    }
    montaje.config = cfg;
    Brain::nuevo(montaje).map_err(|e| format!("Brain::nuevo: {}", e.mensaje()))
}

/// La batería de decisiones: pasa las entradas por el Engine sin tocar al modelo
/// ni la RAM. Mide §3 —qué decide el Brain, a qué coste en tokens y en contexto—
/// y cuesta milisegundos, así que se puede correr a diario.
fn bateria_decisiones(entradas: &[&Entrada], config: &Cargada, opts: &Opciones) {
    use hatboo_brain::decision::engine::Motor;
    let raiz_suite = opts.suite.parent().unwrap_or_else(|| Path::new("."));
    let mut aciertos = 0usize;
    let mut medidos = 0usize;
    let mut aciertos_riesgo = 0usize;
    let mut medidos_riesgo = 0usize;
    let mut fallos: Vec<String> = Vec::new();
    let mut por_categoria: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut niveles: BTreeMap<String, usize> = BTreeMap::new();
    let mut riesgos: BTreeMap<String, usize> = BTreeMap::new();
    let mut contratos: BTreeMap<String, usize> = BTreeMap::new();
    let mut con_tools = 0usize;
    let mut atajo = 0usize;
    let mut microses: Vec<u64> = Vec::new();
    let mut franjas: BTreeMap<&'static str, usize> = BTreeMap::new();
    // G.1 paso 1: cuántas reglas casan por entrada, y qué valor de confianza da
    // cada caso. El 4 es el cubo «cuatro o más».
    let mut coincidentes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut valores_confianza: BTreeMap<String, usize> = BTreeMap::new();

    for &e in entradas {
        // El fixture se lee de su carpeta original: la decisión solo mira si hay
        // tests o build, y eso es idéntico en la copia que usa la corrida.
        let raiz = match &e.fixture {
            Some(f) => raiz_suite.join(f.trim_start_matches("benchmarks/").trim_start_matches("./")),
            None => raiz_suite.to_path_buf(),
        };
        let req = pedido_de(e, config, &raiz, "");
        let mut motor = Motor::nuevo(config.reglas.clone());
        let t0 = Instant::now();
        let d = motor.evaluar(&req);
        microses.push(t0.elapsed().as_micros() as u64);

        // G.1 paso 1, medido con las mismas piezas públicas que usa `evaluar` y por
        // fuera del Motor: la confianza sale del margen entre la primera y la
        // segunda candidatura (`prio × 100 + condiciones`), así que lo que hay que
        // saber es cuántas reglas casaron —no si la decisión fue buena—.
        let sen = hatboo_brain::decision::engine::senales(&req, &config.reglas);
        let casan = config
            .reglas
            .reglas
            .iter()
            .filter(|r| config.reglas.cumple(r, &sen, &sen.language).unwrap_or(false))
            .count();
        if d.source != hatboo_brain::api::vocab::DecisionSource::FastPath {
            *coincidentes.entry(casan.min(4)).or_default() += 1;
            *valores_confianza
                .entry(format!("{:.3}", d.confidence.valor()))
                .or_default() += 1;
        }

        if d.source == hatboo_brain::api::vocab::DecisionSource::FastPath {
            atajo += 1;
        }
        if !d.tools.is_empty() {
            con_tools += 1;
        }
        *niveles.entry(format!("{:?}", d.level)).or_default() += 1;
        *riesgos.entry(format!("{:?}", d.risk)).or_default() += 1;
        *contratos
            .entry(format!("{:?}", d.output_contract))
            .or_default() += 1;
        // §1 usa 0,85 y 0,55. Si los valores reales son siempre 1,0 o ~0,15, la
        // banda del medio no existe y no hay fórmula que tocar: se mide antes de
        // suponerlo.
        let conf = d.confidence.valor();
        let franja = if conf >= 0.85 {
            "≥0,85 (alto)"
        } else if conf >= 0.55 {
            "0,55–0,85 (media)"
        } else if conf > 0.0 {
            "<0,55 (duda)"
        } else {
            "0,00 (empate)"
        };
        *franjas.entry(franja).or_insert(0usize) += 1;

        if let Some(esperado) = e.riesgo_esperado {
            medidos_riesgo += 1;
            if d.risk == esperado {
                aciertos_riesgo += 1;
            } else {
                fallos.push(format!(
                    "  {} · riesgo esperado {:?} · decidió {:?} · nivel {:?} · conf {:.2} · {}",
                    e.id, esperado, d.risk, d.level, d.confidence.valor(), d.por_que
                ));
            }
        }

        if let Some(esperado) = e.nivel_esperado {
            medidos += 1;
            let c = por_categoria.entry(e.categoria.clone()).or_default();
            c.1 += 1;
            if d.level == esperado {
                aciertos += 1;
                c.0 += 1;
            } else {
                fallos.push(format!(
                    "  {} · esperaba {:?} · decidió {:?} · riesgo {:?} · contrato {:?} · tools {} · conf {:.2} · {}",
                    e.id,
                    esperado,
                    d.level,
                    d.risk,
                    d.output_contract,
                    d.tools.len(),
                    d.confidence.valor(),
                    d.por_que
                ));
            }
        }
    }

    println!("\n=== batería de decisiones · {} entradas ===", entradas.len());
    println!(
        "nivel esperado: {aciertos}/{medidos} = {} %",
        (aciertos * 100).checked_div(medidos).unwrap_or(0)
    );
    for (cat, (a, t)) in &por_categoria {
        println!("  {cat:<12} {a}/{t}");
    }
    println!(
        "riesgo esperado: {aciertos_riesgo}/{medidos_riesgo} = {} %",
        (aciertos_riesgo * 100)
            .checked_div(medidos_riesgo)
            .unwrap_or(0)
    );
    for f in &fallos {
        println!("{f}");
    }
    let mut ms = microses.clone();
    ms.sort_unstable();
    let suma: u64 = ms.iter().sum();
    println!(
        "decidir: media {} µs · mediana {} µs · peor {} µs · sin tocar modelo ni RAM",
        if ms.is_empty() { 0 } else { suma / ms.len() as u64 },
        if ms.is_empty() { 0 } else { ms[ms.len() / 2] },
        ms.last().copied().unwrap_or(0)
    );
    println!("  Fast Path {} de {} · con tools {} de {}", atajo, entradas.len(), con_tools, entradas.len());
    println!("  niveles: {:?}", niveles);
    println!("  riesgos: {:?}", riesgos);
    println!("  contratos: {:?}", contratos);
    println!("  confianza: {:?}", franjas);
    // G.1 paso 1: el desglose que falta para poder opinar de la fórmula. Si el 1,000
    // sale de «casó una sola regla», la franja alta no es certeza: es soledad.
    let cubos: Vec<String> = coincidentes
        .iter()
        .map(|(n, c)| {
            if *n == 4 {
                format!("≥4 → {c}")
            } else {
                format!("{n} → {c}")
            }
        })
        .collect();
    println!("  reglas que casan (sin Fast Path): {}", cubos.join(" · "));
    let hist: Vec<String> = valores_confianza
        .iter()
        .rev()
        .map(|(v, c)| format!("{v}×{c}"))
        .collect();
    println!("  valores de confianza: {}", hist.join("  "));
    // Cuánto contexto y salida firma cada lote: es el coste que el Engine está
    // asignando antes de que nadie escriba una línea. Se leen las mismas funciones
    // con las que se firma el Plan, no una copia a mano.
    let mut ctx_total = 0u32;
    let mut out_total = 0u32;
    for &e in entradas {
        let raiz = match &e.fixture {
            Some(f) => raiz_suite.join(f.trim_start_matches("benchmarks/").trim_start_matches("./")),
            None => raiz_suite.to_path_buf(),
        };
        let req = pedido_de(e, config, &raiz, "");
        let mut motor = Motor::nuevo(config.reglas.clone());
        let d = motor.evaluar(&req);
        ctx_total += hatboo_brain::planner::plan::default_context_budget(d.level);
        out_total += hatboo_brain::planner::plan::default_max_output(d.level);
    }
    println!(
        "presupuesto del nivel (antes de medir la carga real): {} de contexto y {} de salida en total (media {} / {} por turno)",
        ctx_total,
        out_total,
        ctx_total / entradas.len().max(1) as u32,
        out_total / entradas.len().max(1) as u32
    );
}

fn informe(todas: &[Corrida], opts: &Opciones) {
    if todas.is_empty() {
        println!("\nno se pudo correr nada.");
        return;
    }
    println!("\n=== informe · {} corridas ===", todas.len());
    let mut por_entrada: BTreeMap<&str, Vec<&Corrida>> = BTreeMap::new();
    for c in todas {
        por_entrada.entry(c.entrada.as_str()).or_default().push(c);
    }
    let (mut pasa, mut falla, mut sin_comprobar) = (0usize, 0usize, 0usize);
    for v in por_entrada.values() {
        let resultados: Vec<&str> = v.iter().map(|c| c.resultado.as_str()).collect();
        if resultados.iter().all(|x| *x == "pasa") {
            pasa += 1;
        } else if resultados.contains(&"falla") {
            falla += 1;
        } else {
            sin_comprobar += 1;
        }
    }
    println!(
        "entradas: pasa {pasa} · falla {falla} · sin_comprobar {sin_comprobar}  \
         (de {} — Unverifiable no es éxito y se cuenta aparte)",
        por_entrada.len()
    );
    fn de_u64(todas: &[Corrida], f: fn(&Corrida) -> u64) -> Vec<f64> {
        todas.iter().map(|c| f(c) as f64).collect()
    }
    println!(
        "latencia total    {}",
        resumen(&de_u64(todas, |c| c.duracion_ms))
    );
    let ttft: Vec<f64> = todas.iter().filter_map(|c| c.ttft_ms.map(|t| t as f64)).collect();
    println!("TTFT              {}", resumen(&ttft));
    let tok: Vec<f64> = todas
        .iter()
        .filter_map(|c| c.tokens_salida.map(|t| t as f64))
        .collect();
    println!("tokens de salida  {}", resumen(&tok));
    let ram: Vec<f64> = todas.iter().filter_map(|c| c.ram_mb.map(|t| t as f64)).collect();
    println!("RAM residente     {}", resumen(&ram));
    let fuera: Vec<f64> = todas.iter().map(|c| c.contexto_rechazado as f64).collect();
    println!("contexto fuera    {}", resumen(&fuera));
    let reintentos: usize = todas.iter().map(|c| c.reintentos as usize).sum();
    let recargas: u32 = todas.iter().map(|c| c.recargas).sum();
    println!("reintentos {reintentos} · recargas {recargas}");
    let fallos: BTreeMap<&str, usize> = todas.iter().fold(BTreeMap::new(), |mut m, c| {
        if let Some(f) = &c.clase_fallo {
            *m.entry(f.as_str()).or_default() += 1;
        }
        m
    });
    if !fallos.is_empty() {
        println!("clases de fallo: {:?}", fallos);
    }
    let medidores: Vec<&Corrida> = todas.iter().filter(|c| c.nivel_acertado.is_some()).collect();
    if !medidores.is_empty() {
        let buenos = medidores.iter().filter(|c| c.nivel_acertado == Some(true)).count();
        println!(
            "precisión de nivel: {buenos}/{} = {:.0} %  (split {})",
            medidores.len(),
            100.0 * buenos as f64 / medidores.len() as f64,
            opts.split
        );
    }
    let estados: BTreeMap<&str, usize> = todas.iter().fold(BTreeMap::new(), |mut m, c| {
        *m.entry(c.estado_salida.as_str()).or_default() += 1;
        m
    });
    println!("estado de la salida: {estados:?}");
    if opts.modo == "baseline" {
        println!(
            "\nOJO: modo baseline. No se administró nada y los checks de fixture no se \
             puntúan. Para la comparación de §9 hay que correr --modo brain en la misma \
             máquina y la misma Ollama."
        );
    }
}

// ───────────────────────────────── sondeo ─────────────────────────────────

/// Carga cada modelo local escalón por escalón, mide la RAM residente y prueba
/// las dos capacidades que la ficha no puede contestar: una tool call real y un
/// `think` real. §16 del plan: en modelos chicos lo declarado no es lo que pasa.
async fn sondeo(ollama: &Arc<OllamaProvider>, registry: &Registry) {
    let sonda = SondaReal::arrancar();
    let mut descriptos: Vec<ModelInfo> = match ollama.list_models().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("no se pudo listar con el proveedor: {e}");
            std::process::exit(2);
        }
    };
    if descriptos.is_empty() {
        eprintln!("el proveedor no devolvió ningún modelo: ¿está Ollama arrancado?");
        std::process::exit(2);
    }
    // La RAM medida del registry vale más que lo declarado: se parte de ella.
    for d in descriptos.iter_mut() {
        if let Some(m) = registry.find(&d.id) {
            for (ctx, ram) in &m.ram_mb_by_ctx {
                d.ram_mb_by_ctx.insert(*ctx, *ram);
            }
        }
    }
    let mut salida: Vec<ModelInfo> = Vec::new();
    for mut info in descriptos {
        if !info.local {
            println!("{} · nube: no se sondea (no carga en esta máquina)", info.id);
            continue;
        }
        println!(
            "\n{} · disco {:?} MB · ctx máx {}",
            info.id,
            info.disco_mb,
            info.max_ctx
        );
        // Los cinco peldaños de la escalera del Governor. Los dos bajos son los que
        // le permiten apretar la ventana en vez de negar el turno, y `ram_para` no
        // interpola: si no se miden aquí, no existen.
        let escalones: Vec<u32> = if info.max_ctx >= 8192 {
            vec![512, 1024, 2048, 4096, 8192]
        } else if info.max_ctx >= 4096 {
            vec![512, 1024, 2048, 4096]
        } else {
            vec![512, 1024, 2048]
        };
        for ctx in escalones {
            let _ = ollama.expulsar(&info.id).await;
            if let Some(libre) = sonda.libre_mb() {
                // Lo que hace falta para intentar ESTE peldaño: la cifra medida del
                // vecino de abajo, si lo hay. Con `max` de toda la tabla, un 512 no
                // se intentaba en una máquina que no llegaba al 8192 —y el peldaño
                // bajo es justamente el que hay que poder medir—.
                let necesita = info
                    .ram_mb_by_ctx
                    .iter()
                    .filter(|(c, _)| **c <= ctx)
                    .map(|(_, r)| *r)
                    .max()
                    .or_else(|| info.ram_mb_by_ctx.values().min().copied())
                    .unwrap_or(0);
                if necesita > 0 && libre < necesita + 800 {
                    println!("  ctx {ctx:>5} · no se intenta: {libre} MB libres para ~{necesita} MB");
                    continue;
                }
            }
            match carga_y_medir(ollama, &info.id, ctx, 16).await {
                Ok((ram, ms)) => {
                    if let Some(ram) = ram {
                        info.ram_mb_by_ctx.insert(ctx, ram);
                        println!("  ctx {ctx:>5} · {ram:>5} MB residentes en {ms} ms");
                    } else {
                        println!("  ctx {ctx:>5} · respondió, pero /api/ps no lo reportó");
                    }
                }
                Err(e) => println!("  ctx {ctx:>5} · no cargó: {e}"),
            }
        }
        let (tools, por_que) = prueba_tool(ollama, &info.id).await;
        info.supports_tools = tools;
        println!("  tools: {tools} · {por_que}");
        let (piensa, por_que) = prueba_pensamiento(ollama, &info.id).await;
        info.supports_thinking = piensa;
        println!("  thinking: {piensa} · {por_que}");
        // §X quiere `structured_output` en el registry, y la ficha de Ollama no
        // lo declara (medido: `capabilities` solo dice completion/tools/thinking).
        // Lo único que lo sabe es un pedido con esquema ceñido y si volvió JSON.
        let esquema = serde_json::json!({
            "type": "object",
            "properties": { "respuesta": { "type": "string" } },
            "required": ["respuesta"],
            "additionalProperties": false
        });
        let (estructurada, por_que) = match ollama.prueba_esquema(&info.id, &esquema).await {
            Ok(texto) => match serde_json::from_str::<serde_json::Value>(&texto) {
                Ok(j) => match hatboo_brain::verification::json::mini_schema(&j, &esquema) {
                    Ok(()) => (true, format!("cumplió el esquema: {}", texto.chars().take(60).collect::<String>())),
                    Err(camino) => (false, format!("JSON válido pero fuera del esquema en {camino}")),
                },
                Err(_) => (false, format!("no devolvió JSON: {}", texto.chars().take(60).collect::<String>())),
            },
            Err(e) => (false, e),
        };
        info.structured_output = estructurada;
        println!("  salida estructurada: {estructurada} · {por_que}");
        let _ = ollama.expulsar(&info.id).await;
        salida.push(info);
    }
    let reg = Registry::nuevo(salida);
    let destino = PathBuf::from("models.sondeado.json");
    match std::fs::write(&destino, reg.a_json()) {
        Ok(()) => println!(
            "\nescrito {}. Déjalo en el directorio de config como models.json: \
             ahí está lo medido en esta máquina, no lo declarado.",
            destino.display()
        ),
        Err(e) => eprintln!("no se pudo escribir {}: {e}", destino.display()),
    }
}

/// Carga el modelo a `ctx` con un prompt mínimo y lee la RAM residente que
/// reporta `/api/ps`, más el tiempo que tardó en estar listo.
async fn carga_y_medir(
    ollama: &Arc<OllamaProvider>,
    id: &str,
    ctx: u32,
    toks: u32,
) -> Result<(Option<u64>, u128), String> {
    let g = GenerationRequest {
        model: id.into(),
        system: String::new(),
        prompt: "Di OK.".into(),
        history: vec![],
        tools: vec![],
        num_ctx: ctx,
        keep_alive: KeepAlive::Segundos(120),
        thinking: ThinkingLevel::Off,
        max_output_tokens: toks,
        temperature: Some(0.0),
        seed: Some(42),
        logprobs: false,
        timeout_s: 300,
        salida_json: false,
    };
    let inicio = Instant::now();
    ollama.generate(g).await.map_err(|e| e.to_string())?;
    let ms = inicio.elapsed().as_millis();
    let ram = ollama
        .cargados()
        .await
        .unwrap_or_default()
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.ram_mb);
    Ok((ram, ms))
}

async fn prueba_tool(ollama: &Arc<OllamaProvider>, id: &str) -> (bool, String) {
    let g = GenerationRequest {
        model: id.into(),
        system: "Cuando te pidan leer un archivo, llama a la tool; no lo narres.".into(),
        prompt: "Lee el archivo a.txt".into(),
        history: vec![],
        tools: vec![ToolSchema {
            name: "read_file".into(),
            description: "Lee un archivo del proyecto y devuelve su contenido.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }],
        num_ctx: 2048,
        keep_alive: KeepAlive::Segundos(120),
        thinking: ThinkingLevel::Off,
        max_output_tokens: 128,
        temperature: Some(0.0),
        seed: Some(42),
        logprobs: false,
        timeout_s: 300,
        salida_json: false,
    };
    match ollama.generate(g).await {
        Ok(r) if !r.tool_calls.is_empty() => (true, format!("llamó «{}»", r.tool_calls[0].tool)),
        Ok(r) => (false, format!("devolvió texto: {}", recorta(&r.texto))),
        Err(e) => (false, e.to_string()),
    }
}

async fn prueba_pensamiento(ollama: &Arc<OllamaProvider>, id: &str) -> (bool, String) {
    let g = GenerationRequest {
        model: id.into(),
        system: String::new(),
        prompt: "¿Cuánto es 17*4? Razona antes de responder.".into(),
        history: vec![],
        tools: vec![],
        num_ctx: 2048,
        keep_alive: KeepAlive::Segundos(120),
        thinking: ThinkingLevel::Low,
        max_output_tokens: 256,
        temperature: Some(0.0),
        seed: Some(42),
        logprobs: false,
        timeout_s: 300,
        salida_json: false,
    };
    match ollama.generate(g).await {
        Ok(r) => {
            let algo = r.razonamiento.as_ref().map(|t| !t.trim().is_empty()).unwrap_or(false);
            let motivo = match &r.razonamiento {
                Some(t) => format!("{} caracteres de razonamiento", t.chars().count()),
                None => "el proveedor no devolvió campo de razonamiento".into(),
            };
            (algo, motivo)
        }
        Err(e) => (false, e.to_string()),
    }
}

// ─────────────────────── comparar: el veredicto de §9 ───────────────────────

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct Volcado {
    modo: String,
    reps: u32,
    split: String,
    brain_version: String,
    corridas: Vec<Corrida>,
}

/// Lo que se le compara a un bando. Todo en **mediana**: medido el 30-09, dos
/// pasadas de la misma petición se desviaron +34 % / −11 % en el total y ±44 %
/// en TTFT, así que afirmar algo con una corrida sería vender ruido.
#[derive(Debug, Default, Clone)]
struct Bando {
    entradas: usize,
    duracion_ms: f64,
    /// Rango de las medianas por entrada (mínimo y máximo). §9 pide medianas **y
    /// rango**: en esta máquina dos pasadas de la misma petición se desviaron
    /// +34 % / −11 %, y una mediana sin rango es un número huérfano.
    duracion_min: f64,
    duracion_max: f64,
    ttft_ms: f64,
    tokens_salida: f64,
    tokens_min: f64,
    tokens_max: f64,
    recargas: f64,
    reintentos: f64,
    pasa: usize,
    falla: usize,
    sin_comprobar: usize,
    verificados: usize,
    /// Entradas que **no llegaron a generar**: en `brain` son los turnos que el
    /// Governor rechazó por falta de RAM, y valen 2 ms y 0 tokens. Contarlos como
    /// velocidad es la mentira que esta guarda viene a impedir.
    sin_salida: usize,
}

/// Dos medianas seguidas: primero entre las repeticiones de una misma entrada,
/// después entre entradas. Si no, una categoría con seis repeticiones pesaría
/// más que una con una.
///
/// **Dentro de cada entrada solo entran las repeticiones que generaron.** Con las
/// rechazadas dentro, una entrada con dos rechazos de 2 ms y una generación de
/// 900 ms saca mediana 2 ms: la Suite del coder salió así, con una «duración» de
/// 0 ms que no era velocidad sino el modelo sin encender.
/// Si esta repetición llegó a generar texto. Un `None` o un cero es un turno que
/// el Governor rechazó antes de encender el modelo.
fn genero(c: &Corrida) -> bool {
    c.tokens_salida.unwrap_or(0) > 0
}

fn bando_de(corridas: &[Corrida]) -> Bando {
    let mut por_entrada: std::collections::BTreeMap<&str, Vec<&Corrida>> = Default::default();
    for c in corridas {
        por_entrada.entry(c.entrada.as_str()).or_default().push(c);
    }
    let de = |pick: &dyn Fn(&Corrida) -> f64| -> (f64, f64, f64) {
        let por: Vec<f64> = por_entrada
            .values()
            .filter_map(|g| {
                let v: Vec<f64> = g.iter().filter(|c| genero(c)).map(|c| pick(c)).collect();
                if v.is_empty() {
                    None
                } else {
                    Some(mediana(&v))
                }
            })
            .collect();
        if por.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        (
            mediana(&por),
            por.iter().copied().fold(f64::INFINITY, f64::min),
            por.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let (duracion_ms, duracion_min, duracion_max) = de(&|c| c.duracion_ms as f64);
    let (ttft_ms, _, _) = de(&|c| c.ttft_ms.unwrap_or(0) as f64);
    let (tokens_salida, tokens_min, tokens_max) = de(&|c| c.tokens_salida.unwrap_or(0) as f64);
    let cuenta = |etiqueta: &str| corridas.iter().filter(|c| c.resultado == etiqueta).count();
    // Los contadores de **eventos** (recargas, reintentos) van en suma directa
    // sobre las corridas, no en mediana por entrada: una recarga en una de tres
    // repeticiones es una recarga pagada — 3,7 s de recarga de contexto medidos en
    // esta máquina — y la mediana la convertía en cero. El informe de 2026-10-05
    // decía «recargas 0» en un bando que el propio pase había contado 24.
    let total = |pick: &dyn Fn(&Corrida) -> f64| -> f64 {
        corridas.iter().map(pick).sum()
    };
    Bando {
        entradas: por_entrada.len(),
        duracion_ms,
        duracion_min,
        duracion_max,
        ttft_ms,
        tokens_salida,
        tokens_min,
        tokens_max,
        recargas: total(&|c| c.recargas as f64),
        reintentos: total(&|c| c.reintentos as f64),
        pasa: cuenta("pasa"),
        falla: cuenta("falla"),
        sin_comprobar: cuenta("sin_comprobar"),
        verificados: corridas.iter().filter(|c| c.estado_salida == "Verificado").count(),
        sin_salida: por_entrada.values().filter(|g| !g.iter().any(|c| genero(c))).count(),
    }
}

/// Porcentaje de `b` respecto de `a`. `None` cuando no hay base con la que
/// comparar, que es distinto de «cero por ciento».
fn delta(a: f64, b: f64) -> Option<f64> {
    if !a.is_finite() || !b.is_finite() || a == 0.0 {
        return None;
    }
    Some((b - a) / a * 100.0)
}

/// El veredicto de una métrica de coste. §1 fija el umbral en 5 % y §9 lo pide
/// sobre medianas de al menos tres repeticiones: con menos, la respuesta honesta
/// es que no se puede afirmar nada.
fn veredicto(a: f64, b: f64, reps: u32) -> &'static str {
    if reps < 3 {
        return "sin afirmar (faltan repeticiones)";
    }
    match delta(a, b) {
        None => "sin base",
        Some(d) if d > 5.0 => "REGRESIÓN",
        Some(d) if d < -5.0 => "mejora",
        Some(_) => "dentro del ruido",
    }
}

fn por_modelo(v: &Volcado) -> std::collections::BTreeMap<String, Vec<Corrida>> {
    let mut m: std::collections::BTreeMap<String, Vec<Corrida>> = Default::default();
    for c in &v.corridas {
        m.entry(c.modelo.clone()).or_default().push(c.clone());
    }
    m
}

/// Las entradas que de verdad generaron en un grupo de corridas.
fn generaron(corridas: &[Corrida]) -> std::collections::BTreeSet<String> {
    corridas
        .iter()
        .filter(|c| c.tokens_salida.unwrap_or(0) > 0)
        .map(|c| c.entrada.clone())
        .collect()
}

/// §9 hecho bien: **se comparan las entradas que generaron en los dos bandos**, no
/// dos conjuntos distintos. El Brain rechaza por RAM y esas entradas valen 0 ms;
/// si se dejan dentro, el informe vende «-99,9 % de duración» por no haber
/// contestado. Y si se comparan 24 entradas contra 12, la mediana tampoco es una
/// comparación.
///
/// Devuelve los dos bandos recortados y **cuántas entradas se quedaron fuera por
/// culpa del contrario** en cada lado: `fuera_a` son las que generó `a` y no
/// generó `b`. La atribución de quién falló la lleva `sin_salida` de cada bando,
/// que es lo que se imprime arriba; mezclar las dos cifras es lo que hacía decir
/// «no generaron en baseline» de entradas que el baseline generó todas.
fn emparejar(a: &[Corrida], b: &[Corrida]) -> (Vec<Corrida>, Vec<Corrida>, usize, usize) {
    let (ga, gb) = (generaron(a), generaron(b));
    let pares: std::collections::BTreeSet<&String> = ga.intersection(&gb).collect();
    let queda = |v: &[Corrida]| -> Vec<Corrida> {
        v.iter().filter(|c| pares.contains(&c.entrada)).cloned().collect()
    };
    (
        queda(a),
        queda(b),
        ga.len() - pares.len(),
        gb.len() - pares.len(),
    )
}

/// «58/72 (sin generar 14)»: la cobertura del bando. Va en la portada del informe
/// porque sin ella no se puede saber si las medianas de coste hablan de respuestas
/// o de rechazos.
fn formato(b: &Bando) -> String {
    if b.sin_salida == 0 {
        format!("{}/{}", b.entradas, b.entradas)
    } else {
        format!(
            "{}/{} (sin generar {})",
            b.entradas - b.sin_salida,
            b.entradas,
            b.sin_salida
        )
    }
}

/// Las tres palabras que pide §9, por modelo. `no se puede afirmar` no es una
/// cortesanía: es el caso en el que no hay entradas que generaron en los dos bandos,
/// y decir «mejora» ahí sería vender una ausencia como un resultado.
fn veredicto_de(mejoras: usize, regresiones: usize, comparable: bool) -> &'static str {
    if !comparable {
        "NO SE PUEDE AFIRMAR"
    } else if regresiones > 0 {
        "REGRESIÓN"
    } else if mejoras > 0 {
        "mejora"
    } else {
        "dentro del ruido"
    }
}

fn linea(nombre: &str, a: f64, b: f64, reps: u32) -> String {
    let d = delta(a, b).map(|x| format!("{x:+.1} %")).unwrap_or_else(|| "—".into());
    format!(
        "  {nombre:<14} {a:>12.0} → {b:>12.0}   {d:>10}   {}",
        veredicto(a, b, reps)
    )
}

fn leer_volcado(ruta: &Path) -> Result<Volcado, String> {
    let texto = std::fs::read_to_string(ruta).map_err(|e| format!("{}: {e}", ruta.display()))?;
    serde_json::from_str(&texto)
        .map_err(|e| format!("{} no es un volcado del arnés: {e}", ruta.display()))
}

/// El paso que faltaba: §9 no se cumple corriendo una vez, se cumple enfrentando
/// dos volcados y diciendo en qué caso no se puede afirmar nada.
fn comparar(a_ruta: &Path, b_ruta: &Path, informe: Option<&Path>) -> Result<(), String> {
    let a = leer_volcado(a_ruta)?;
    let b = leer_volcado(b_ruta)?;
    if a.corridas.is_empty() || b.corridas.is_empty() {
        return Err("uno de los dos volcados no tiene corridas".into());
    }
    let (ga, gb) = (por_modelo(&a), por_modelo(&b));
    let reps = a.reps.min(b.reps);
    let mut md = String::new();
    md.push_str("# Baseline vs Brain (§9)\n\n");
    md.push_str(&format!(
        "- ficheros: `{}` (modo {}, {} reps, split {}) · `{}` (modo {}, {} reps, split {})\n",
        a_ruta.display(), a.modo, a.reps, a.split,
        b_ruta.display(), b.modo, b.reps, b.split,
    ));
    md.push_str(&format!(
        "- brain_version: `{}` vs `{}`\n",
        if a.brain_version.is_empty() { "?" } else { &a.brain_version },
        if b.brain_version.is_empty() { "?" } else { &b.brain_version },
    ));
    md.push_str("- umbral de regresión 5 % sobre medianas; con menos de 3 repeticiones **no se afirma nada** (ruido medido por corrida: +34 % / −11 %).\n");
    md.push_str("- **solo se comparan las entradas que generaron en los dos bandos**: una entrada que el Governor rechazó no es una respuesta rápida, y mezclarlas da «-99,9 % de duración» sin haber contestado. Lo que se queda fuera va escrito abajo.\n\n");

    let mut regresiones = 0;
    let mut mejoras = 0;
    let mut sin_comparar = 0;
    for (modelo, ca) in &ga {
        let Some(cb) = gb.get(modelo) else {
            println!("· {modelo}: solo está en un fichero, no hay con qué comparar.");
            md.push_str(&format!("- **{modelo}**: solo en un fichero.\n"));
            continue;
        };
        // Primero el retrato completo de cada bando (para decir la cobertura), y
        // después la comparación, que va sobre el emparejamiento.
        let (todo_a, todo_b) = (bando_de(ca), bando_de(cb));
        let (pa, pb, fuera_a, fuera_b) = emparejar(ca, cb);
        let (bando_a, bando_b) = (bando_de(&pa), bando_de(&pb));
        println!(
            "\n== {modelo} · {} entradas en la suite",
            todo_a.entradas.max(todo_b.entradas)
        );
        let cobertura = format!(
            "  {:<14} {:>12} → {:>12}",
            "generaron", formato(&todo_a), formato(&todo_b)
        );
        println!("{cobertura}");
        md.push_str(&format!("## {modelo}\n\n```text\n"));
        md.push_str(&format!("{cobertura}\n"));
        if bando_a.entradas == 0 {
            let v = veredicto_de(0, 0, false);
            println!("  Veredicto {modelo}: {v} — ninguna entrada generó en los dos bandos.");
            md.push_str(&format!("**Veredicto {modelo}**: {v} — ninguna entrada generó en los dos bandos, así que no hay medianas que comparar.\n\n"));
            sin_comparar += 1;
            continue;
        }
        if fuera_a > 0 || fuera_b > 0 {
            println!(
                "  · Comparado sobre **{} entradas** (las que generaron en los dos). Fuera: {fuera_a} \
                 que generó el baseline y no llegó a generar el brain, {fuera_b} al revés.",
                bando_a.entradas
            );
            md.push_str(&format!(
                "Comparado sobre {} entradas (las que generaron en los dos bandos). Fuera: {fuera_a} generadas solo en baseline, {fuera_b} solo en brain.\n",
                bando_a.entradas
            ));
        }
        // Dos preguntas distintas con dos poblaciones distintas. El **coste por
        // respuesta** solo se puede comparar en las entradas que respondieron en
        // los dos bandos. El **gasto total** de la pasada (recargas y reintentos)
        // es de todo el pase: las recargas que pagó el Brain para rechazar 32
        // entradas son dinero salido de la máquina, y contarlas solo en las 6 que
        // sobrevivieron al emparejamiento las ocultaría.
        let (mut mi, mut ri) = (0usize, 0usize);
        for (nombre, x, y) in [
            ("duración ms", bando_a.duracion_ms, bando_b.duracion_ms),
            ("ttft ms", bando_a.ttft_ms, bando_b.ttft_ms),
            ("tokens salida", bando_a.tokens_salida, bando_b.tokens_salida),
        ] {
            let l = linea(nombre, x, y, reps);
            println!("{l}");
            if l.ends_with("REGRESIÓN") {
                ri += 1;
            } else if l.ends_with("mejora") {
                mi += 1;
            }
            md.push_str(&format!("{l}\n"));
        }
        let rangos = format!(
            "  {:<14} {:>12} → {:>12}\n  {:<14} {:>12} → {:>12}",
            "rango dur ms",
            format!("[{:.0}–{:.0}]", bando_a.duracion_min, bando_a.duracion_max),
            format!("[{:.0}–{:.0}]", bando_b.duracion_min, bando_b.duracion_max),
            "rango toks",
            format!("[{:.0}–{:.0}]", bando_a.tokens_min, bando_a.tokens_max),
            format!("[{:.0}–{:.0}]", bando_b.tokens_min, bando_b.tokens_max),
        );
        println!("{rangos}");
        md.push_str(&format!("{rangos}\n"));
        // El gasto no entra en el veredicto de coste: son contadores de otra
        // naturaleza (una recarga son 3,7 s de máquina, no una respuesta más lenta)
        // y mezclarlos con las medianas de duración contaría dos veces lo mismo.
        for (nombre, x, y) in [
            ("recargas tot", todo_a.recargas, todo_b.recargas),
            ("reintentos tot", todo_a.reintentos, todo_b.reintentos),
        ] {
            let mut l = linea(nombre, x, y, reps);
            // Un gasto que solo tiene un lado no es una «mejora»: es el coste de
            // no haber contestado. Se dice al lado del número.
            if l.ends_with("sin base") && x == 0.0 && y > 0.0 {
                l = format!("{l}  (gasto nuevo del brain, no evitado)");
            }
            println!("{l}");
            md.push_str(&format!("{l}\n"));
        }
        let calidad = format!(
            "  {:<14} {:>12} → {:>12}",
            "checks ✓/✗/—",
            format!("{} / {} / {}", todo_a.pasa, todo_a.falla, todo_a.sin_comprobar),
            format!("{} / {} / {}", todo_b.pasa, todo_b.falla, todo_b.sin_comprobar),
        );
        let verif = format!(
            "  {:<14} {:>12} → {:>12}",
            "Verificado", todo_a.verificados, todo_b.verificados
        );
        println!("{calidad}\n{verif}");
        let v = veredicto_de(mi, ri, true);
        println!("  Veredicto {modelo}: {v}  ({mi} métricas mejor, {ri} peor, sobre {} entradas comparables)", bando_a.entradas);
        md.push_str(&format!(
            "{calidad}\n{verif}\n```\n\n**Veredicto {modelo}**: {v} — {mi} métricas por debajo del umbral y {ri} por encima, sobre {} entradas comparables.\n\n",
            bando_a.entradas
        ));
        regresiones += ri;
        mejoras += mi;
    }
    let resumen = if sin_comparar > 0 && regresiones + mejoras == 0 && sin_comparar == ga.len() {
        "NO SE PUEDE AFIRMAR: ningún modelo tiene entradas que generaron en los dos bandos.".to_string()
    } else if regresiones > 0 {
        format!(
            "REGRESIÓN en {regresiones} métricas de coste por respuesta ({mejoras} por debajo del umbral), sobre {reps} repeticiones compartidas. Ver arriba el veredicto de cada modelo: los dos no dicen lo mismo."
        )
    } else if mejoras > 0 {
        format!(
            "mejora en {mejoras} métricas de coste por respuesta, sin ninguna por encima del umbral, sobre {reps} repeticiones compartidas."
        )
    } else {
        format!("dentro del ruido: 0 métricas fuera del umbral del 5 %, sobre {reps} repeticiones.")
    };
    println!("\nVeredicto: {resumen}");
    if reps < 3 {
        println!("Con menos de 3 repeticiones esto es lectura, no veredicto: corre `--reps 3`.");
    }
    md.push_str(&format!("**Veredicto**: {resumen}\n"));
    if let Some(ruta) = informe {
        std::fs::write(ruta, md).map_err(|e| format!("no se pudo escribir el informe: {e}"))?;
        println!("informe en {}", ruta.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests_comparar {
    use super::*;

    fn c(j: serde_json::Value) -> Corrida {
        serde_json::from_value(j).unwrap()
    }

    #[test]
    fn el_delta_no_inventa_una_base() {
        assert_eq!(delta(100.0, 105.0), Some(5.0));
        assert_eq!(delta(0.0, 10.0), None, "dividir por cero no es un 0 %");
        assert_eq!(delta(100.0, f64::NAN), None);
    }

    /// La regla que más se rompe al mirar números: sin tres repeticiones no hay
    /// veredicto, por muy bonito que salga el porcentaje.
    #[test]
    fn con_pocas_repeticiones_no_se_afirma_nada() {
        assert_eq!(veredicto(100.0, 60.0, 1), "sin afirmar (faltan repeticiones)");
        assert_eq!(veredicto(100.0, 60.0, 3), "mejora");
        assert_eq!(veredicto(100.0, 140.0, 3), "REGRESIÓN");
        assert_eq!(veredicto(100.0, 102.0, 3), "dentro del ruido");
        assert_eq!(veredicto(0.0, 5.0, 3), "sin base");
    }

    #[test]
    fn el_bando_promedia_por_entrada_no_por_repeticion() {
        // Una entrada con tres reps y otra con una: si se promediara por corrida,
        // la primera pesaría el triple.
        let corridas = vec![
            c(serde_json::json!({"modelo":"m","entrada":"a","duracion_ms":100,"tokens_salida":10,"resultado":"pasa","estado_salida":"Verificado"})),
            c(serde_json::json!({"modelo":"m","entrada":"a","duracion_ms":900,"tokens_salida":10,"resultado":"pasa","estado_salida":"Verificado"})),
            c(serde_json::json!({"modelo":"m","entrada":"a","duracion_ms":500,"tokens_salida":10,"resultado":"falla","estado_salida":"Propuesto"})),
            c(serde_json::json!({"modelo":"m","entrada":"b","duracion_ms":60,"tokens_salida":4,"recargas":1,"resultado":"sin_comprobar","estado_salida":"Propuesto"})),
        ];
        let b = bando_de(&corridas);
        assert_eq!(b.entradas, 2);
        assert_eq!(b.duracion_ms, 280.0, "mediana de las medianas [500, 60]");
        // La recarga pasó en una de dos entradas: contada como mediana sería 0,5
        // y en la pantalla «no hubo recargas». Es una recarga pagada.
        assert_eq!(b.recargas, 1.0, "los contadores van en suma");
        assert_eq!((b.pasa, b.falla, b.sin_comprobar), (2, 1, 1));
        assert_eq!(b.verificados, 2);
    }

    /// El caso del informe del 05-10 con `qwen2.5-coder:1.5b`: al Brain el
    /// Governor le rechazó 32 de 72 corridas a 2 ms y 0 tokens, y la mediana de
    /// duración del bando salió **0 ms**, más rápido que el modelo funcionando.
    /// Era la duración de no haber encendido nada.
    #[test]
    fn un_rechazo_no_es_una_respuesta_rapida() {
        let corridas = vec![
            c(serde_json::json!({"modelo":"m","entrada":"a","duracion_ms":900,"tokens_salida":40,"resultado":"pasa","estado_salida":"Verificado"})),
            c(serde_json::json!({"modelo":"m","entrada":"b","duracion_ms":2,"tokens_salida":0,"resultado":"falla","estado_salida":"Rechazado"})),
            c(serde_json::json!({"modelo":"m","entrada":"c","duracion_ms":1100,"tokens_salida":60,"resultado":"pasa","estado_salida":"Propuesto"})),
            // La que rompía la mediana: dos repeticiones rechazadas y una que sí
            // generó, dentro de la misma entrada.
            c(serde_json::json!({"modelo":"m","entrada":"d","duracion_ms":2,"tokens_salida":0,"resultado":"falla","estado_salida":"Rechazado"})),
            c(serde_json::json!({"modelo":"m","entrada":"d","duracion_ms":3,"tokens_salida":0,"resultado":"falla","estado_salida":"Rechazado"})),
            c(serde_json::json!({"modelo":"m","entrada":"d","duracion_ms":700,"tokens_salida":20,"resultado":"pasa","estado_salida":"Propuesto"})),
        ];
        let b = bando_de(&corridas);
        assert_eq!(b.entradas, 4);
        assert_eq!(b.sin_salida, 1, "una entrada no llegó a generar en ninguna repetición");
        // Medianas por entrada sobre lo que generó: [900, 1100, 700] → 900. Con
        // los rechazos dentro salía 2 ms y el informe lo llamaba velocidad.
        assert_eq!(b.duracion_ms, 900.0, "la mediana ignora las repeticiones que no generaron");
        assert_eq!(b.tokens_salida, 40.0, "ídem con los tokens");
        assert_eq!(formato(&b), "3/4 (sin generar 1)");
    }

    #[test]
    fn el_emparejamiento_compara_lo_que_corrio_en_los_dos_bandos() {
        let corrio = |e: &str, ms: u64| c(serde_json::json!({"modelo":"m","entrada":e,"duracion_ms":ms,"tokens_salida":30,"resultado":"pasa","estado_salida":"Propuesto"}));
        let rechazo = |e: &str| c(serde_json::json!({"modelo":"m","entrada":e,"duracion_ms":2,"tokens_salida":0,"resultado":"falla","estado_salida":"Rechazado"}));
        let a = vec![corrio("x", 900), corrio("y", 1000), corrio("z", 1100)];
        let b = vec![corrio("x", 880), rechazo("y"), rechazo("z")];
        let (pa, pb, fuera_a, fuera_b) = emparejar(&a, &b);
        let (ba, bb) = (bando_de(&pa), bando_de(&pb));
        assert_eq!((ba.entradas, bb.entradas), (1, 1), "solo «x» corrió en los dos");
        assert_eq!(
            (fuera_a, fuera_b),
            (2, 0),
            "las dos entradas que se pierden son las que solo generó el primer bando"
        );
        assert_eq!((ba.duracion_ms, bb.duracion_ms), (900.0, 880.0));
        assert_eq!(bb.sin_salida, 0, "el par emparejado generó por construcción");
        // Y sin emparejar, el bando `b` ya no miente por las repeticiones
        // rechazadas: mediana sobre lo que generó = 880, no 2 ms.
        assert_eq!(bando_de(&b).duracion_ms, 880.0);
        assert_eq!(
            bando_de(&b).sin_salida,
            2,
            "las dos que no encendieron el modelo, contadas aparte"
        );
    }

    /// El otro agujero del informe del 05-10: el pase de `qwen2.5-coder:1.5b`
    /// cerró su resumen diciendo «reintentos 16 · recargas 24» y el `--comparar`
    /// de ese mismo volcado sacaba **0**. Era la mediana por entrada: una recarga
    /// en una de tres repeticiones promedia a cero, y aquí una recarga son 3,7 s
    /// medidos. Los contadores de eventos van en suma sobre las corridas.
    #[test]
    fn una_recarga_entre_tres_repeticiones_se_paga_y_se_cuenta() {
        let rep = |rec: u32, r: u8| {
            c(serde_json::json!({"modelo":"m","entrada":"a","duracion_ms":900,"tokens_salida":40,
                                 "recargas":rec,"reintentos":r,"resultado":"pasa","estado_salida":"Propuesto"}))
        };
        let b = bando_de(&[rep(1, 0), rep(0, 1), rep(0, 0)]);
        assert_eq!(b.recargas, 1.0, "una recarga pagada, no una mediana a cero");
        assert_eq!(b.reintentos, 1.0, "ídem con los reintentos");
        assert_eq!(b.entradas, 1);
    }

    /// El veredicto en las tres palabras de §9, y el rango que exige el gate.
    #[test]
    fn el_veredicto_se_dice_en_tres_palabras_y_el_rango_no_se_pierde() {
        assert_eq!(veredicto_de(2, 0, true), "mejora");
        assert_eq!(veredicto_de(0, 2, true), "REGRESIÓN");
        assert_eq!(veredicto_de(1, 1, true), "REGRESIÓN", "una regresión pesa más que una mejora: no se promedian");
        assert_eq!(veredicto_de(0, 0, true), "dentro del ruido");
        assert_eq!(veredicto_de(0, 0, false), "NO SE PUEDE AFIRMAR");

        let v = |e: &str, ms: u64| {
            c(serde_json::json!({"modelo":"m","entrada":e,"duracion_ms":ms,"tokens_salida":20,"resultado":"pasa","estado_salida":"Propuesto"}))
        };
        let b = bando_de(&[v("a", 100), v("b", 900), v("c", 500)]);
        assert_eq!((b.duracion_ms, b.duracion_min, b.duracion_max), (500.0, 100.0, 900.0));
        assert_eq!((b.tokens_min, b.tokens_max), (20.0, 20.0));
    }
}
