//! El `Armador`: `DecisionResult` + modelo elegido + consejo del Governor → `Plan`
//! validado y firmado. Si el plan sale inválido, se firma el **plan seguro** y se
//! avisa: §VIII del Canon.

use super::plan::{
    default_max_output, default_timeout_s, thinking_extra_tokens, Plan,
};
use super::validation::{validate_plan, PlanContext, PlanViolation};
use super::SCHEMA_VERSION;
use crate::api::request::BrainRequest;
use crate::api::vocab::{
    ExecutionTarget, KeepAlive, Level, ModelId, Risk, ThinkingLevel, ToolId,
};
use crate::decision::DecisionResult;
use crate::models::ModelInfo;
use crate::prompt::ContadorTokens;
use crate::resources::Consejo;

/// Qué hizo el armador: el plan y por qué ese modelo.
#[derive(Debug, Clone, PartialEq)]
pub struct Pie {
    pub plan: Plan,
    pub porque_este_modelo: String,
    pub descartados: Vec<(ModelId, String)>,
    /// `true` si hubo que firmar el plan seguro porque el pedido era inválido.
    pub degradado: bool,
    pub violacion: Option<PlanViolation>,
}

/// Qué vale lo que el usuario trae en este turno, medido con el contador del
/// producto. `historial` está en el mismo orden que `req.history`: el más antiguo
/// primero, así que lo que se descarta al recortar son los turnos viejos, no los
/// que acaban de pasar.
#[derive(Debug, Clone, Default)]
pub struct Carga {
    pub mensaje_tokens: u32,
    pub historial: Vec<u32>,
}

impl Carga {
    /// Cuántos turnos de los que llegan caben en `libre`, empezando por el más
    /// reciente. Devuelve (turnos, tokens).
    fn sufijo_que_cabe(&self, libre: u32) -> (u8, u32) {
        let mut usados = 0u32;
        let mut turnos = 0u8;
        for t in self.historial.iter().rev() {
            let nuevo = usados.saturating_add(*t);
            if nuevo > libre || turnos == u8::MAX {
                break;
            }
            usados = nuevo;
            turnos += 1;
        }
        (turnos, usados)
    }
}

/// Todo lo que el Armador recibe del runtime. Es un solo argumento a propósito:
/// eran ocho posiciones seguidas y dos parejas del mismo tipo se podían intercambiar
/// sin que el compilador se enterara.
pub struct Entrada<'a> {
    pub req: &'a BrainRequest,
    pub decision: &'a DecisionResult,
    pub modelo: &'a ModelInfo,
    pub consejo: &'a Consejo,
    /// El system tal cual se va a mandar. El Armador lo mide con el contador del
    /// producto en vez de recibir un número suelto: así no puede haber un
    /// `system_tokens` de relleno firmando que algo cabe.
    pub system: &'a str,
    pub carga: &'a Carga,
    pub ctx: &'a PlanContext<'a>,
}

/// Las magnitudes que el Armador mueve para que el turno quepa en `num_ctx`. Lo que
/// NO se mueve son `fijo` (el system y el mensaje de ahora) y `extra` (el
/// razonamiento firmado): recortar eso es mentirle al usuario.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Presupuesto {
    fijo: u32,
    extra: u32,
    num_ctx: u32,
    contexto: u32,
    max_output: u32,
}

/// Suelo por debajo del cual el plan no sirve para nada: 64 tokens de contexto no
/// dan ni para la cabecera de un archivo, y 64 de salida no dan una respuesta.
const SUELO: u32 = 64;

impl Presupuesto {
    fn usado(&self, historial: u32) -> u32 {
        self.fijo + historial + self.contexto + self.max_output + self.extra
    }

    /// Lo que queda para el historial, medido **sin** historial dentro: medido con
    /// él, la segunda vuelta del encaje vería 0 de holgura y lo tiraría entero.
    fn holgura(&self) -> u32 {
        self.num_ctx.saturating_sub(self.usado(0))
    }

