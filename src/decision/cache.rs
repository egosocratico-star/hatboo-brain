//! Caché de **decisiones**, nunca de respuestas (§V y §VII del Canon: la caché de
//! prosa sería memoria entre conversaciones, que está prohibida).

use super::DecisionResult;
use crate::api::request::BrainRequest;
use crate::observability::hash;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct Entrada {
    decision: DecisionResult,
    a: Instant,
}

/// La caché caduca y se agota: sin TTL, una regla corregida seguiría sirviendo
/// decisiones viejas para siempre.
#[derive(Debug, Clone)]
pub struct CacheDecisiones {
    entradas: HashMap<String, Entrada>,
    max: usize,
    ttl: Duration,
    golpes: u64,
    fallos: u64,
}

impl Default for CacheDecisiones {
    fn default() -> Self {
        CacheDecisiones::nueva(256, Duration::from_secs(900))
    }
}

impl CacheDecisiones {
    pub fn nueva(max: usize, ttl: Duration) -> Self {
        CacheDecisiones {
            entradas: HashMap::new(),
            max: max.max(1),
            ttl,
            golpes: 0,
            fallos: 0,
        }
    }

    /// Firma de todo lo que puede cambiar una decisión: mensaje normalizado, modo,
    /// producto, tools, policy, modelo pedido y hechos del proyecto. Si cambia
    /// cualquiera, la clave es otra y la entrada vieja no sirve.
    pub fn clave(req: &BrainRequest) -> String {
        let mut partes = Vec::with_capacity(8);
        partes.push(crate::decision::fast_path::normalizar(&req.message));
        partes.push(req.product.clone());
        partes.push(req.mode.clone());
        partes.push(
            req.preferred_model
                .clone()
                .unwrap_or_else(|| "-".into()),
        );
        partes.push(format!("{:?}", req.policies));
        partes.push(format!("{:?}", req.approval_level));
        let mut tools: Vec<String> = req.tools.ids();
        tools.sort();
        partes.push(tools.join(","));
        match &req.project {
            Some(p) => partes.push(format!(
                "{}|{}|{}|{}|{}",
                p.root, p.has_tests, p.has_lint, p.has_build, p.language.clone().unwrap_or_default()
            )),
            None => partes.push("-".into()),
        }
        partes.push(format!("{:?}", req.thinking_ceiling));
        // Lo que el producto añade después de un fallo también decide: si no
        // entra en la clave, el mismo mensaje reenviado con riesgo High o con un
        // fallo encima se come la decisión vieja durante todo el TTL.
        partes.push(format!("{:?}", req.risk_hint));
        partes.push(match req.session_failures.last() {
            Some(f) => format!("{f:?}"),
            None => "-".into(),
        });
        hash::fnv1a64(&partes.join("\u{1}"))
    }

    pub fn obtener(&mut self, clave: &str) -> Option<DecisionResult> {
        match self.entradas.get(clave) {
            Some(e) if e.a.elapsed() <= self.ttl => {
                self.golpes += 1;
                Some(e.decision.clone())
            }
            Some(_) => {
                self.entradas.remove(clave);
                self.fallos += 1;
                None
            }
            None => {
                self.fallos += 1;
                None
            }
        }
    }

    /// No se cachea nada con duda: una decisión insegura tiene que volver a
    /// pasar por las reglas, no repetirse desde la memoria.
    pub fn guardar(&mut self, clave: &str, decision: &DecisionResult) {
        if decision.confidence.hay_duda() || decision.source == crate::api::vocab::DecisionSource::Cache {
            return;
        }
        if self.entradas.len() >= self.max {
            // Sin LRU completo: se tira la más antigua. Es un cache, no un almacén.
            if let Some(vieja) = self
                .entradas
                .iter()
                .min_by_key(|(_, e)| e.a)
                .map(|(k, _)| k.clone())
            {
                self.entradas.remove(&vieja);
            }
        }
        self.entradas.insert(
            clave.to_string(),
            Entrada {
                decision: decision.clone(),
                a: Instant::now(),
            },
        );
    }

    pub fn size(&self) -> usize {
        self.entradas.len()
    }

    pub fn estadisticas(&self) -> (u64, u64) {
        (self.golpes, self.fallos)
    }

    pub fn limpiar(&mut self) {
        self.entradas.clear();
        self.golpes = 0;
        self.fallos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{ApprovalLevel, ExecutionPolicy, Intent, Mode, OutputContract, VerificationMode, DecisionSource, Confidence, Level, Risk, ExecutionTarget, ModelTarget};

    fn decision() -> DecisionResult {
        DecisionResult {
            intent: Intent::Ask,
            level: Level::N1,
            risk: Risk::Low,
            skip_generative: false,
            execution_target: ExecutionTarget::Local,
            model_target: ModelTarget::Indistinto,
            tools: vec![],
            output_contract: OutputContract::Texto,
            verification: VerificationMode::Formato,
            confidence: Confidence(1.0),
            source: DecisionSource::Reglas,
            por_que: "prueba".into(),
            salida_directa: None,
            lectura_directa: None,
        }
    }

    fn pedido(msg: &str) -> BrainRequest {
        BrainRequest::nuevo("hatboo", Mode::CHAT, msg).con_policy(ExecutionPolicy::LocalPreferred)
    }

    #[test]
    fn la_clave_cambia_con_cualquier_pieza() {
        let base = CacheDecisiones::clave(&pedido("hola"));
        let otro_modo = CacheDecisiones::clave(&BrainRequest::nuevo("hatboo", Mode::WORK, "hola"));
        let otro_producto = CacheDecisiones::clave(&BrainRequest::nuevo("otro", Mode::CHAT, "hola"));
        let con_aprobacion = pedido("hola").con_aprobacion(ApprovalLevel::FullAccess);
        assert_ne!(base, otro_modo);
        assert_ne!(base, otro_producto);
        assert_ne!(base, CacheDecisiones::clave(&con_aprobacion));
        // Mismo pedido, misma firma, aunque cambie la puntuación del mensaje.
        assert_eq!(base, CacheDecisiones::clave(&pedido("¡HOLA!")));
    }

    #[test]
    fn guarda_y_recupera_lo_no_dudoso() {
        let mut c = CacheDecisiones::default();
        let k = CacheDecisiones::clave(&pedido("hola"));
        c.guardar(&k, &decision());
        assert_eq!(c.size(), 1);
        assert!(c.obtener(&k).is_some());
        let (g, f) = c.estadisticas();
        assert_eq!((g, f), (1, 0));
    }

    #[test]
    fn la_duda_no_se_cachea() {
        let mut c = CacheDecisiones::default();
        let mut d = decision();
        d.confidence = Confidence(0.6);
        let k = "x";
        c.guardar(k, &d);
        assert_eq!(c.size(), 0);
    }

    #[test]
    fn se_agota_sin_romperse() {
        let mut c = CacheDecisiones::nueva(2, Duration::from_secs(60));
        for i in 0..10 {
            c.guardar(&format!("k{i}"), &decision());
            assert!(c.size() <= 2);
        }
    }
}
