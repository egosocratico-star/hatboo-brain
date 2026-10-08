//! El Selector: elige el `ModelId`. El Governor dice qué cabe; el Planner firma.
//! Nunca «el más grande»: se recorre de tier bajo hacia arriba y se respeta el
//! modelo pedido si cabe. Con `respetarModelo` del config, además, el pedido manda
//! aunque no quepa: se falla con las cifras del Governor y no se firma otro.

use crate::api::error::BrainError;
use crate::api::request::BrainRequest;
use crate::api::vocab::{ExecutionPolicy, Level, ModelId, ModelTarget};
use crate::decision::DecisionResult;
use crate::models::{ModelInfo, ModelKind, Registry};
use crate::resources::{Consejo, Governor};

#[derive(Debug, Clone, PartialEq)]
pub struct Eleccion {
    pub modelo: ModelInfo,
    pub consejo: Consejo,
    /// Lo que el usuario ve en el panel «por qué».
    pub porque: String,
    /// Cada descartado con su motivo concreto.
    pub descartados: Vec<(ModelId, String)>,
}

/// Candidatos en el orden en que se prueban: el pedido primero, luego tier
/// ascendente y, empate, el de menos RAM medida.
fn orden_candidatos<'a>(
    req: &BrainRequest,
    d: &DecisionResult,
    registry: &'a Registry,
) -> Vec<&'a ModelInfo> {
    let mut v = registry.elegibles(d.level, d.necesita_tools(), req.policies);
    v.sort_by(|a, b| {
        let pedido_a = req.preferred_model.as_deref() == Some(a.id.as_str());
        let pedido_b = req.preferred_model.as_deref() == Some(b.id.as_str());
        pedido_b
            .cmp(&pedido_a)
            .then(a.tier.cmp(&b.tier))
            .then_with(|| ram_conocida(a, d.level).cmp(&ram_conocida(b, d.level)))
    });
    // Un `model_target` de perfil restringe la lista; si no queda nada, se vuelve
    // a la lista completa: mejor un perfil no pedido que no responder.
    if let ModelTarget::Perfil(p) = d.model_target {
        let filtrado: Vec<&ModelInfo> = v.iter().copied().filter(|m| m.profile == p).collect();
        if !filtrado.is_empty() {
            return filtrado;
        }
    }
    if let ModelTarget::Tier(t) = d.model_target {
        let filtrado: Vec<&ModelInfo> = v.iter().copied().filter(|m| m.tier >= t).collect();
        if !filtrado.is_empty() {
            return filtrado;
        }
    }
    v
}

fn ram_conocida(m: &ModelInfo, nivel: Level) -> u64 {
    m.ram_para(nivel.num_ctx_minimo()).unwrap_or(u64::MAX)
}

/// Por qué un modelo que está en el registry no entra en la lista de candidatos.
/// Son los mismos filtros de `Registry::elegibles` y de `orden_candidatos`, dichos
/// uno a uno: «no cumple» a secas no le sirve a nadie, y quien elige el modelo tiene
/// que poder corregirlo (bajar el nivel, cambiar el modelo, soltar la policy).
fn no_cumple(m: &ModelInfo, d: &DecisionResult, policy: ExecutionPolicy) -> Vec<String> {
    let mut v = Vec::new();
    if m.kind != ModelKind::Generativo {
        v.push("es un modelo de decisión: no responde al usuario".to_string());
    }
    if m.max_ctx < d.level.num_ctx_suelo() {
        v.push(format!(
            "su contexto máximo ({}) no llega a los {} con los que {:?} puede firmar algo",
            m.max_ctx,
            d.level.num_ctx_suelo(),
            d.level
        ));
    }
    if d.necesita_tools() && !m.supports_tools {
        v.push("el contrato del Plan pide tools y el modelo no las declara".to_string());
    }
    match policy {
        ExecutionPolicy::LocalOnly | ExecutionPolicy::LocalPreferred if !m.local => {
            v.push(format!("la policy {policy:?} no admite un modelo de nube"))
        }
        ExecutionPolicy::CloudOnly if m.local => {
            v.push(format!("la policy {policy:?} no admite un modelo local"))
        }
        _ => {}
    }
    match d.model_target {
        ModelTarget::Perfil(p) if m.profile != p => v.push(format!(
            "el Plan pidió el perfil {p:?} y este modelo es {:?}",
            m.profile
        )),
        ModelTarget::Tier(t) if m.tier < t => {
            v.push(format!("el Plan pidió tier ≥ {t} y este es tier {}", m.tier))
        }
        _ => {}
    }
    v
}

