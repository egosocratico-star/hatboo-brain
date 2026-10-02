//! El runtime: el bucle que administra la corrida. Decide, firma, prepara, genera,
//! verifica y se recupera. El modelo no manda el ciclo (§12 del plan).

use super::policy;
use crate::api::error::BrainError;
use crate::api::request::{BrainRequest, TrustState};
use crate::api::response::{
    BrainResult, DecisionTrace, Output, OutputStatus, TaskMetrics, ToolCall,
};
use crate::api::vocab::{
    ApprovalLevel, ExecutionTarget, FailureClass, ThinkingLevel, ToolId,
};
use crate::config::schema::BrainConfig;
use crate::context::{self, Prioridad};
use crate::decision::engine::Motor;
use crate::decision::rules::Reglas;
use crate::decision::DecisionResult;
use crate::models::{selector, Registry};
use crate::observability::events::{BrainEvent, CancelToken, Emitidor, VerificacionEvent};
use crate::observability::logger::{Bitacora, Registro};
use crate::observability::metrics::Referencia;
use crate::planner::{Armador, Plan, PlanContext};
use crate::prompt::{self, ContadorTokens, Identidad};
use crate::providers::{
    GenerationRequest, GenerationResult, ModelProvider, ProviderError, StreamDelta, ToolSchema,
};
use crate::recovery::{classifier, escalator::Accion, Escalador};
use crate::resources::{Governor, ResourceProbe};
use crate::tools::{Deciso, Herramientas, Puerta};
use crate::verification::{self, Candidato, Entorno as EntornoV, Lector, VerificationResult};
use crate::brain::state::EstadoTarea;
use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};

/// Rondas máximas de tool → modelo → tool. El Plan pone `max_tool_calls`; esto es
/// solo el seguro contra un bucle que el presupuesto no corta (medido: dos vueltas
/// idénticas ya son un bucle).
const MAX_RONDAS: u8 = 8;

/// Lo que el producto ejecuta. El Brain administra cuándo y con qué presupuesto;
/// el cómo, el dónde y el sandbox son del adaptador (§5 del plan).
#[async_trait]
pub trait EjecutarTool: Send + Sync {
    /// El resultado, en texto, es lo que el modelo lee en la vuelta siguiente.
    /// `Err` = la tool falló → clase `tool`.
    async fn ejecutar(&self, tool: &ToolId, args: &serde_json::Value) -> Result<String, String>;

    /// `true` deja la llamada pendiente en el resultado, sin ejecutar: la
    /// aprobación es del producto, nunca de este crate.
    fn requiere_aprobacion(&self, _tool: &ToolId, _args: &serde_json::Value) -> bool {
        false
    }
}

/// Con qué se construye un Brain: lo que el producto sabe y el crate no adivina.
pub struct Montaje {
    pub config: BrainConfig,
    pub proveedores: Vec<Arc<dyn ModelProvider>>,
    pub registry: Registry,
    pub reglas: Reglas,
    pub herramientas: Herramientas,
    pub sonda: Arc<dyn ResourceProbe>,
    pub contador: Arc<dyn ContadorTokens>,
    pub identidad: Identidad,
    pub ejecutor_tools: Option<Arc<dyn EjecutarTool>>,
    pub ejecutor_comandos: Option<Arc<dyn crate::verification::execution::Ejecutor>>,
    pub lector: Option<Arc<Lector>>,
    /// El sí explícito del usuario a que este pedido salga del equipo. Sin él,
    /// ninguna policy manda nada a una API (§15.2 del plan).
    pub consentimiento_api: bool,
}