    /// El orden del apriete es la prioridad: primero se corta el presupuesto de
    /// contexto (los archivos y las observaciones del prompt) y al final la salida.
    /// Al revés, un archivo grande se comía la respuesta del usuario.
    fn apretar(&mut self, historial: u32) {
        while self.usado(historial) > self.num_ctx && self.contexto > SUELO {
            self.contexto -= SUELO;
        }
        while self.usado(historial) > self.num_ctx && self.max_output > SUELO {
            self.max_output -= SUELO;
        }
    }
}

/// Cuánto historial entra y cuánto hay que apretar para que el turno quepa. Es un
/// punto fijo de dos piezas —el historial ocupa la holgura del presupuesto vigente,
/// y el apriete libera holgura— y para en cuatro vueltas porque el apriete solo
/// libera tokens, no los come.
fn encajar(mut p: Presupuesto, carga: &Carga) -> (Presupuesto, u8, u32) {
    let mut turnos = 0u8;
    let mut historial = 0u32;
    for _ in 0..4 {
        let antes = (turnos, historial, p.contexto, p.max_output);
        let (t, h) = carga.sufijo_que_cabe(p.holgura());
        turnos = t;
        historial = h;
        p.apretar(historial);
        if antes == (turnos, historial, p.contexto, p.max_output) {
            break;
        }
    }
    (p, turnos, historial)
}

pub struct Armador;

impl Armador {
    /// Une todo. §11: lo que va al modelo —system, el turno, el historial admitido,
    /// el presupuesto de contexto y la salida, contando el razonamiento como
    /// salida— no puede pasar del `num_ctx` firmado.
    pub fn armar(e: Entrada, contador: &dyn ContadorTokens) -> Pie {
        let nivel = e.decision.level;
        let system_tokens = contador.cuenta(e.system);
        let num_ctx = if e.consejo.cabe {
            e.consejo.num_ctx
        } else {
            nivel.num_ctx_minimo()
        };

        // thinking: techo del producto, mínimo del nivel, y lo que el modelo sabe
        // hacer. Si el modelo no razona, se ignora y queda dicho.
        let thinking = elegir_thinking(e.req, e.decision, e.modelo, e.consejo);
        let mut razon_ignorado = String::new();
        let thinking = if thinking != ThinkingLevel::Off && !e.modelo.supports_thinking {
            razon_ignorado = format!(" · «{}» no razona: thinking vuelve a off", e.modelo.id);
            ThinkingLevel::Off
        } else {
            thinking
        };

        let extra = thinking_extra_tokens(&thinking);
        // Lo que el turno trae y no se puede recortar: el system y lo que preguntó
        // ahora mismo. Hasta aquí no entraba en la cuenta, así que un plan firmado
        // como «cabe» podía mandar 1.500 tokens de mensaje que nadie presupuestó.
        let p = Presupuesto {
            fijo: system_tokens + e.carga.mensaje_tokens,
            extra,
            num_ctx,
            contexto: decision_a_contexto(e.decision),
            max_output: default_max_output(nivel),
        };
        let (p, turnos, historial) = encajar(p, e.carga);
        let (contexto, max_output) = (p.contexto, p.max_output);
        // El historial que se quedó fuera se dice en el `reason`, no se oculta: el
        // panel tiene que poder explicar por qué el modelo no acuerda algo que el
        // usuario sí dijo.
        let recorte = if usize::from(turnos) < e.carga.historial.len() {
            format!(
                " · historial recortado a {turnos} de {} turnos",
                e.carga.historial.len()
            )
        } else {
            String::new()
        };

        let tools = tools_del_plan(e.req, e.decision);
        let mut plan = Plan {
            schema_version: SCHEMA_VERSION,
            level: nivel,
            intent: e.decision.intent,
            model: e.modelo.id.clone(),
            provider: e.modelo.provider.clone(),
            execution_target: decision_target(e.req, e.decision, e.modelo),
            num_ctx,
            keep_alive: KeepAlive::PorDefecto,
            thinking,
            max_output_tokens: max_output,
            context_budget_tokens: contexto,
            tools,
            max_tool_calls: max_tool_calls(nivel, e.decision),
            max_write_actions: max_writes(nivel, e.decision),
            timeout_s: default_timeout_s(nivel),
            output_contract: e.decision.output_contract,
            verification: e.decision.verification,
            max_retries: nivel.reintentos(),
            escalate_to: None,
            reason: format!(
                "{} · {} · ctx {num_ctx} · salida {max_output}{}{}",
                e.decision.por_que,
                e.decision.level.etiqueta(),
                razon_ignorado,
                recorte
            ),
            plan_hash: None,
            parent_plan_hash: None,
            system_tokens,
            mensaje_tokens: e.carga.mensaje_tokens,
            historial_turnos: turnos,
            historial_tokens: historial,
            risk: e.decision.risk,
        };

        match validate_plan(&plan, e.ctx) {
            Ok(()) => {
                plan.calcular_hash();
                Pie {
                    porque_este_modelo: e.consejo.porque.clone(),
                    plan,
                    descartados: vec![],
                    degradado: false,
                    violacion: None,
                }
            }
            Err(v) => {
                let mut seguro = Plan::seguro(
                    &e.req.mode,
                    plan.model.clone(),
                    plan.provider.clone(),
                    tools_del_plan(e.req, e.decision),
                );
                seguro.system_tokens = system_tokens;
                // El plan seguro se manda CON EL MISMO turno: si aquí no se copia y
                // se encaja la carga, firma que cabe algo que nadie midió.
                seguro.mensaje_tokens = e.carga.mensaje_tokens;
                if e.consejo.cabe {
                    seguro.num_ctx = e.consejo.num_ctx;
                }
                let (sp, st, sh) = encajar(
                    Presupuesto {
                        fijo: system_tokens + e.carga.mensaje_tokens,
                        extra: thinking_extra_tokens(&seguro.thinking),
                        num_ctx: seguro.num_ctx,
                        contexto: seguro.context_budget_tokens,
                        max_output: seguro.max_output_tokens,
                    },
                    e.carga,
                );
                seguro.context_budget_tokens = sp.contexto;
                seguro.max_output_tokens = sp.max_output;
                seguro.historial_turnos = st;
                seguro.historial_tokens = sh;
                seguro.reason = format!("plan pedido inválido ({}); plan seguro", v.mensaje());
                let degradado = validate_plan(&seguro, e.ctx).is_err();
                seguro.calcular_hash();
                Pie {
                    plan: seguro,
                    porque_este_modelo: e.consejo.porque.clone(),
                    descartados: vec![],
                    degradado: !degradado,
                    violacion: Some(v),
                }
            }
        }
    }
}