/// La negativa de `respetarModelo`: el producto eligió y su modelo no puede correr.
/// Se falla **antes** de candidatear a los demás, que es justo lo que la opción
/// quita. Las cifras son las del consejo que ya dio el Governor —el margen con el
/// que comprobó, no el de la spec—, y la frase es la suya: decir otras cifras aquí
/// sería explicar un rechazo que no se hizo.
fn negativa(
    m: &ModelInfo,
    consejo: &Consejo,
    d: &DecisionResult,
    policy: ExecutionPolicy,
) -> BrainError {
    let mut porque = Vec::new();
    if !consejo.cabe {
        porque.push(consejo.porque.clone());
    }
    let sin_cumplir = no_cumple(m, d, policy);
    if !sin_cumplir.is_empty() {
        // La frase del Governor ya nombra al modelo; las cláusulas de capacidad van
        // detrás, y solo llevan el nombre delante cuando el Governor no habló porque
        // el modelo sí cabía. El aviso tiene que nombrar siempre al pedido.
        let l = sin_cumplir.join(" · ");
        porque.push(if porque.is_empty() {
            format!("«{}» no cumple lo que pide este Plan: {l}", m.id)
        } else {
            l
        });
    }
    BrainError::ModeloPedidoNoCorre {
        modelo: m.id.clone(),
        motivo: porque.join(" · "),
        needed_mb: consejo
            .ram_mb
            .map(|r| (r + consejo.margen_mb) as u32)
            .unwrap_or(0),
        margen_mb: consejo.margen_mb as u32,
        free_mb: consejo.libre_mb.unwrap_or(0) as u32,
    }
}

pub fn elegir(
    req: &BrainRequest,
    d: &DecisionResult,
    registry: &Registry,
    governor: &Governor,
    sonda: &dyn crate::resources::ResourceProbe,
    respetar_modelo: bool,
) -> Result<Eleccion, BrainError> {
    if registry.modelos.is_empty() {
        return Err(BrainError::Config(crate::config::ConfigError::SinBackend));
    }
    let candidatos = orden_candidatos(req, d, registry);

    // `respetarModelo`: se comprueba el pedido antes de mirar a los demás, y se
    // comprueba aunque no esté en la lista de candidatos —si el Plan le pide tools
    // que no declara, tampoco está, y el aviso tiene que nombrar su modelo—.
    if respetar_modelo {
        if let Some(pedido) = req.preferred_model.as_deref() {
            if let Some(m) = registry.find(pedido) {
                let consejo = governor.aconsejar(sonda, m, d.level);
                let en_candidatos = candidatos.iter().any(|c| c.id == m.id);
                if !consejo.cabe || !en_candidatos {
                    return Err(negativa(m, &consejo, d, req.policies));
                }
            }
        }
    }

    if candidatos.is_empty() {
        return Err(BrainError::NoEligibleModel);
    }

    // El modelo pedido se prueba primero aunque el Governor no lo vea cómodo:
    // §II.8 dice que se respeta si cabe, y para saber si cabe hay que preguntarlo.
    let mut descartados = Vec::new();
    let mut mas_barato: Option<(u64, Consejo)> = None;

    for m in &candidatos {
        let consejo = governor.aconsejar(sonda, m, d.level);
        if consejo.cabe {
            let pedido = req.preferred_model.as_deref() == Some(m.id.as_str());
            let porque = if pedido {
                // El nombre del modelo y la cuenta de si cabe ya van en `consejo.porque`
                // (lo redacta el Governor). Aquí no se repite ninguno de los dos:
                // ««q» es el que elegiste y cabe: «q» cabe a 2048…» se leyó en la nota
                // de un chat el 07-10, y era la misma palabra dos veces.
                format!("Elegiste tú este modelo: {}", consejo.porque)
            } else if let Some(pref) = &req.preferred_model {
                format!(
                    "«{pref}» no cabe ahora; sigue «{}» ({})",
                    m.id, consejo.porque
                )
            } else {
                format!("«{}» es el más barato que cumple: {}", m.id, consejo.porque)
            };
            return Ok(Eleccion {
                modelo: (*m).clone(),
                consejo,
                porque,
                descartados,
            });
        }
        descartados.push((m.id.clone(), consejo.porque.clone()));
        // Para el «no cabe» se dan las cifras del descarte que menos pedía: es lo
        // único que podría haber funcionado, y decir la más alta asustaría sin causa.
        if let Some(ram) = consejo.ram_mb {
            if mas_barato
                .as_ref()
                .map(|(mejor, _)| ram < *mejor)
                .unwrap_or(true)
            {
                mas_barato = Some((ram, consejo));
            }
        }
    }

    // Con el Governor de hoy aquí solo se llega sin datos: sonda ciega, modelo sin
    // medir, o un techo de contexto por debajo del suelo del nivel. Las cifras del
    // descarte que menos pedía se dan si existen —la más alta asustaría sin causa,
    // y un `0 MB` sería inventar—; sin ninguna, el error es `NoEligibleModel`, que
    // es lo que pasó: no es que no quepa, es que no lo sabemos.
    match mas_barato {
        Some((ram, c)) => Err(BrainError::ResourceExhausted {
            // El margen del consejo, no `config.margen_mb`: es el que estrecha el
            // Governor cuando conoce el total, y la suma que se le enseña al
            // usuario tiene que ser la que produjo el descarte.
            needed_mb: (ram + c.margen_mb) as u32,
            free_mb: c.libre_mb.unwrap_or(0) as u32,
        }),
        None => Err(BrainError::NoEligibleModel),
    }
}

