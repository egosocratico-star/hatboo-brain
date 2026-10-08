//! El consejo del Governor. Números de §11 del Canon y de lo medido en Fase 0:
//! margen 1,5 GB, `num_ctx` en valores fijos, y **3,7 s por cada cambio de
//! `num_ctx`** porque Ollama rehace el grafo y el KV.

use super::ResourceProbe;
use crate::api::vocab::{KeepAlive, Level};
use crate::models::{ModelInfo, ModeloCargado};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GovernorConfig {
    /// Margen de RAM libre que hay que dejar sin tocar. 1500 MB por defecto.
    pub margen_mb: u64,
    /// Conjunto de `num_ctx` válidos. Ampliarlo es config por modelo (§15.5).
    ///
    /// Los peldaños de abajo (512 y 1024) no son un detalle: son lo que permite al
    /// Governor **apretar la ventana en vez de negar el turno** cuando la máquina
    /// va justa. Requieren RAM medida en esos ctx —`ram_para` no interpola, §X del
    /// Canon—, y sin esa medición solo se usan por la vía de `SinMargen`.
    pub ctx_permitidos: Vec<u32>,
    /// Perfil ligero bajo presión: baja el contexto pedido un escalón.
    pub perfil_ligero_presion: bool,
    /// A partir de qué batería (o CPU alta) se activa el perfil ligero.
    pub presion_bateria: Option<f32>,
    pub presion_cpu: Option<f32>,
    /// Cuánto cuesta cambiar num_ctx. Medido: 3700 ms.
    pub recarga_por_ctx_ms: u64,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        GovernorConfig {
            margen_mb: 1500,
            ctx_permitidos: vec![512, 1024, 2048, 4096, 8192],
            perfil_ligero_presion: true,
            presion_bateria: Some(0.35),
            presion_cpu: Some(0.9),
            recarga_por_ctx_ms: 3700,
        }
    }
}

/// Qué tuvo que ceder el consejo para que el turno saliera adelante. `Ninguno` es
/// el caso limpio: el modelo cabe donde pide el nivel y con el margen de la spec.
///
/// Existe porque el Governor fue diseñado como puerta (`cabe: false` → nadie corre
/// el modelo) y en una máquina de 8 GB eso convertía un saludo en un rechazo por
/// 114 MB que un saludo no necesita. Aquí la puerta desaparece: **ningún turno se
/// niega por RAM**. Lo que este tipo permite es decir cuánto se apretó para no
/// negarlo, que es lo que el panel y el registro tienen que enseñar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ajuste {
    /// Ventana y margen como pide el nivel.
    #[default]
    Ninguno,
    /// Se corre con menos contexto del que pide el nivel: el modelo ve menos
    /// turnos y menos archivos por turno, pero contesta.
    VentanaCorta,
    /// Ningún peldaño cabía con margen: se corre en el más pequeño que el modelo
    /// admite y se dicen las cifras que fallaron. Es el caso en el que Ollama
    /// puede no llegar a cargar, y entonces el error es suyo, no nuestro.
    SinMargen,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consejo {
    /// El `num_ctx` recomendado. 0 = no se puede firmar nada.
    pub num_ctx: u32,
    pub cabe: bool,
    /// RAM que ocuparía el modelo a ese `num_ctx`, si la tenemos medida.
    pub ram_mb: Option<u64>,
    /// Cuánta RAM queda según la sonda. `None` = la sonda no lo sabe.
    pub libre_mb: Option<u64>,
    /// El modelo ya está residente a este ctx: no hay que cargarlo.
    pub residente: bool,
    /// Cambiar el ctx de algo que ya está cargado cuesta una recarga.
    pub recarga_ms: Option<u64>,
    /// Perfíl ligero activo (batería o CPU alta).
    pub presion: bool,
    /// El margen **con el que se comprobó** este consejo. No es `config.margen_mb`
    /// cuando la sonda conoce el total (ahí se estrecha a la octava parte): el
    /// Selector y el aviso de «no cabe» tienen que citar este número, porque es el
    /// que decidión. Mientras citaron el de la spec, se reportaba «hacen falta
    /// 2400» por una comprobación que pedía 1956.
    #[serde(default)]
    pub margen_mb: u64,
    /// Lo que hay cargado, para el panel y para decidir expulsiones.
    pub cargados: Vec<ModeloCargado>,
    /// Qué se cedió para que este turno se corra. Se registra y se enseña.
    #[serde(default)]
    pub ajuste: Ajuste,
    /// Por qué este consejo. Se muestra.
    pub porque: String,
}