fn decision_a_contexto(d: &DecisionResult) -> u32 {
    match d.level {
        Level::N0 => 128,
        Level::N1 => 384,
        Level::N2 => 1024,
        Level::N3 => 2048,
    }
}

/// El techo del producto manda; si el nivel lo apaga, se apaga.
fn elegir_thinking(
    req: &BrainRequest,
    d: &DecisionResult,
    _m: &ModelInfo,
    _c: &Consejo,
) -> ThinkingLevel {
    if matches!(d.level, Level::N0 | Level::N1) {
        return ThinkingLevel::Off;
    }
    let techo = req.thinking_ceiling.unwrap_or(ThinkingLevel::Off);
    if techo == ThinkingLevel::Off {
        return ThinkingLevel::Off;
    }
    // N2: off o low. N3: lo que pida el producto, sin pasar de su techo.
    match d.level {
        Level::N2 => techo.min(ThinkingLevel::Low),
        Level::N3 => techo,
        _ => ThinkingLevel::Off,
    }
}

fn decision_target(req: &BrainRequest, d: &DecisionResult, m: &ModelInfo) -> ExecutionTarget {
    if req.policies == crate::api::vocab::ExecutionPolicy::LocalOnly {
        return ExecutionTarget::Local;
    }
    if d.execution_target == ExecutionTarget::Api && !m.local {
        return ExecutionTarget::Api;
    }
    if m.local {
        ExecutionTarget::Local
    } else {
        ExecutionTarget::Api
    }
}