/// Si el modelo pedido no está ni en el registry: error tipado con mensaje
/// accionable, no un selector silencioso.
pub fn pedir_no_instalado(id: &str) -> BrainError {
    BrainError::Config(crate::config::ConfigError::ModeloNoInstalado(id.to_string()))
}

/// ¿La policy permite mirar APIs si no queda nada local? `local_only` no, nunca.
pub fn puede_salir_del_equipo(policy: ExecutionPolicy) -> bool {
    !matches!(policy, ExecutionPolicy::LocalOnly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{
        Confidence, DecisionSource, ExecutionTarget, Intent, OutputContract, Profile, Risk,
        VerificationMode,
    };
    use crate::models::ModelKind;
    use crate::resources::{GovernorConfig, SondaFija};
    use std::collections::BTreeMap;

    fn m(id: &str, tier: u8, ram: u64, local: bool, tools: bool) -> ModelInfo {
        let mut map: BTreeMap<u32, u64> = BTreeMap::new();
        map.insert(2048, ram);
        map.insert(4096, ram + 200);
        map.insert(8192, ram + 800);
        ModelInfo {
            id: id.into(),
            provider: if local { "ollama".into() } else { "openai".into() },
            local,
            kind: ModelKind::Generativo,
            profile: if ram > 3000 { Profile::Small } else { Profile::Nano },
            tier,
            ram_mb_by_ctx: map,
            max_ctx: 131072,
            strengths: vec![],
            supports_tools: tools,
            supports_thinking: false,
            supports_vision: false,
            structured_output: false,
            disco_mb: None,
        }
    }

    fn decision(level: Level, tools: Vec<String>) -> DecisionResult {
        DecisionResult {
            intent: Intent::Ask,
            level,
            risk: Risk::Low,
            skip_generative: false,
            execution_target: ExecutionTarget::Local,
            model_target: ModelTarget::Indistinto,
            tools,
            output_contract: OutputContract::Texto,
            verification: VerificationMode::Formato,
            confidence: Confidence(1.0),
            source: DecisionSource::Reglas,
            por_que: "prueba".into(),
            salida_directa: None,
            lectura_directa: None,
        }
    }

    fn reg() -> Registry {
        Registry::nuevo(vec![
            m("grande:9b", 3, 6000, true, true),
            m("nano:0.8b", 1, 900, true, false),
            m("medio:4b", 2, 3000, true, true),
            m("nube:gpt", 2, 0, false, true),
        ])
    }

    fn gov() -> Governor {
        Governor::nuevo(GovernorConfig::default())
    }

    #[test]
    fn respeta_el_pedido_si_cabe_y_lo_dice() {
        let r = reg();
        let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("medio:4b");
        let sonda = SondaFija {
            libre: Some(8000),
            ..Default::default()
        };
        let e = elegir(
            &req,
            &decision(Level::N1, vec![]),
            &r,
            &gov(),
            &sonda,
            false,
        )
        .unwrap();
        assert_eq!(e.modelo.id, "medio:4b");
        assert!(e.porque.contains("Elegiste tú este modelo"), "{}", e.porque);
        // Ni el nombre ni el verbo dos veces: la cuenta la redacta el Governor una
        // sola vez (««medio:4b» cabe a 2048: …») y el Selector no la repite. Era
        // ««q» es el que elegiste y cabe: «q» cabe a 2048…», leído en la nota de un
        // chat el 07-10.
        assert_eq!(e.porque.matches("medio:4b").count(), 1, "{}", e.porque);
        assert_eq!(e.porque.matches("cabe").count(), 1, "{}", e.porque);
    }

    #[test]
    fn si_no_cabe_con_margen_se_corre_el_pedido_apurado() {
        let r = reg();
        let req = BrainRequest::nuevo("hatboo", "work", "arregla x")
            .con_modelo("grande:9b");
        let sonda = SondaFija {
            libre: Some(5000),
            ..Default::default()
        };
        let e = elegir(
            &req,
            &decision(Level::N2, vec!["read_file".into()]),
            &r,
            &gov(),
            &sonda,
            false,
        )
        .unwrap();
        // Antes esto devolvía `medio:4b` con un «no cabe» en el motivo: el Governor
        // usaba la RAM como puerta y el usuario se iba a otro modelo sin pedirlo.
        // Hoy el que eligió se corre con la ventana apretada y el desvío no existe.
        assert_eq!(e.modelo.id, "grande:9b", "{}", e.porque);
        assert_eq!(e.consejo.ajuste, crate::resources::Ajuste::SinMargen);
        assert!(e.descartados.is_empty(), "{:?}", e.descartados);
    }

    #[test]
    fn sin_preferencia_gana_el_mas_barato_que_cumple() {
        let r = reg();
        let req = BrainRequest::nuevo("hatboo", "work", "usa tools");
        let sonda = SondaFija {
            libre: Some(8000),
            ..Default::default()
        };
        let e = elegir(
            &req,
            &decision(Level::N2, vec!["read_file".into()]),
            &r,
            &gov(),
            &sonda,
            false,
        )
        .unwrap();
        // N2 pide tools: nano:0.8b no las declara, así que gana el tier 2 local.
        assert_eq!(e.modelo.id, "medio:4b", "{:?}", e.descartados);
        assert_eq!(e.modelo.tier, 2);
    }

    #[test]
    fn local_only_nunca_salta_a_la_api() {
        let r = reg();
        let req = BrainRequest::nuevo("hatboo", "work", "usa tools")
            .con_policy(ExecutionPolicy::LocalOnly);
        // RAM justa: solo cabrían los locales pequeños, que no tienen tools. La
        // policy sigue ganando: se corre el local apurando la ventana, y la nube
        // ni se nombra.
        let sonda = SondaFija {
            libre: Some(1200),
            ..Default::default()
        };
        let e = elegir(
            &req,
            &decision(Level::N2, vec!["read_file".into()]),
            &r,
            &gov(),
            &sonda,
            false,
        )
        .unwrap();
        assert!(e.modelo.local, "{:?}", e.modelo.id);
        assert_eq!(e.consejo.ajuste, crate::resources::Ajuste::SinMargen);
        assert!(!format!("{e:?}").contains("nube"), "local_only no propone la nube");
    }

    #[test]
    fn sin_registry_no_hay_cerebro() {
        let req = BrainRequest::nuevo("hatboo", "chat", "hola");
        let e = elegir(
            &req,
            &decision(Level::N1, vec![]),
            &Registry::default(),
            &gov(),
            &SondaFija::default(),
            false,
        );
        assert!(matches!(
            e,
            Err(BrainError::Config(crate::config::ConfigError::SinBackend))
        ));
    }

    #[test]
    fn la_maquina_sin_sitio_se_corre_igual_y_dice_las_cifras() {
        let r = reg();
        let req = BrainRequest::nuevo("hatboo", "chat", "hola").con_modelo("nano:0.8b");
        let sonda = SondaFija {
            libre: Some(300),
            ..Default::default()
        };
        let e = elegir(&req, &decision(Level::N0, vec![]), &r, &gov(), &sonda, false).unwrap();
        // 300 MB libres para un modelo de 900 + 1500 de margen: ni apretando la
        // ventana hay forma de cuadrar la cuenta, y aun así el usuario recibe su
        // respuesta (o el error de Ollama al cargar, que es la verdad del momento).
        assert_eq!(e.consejo.ajuste, crate::resources::Ajuste::SinMargen);
        assert_eq!(e.consejo.num_ctx, 512);
        let p = &e.consejo.porque;
        assert!(p.contains("900") && p.contains("300") && p.contains("margen 1500"), "{p}");
    }
}
