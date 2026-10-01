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
    ApprovalLevel, ExecutionPolicy, KeepAlive, Level, ThinkingLevel,
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
    salida: Option<PathBuf>,
}

impl Default for Opciones {
    fn default() -> Self {
        Opciones {
            sondear: false,
            suite: PathBuf::from("benchmarks/base.json"),
            config: PathBuf::from("config"),
            modelos: Vec::new(),
            reps: 3,
            modo: "brain".into(),
            split: "dev".into(),
            solo_categoria: None,
            limite_salida: 1024,
            contexto: None,
            salida: None,
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
  --salida RUTA       volcar los datos crudos en JSON
  --sondear           medir RAM por ctx y probar tools/thinking; escribe models.sondeado.json
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
            "--salida" => {
                let v = val!("--salida");
                o.salida = Some(PathBuf::from(v));
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
        SondaReal {
            libre,
            cargados: Arc::new(Mutex::new(Vec::new())),
        }
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
                if pila.pop().is_none() {
                    return None;
                }
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
            if ["target", "node_modules", ".git"]
                .iter()
                .any(|x| nombre == std::ffi::OsString::from(*x))
            {
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

#[derive(Debug, Clone, serde::Serialize)]
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
    if let Some(l) = SondaReal::arrancar().libre_mb() {
        println!("RAM libre medida: {l} MB");
    } else {
        println!(
            "RAM libre: sin medir (el SO no la dio); el Governor puede rechazar cargas por prudencia"
        );
    }

    let sonda = Arc::new(SondaReal::arrancar());
    let ejecutor = Arc::new(ComandoReal::default());
    let mut todas: Vec<Corrida> = Vec::new();
    for modelo in &modelos {
        match corre_suite(modelo, &entradas, &opts, &config, &ollama, &sonda, &ejecutor).await {
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

#[allow(clippy::too_many_arguments)]
async fn corre_suite(
    modelo: &str,
    entradas: &[&Entrada],
    opts: &Opciones,
    config: &Cargada,
    ollama: &Arc<OllamaProvider>,
    sonda: &Arc<SondaReal>,
    ejecutor: &Arc<ComandoReal>,
) -> Result<Vec<Corrida>, String> {
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
            match una_corrida(e, rep, &raiz, opts, config, ollama, sonda, ejecutor, &registry, modelo).await {
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
                    eprintln!("  rep {rep:>2} · ERROR en {}: {err}", e.id);
                    return Err(format!("{}: {err}", e.id));
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

#[allow(clippy::too_many_arguments)]
async fn una_corrida(
    e: &Entrada,
    rep: u32,
    raiz: &Path,
    opts: &Opciones,
    config: &Cargada,
    ollama: &Arc<OllamaProvider>,
    sonda: &Arc<SondaReal>,
    ejecutor: &Arc<ComandoReal>,
    registry: &Registry,
    modelo: &str,
) -> Result<Corrida, String> {
    let modo = match e.categoria.as_str() {
        "saludo" | "ambiguedad" => "chat",
        _ => "work",
    };
    let raiz_texto = raiz.to_string_lossy().to_string();
    let con_fixture = e.fixture.is_some();
    let mut req = BrainRequest::nuevo("brain-bench", modo, &e.prompt)
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
    if con_fixture {
        req.project = Some(ProjectContext {
            root: raiz_texto.clone(),
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
            recargas: 0,
            reintentos: 0,
            contexto_rechazado: 0,
            clase_fallo: None,
            coste: None,
        };
        (r.texto, None, "—".into(), "—".into(), m)
    } else {
        let brain = arma_brain(opts, config, registry, ollama, sonda, ejecutor, raiz, con_fixture)?;
        let r = brain
            .run(&req)
            .await
            .map_err(|err| format!("run: {}", err.mensaje()))?;
        let estado = format!("{:?}", r.output.status);
        let verif = format!("{:?}", r.verification);
        (r.output.texto.clone(), Some(format!("{:?}", r.plan.level)), verif, estado, r.metrics.clone())
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
        temperature: 0.0,
        seed: 42,
        timeout_s: 300,
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
    raiz: &Path,
    con_fixture: bool,
) -> Result<Brain, String> {
    let mut montaje = Montaje::de_proveedor(ollama.clone());
    montaje.registry = registry.clone();
    montaje.reglas = config.reglas.clone();
    montaje.herramientas = config.herramientas.clone();
    montaje.sonda = sonda.clone();
    montaje.contador = Arc::new(Estimador);
    if con_fixture {
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
    let mut cfg = BrainConfig::default();
    cfg.dir_config = Some(opts.config.clone());
    if let Some(ctx) = opts.contexto {
        cfg.governor.ctx_permitidos = vec![ctx];
    }
    montaje.config = cfg;
    Brain::nuevo(montaje).map_err(|e| format!("Brain::nuevo: {}", e.mensaje()))
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
        } else if resultados.iter().any(|x| *x == "falla") {
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
        let escalones: Vec<u32> = if info.max_ctx >= 8192 {
            vec![2048, 4096, 8192]
        } else if info.max_ctx >= 4096 {
            vec![2048, 4096]
        } else {
            vec![2048]
        };
        for ctx in escalones {
            let _ = ollama.expulsar(&info.id).await;
            if let Some(libre) = sonda.libre_mb() {
                let necesita = *info.ram_mb_by_ctx.values().max().unwrap_or(&0);
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
        temperature: 0.0,
        seed: 42,
        timeout_s: 300,
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
        temperature: 0.0,
        seed: 42,
        timeout_s: 300,
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
        temperature: 0.0,
        seed: 42,
        timeout_s: 300,
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
