//! Log local con rotación. **Sin texto completo por defecto** (§7 del plan): lo
//! que se guarda es la decisión y la medida, no la conversación.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registro {
    pub brain_version: &'static str,
    pub plan_schema: u32,
    pub producto: String,
    pub intent: String,
    pub level: String,
    pub plan_hash: Option<String>,
    pub parent_plan_hash: Option<String>,
    pub modelo: String,
    pub provider: String,
    pub target: String,
    pub num_ctx: u32,
    pub thinking: String,
    pub tokens_entrada: Option<u64>,
    pub tokens_salida: Option<u64>,
    pub contexto_rechazado: u32,
    pub clase_fallo: Option<String>,
    pub latencia_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub tok_s: Option<f32>,
    pub ram_mb: Option<u64>,
    pub verificacion: String,
    pub reintentos: u8,
    pub recargas: u32,
    /// Coste normalizado contra la línea base del modelo. `None` si ese modelo
    /// no tiene línea base medida.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coste: Option<f32>,
    pub origen: String,
    pub confianza: f32,
    /// El motivo que también ve el usuario.
    pub reason: String,
    /// Solo con `texto_completo: true`, y aun así redactado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muestra: Option<String>,
}

impl Registro {
    pub fn nuevo(producto: &str) -> Registro {
        Registro {
            brain_version: crate::version(),
            plan_schema: crate::planner::SCHEMA_VERSION,
            producto: producto.to_string(),
            intent: "-".into(),
            level: "-".into(),
            plan_hash: None,
            parent_plan_hash: None,
            modelo: "-".into(),
            provider: "-".into(),
            target: "-".into(),
            num_ctx: 0,
            thinking: "off".into(),
            tokens_entrada: None,
            tokens_salida: None,
            contexto_rechazado: 0,
            clase_fallo: None,
            latencia_ms: None,
            ttft_ms: None,
            tok_s: None,
            ram_mb: None,
            verificacion: "-".into(),
            reintentos: 0,
            recargas: 0,
            coste: None,
            origen: "-".into(),
            confianza: 0.0,
            reason: String::new(),
            muestra: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BitacoraConfig {
    pub ruta: Option<PathBuf>,
    /// MB por fichero antes de rotar.
    pub max_mb: u64,
    /// Cuántos ficheros rotados se conservan.
    pub max_ficheros: usize,
    /// Guardar un trozo del texto (redactado). Off por defecto.
    pub texto_completo: bool,
}

impl Default for BitacoraConfig {
    fn default() -> Self {
        BitacoraConfig {
            ruta: None,
            max_mb: 4,
            max_ficheros: 3,
            texto_completo: false,
        }
    }
}

pub struct Bitacora {
    config: BitacoraConfig,
    fichero: Option<File>,
    escritos: u64,
}

impl Bitacora {
    pub fn nueva(config: BitacoraConfig) -> std::io::Result<Bitacora> {
        let mut b = Bitacora {
            config,
            fichero: None,
            escritos: 0,
        };
        b.abrir()?;
        Ok(b)
    }

    /// Sin `ruta` configurada, registrar es no-op: un producto que no quiere log
    /// no tiene que escribir un archivo vacío.
    fn abrir(&mut self) -> std::io::Result<()> {
        let Some(ruta) = &self.config.ruta else {
            self.fichero = None;
            return Ok(());
        };
        if let Some(dir) = ruta.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        self.fichero = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(ruta)?,
        );
        Ok(())
    }

    pub fn registrar(&mut self, mut r: Registro) -> std::io::Result<()> {
        if !self.config.texto_completo {
            r.muestra = None;
        } else if let Some(m) = &r.muestra {
            r.muestra = Some(crate::security::redact::texto(&truncar(m, 400)));
        }
        // El `reason` lo escribe el modelo o copia algo del pedido: es el único
        // campo de texto que viaja siempre, así que se redacta igual que la
        // muestra. Sin esto, un «usa mi clave sk-ant-…» queda en el log en claro.
        r.reason = crate::security::redact::texto(&r.reason);
        let Some(f) = &mut self.fichero else {
            return Ok(());
        };
        let linea = serde_json::to_string(&r).unwrap_or_else(|_| "{}".into());
        writeln!(f, "{linea}")?;
        f.flush()?;
        self.escritos += linea.len() as u64 + 1;
        if self.escritos >= self.config.max_mb * 1_000_000 {
            self.rotar()?;
        }
        Ok(())
    }

    /// `brain.log` → `brain.log.1`, y `brain.log.3` se tira. Simple y sin
    /// dependencias; el límite de ficheros es lo que impide que el disco se llene.
    fn rotar(&mut self) -> std::io::Result<()> {
        let Some(ruta) = self.config.ruta.clone() else {
            return Ok(());
        };
        drop(self.fichero.take());
        let ultimo = format!("{}.{}", ruta.display(), self.config.max_ficheros);
        let _ = std::fs::remove_file(&ultimo);
        for i in (1..self.config.max_ficheros).rev() {
            let de = format!("{}.{}", ruta.display(), i);
            let para = format!("{}.{}", ruta.display(), i + 1);
            if std::path::Path::new(&de).exists() {
                let _ = std::fs::rename(&de, &para);
            }
        }
        if ruta.exists() {
            let _ = std::fs::rename(&ruta, format!("{}.1", ruta.display()));
        }
        self.escritos = 0;
        self.abrir()
    }

    pub fn con_texto(&mut self, si: bool) {
        self.config.texto_completo = si;
    }
}

fn truncar(s: &str, chars: usize) -> String {
    s.chars().take(chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(nombre: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("hatboo-brain-test-{}-{nombre}", std::process::id()));
        p
    }

    #[test]
    fn sin_ruta_no_escribe_nada_y_no_falla() {
        let mut b = Bitacora::nueva(BitacoraConfig::default()).unwrap();
        assert!(b.registrar(Registro::nuevo("hatboo")).is_ok());
    }

    #[test]
    fn escribe_una_linea_json_sin_el_texto() {
        let ruta = tmp("log1");
        let _ = std::fs::remove_file(&ruta);
        let mut b = Bitacora::nueva(BitacoraConfig {
            ruta: Some(ruta.clone()),
            ..Default::default()
        })
        .unwrap();
        let mut r = Registro::nuevo("hatboo");
        r.modelo = "gemma3:1b".into();
        r.muestra = Some("el contenido íntegro del chat".into());
        b.registrar(r).unwrap();
        let leido = std::fs::read_to_string(&ruta).unwrap();
        assert!(leido.contains("\"gemma3:1b\""));
        assert!(!leido.contains("íntegro"), "el texto no debe guardarse");
        let _ = std::fs::remove_file(&ruta);
    }

    #[test]
    fn con_texto_completo_almenos_sale_redactado() {
        let ruta = tmp("log2");
        let _ = std::fs::remove_file(&ruta);
        let mut b = Bitacora::nueva(BitacoraConfig {
            ruta: Some(ruta.clone()),
            texto_completo: true,
            ..Default::default()
        })
        .unwrap();
        let mut r = Registro::nuevo("hatboo");
        r.muestra = Some("mi clave sk-proj-AAAAAAAAAAAAAAAAAAAAAA está aquí".into());
        b.registrar(r).unwrap();
        let leido = std::fs::read_to_string(&ruta).unwrap();
        assert!(!leido.contains("AAAAAAAAAAAAAAAAAAAAAA"), "{leido}");
        assert!(leido.contains("sk-proj-***"));
        let _ = std::fs::remove_file(&ruta);
    }

    #[test]
    fn rota_cuando_pesa() {
        let ruta = tmp("log3");
        let _ = std::fs::remove_file(&ruta);
        let mut b = Bitacora::nueva(BitacoraConfig {
            ruta: Some(ruta.clone()),
            max_mb: 0, // cualquier byte desata la rotación
            max_ficheros: 2,
            texto_completo: false,
        })
        .unwrap();
        b.registrar(Registro::nuevo("hatboo")).unwrap();
        b.registrar(Registro::nuevo("hatboo")).unwrap();
        assert!(std::path::Path::new(&format!("{}.1", ruta.display())).exists());
        let _ = std::fs::remove_file(&ruta);
        let _ = std::fs::remove_file(format!("{}.1", ruta.display()));
    }
}