pub struct Governor {
    pub config: GovernorConfig,
}

/// Cuánto conviene dejar el modelo residente **después** de responder, según lo que
/// costó que cupiera. Es la otra mitad de la sincronía con el modelo: expulsarlo y
/// volver a cargarlo cuesta 7,3 s medidos (2,4–6,7 s la carga fría sola), así que en
/// una charla seguida dejarlo aparcado es lo que quita tiempo; y en una máquina que
/// iba justa, dejar 1,1 GB estacionados quince minutos es lo que provoca el
/// estrangulamiento siguiente. Con presión de batería o CPU se acorta siempre.
///
/// No lo decide el Plan ni el producto: lo decide el módulo de recursos, que es el
/// único que sabe con qué ajuste salió el consejo.
pub fn mantener(ajuste: Ajuste, presion: bool) -> KeepAlive {
    let base = match ajuste {
        // Cabía con margen: la máquina va holgada, que se quede.
        Ajuste::Ninguno => 900,
        // Hubo que bajar de peldaño: media hora no, pero un cuarto de hora tampoco.
        Ajuste::VentanaCorta => 300,
        // Se gastó el colchón: un minuto de seguimiento y se devuelve la RAM.
        Ajuste::SinMargen => 60,
    };
    KeepAlive::Segundos(if presion { base.min(60) } else { base })
}

impl Governor {
    pub fn nuevo(config: GovernorConfig) -> Self {
        let mut g = Governor { config };
        g.config.ctx_permitidos.sort_unstable();
        g.config.ctx_permitidos.dedup();
        g
    }

    pub fn por_defecto() -> Self {
        Governor::nuevo(GovernorConfig::default())
    }

    fn bajo_presion(&self, sonda: &dyn ResourceProbe) -> bool {
        if !self.config.perfil_ligero_presion {
            return false;
        }
        let bateria = self
            .config
            .presion_bateria
            .zip(sonda.bateria())
            .map(|(lim, b)| b <= lim)
            .unwrap_or(false);
        let cpu = self
            .config
            .presion_cpu
            .zip(sonda.cpu())
            .map(|(lim, c)| c >= lim)
            .unwrap_or(false);
        bateria || cpu
    }

    /// El margen de §11 son 1500 MB, pero en una máquina de 8,45 GB eso hace
    /// que NUNCA quepa un modelo local: con 1,6 GB libres y un modelo de 878,
    /// pedir 2378 es rechazarlo siempre. Donde se conoce el total, el margen
    /// se estrecha a la octava parte (1056 MB aquí) y se sigue dejando constar
    /// en el aviso; sin ese dato, manda el de la spec.
    pub fn margen_efectivo(&self, total_mb: Option<u64>) -> u64 {
        match total_mb {
            Some(t) if t > 0 => self.config.margen_mb.min(t / 8),
            _ => self.config.margen_mb,
        }
    }

