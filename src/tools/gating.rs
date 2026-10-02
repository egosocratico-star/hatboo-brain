//! La puerta. Dos controles distintos que no hay que mezclar:
//! - **dentro del Plan**: ¿esta tool está en `plan.tools`?
//! - **presupuesto**: ¿quedan llamadas / escrituras?
//!   Ambos se deciden en código, antes de que nada se ejecute.

use crate::api::vocab::ToolId;
use crate::planner::Plan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deciso {
    Permitida,
    /// Fuera del Plan: no se describe, no se ejecuta, y se emite `ToolDenegada`.
    RechazadaFueraDelPlan,
    /// El Plan la tenía, pero ya no queda presupuesto de acciones.
    RechazadaPresupuesto,
    /// Era una escritura y no quedan escrituras.
    RechazadaPresupuestoEscrituras,
}

impl Deciso {
    pub fn permitida(&self) -> bool {
        matches!(self, Deciso::Permitida)
    }
}

/// Contador de acciones de una corrida. No es reentrante a propósito: lo maneja
/// el runtime y cada reintento **nuevo** Plan trae su cuenta nueva.
pub struct Puerta<'a> {
    plan: &'a Plan,
    llamadas: u32,
    escrituras: u32,
    /// Si la tool escribe o no. La decide el producto (su catálogo), no el texto.
    escribe: Box<dyn Fn(&str) -> bool + Send + Sync + 'a>,
}

impl<'a> Puerta<'a> {
    pub fn nueva(
        plan: &'a Plan,
        escribe: impl Fn(&str) -> bool + Send + Sync + 'a,
    ) -> Puerta<'a> {
        Puerta {
            plan,
            llamadas: 0,
            escrituras: 0,
            escribe: Box::new(escribe),
        }
    }

    /// Arranca con el presupuesto ya gastado en rondas anteriores del **mismo**
    /// Plan. Sin esto, construir la puerta cada ronda pone los contadores a cero
    /// y `max_write_actions` se multiplica por el número de rondas.
    pub fn con_consumo(mut self, llamadas: u32, escrituras: u32) -> Self {
        self.llamadas = llamadas;
        self.escrituras = escrituras;
        self
    }

    /// Comprueba y **consume** presupuesto si pasa. Llamar a `autorizar` dos veces
    /// con la misma tool cuenta dos veces: es lo que se quiere.
    pub fn autorizar(&mut self, tool: &str) -> Deciso {
        if !self.plan.tools.iter().any(|t: &ToolId| t == tool) {
            return Deciso::RechazadaFueraDelPlan;
        }
        if self.llamadas + 1 > self.plan.max_tool_calls {
            return Deciso::RechazadaPresupuesto;
        }
        if (self.escribe)(tool) && self.escrituras + 1 > self.plan.max_write_actions {
            return Deciso::RechazadaPresupuestoEscrituras;
        }
        self.llamadas += 1;
        if (self.escribe)(tool) {
            self.escrituras += 1;
        }
        Deciso::Permitida
    }

    /// Solo mira, no gasta.
    pub fn cabe(&self, tool: &str) -> bool {
        self.plan.tools.iter().any(|t| t == tool)
            && self.llamadas < self.plan.max_tool_calls
            && !((self.escribe)(tool) && self.escrituras >= self.plan.max_write_actions)
    }

    pub fn consumo(&self) -> (u32, u32) {
        (self.llamadas, self.escrituras)
    }

    pub fn restantes(&self) -> (u32, u32) {
        (
            self.plan.max_tool_calls.saturating_sub(self.llamadas),
            self.plan.max_write_actions.saturating_sub(self.escrituras),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::vocab::{
        ExecutionTarget, Intent, Level, OutputContract, ThinkingLevel, VerificationMode,
    };

    fn plan(tools: &[&str], llamadas: u32, escrituras: u32) -> Plan {
        let mut p = Plan::firmar(
            Level::N2,
            Intent::Modify,
            "gemma3:1b".into(),
            "ollama".into(),
            ExecutionTarget::Local,
            4096,
            ThinkingLevel::Off,
            tools.iter().map(|t| t.to_string()).collect(),
            OutputContract::Patch,
            VerificationMode::Determinista,
            "prueba".into(),
        );
        p.max_tool_calls = llamadas;
        p.max_write_actions = escrituras;
        p
    }

    const ESCRITURAS: &[&str] = &["write_file", "run_command"];

    fn puerta(p: &Plan) -> Puerta<'_> {
        Puerta::nueva(p, |t| ESCRITURAS.contains(&t))
    }

    #[test]
    fn fuera_del_plan_ni_siquiera_se_descrive() {
        let p = plan(&["read_file"], 4, 0);
        let mut g = puerta(&p);
        assert_eq!(g.autorizar("borra_todo"), Deciso::RechazadaFueraDelPlan);
        // Y no gastó presupuesto.
        assert_eq!(g.consumo(), (0, 0));
    }

    #[test]
    fn las_llamadas_se_agotan() {
        let p = plan(&["read_file"], 2, 0);
        let mut g = puerta(&p);
        assert!(g.autorizar("read_file").permitida());
        assert!(g.autorizar("read_file").permitida());
        assert_eq!(g.autorizar("read_file"), Deciso::RechazadaPresupuesto);
        assert_eq!(g.consumo(), (2, 0));
        assert_eq!(g.restantes(), (0, 0));
    }

    #[test]
    fn las_escrituras_tienen_su_propio_tope() {
        let p = plan(&["read_file", "write_file"], 8, 1);
        let mut g = puerta(&p);
        assert!(g.autorizar("read_file").permitida(), "leer no gasta escrituras");
        assert_eq!(g.consumo(), (1, 0));
        assert!(g.autorizar("write_file").permitida());
        assert_eq!(g.autorizar("write_file"), Deciso::RechazadaPresupuestoEscrituras);
        assert_eq!(g.consumo(), (2, 1));
        // Quedan llamadas, pero no escrituras: `cabe` lo sabe.
        assert!(g.cabe("read_file"));
        assert!(!g.cabe("write_file"));
    }

    #[test]
    fn un_plan_sin_tools_no_deja_nada() {
        let p = plan(&[], 8, 3);
        let mut g = puerta(&p);
        assert_eq!(g.autorizar("read_file"), Deciso::RechazadaFueraDelPlan);
    }

    #[test]
    fn el_presupuesto_se_hereda_entre_rondas_del_mismo_plan() {
        // Reconstruir la puerta cada ronda poniendo los contadores a cero era el
        // agujero: tres escrituras firmadas acababan en una por ronda.
        let p = plan(&["write_file"], 8, 3);
        let mut ronda_1 = puerta(&p);
        for _ in 0..3 {
            assert!(ronda_1.autorizar("write_file").permitida());
        }
        let gastado = ronda_1.consumo();
        assert_eq!(gastado.1, 3);
        let mut ronda_2 = Puerta::nueva(&p, |t| t == "write_file").con_consumo(gastado.0, gastado.1);
        assert_eq!(
            ronda_2.autorizar("write_file"),
            Deciso::RechazadaPresupuestoEscrituras,
            "la ronda nueva no puede gastar escrituras que el Plan ya agotó"
        );
    }
}