fn tools_del_plan(req: &BrainRequest, d: &DecisionResult) -> Vec<ToolId> {
    if !d.level.permite_tools() {
        return vec![];
    }
    // La intersección con lo que el producto ofrece: la decisión ya candidatesó
    // sobre esa lista, pero el approval puede haber cambiado entre los dos sitios.
    let disponibles: Vec<ToolId> = req.tools.ids();
    d.tools
        .iter()
        .filter(|t| disponibles.iter().any(|x| x == *t))
        .cloned()
        .collect()
}

fn max_tool_calls(nivel: Level, d: &DecisionResult) -> u32 {
    let base = super::plan::default_max_tool_calls(nivel);
    if d.risk == Risk::High && base > 4 {
        4
    } else {
        base
    }
}

fn max_writes(nivel: Level, d: &DecisionResult) -> u32 {
    let base = super::plan::default_max_writes(nivel);
    match d.risk {
        Risk::High => 1,
        Risk::Medium => base.min(2),
        Risk::Low => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un N1 normal y corriente: system 200, contexto 384, salida 512.
    fn presupuesto(num_ctx: u32) -> Presupuesto {
        Presupuesto {
            fijo: 200,
            extra: 0,
            num_ctx,
            contexto: 384,
            max_output: 512,
        }
    }

    #[test]
    fn el_historial_que_cabe_a_medias_no_se_tira_entero() {
        // La holgura se mide sin el historial dentro. Medida con él, la segunda
        // vuelta del punto fijo vería 352 de holgura, diría «no cabe ninguno» y se
        // quedaría fuera un turno que sí cabía.
        let carga = Carga {
            mensaje_tokens: 0,
            historial: vec![600, 600],
        };
        let (p, turnos, hist) = encajar(presupuesto(2048), &carga);
        assert_eq!(turnos, 1, "el turno más reciente entra");
        assert_eq!(hist, 600);
        assert!(p.usado(hist) <= p.num_ctx, "{p:?}");
    }

    #[test]
    fn se_empieza_por_el_final_del_historial_no_por_el_principio() {
        let carga = Carga {
            mensaje_tokens: 0,
            historial: vec![1000, 50, 50],
        };
        let (t, h) = carga.sufijo_que_cabe(100);
        assert_eq!((t, h), (2, 100), "los dos más recientes, no los dos primeros");
    }

    #[test]
    fn se_aprieta_el_contexto_antes_que_la_salida() {
        let carga = Carga {
            mensaje_tokens: 0,
            historial: vec![],
        };
        let mut p = presupuesto(2048);
        p.fijo = 1500;
        let (p, _, _) = encajar(p, &carga);
        assert_eq!(p.contexto, 64, "el presupuesto de archivos es lo primero que se corta");
        assert_eq!(p.max_output, 448, "la salida se toca solo con el contexto en el suelo");
        assert!(p.usado(0) <= p.num_ctx, "{p:?}");
    }

    #[test]
    fn un_mensaje_grande_no_puede_firmarse_como_si_cabiera() {
        // `fijo` es system + el mensaje del turno: 1900 de carga en un ctx de 2048
        // no deja ni respuesta. El encaje se planta en los suelos de 64 y la
        // invariante se sigue cumpliendo —que es lo que `validate_plan` mira—, y el
        // historial se queda fuera en vez de desbordar la ventana del modelo.
        let carga = Carga {
            mensaje_tokens: 0,
            historial: vec![500, 500],
        };
        let mut p = presupuesto(2048);
        p.fijo = 1900;
        let (p, turnos, hist) = encajar(p, &carga);
        assert_eq!((turnos, hist), (0, 0), "sin holgura, el historial se queda fuera");
        assert_eq!(p.contexto, 64, "el presupuesto de archivos es lo primero que cae");
        assert_eq!(p.max_output, 64, "y la salida después, no antes");
        assert!(p.usado(hist) <= p.num_ctx, "{p:?}");
    }
}