    /// ¿Cabe el modelo pedido, y a qué `num_ctx`? Recorre la escalera de abajo
    /// arriba: el contexto más pequeño que cumpla el nivel gana, porque cada
    /// escalón cuesta RAM y a veces una recarga.
    ///
    /// **No es una puerta.** Lo que pide el nivel (`num_ctx_minimo`) es preferencia:
    /// si ningún peldaño de esa zona cabe se baja por debajo de él
    /// (`Ajuste::VentanaCorta`, de caro a barato porque ahí cada token de ventana
    /// vale), y si aun así la máquina no llega se corre en el peldaño más pequeño
    /// que el modelo admite (`Ajuste::SinMargen`) diciendo las cifras que fallaron.
    /// Medido el 05-10: con el 2048 como suelo duro, un saludo con
    /// `qwen2.5-coder:1.5b` se rechazaba por 114 MB de RAM que un saludo no
    /// necesita. Lo único que sigue negando un turno es no saber nada del modelo.
    pub fn aconsejar(&self, sonda: &dyn ResourceProbe, modelo: &ModelInfo, nivel: Level) -> Consejo {
        let libre = sonda.libre_mb();
        let cargados = sonda.cargados();
        let residente_actual = cargados.iter().find(|c| c.id == modelo.id).map(|c| c.num_ctx);
        let presion = self.bajo_presion(sonda);

        let minimo = nivel.num_ctx_minimo();
        let margen = self.margen_efectivo(sonda.total_mb());
        let nota_margen = if margen != self.config.margen_mb {
            format!(
                " (margen {margen}, estrechado de los {} de la spec a la octava parte del total)",
                self.config.margen_mb
            )
        } else {
            format!(" (margen {margen})")
        };
        let escalera: Vec<u32> = self
            .config
            .ctx_permitidos
            .iter()
            .filter(|c| **c >= nivel.num_ctx_suelo())
            .filter(|c| **c <= modelo.max_ctx)
            .copied()
            .collect();

        if escalera.is_empty() {
            return Consejo {
                num_ctx: 0,
                cabe: false,
                ram_mb: None,
                libre_mb: libre,
                residente: false,
                recarga_ms: None,
                presion,
                margen_mb: margen,
                cargados,
                ajuste: Ajuste::Ninguno,
                porque: format!(
                    "{} no admite ningún num_ctx del conjunto permitido para {nivel:?} (máx. declarado {}, y el nivel no firma por debajo de {})",
                    modelo.id,
                    modelo.max_ctx,
                    nivel.num_ctx_suelo()
                ),
            };
        }

        // Histéresis: con el modelo residente a una ventana que ya cumple el nivel
        // no se le mueve, aunque haya un peldaño más barato quepa. Cada cambio de
        // `num_ctx` son 3700 ms rehaciendo grafo y KV (7300 si antes hay que
        // expulsar), y esa RAM ya está pagada. En la corrida F del 05-10 esto costó
        // 24 recargas y un +344 % de duración sobre el mismo modelo.
        if let Some(ctx) = residente_actual {
            if ctx >= minimo && escalera.contains(&ctx) {
                return Consejo {
                    num_ctx: ctx,
                    cabe: true,
                    ram_mb: modelo.ram_para(ctx),
                    libre_mb: libre,
                    residente: true,
                    recarga_ms: Some(0),
                    presion,
                    margen_mb: margen,
                    cargados,
                    ajuste: Ajuste::Ninguno,
                    porque: format!("«{}» ya está residente a {ctx}: no se le mueve", modelo.id),
                };
            }
        }

        // Los peldaños que cumplen el nivel, de barato a caro, y detrás los que no,
        // de caro a barato. Bajo presión se cede el más alto, si queda alguno.
        let arriba: Vec<u32> = escalera.iter().filter(|c| **c >= minimo).copied().collect();
        let mut intentos: Vec<u32> = if presion && arriba.len() > 1 {
            arriba[..arriba.len() - 1].to_vec()
        } else {
            arriba
        };
        intentos.extend(escalera.iter().filter(|c| **c < minimo).rev().copied());

        for &ctx in &intentos {
            let Some(ram) = modelo.ram_para(ctx) else {
                continue;
            };
            let cabe = match libre {
                // Sin dato de RAM no se afirma que quepa: ahí no hay ni para apretar.
                None => false,
                // Si ya está residente a este `num_ctx` no sale RAM nueva: exigir
                // el margen otra vez hacía rechazar el modelo que tenía delante,
                // y expulsarlo y recargarlo cuesta 7,3 s medidos.
                Some(l) => ram + margen <= l || residente_actual == Some(ctx),
            };
            if cabe {
                let residente = residente_actual == Some(ctx);
                let recarga = match residente_actual {
                    Some(otro) if otro != ctx => Some(self.config.recarga_por_ctx_ms),
                    Some(_) => Some(0),
                    None => None,
                };
                let corta = ctx < minimo;
                let mut porque = match (residente, recarga) {
                    (true, _) => format!("«{}» ya está residente a {ctx}", modelo.id),
                    (false, Some(ms)) if ms > 0 => format!(
                        "«{}» cabe a {ctx} ({} MB + {} de margen); cambia de ctx: +{ms} ms de recarga",
                        modelo.id,
                        ram,
                        margen
                    ),
                    _ => format!(
                        "«{}» cabe a {ctx}: {} MB + {} MB de margen sobre {} MB libres",
                        modelo.id,
                        ram,
                        margen,
                        libre.unwrap_or(0)
                    ),
                };
                // Se dice siempre que el nivel pedía más: el panel tiene que poder
                // explicar por qué el modelo acuerda menos de lo que el usuario eligió.
                if corta {
                    porque.push_str(&format!(" · ventana corta: {nivel:?} pedía {minimo}"));
                }
                if modelo.ram_es_estimada(ctx) {
                    porque.push_str(" · RAM estimada por peso en disco");
                }
                return Consejo {
                    num_ctx: ctx,
                    cabe: true,
                    ram_mb: Some(ram),
                    libre_mb: libre,
                    residente,
                    recarga_ms: recarga,
                    presion,
                    margen_mb: margen,
                    cargados,
                    ajuste: if corta { Ajuste::VentanaCorta } else { Ajuste::Ninguno },
                    porque,
                };
            }
        }

        // Nadie cabe con margen. Dos casos en los que no se puede afirmar nada y por
        // tanto no se corre: la sonda no ve la RAM libre, y el modelo no tiene ni una
        // cifra medida ni peso en disco. No son un limitador: es que no hay datos.
        let Some(l) = libre else {
            return Consejo {
                num_ctx: 0,
                cabe: false,
                ram_mb: None,
                libre_mb: None,
                residente: residente_actual.is_some(),
                recarga_ms: None,
                presion,
                margen_mb: margen,
                cargados,
                ajuste: Ajuste::Ninguno,
                porque: format!("no se puede afirmar que «{}» quepa: la sonda no mide RAM libre", modelo.id),
            };
        };
        let menor = escalera[0];
        // La cifra más pequeña que se conoce del modelo, para citar la que de verdad
        // no alcanzó. Si no hay ni una, no hay apriete que decidir: apretar la
        // ventana se decide con números.
        let menor_medido = intentos.iter().find_map(|c| modelo.ram_para(*c).map(|r| (*c, r)));
        let Some((ctx_mas_bajo, ram_mas_baja)) = menor_medido else {
            return Consejo {
                num_ctx: 0,
                cabe: false,
                ram_mb: None,
                libre_mb: Some(l),
                residente: residente_actual.is_some(),
                recarga_ms: None,
                presion,
                margen_mb: margen,
                cargados,
                ajuste: Ajuste::Ninguno,
                porque: format!("«{}» no tiene RAM medida para ningún num_ctx del conjunto", modelo.id),
            };
        };
        // Sin margen se corre en el **primer peldaño que ya sirve para el nivel**:
        // los que lo cumplen, de barato a caro, y solo después los que no, de caro a
        // barato. No es lo mismo que «el más pequeño que quepa» —medido el 05-10, la
        // diferencia entre 512 y 2048 son 45 MB y 1536 tokens, y cambiar ventana por
        // 45 MB de holgura es un mal cambio—, ni «el más grande que quepa»: con 2000
        // libres `qwen3.5:0.8b` cabría a 8192 (1863 MB) y dejaría 137 MB de colchón
        // por un contexto que un N0 no va a usar.
        let sin_margen = intentos
            .iter()
            .find_map(|c| modelo.ram_para(*c).map(|r| (*c, r)).filter(|(_, r)| *r <= l));
        let (num_ctx, ctx_citado, ram_citada) = match sin_margen {
            Some((c, r)) => (c, c, r),
            // Ni sin margen cabe lo medido: se aprieta hasta el suelo y que Ollama
            // diga. Es el único caso en el que se corre sin cifra delante.
            None => (menor, ctx_mas_bajo, ram_mas_baja),
        };
        let mut porque = format!(
            "ningún peldaño cabe con margen: «{}» mide {} MB a {ctx_citado} y hay {l} libres{nota_margen}. Se corre a {num_ctx}{}",
            modelo.id,
            ram_citada,
            if num_ctx == ctx_citado {
                " sin margen"
            } else {
                " sin margen y sin medición que lo respalde"
            }
        );
        if num_ctx < minimo {
            porque.push_str(&format!(" · ventana corta: {nivel:?} pedía {minimo}"));
        }
        // El que dice si la máquina aguanta es Ollama al cargar, no nosotros
        // negándole la respuesta al usuario.
        Consejo {
            num_ctx,
            cabe: true,
            ram_mb: modelo.ram_para(num_ctx),
            libre_mb: Some(l),
            residente: residente_actual == Some(num_ctx),
            recarga_ms: match residente_actual {
                Some(otro) if otro != num_ctx => Some(self.config.recarga_por_ctx_ms),
                Some(_) => Some(0),
                None => None,
            },
            presion,
            margen_mb: margen,
            cargados,
            ajuste: Ajuste::SinMargen,
            porque,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::Profile;
    use crate::models::ModelKind;
    use crate::resources::SondaFija;
    use std::collections::BTreeMap;

    fn modelo(id: &str, ram_2048: u64, ram_8192: u64, max_ctx: u32) -> ModelInfo {
        let mut m: BTreeMap<u32, u64> = BTreeMap::new();
        m.insert(2048, ram_2048);
        m.insert(4096, ram_2048 + (ram_8192 - ram_2048) / 2);
        m.insert(8192, ram_8192);
        ModelInfo {
            id: id.into(),
            provider: "ollama".into(),
            local: true,
            kind: ModelKind::Generativo,
            profile: Profile::Nano,
            tier: 1,
            ram_mb_by_ctx: m,
            max_ctx,
            strengths: vec![],
            supports_tools: true,
            supports_thinking: false,
            supports_vision: false,
            structured_output: false,
            disco_mb: None,
        }
    }

    #[test]
    fn elige_el_ctx_mas_barato_que_cumple_el_nivel() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(4000),
            ..Default::default()
        };
        let m = modelo("gemma3:1b", 878, 1197, 32768);
        let c = g.aconsejar(&sonda, &m, Level::N1);
        assert!(c.cabe);
        assert_eq!(c.num_ctx, 2048, "N1 no pide más contexto del necesario");
        let c2 = g.aconsejar(&sonda, &m, Level::N2);
        assert_eq!(c2.num_ctx, 4096, "N2 exige 4096 de mínimo");
    }