impl Montaje {
    /// Lo mínimo para arrancar con un proveedor. Sin registry, los modelos se
    /// describen llamándolo; sin sonda, el Governor no afirma que nada quepa.
    pub fn de_proveedor(p: Arc<dyn ModelProvider>) -> Montaje {
        Montaje {
            config: BrainConfig::default(),
            proveedores: vec![p],
            registry: Registry::nuevo(Vec::new()),
            reglas: Reglas::default(),
            herramientas: Herramientas::default(),
            sonda: Arc::new(crate::resources::SinSonda),
            contador: Arc::new(prompt::Estimador),
            identidad: Identidad::default(),
            ejecutor_tools: None,
            ejecutor_comandos: None,
            lector: None,
            consentimiento_api: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct OpcionesDeCorrida {
    pub cancelar: CancelToken,
    pub eventos: Option<Arc<dyn Emitidor>>,
}

impl OpcionesDeCorrida {
    pub fn nueva() -> OpcionesDeCorrida {
        OpcionesDeCorrida::default()
    }

    pub fn con_eventos(mut self, e: Arc<dyn Emitidor>) -> OpcionesDeCorrida {
        self.eventos = Some(e);
        self
    }

    pub fn con_cancelacion(mut self, c: CancelToken) -> OpcionesDeCorrida {
        self.cancelar = c;
        self
    }
}

/// El cerebro. Se construye una vez por producto; `run` se llama por pedido.
pub struct Brain {
    motor: Mutex<Motor>,
    governor: Governor,
    pub config: BrainConfig,
    pub registry: Registry,
    proveedores: Vec<Arc<dyn ModelProvider>>,
    sonda: Arc<dyn ResourceProbe>,
    contador: Arc<dyn ContadorTokens>,
    identidad: Identidad,
    herramientas: Herramientas,
    ejecutor_tools: Option<Arc<dyn EjecutarTool>>,
    ejecutor_comandos: Option<Arc<dyn crate::verification::execution::Ejecutor>>,
    lector: Option<Arc<Lector>>,
    bitacora: Mutex<Bitacora>,
    consentimiento_api: bool,
}

impl Brain {
    pub fn nuevo(m: Montaje) -> Result<Brain, BrainError> {
        if m.proveedores.is_empty() {
            return Err(BrainError::Config(crate::config::ConfigError::SinBackend));
        }
        let bitacora = Bitacora::nueva(m.config.bitacora.clone()).map_err(|e| {
            BrainError::Config(crate::config::ConfigError::Incoherente(format!(
                "el log no se pudo abrir: {e}"
            )))
        })?;
        let motor = Motor::nuevo(m.reglas)
            .con_tope_de_duda(m.config.tope_de_duda)
            .con_cache(crate::decision::cache::CacheDecisiones::nueva(
                m.config.cache_max,
                std::time::Duration::from_secs(m.config.cache_ttl_s),
            ));
        Ok(Brain {
            motor: Mutex::new(motor),
            governor: Governor::nuevo(m.config.governor.clone()),
            config: m.config,
            registry: m.registry,
            proveedores: m.proveedores,
            sonda: m.sonda,
            contador: m.contador,
            identidad: m.identidad,
            herramientas: m.herramientas,
            ejecutor_tools: m.ejecutor_tools,
            ejecutor_comandos: m.ejecutor_comandos,
            lector: m.lector,
            bitacora: Mutex::new(bitacora),
            consentimiento_api: m.consentimiento_api,
        })
    }

    pub fn proveedor(&self, id: &str) -> Option<Arc<dyn ModelProvider>> {
        self.proveedores.iter().find(|p| p.id() == id).cloned()
    }

    pub fn proveedores(&self) -> &[Arc<dyn ModelProvider>] {
        &self.proveedores
    }

    pub fn reglas(&self) -> Reglas {
        self.motor.lock().unwrap().reglas_clon()
    }

    pub fn estadisticas_cache(&self) -> (usize, u64, u64) {
        self.motor.lock().unwrap().estadisticas_cache()
    }

    /// Decide y firma sin generar: es lo que la UI muestra antes de gastar tokens.
    pub async fn plan(&self, req: &BrainRequest) -> Result<Plan, BrainError> {
        req.validar()?;
        let decision = self.decidir(req);
        let (pie, _) = self.planificar(req, &decision).await?;
        Ok(pie.plan)
    }

    /// La traza de decisión (§7 del plan). Nunca toca al proveedor.
    pub async fn inspect(&self, req: &BrainRequest) -> Result<DecisionTrace, BrainError> {
        req.validar()?;
        let reglas = self.reglas();
        let senales = crate::decision::engine::senales(req, &reglas);
        let decision = self.decidir(req);
        let fast_path = (decision.source == crate::api::vocab::DecisionSource::FastPath)
            .then(|| decision.por_que.clone());
        let (pie, porque) = self.planificar(req, &decision).await?;
        Ok(DecisionTrace {
            senales,
            fast_path,
            decision,
            modelo_elegido: pie.plan.model.clone(),
            porque_este: porque,
            descartados: pie.descartados.clone(),
            contexto_rechazado: vec![],
            recuperacion: vec![],
        })
    }

    pub async fn run(&self, req: &BrainRequest) -> Result<BrainResult, BrainError> {
        self.run_with(req, OpcionesDeCorrida::nueva()).await
    }

    /// La corrida administrada. `Ok` siempre que haya salida que mostrar, incluso
    /// sin verificar; `Err` solo si no hay nada que enseñar (§1 del plan).
    ///
    /// `Completado` y `Cancelado` se emiten aquí, en un solo sitio: antes el
    /// consumidor del canal de eventos se quedaba esperando un final que el Brain
    /// ya había decidido (§7 lista los dos eventos).
    pub async fn run_with(
        &self,
        req: &BrainRequest,
        opts: OpcionesDeCorrida,
    ) -> Result<BrainResult, BrainError> {
        match self.correr(req, &opts).await {
            Ok(r) => {
                self.emitir(&opts, BrainEvent::Completado(r.metrics.clone()));
                Ok(r)
            }
            e @ Err(BrainError::Cancelled) => {
                self.emitir(&opts, BrainEvent::Cancelado);
                e
            }
            e => e,
        }
    }

    async fn correr(
        &self,
        req: &BrainRequest,
        opts: &OpcionesDeCorrida,
    ) -> Result<BrainResult, BrainError> {
        req.validar()?;
        let inicio = std::time::Instant::now();
        let mut decision = self.decidir(req);

        // `skip_generative` es una promesa: el que la pone trae la respuesta
        // calculada (`salida_directa`) o el archivo que hay que leer
        // (`lectura_directa`). Si no trae nada de los dos, la salida sale vacía,
        // así que la promesa se comprueba aquí y se cae al modelo.
        if decision.skip_generative
            && (decision.salida_directa.is_some() || decision.lectura_directa.is_some())
        {
            return self.sin_generativo(&decision, inicio, opts);
        }

        let mut escalador = Escalador::nuevo(decision.level.reintentos().max(1));
        let mut observaciones: Vec<String> = Vec::new();
        let mut reintentos: u8 = 0;
        let mut recargas: u32 = 0;
        let mut rondas: u8 = 0;
        let mut recuperacion: Vec<String> = Vec::new();
        // Presupuesto de tools del Plan vigente: se reinicia solo cuando cambia el
        // plan firmado. Reiniciarlo cada ronda multiplica `max_write_actions` por
        // el número de rondas.
        let mut consumo: (u32, u32) = (0, 0);
        let mut plan_vigente: Option<String> = None;
        // Última salida con su presupuesto, por si hay que entregar sin volver a
        // generar (rondas agotadas).
        let mut ultima: Option<(Plan, GenerationResult, Preparado)> = None;

        loop {
            if opts.cancelar.cancelado() {
                return Err(BrainError::Cancelled);
            }
            rondas += 1;
            if rondas > MAX_RONDAS {
                // El presupuesto de rondas se acabó. Si en alguna ronda hubo
                // salida, se entrega lo que hay —marcado como sin verificar—;
                // convertirlo en un error tiraría a la basura minutos de modelo.
                let (plan, g, preparado) = match ultima.take() {
                    Some(u) => u,
                    None => return Err(BrainError::Timeout),
                };
                recuperacion.push("rondas de herramientas agotadas".into());
                return Ok(self.resultado(
                    req,
                    &plan,
                    &g,
                    vec![],
                    VerificationResult::NoRequerida,
                    OutputStatus::SinVerificar,
                    inicio,
                    reintentos,
                    recargas,
                    &preparado,
                ));
            }

            let (pie, porque_modelo) = self.planificar(req, &decision).await?;
            let plan = pie.plan.clone();
            if plan_vigente.as_deref() != plan.plan_hash.as_deref() {
                plan_vigente = plan.plan_hash.clone();
                consumo = (0, 0);
            }
            if pie.degradado {
                recuperacion.push(plan.reason.clone());
            }
            self.emitir(opts, BrainEvent::PlanCreated(plan.clone()));
            self.emitir(opts, BrainEvent::ModeloElegido {
                modelo: plan.model.clone(),
                porque: porque_modelo,
                desvio: req
                    .preferred_model
                    .as_ref()
                    .map(|p| p != &plan.model)
                    .unwrap_or(false),
            });

            let preparado = self.preparar(req, &plan, &observaciones)?;
            if !preparado.residente && plan.execution_target == crate::api::vocab::ExecutionTarget::Local {
                recargas += 1;
            }

            let g = match self.generar(&plan, &preparado, req, opts).await {
                Ok(g) => g,
                Err(e) => {
                    let clase = classifier::clasificar(&classifier::Origen::Error(&e));
                    match escalador.decidir(clase) {
                        Accion::Reintentar { porque, intento, .. } => {
                            reintentos = intento;
                            recuperacion.push(porque);
                            self.emitir(opts, BrainEvent::Reintento { clase, intento });
                            continue;
                        }
                        Accion::NuevoPlan {
                            subir_tier,
                            porque,
                        } => {
                            reintentos += 1;
                            recuperacion.push(porque);
                            if subir_tier {
                                self.subir_tier(&mut decision, req);
                            }
                            continue;
                        }
                        Accion::Abortar { porque } => {
                            recuperacion.push(porque);
                            return Err(e);
                        }
                    }
                }
            };

            // Se guarda la ronda por si el presupuesto de rondas se agota después:
            // mejor entregar esto marcado como sin verificar que tirar los minutos.
            ultima = Some((plan.clone(), g.clone(), preparado.clone()));

            // Tools: se ejecutan lo que el Plan y el producto dejan; lo demás se
            // rechaza en código y se le dice al modelo.
            if !g.tool_calls.is_empty() {
                let (llamadas, algo_se_ejecuto, nuevo_consumo) = self
                    .herramientas_del_turno(&plan, &g, req, opts, consumo)
                    .await;
                consumo = nuevo_consumo;
                // Lo que devolvió cada tool entra en el turno siguiente como
                // observación. Sin esto el modelo nunca lee el resultado y repite
                // la misma llamada hasta agotar las rondas: medido, ocho
                // generaciones idénticas y un `Timeout` por cara.
                for c in llamadas.iter() {
                    if let Some(r) = &c.resultado {
                        observaciones.push(format!(
                            "{} devolvió: {}",
                            c.tool,
                            if r.chars().count() > 4000 {
                                r.chars().take(4000).collect::<String>()
                            } else {
                                r.clone()
                            }
                        ));
                    }
                }
                if algo_se_ejecuto {
                    continue;
                }
                // Nadie ejecutó: se devuelven las llamadas pendientes tal cual.
                return Ok(self.resultado(
                    req,
                    &plan,
                    &g,
                    llamadas,
                    VerificationResult::NoRequerida,
                    OutputStatus::Propuesto,
                    inicio,
                    reintentos,
                    recargas,
                    &preparado,
                ));
            }

            let verificacion = self.verificar_salida(req, &plan, &g.texto);
            self.emitir(
                opts,
                BrainEvent::Verificacion {
                    clase: if verificacion.es_pass() {
                        VerificacionEvent::Pass
                    } else if verificacion.es_unverifiable() {
                        VerificacionEvent::Unverifiable
                    } else {
                        VerificacionEvent::Fail
                    },
                    motivo: motivo_de(&verificacion),
                },
            );

            if verificacion.es_pass() {
                return Ok(self.resultado(
                    req,
                    &plan,
                    &g,
                    vec![],
                    verificacion,
                    OutputStatus::Verificado,
                    inicio,
                    reintentos,
                    recargas,
                    &preparado,
                ));
            }

            if let VerificationResult::Unverifiable { .. } = verificacion {
                // No es éxito y no es error: se entrega así, con el status honesto.
                return Ok(self.resultado(
                    req,
                    &plan,
                    &g,
                    vec![],
                    verificacion,
                    OutputStatus::SinVerificar,
                    inicio,
                    reintentos,
                    recargas,
                    &preparado,
                ));
            }

            if matches!(verificacion, VerificationResult::NoRequerida) {
                // El Plan no pidió verificación —es lo que firma un N0—, así que
                // no hay nada que reparar: reintentar sería cobrarle una segunda
                // llamada al modelo por un turno que no tenía nada que comprobar.
                return Ok(self.resultado(
                    req,
                    &plan,
                    &g,
                    vec![],
                    verificacion,
                    OutputStatus::Propuesto,
                    inicio,
                    reintentos,
                    recargas,
                    &preparado,
                ));
            }

            let (clase, porque) = match &verificacion {
                VerificationResult::Fail { clase, motivo } => (*clase, motivo.clone()),
                _ => (FailureClass::Formato, "sin clase".to_string()),
            };
            match escalador.decidir(clase) {
                Accion::Reintentar {
                    ajustes,
                    intento,
                    porque: por,
                } => {
                    reintentos = intento;
                    recuperacion.push(format!("{porque} → {por}"));
                    self.emitir(opts, BrainEvent::Reintento { clase, intento });
                    if ajustes.retractar {
                        self.emitir(opts, BrainEvent::StreamRetracted {
                            motivo: porque.clone(),
                        });
                    }
                    if ajustes.mas_contexto {
                        observaciones.push(
                            "La respuesta anterior no cumplió el contrato; se reintentó con más contexto.".into(),
                        );
                    } else {
                        // `porque` trae el motivo del veredicto, y ese motivo puede
                        // ser una línea del archivo o el stderr de un comando. Sin
                        // redactar, el reintento se lleva ese texto al proveedor.
                        observaciones.push(format!(
                            "Tu respuesta anterior fue rechazada: {}. Entrega la salida en el contrato pedido.",
                            crate::security::redact::texto(&porque)
                        ));
                    }
                    continue;
                }
                Accion::NuevoPlan {
                    subir_tier,
                    porque: por,
                } => {
                    reintentos += 1;
                    recuperacion.push(format!("{porque} → {por}"));
                    if subir_tier {
                        self.subir_tier(&mut decision, req);
                    }
                    continue;
                }
                Accion::Abortar { porque: por } => {
                    recuperacion.push(format!("{porque} → {por}"));
                    // Se entrega lo que hay, marcado como rechazado: no se finge
                    // que está listo.
                    return Ok(self.resultado(
                        req,
                        &plan,
                        &g,
                        vec![],
                        verificacion,
                        OutputStatus::Rechazado,
                        inicio,
                        reintentos,
                        recargas,
                        &preparado,
                    ));
                }
            }
        }
    }

    // ---- piezas internas ----

    fn decidir(&self, req: &BrainRequest) -> DecisionResult {
        self.motor.lock().unwrap().evaluar(req)
    }

    fn subir_tier(&self, d: &mut DecisionResult, req: &BrainRequest) {
        if let Some(s) = d.level.siguiente() {
            d.level = s;
            d.verification = s.verificacion_minima().max(d.verification);
            if d.tools.is_empty() && s.permite_tools() {
                d.tools = req.tools.ids();
            }
            d.por_que.push_str(" · escaló de nivel");
        }
    }

    /// Governor + Selector + Armador. Devuelve el plan firmado y el por qué.
    async fn planificar(
        &self,
        req: &BrainRequest,
        decision: &DecisionResult,
    ) -> Result<(crate::planner::Pie, String), BrainError> {
        let mut registry = self.registry.clone();
        if registry.modelos.is_empty() {
            let mut v = Vec::new();
            for p in &self.proveedores {
                v.extend(p.list_models().await.unwrap_or_default());
            }
            registry = Registry::nuevo(v);
        }
        let eleccion =
            selector::elegir(req, decision, &registry, &self.governor, self.sonda.as_ref())?;
        self.proveedor(&eleccion.modelo.provider).ok_or_else(|| {
            BrainError::InvalidPlan(crate::planner::PlanViolation::ProviderNotAllowed(
                eleccion.modelo.provider.clone(),
            ))
        })?;

        let target = crate::models::target_de(&eleccion.modelo);
        if !policy::destino_permitido(
            req.policies,
            target,
            self.hay_local_elegible(&registry, decision),
            self.consentimiento_api,
        ) {
            return Err(BrainError::InvalidPlan(
                crate::planner::PlanViolation::ProviderNotAllowed(eleccion.modelo.provider.clone()),
            ));
        }

        let tools_del_producto = req.tools.ids();
        // El system se mide antes de firmar: la invariante de presupuesto lo pide.
        let system = self.system_de(req, &decision.tools);
        let system_tokens = self.contador.cuenta(&system);
        let ctx = PlanContext {
            approval: req.approval_level,
            policy: req.policies,
            tools_del_producto: &tools_del_producto,
            modelos: &registry.modelos,
            ceiling: req.thinking_ceiling.or(self.config.thinking_ceiling),
            ctx_permitidos: &self.config.governor.ctx_permitidos,
        };
        let pie = Armador::armar(
            req,
            decision,
            &eleccion.modelo,
            &eleccion.consejo,
            system_tokens,
            &ctx,
        );
        Ok((pie, eleccion.porque))
    }

    fn hay_local_elegible(&self, registry: &Registry, d: &DecisionResult) -> bool {
        registry
            .modelos
            .iter()
            .any(|m| m.local && m.max_ctx >= d.level.num_ctx_minimo())
    }

    fn system_de(&self, req: &BrainRequest, tools: &[ToolId]) -> String {
        let md = req.project.as_ref().and_then(|p| match p.trust_state {
            TrustState::Aprobado => p.hatboo_md.as_deref(),
            _ => None,
        });
        let idioma = if crate::decision::engine::idiomas(&req.message).0 == "en" {
            "Responde en inglés."
        } else {
            "Responde en español."
        };
        prompt::build_system(&self.identidad, &req.mode, tools, md, None, idioma)
    }

    fn preparar(
        &self,
        req: &BrainRequest,
        plan: &Plan,
        observaciones: &[String],
    ) -> Result<Preparado, BrainError> {
        let estado = EstadoTarea::nuevo(req.message.clone());
        let mut piezas =
            context::sources::piezas_de_seguridad(req, &approval_texto(req.approval_level));
        piezas.extend(context::sources::piezas_del_pedido(&req.clone(), Some(&estado), None));
        for (i, o) in observaciones.iter().enumerate().rev() {
            piezas.push(crate::context::Pieza::nueva(
                Prioridad::Objetivo,
                format!("observación:{i}"),
                o.clone(),
            ));
        }
        let armado = context::armar(piezas, plan.context_budget_tokens, self.contador.as_ref());
        let system = self.system_de(req, &plan.tools);
        let ctx = armado.a_contexto(system.clone());
        let turno = crate::prompt::dynamic::texto_del_turno(&ctx, &req.message);
        let residente = self
            .sonda
            .cargados()
            .iter()
            .any(|c| c.id == plan.model && c.num_ctx == plan.num_ctx);
        Ok(Preparado {
            system,
            turno,
            rechazado: ctx.rechazado,
            residente,
        })
    }

    async fn generar(
        &self,
        plan: &Plan,
        p: &Preparado,
        req: &BrainRequest,
        opts: &OpcionesDeCorrida,
    ) -> Result<GenerationResult, BrainError> {
        let proveedor = self
            .proveedor(&plan.provider)
            .ok_or(BrainError::NoEligibleModel)?;
        let g = GenerationRequest {
            model: plan.model.clone(),
            system: p.system.clone(),
            prompt: p.turno.clone(),
            history: req.history.clone(),
            tools: self.schemas_de(&plan.tools),
            num_ctx: plan.num_ctx,
            keep_alive: plan.keep_alive,
            thinking: plan.thinking,
            max_output_tokens: plan.max_output_tokens,
            temperature: 0.0,
            seed: 42,
            timeout_s: plan.timeout_s,
        };
        if opts.eventos.is_some() {
            let mut stream = proveedor.stream(g).await.map_err(con_error)?;
            let mut texto = String::new();
            let mut final_r: Option<GenerationResult> = None;
            let inicio = std::time::Instant::now();
            // El timeout del proveedor va en la petición, pero si la conexión se
            // abre y no vuelve a enviar nada, ese timeout no llega a correr nunca:
            // el turno queda colgado hasta que alguien cancele a mano. Aquí se le
            // pone fecha de vencimiento al drenado del stream.
            let limite = std::time::Duration::from_secs(plan.timeout_s.max(1) as u64);
            loop {
                let queda = limite.saturating_sub(inicio.elapsed());
                if queda.is_zero() {
                    return Err(BrainError::Timeout);
                }
                if opts.cancelar.cancelado() {
                    return Err(BrainError::Cancelled);
                }
                match tokio::time::timeout(queda, stream.next()).await {
                    Err(_) => return Err(BrainError::Timeout),
                    Ok(None) => break,
                    Ok(Some(parte)) => match parte.map_err(con_error)? {
                        StreamDelta::Texto(t) => {
                            texto.push_str(&t);
                            self.emitir(opts, BrainEvent::Token(t));
                        }
                        StreamDelta::Razonamiento(t) => self.emitir(opts, BrainEvent::Reasoning(t)),
                        StreamDelta::Final(r) => final_r = Some(r),
                    },
                }
            }
            let mut r = final_r.ok_or_else(|| {
                BrainError::Provider(ProviderError::RespuestaInvalida(
                    "el stream terminó sin línea final".into(),
                ))
            })?;
            if r.texto.is_empty() {
                r.texto = texto;
            }
            if r.ttft_ms.is_none() {
                r.ttft_ms = Some(inicio.elapsed().as_millis() as u64);
            }
            Ok(r)
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(plan.timeout_s as u64 + 30),
                proveedor.generate(g),
            )
            .await
            .map_err(|_| BrainError::Timeout)?
            .map_err(con_error)
        }
    }

    async fn herramientas_del_turno(
        &self,
        plan: &Plan,
        g: &GenerationResult,
        req: &BrainRequest,
        opts: &OpcionesDeCorrida,
        consumo: (u32, u32),
    ) -> (Vec<ToolCall>, bool, (u32, u32)) {
        let mut puerta = Puerta::nueva(plan, |t| self.herramientas.escribe(t)).con_consumo(
            consumo.0,
            consumo.1,
        );
        let mut out = Vec::new();
        let mut algo = false;
        for l in &g.tool_calls {
            let decis = puerta.autorizar(&l.tool);
            if !decis.permitida() {
                self.emitir(opts, BrainEvent::ToolDenegada {
                    tool: l.tool.clone(),
                    contra_presupuesto: !matches!(decis, Deciso::RechazadaFueraDelPlan),
                });
                continue;
            }
            let necesita = self
                .ejecutor_tools
                .as_ref()
                .map(|e| e.requiere_aprobacion(&l.tool, &l.args))
                .unwrap_or(true)
                || matches!(
                    policy::permiso(
                        self.herramientas.escribe(&l.tool),
                        req.approval_level,
                        plan.risk,
                        true,
                    ),
                    crate::brain::policy::Permiso::RequiereAprobacion
                );
            if necesita {
                out.push(ToolCall {
                    tool: l.tool.clone(),
                    args: l.args.clone(),
                    resultado: None,
                    ok: false,
                });
                continue;
            }
            let Some(e) = &self.ejecutor_tools else {
                out.push(ToolCall {
                    tool: l.tool.clone(),
                    args: l.args.clone(),
                    resultado: None,
                    ok: false,
                });
                continue;
            };
            self.emitir(opts, BrainEvent::ToolLlamada {
                tool: l.tool.clone(),
                args: l.args.clone(),
            });
            match e.ejecutar(&l.tool, &l.args).await {
                Ok(salida) => {
                    algo = true;
                    out.push(ToolCall {
                        tool: l.tool.clone(),
                        args: l.args.clone(),
                        resultado: Some(corta(&salida, 4000)),
                        ok: true,
                    });
                }
                Err(err) => out.push(ToolCall {
                    tool: l.tool.clone(),
                    args: l.args.clone(),
                    resultado: Some(err),
                    ok: false,
                }),
            }
        }
        (out, algo, puerta.consumo())
    }

    fn schemas_de(&self, tools: &[ToolId]) -> Vec<ToolSchema> {
        tools
            .iter()
            .filter_map(|t| self.herramientas.find(t))
            .map(|t| ToolSchema {
                name: t.id.clone(),
                description: t.descripcion.clone(),
                parameters: if t.argumentos.is_object() {
                    t.argumentos.clone()
                } else {
                    serde_json::json!({"type":"object","properties":{}})
                },
            })
            .collect()
    }

    fn verificar_salida(&self, req: &BrainRequest, plan: &Plan, texto: &str) -> VerificationResult {
        let root = req.raiz().unwrap_or("");
        let comando = req.project.as_ref().and_then(|p| p.verify.check.as_deref());
        let e = EntornoV {
            comando,
            ejecutor: self.ejecutor_comandos.as_ref().map(|x| x.as_ref()),
            leer: self.lector.as_ref().map(|x| x.as_ref()),
            esquema: None,
            idioma_pedido: if crate::decision::engine::idiomas(&req.message).0 == "en" {
                Some("en")
            } else {
                Some("es")
            },
            timeout_s: plan.timeout_s,
            // El Brain no escribe archivos: aplicar el parche en una copia y
            // correr el comando ahí le toca al producto. Sin eso el comando
            // comprueba el estado anterior, y `determinista` lo dice en vez de
            // dar un Pass que no es.
            parche_aplicado: false,
        };
        let c = Candidato {
            texto,
            contrato: plan.output_contract,
            root: if root.is_empty() { None } else { Some(root) },
            archivo_objetivo: None,
        };
        verification::verificar(plan.verification, &c, &e)
    }

    #[allow(clippy::too_many_arguments)]
    fn resultado(
        &self,
        req: &BrainRequest,
        plan: &Plan,
        g: &GenerationResult,
        llamadas: Vec<ToolCall>,
        verificacion: VerificationResult,
        status: OutputStatus,
        inicio: std::time::Instant,
        reintentos: u8,
        recargas: u32,
        preparado: &Preparado,
    ) -> BrainResult {
        let metrics = self.metricas(plan, g, inicio, reintentos, recargas, preparado);
        let r = BrainResult {
            output: Output {
                texto: g.texto.clone(),
                status,
                tool_calls: llamadas,
                reasoning: g.razonamiento.clone(),
            },
            plan: plan.clone(),
            verification: verificacion,
            metrics,
        };
        self.registrar(req, plan, &r);
        r
    }

    fn metricas(
        &self,
        plan: &Plan,
        g: &GenerationResult,
        inicio: std::time::Instant,
        reintentos: u8,
        recargas: u32,
        preparado: &Preparado,
    ) -> TaskMetrics {
        let tokens = g.tokens_entrada.unwrap_or(0) + g.tokens_salida.unwrap_or(0);
        let coste = self
            .config
            .referencia(&plan.model)
            .map(|r: Referencia| {
                r.coste(
                    r.ram_gb,
                    inicio.elapsed().as_secs_f32(),
                    tokens,
                    reintentos,
                )
                .total
            });
        TaskMetrics {
            duracion_ms: inicio.elapsed().as_millis() as u64,
            ttft_ms: g.ttft_ms,
            tokens_entrada: g.tokens_entrada,
            tokens_salida: g.tokens_salida,
            tok_s: g.tok_s,
            // El crate no mide la RAM del proceso ajeno: la pone el producto en
            // su panel. Aquí se deja en None si no hay medida.
            ram_mb: None,
            recargas,
            reintentos,
            contexto_rechazado: preparado.rechazado.len() as u32,
            clase_fallo: None,
            coste,
        }
    }

    /// Un camino sin generativo: lo calculado en código va verificado; la lectura
    /// directa se devuelve como pendiente porque la ejecuta el producto.
    fn sin_generativo(
        &self,
        d: &DecisionResult,
        inicio: std::time::Instant,
        opts: &OpcionesDeCorrida,
    ) -> Result<BrainResult, BrainError> {
        let pendientes: Vec<ToolCall> = d
            .lectura_directa
            .iter()
            .map(|ruta| ToolCall {
                tool: "read_file".into(),
                args: serde_json::json!({ "path": ruta }),
                resultado: None,
                ok: false,
            })
            .collect();
        let calculado = d.salida_directa.clone();
        // El Fast Path decide su propio nivel. Envolverlo en `Plan::seguro` le
        // subía a N1 —o a N2 en modo trabajo— una decisión N0 que además no
        // llama a ningún modelo, y con eso el registro y la precisión de nivel
        // del arnés decían otra cosa.
        let mut plan = Plan::firmar(
            d.level,
            d.intent,
            "sin-modelo".into(),
            "ninguno".into(),
            ExecutionTarget::Local,
            d.level.num_ctx_minimo(),
            ThinkingLevel::Off,
            vec![],
            d.output_contract,
            d.verification,
            d.por_que.clone(),
        );
        plan.risk = d.risk;
        // 2048 es el suelo con el que la invariante de presupuesto de §11 se
        // sostiene sin proveedor delante.
        plan.num_ctx = plan.num_ctx.max(2048);
        plan.calcular_hash();
        let r = BrainResult {
            output: Output {
                texto: calculado.clone().unwrap_or_default(),
                status: if calculado.is_some() {
                    OutputStatus::Verificado
                } else {
                    OutputStatus::Propuesto
                },
                tool_calls: pendientes,
                reasoning: None,
            },
            plan,
            verification: VerificationResult::NoRequerida,
            metrics: TaskMetrics {
                duracion_ms: inicio.elapsed().as_millis() as u64,
                ..Default::default()
            },
        };
        self.emitir(opts, BrainEvent::Completado(r.metrics.clone()));
        Ok(r)
    }

    fn emitir(&self, opts: &OpcionesDeCorrida, e: BrainEvent) {
        if let Some(em) = &opts.eventos {
            em.emitir(e);
        }
    }

    fn registrar(&self, req: &BrainRequest, plan: &Plan, r: &BrainResult) {
        let mut reg = Registro::nuevo(&req.product);
        reg.intent = format!("{:?}", plan.intent).to_lowercase();
        reg.level = format!("{:?}", plan.level);
        reg.plan_hash = plan.plan_hash.clone();
        reg.parent_plan_hash = plan.parent_plan_hash.clone();
        reg.modelo = plan.model.clone();
        reg.provider = plan.provider.clone();
        reg.target = format!("{:?}", plan.execution_target).to_lowercase();
        reg.num_ctx = plan.num_ctx;
        reg.thinking = format!("{:?}", plan.thinking).to_lowercase();
        reg.tokens_entrada = r.metrics.tokens_entrada;
        reg.tokens_salida = r.metrics.tokens_salida;
        reg.contexto_rechazado = r.metrics.contexto_rechazado;
        reg.clase_fallo = match &r.verification {
            VerificationResult::Fail { clase, .. } => Some(format!("{clase:?}")),
            _ => None,
        };
        reg.latencia_ms = Some(r.metrics.duracion_ms);
        reg.ttft_ms = r.metrics.ttft_ms;
        reg.tok_s = r.metrics.tok_s;
        reg.ram_mb = r.metrics.ram_mb;
        reg.recargas = r.metrics.recargas;
        reg.verificacion = r.verification.etiqueta().to_string();
        reg.reintentos = r.metrics.reintentos;
        reg.coste = r.metrics.coste;
        reg.reason = plan.reason.clone();
        let mut b = match self.bitacora.lock() {
            Ok(b) => b,
            Err(_) => return,
        };
        let _ = b.registrar(reg);
    }
}

#[derive(Clone)]
struct Preparado {
    system: String,
    turno: String,
    rechazado: Vec<String>,
    residente: bool,
}

fn con_error(e: ProviderError) -> BrainError {
    BrainError::Provider(ProviderError::redactado(e))
}

fn corta(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n).collect();
    t.push_str(" […]");
    t
}

fn motivo_de(v: &VerificationResult) -> Option<String> {
    match v {
        VerificationResult::Fail { motivo, .. } => Some(motivo.clone()),
        VerificationResult::Unverifiable { motivo } => Some(format!("{motivo:?}")),
        _ => None,
    }
}

fn approval_texto(a: ApprovalLevel) -> String {
    match a {
        ApprovalLevel::AskAlways => "preguntar siempre",
        ApprovalLevel::ApproveForMe => "aprobar por mí",
        ApprovalLevel::AutoSandbox => "automático en sandbox",
        ApprovalLevel::FullAccess => "acceso total",
    }
    .to_string()
}