    #[test]
    fn sin_margen_se_corre_apurando_y_lo_dice_concifras() {
        // La máquina del 05-10: modelo de 1645 MB, RAM corta y nadie quiere que un
        // saludo termine en «no hay respuesta». El Governor ya no niega el turno:
        // lo corre en el peldaño más pequeño y enseña la cuenta que no cuadró.
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(1200),
            ..Default::default()
        };
        let c = g.aconsejar(&sonda, &modelo("qwen3:1.7b", 1645, 2419, 40960), Level::N0);
        assert!(c.cabe, "{}", c.porque);
        assert_eq!(c.ajuste, Ajuste::SinMargen);
        assert_eq!(c.num_ctx, 512, "sin margen se aprieta la ventana al máximo");
        assert!(
            c.porque.contains("1645") && c.porque.contains("1200") && c.porque.contains("margen 1500"),
            "{}",
            c.porque
        );
    }

    #[test]
    fn una_ventana_baja_medida_evita_correr_sin_margen() {
        // El caso del rechazo: `qwen2.5-coder:1.5b` mide 1109 MB a 2048 y el margen
        // de Ajustes es 250. Con 1330 libres el 2048 no cuadra (1359) y el 1024 sí
        // (1316), así que el turno sale con ventana corta en vez de ir apurado. La
        // fila de 1024 es la que falta medir en `cerebro/medidas.rs` de Hatboo.
        let mut m = modelo("qwen2.5-coder:1.5b", 1109, 1368, 32768);
        m.ram_mb_by_ctx.insert(1024, 1066);
        let g = Governor::nuevo(GovernorConfig {
            margen_mb: 250,
            ..GovernorConfig::default()
        });
        let sonda = SondaFija {
            libre: Some(1330),
            total: Some(8000),
            ..Default::default()
        };
        let c = g.aconsejar(&sonda, &m, Level::N1);
        assert!(c.cabe, "{}", c.porque);
        assert_eq!(c.num_ctx, 1024);
        assert_eq!(c.ajuste, Ajuste::VentanaCorta);
        assert!(c.porque.contains("ventana corta"), "{}", c.porque);
    }

    #[test]
    fn sin_margen_se_corre_a_la_ventana_del_nivel_no_a_su_suelo() {
        // El caso del rechazo, con sus números reales: `qwen2.5-coder:1.5b` mide
        // 1109 MB a 2048, la sonda ve 1245 libres y el margen de Ajustes es 250.
        // Con margen no cuadra (1359), pero 1109 SÍ caben en 1245: se corre a 2048
        // gastando el colchón. Bajar a 512 para ahorrar los 45 MB que separan los dos
        // peldaños sería cambiar 1536 tokens de ventana por nada.
        let g = Governor::nuevo(GovernorConfig {
            margen_mb: 250,
            ..GovernorConfig::default()
        });
        let sonda = SondaFija {
            libre: Some(1245),
            total: Some(8000),
            ..Default::default()
        };
        let m = modelo("qwen2.5-coder:1.5b", 1109, 1368, 32768);
        let c = g.aconsejar(&sonda, &m, Level::N1);
        assert!(c.cabe, "{}", c.porque);
        assert_eq!(c.ajuste, Ajuste::SinMargen);
        assert_eq!(c.num_ctx, 2048, "la ventana que pide el nivel, no su suelo");
        assert_eq!(c.ram_mb, Some(1109));
    }

    #[test]
    fn lo_residente_que_cumple_el_nivel_no_se_mueve() {
        // Sin histéresis la escalera va de barato a caro y, con 8 GB libres, un N1
        // con el modelo residente a 4096 pedía bajar a 2048: 3700 ms de recarga por
        // ahorrar una RAM que ya estaba pagada. En la corrida F fueron 24.
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(8000),
            cargados: vec![ModeloCargado {
                id: "gemma3:1b".into(),
                ram_mb: 881,
                num_ctx: 4096,
            }],
            ..Default::default()
        };
        let m = modelo("gemma3:1b", 878, 1197, 32768);
        for nivel in [Level::N0, Level::N1, Level::N2] {
            let c = g.aconsejar(&sonda, &m, nivel);
            assert_eq!(c.num_ctx, 4096, "{nivel:?}: se le bajó la ventana a lo residente");
            assert!(c.residente);
            assert_eq!(c.recarga_ms, Some(0), "{nivel:?}: recarga que no existió");
        }
        // Y sí sube cuando el nivel lo pide: la histéresis no es un candado.
        let subido = g.aconsejar(&sonda, &m, Level::N3);
        assert_eq!(subido.num_ctx, 8192);
        assert_eq!(subido.recarga_ms, Some(3700));
    }

    #[test]
    fn un_modelo_de_ctx_corto_baja_la_ventana_en_vez_de_negarse() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(16000),
            ..Default::default()
        };
        let m = modelo("chiquito:1b", 800, 800, 4096);
        let c = g.aconsejar(&sonda, &m, Level::N3);
        assert!(c.cabe, "{}", c.porque);
        assert_eq!(c.num_ctx, 4096, "N3 pide 8192 y el techo del modelo es 4096");
        assert_eq!(c.ajuste, Ajuste::VentanaCorta);
        assert!(c.porque.contains("ventana corta"), "{}", c.porque);
    }

    #[test]
    fn un_modelo_que_no_llega_al_suelo_del_nivel_si_se_niega() {
        // La única negativa física que queda: por debajo del suelo no hay plan que
        // firmar, así que correr sería mentirle al usuario con una respuesta vacía.
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(16000),
            ..Default::default()
        };
        let m = modelo("mini:0.5b", 400, 400, 256);
        let c = g.aconsejar(&sonda, &m, Level::N2);
        assert!(!c.cabe);
        assert_eq!(c.num_ctx, 0);
        assert!(c.porque.contains("máx. declarado 256"), "{}", c.porque);
    }

    #[test]
    fn sin_sonda_no_se_afirma_que_quepa() {
        let g = Governor::por_defecto();
        let c = g.aconsejar(&crate::resources::SinSonda, &modelo("a:1b", 900, 1200, 32768), Level::N0);
        assert!(!c.cabe);
        assert!(c.porque.contains("no mide RAM"), "{}", c.porque);
    }

    #[test]
    fn cambiar_de_ctx_cuesta_la_recarga_medida() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(8000),
            cargados: vec![ModeloCargado {
                id: "gemma3:1b".into(),
                ram_mb: 878,
                num_ctx: 2048,
            }],
            ..Default::default()
        };
        let m = modelo("gemma3:1b", 878, 1197, 32768);
        let mismo = g.aconsejar(&sonda, &m, Level::N0);
        assert!(mismo.residente);
        assert_eq!(mismo.recarga_ms, Some(0));
        let subido = g.aconsejar(&sonda, &m, Level::N3);
        assert_eq!(subido.recarga_ms, Some(3700), "cambiar ctx recarga");
        assert!(subido.porque.contains("recarga"));
    }

    #[test]
    fn bateria_baja_baja_un_escalon_el_contexto() {
        let g = Governor::por_defecto();
        let sonda = SondaFija {
            libre: Some(8000),
            bateria: Some(0.2),
            ..Default::default()
        };
        let m = modelo("gemma3:1b", 878, 1197, 32768);
        let c = g.aconsejar(&sonda, &m, Level::N2);
        assert!(c.presion);
        assert_eq!(c.num_ctx, 4096, "con presión se baja a N2 su mínimo, no a 2048");
        let c3 = g.aconsejar(&sonda, &m, Level::N3);
        // N3 solo tiene un escalón válido (8192): si no hay escalón que ceder, se queda.
        assert_eq!(c3.num_ctx, 8192);
    }

    #[test]
    fn lo_apurado_devuelve_la_memoria_antes_que_lo_holgado() {
        // La carga fría cuesta 2,4–6,7 s y expulsar+cargar 7,3 s, así que una charla
        // seguida debe dejar el modelo puesto; pero si el turno salió gastando el
        // margen, tener 1,1 GB aparcados quince minutos es el estrangulamiento
        // siguiente. Presión de batería o CPU acorta siempre.
        assert_eq!(mantener(Ajuste::Ninguno, false), KeepAlive::Segundos(900));
        assert_eq!(mantener(Ajuste::VentanaCorta, false), KeepAlive::Segundos(300));
        assert_eq!(mantener(Ajuste::SinMargen, false), KeepAlive::Segundos(60));
        assert_eq!(mantener(Ajuste::Ninguno, true), KeepAlive::Segundos(60));
    }

    #[test]
    fn un_turno_apurado_devuelve_la_memoria_pronto() {
        // No basta con que la función exista: hay que comprobar que lo apurado sale
        // apurado, porque es el caso en el que dejar el modelo puesto estrangula.
        let g = Governor::nuevo(GovernorConfig {
            margen_mb: 250,
            ..GovernorConfig::default()
        });
        let sonda = SondaFija {
            libre: Some(300),
            total: Some(8000),
            ..Default::default()
        };
        let c = g.aconsejar(&sonda, &modelo("nano:1b", 900, 1200, 32768), Level::N0);
        assert_eq!(c.ajuste, Ajuste::SinMargen);
        assert_eq!(mantener(c.ajuste, c.presion), KeepAlive::Segundos(60));
    }
}
